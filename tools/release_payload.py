"""Stage the same distribution assets for installer builds and source archives."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]
MANAGER = Path("dist/Action75Manager/Action75Manager.exe")
PLAYER = Path("dist/Action75Native/Action75Native.exe")
WORKER = PLAYER.with_name("Action75WallpaperWorker.exe")
# Prefer the project compiler; CI and developer machines can use an installed copy.
_bundled_compiler = ROOT / "tools/vendor/InnoSetup/ISCC.exe"
_compiler_locations = [_bundled_compiler, shutil.which("ISCC.exe")]
for variable in ("ProgramFiles(x86)", "ProgramFiles"):
    if directory := os.getenv(variable):
        _compiler_locations.extend(Path(directory) / f"Inno Setup {major}/ISCC.exe" for major in (7, 6))
DEFAULT_COMPILER = next((Path(path) for path in _compiler_locations if path and Path(path).is_file()), _bundled_compiler)


def version(root=ROOT):
    cargo = (root / "apps/manager/Cargo.toml").read_text(encoding="utf-8")
    return tomllib.loads(cargo)["package"]["version"]


def inventory(root=ROOT):
    return json.loads((root / "tools/release-assets.json").read_text(encoding="utf-8"))


def wallpaper_abi(assets, pack_id):
    return 1 if pack_id in assets.get("prebuiltNativeWallpapers", ()) else 2


def wallpaper_files(assets, pack_id, directory, *, include_generated=True):
    names = assets["wallpapers"][pack_id]
    if not include_generated:
        return names
    generated = assets.get("generatedWallpaperFiles", {}).get(pack_id, [])
    present = [name for name in generated if (directory / name).is_file()]
    if present and len(present) != len(generated):
        raise ValueError(f"Incomplete generated assets in {directory}: {sorted(set(generated) - set(present))}")
    return [*names, *present]


def external_wallpaper_assets(root=ROOT):
    included = inventory(root)
    files = set()
    for pack_id, names in included.get("externalWallpaperFiles", {}).items():
        if not set(names).issubset(included["wallpapers"].get(pack_id, [])):
            raise ValueError(f"External assets for {pack_id} must appear in the release inventory")
        files.update(Path("wallpapers-native", pack_id, name) for name in names)
    return files


def release_asset(root, relative, wallpapers=None):
    if wallpapers is not None and relative.parts[0] == "wallpapers-native":
        return wallpapers.joinpath(*relative.parts[1:])
    return root / relative


def release_files(root=ROOT, *, include_external=True, wallpapers=None):
    root = root.resolve()
    wallpapers = wallpapers.resolve(strict=True) if wallpapers is not None else None
    included = inventory(root)
    files = [Path(name) for name in included["files"]]
    for pack_id in included["wallpapers"]:
        directory = Path("wallpapers-native") / pack_id
        names = wallpaper_files(included, pack_id, release_asset(root, directory, wallpapers),
                                include_generated=include_external)
        if "manifest.json" not in names:
            raise ValueError(f"{pack_id} is missing its manifest in release-assets.json")
        files.extend(directory / name for name in names)
    if len(set(files)) != len(files):
        raise ValueError("Duplicate paths in release-assets.json")
    if not include_external:
        files = set(files) - external_wallpaper_assets(root)
    for relative in files:
        source = release_asset(root, relative, wallpapers)
        if relative.is_absolute() or ".." in relative.parts or source.resolve() != source:
            raise ValueError(f"Release assets must be local files without redirects: {relative}")
        if not source.is_file():
            raise FileNotFoundError(source)
    for pack_id in included["wallpapers"]:
        manifest = release_asset(root, Path("wallpapers-native", pack_id, "manifest.json"), wallpapers)
        if json.loads(manifest.read_text(encoding="utf-8-sig"))["id"] != pack_id:
            raise ValueError(f"Wallpaper ID does not match the release inventory: {manifest}")
    return sorted(files)


def stage_assets(root, destination, *, wallpapers=None):
    files = release_files(root, wallpapers=wallpapers)
    destination.mkdir()
    for relative in files:
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(release_asset(root, relative, wallpapers), target)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def build_installer(compiler, receipt, output_dir, *, root=ROOT, qa_id=None, wallpapers=None):
    from release_build import validate_receipt

    compiler = compiler.resolve(strict=True)
    output_dir = output_dir.resolve()
    if qa_id is not None and not re.fullmatch(r"[a-f0-9]{32}", qa_id):
        raise ValueError("The QA installer ID must be a UUID in lowercase hex")
    build, binaries = validate_receipt(root, receipt, qa_id)
    app_version = build["version"]
    edition = "-QA" if qa_id is not None else ""
    filename = f"Action75-Wallpaper{edition}-Setup-{app_version}-x64.exe"
    output_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".action75-build-", dir=output_dir) as scratch:
        workspace = Path(scratch)
        payload = workspace / "payload"
        stage_assets(root, payload, wallpapers=wallpapers)
        subprocess.run([
            sys.executable, "-B", root / "tools/verify-native-wallpapers.py", "--builtins",
            "--library-dir", payload / "wallpapers-native", "--report", workspace / "native-pack-assets.json",
        ], check=True)
        for relative, source in binaries.items():
            target = payload / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            if digest(target) != build["sha256"][relative.as_posix()]:
                raise ValueError(f"Build output changed during staging: {source}")
        subprocess.run(
            [sys.executable, str(root / "tools/license-notices.py"), str(payload / "Licenses/Rust")],
            check=True,
        )
        pack_entries = workspace / "wallpaper-files.iss"
        write_pack_entries(root, pack_entries)
        definitions = [f"/DAppVersion={app_version}", f"/DPayloadDir={payload}",
                       f"/DPackEntries={pack_entries}"]
        if qa_id is not None:
            definitions.append(f"/DQaId={qa_id}")
        subprocess.run(
            [str(compiler), "/Q", f"/O{workspace}", *definitions, str(root / "tools/setup.iss")],
            check=True,
        )
        output = output_dir / filename
        (workspace / filename).replace(output)
    return output


def write_pack_entries(root, path):
    lines = []
    for pack_id in sorted(inventory(root)["wallpapers"]):
        if not re.fullmatch(r"[A-Za-z0-9_-]+", pack_id):
            raise ValueError(f"Invalid wallpaper ID: {pack_id}")
        directory = rf"wallpapers-native\{pack_id}"
        lines.append(
            rf'''Source: "{{#PayloadDir}}\{directory}\*"; DestDir: "{{app}}\{directory}"; '''
            rf'''Flags: ignoreversion recursesubdirs createallsubdirs; Check: CanInstallPack('{pack_id}')'''
        )
    path.write_text("\n".join(lines) + "\n", encoding="ascii")


def main():
    from release_build import build_release

    parser = argparse.ArgumentParser(description="Build an installer from one verified distribution build.")
    parser.add_argument("--compiler", type=Path, default=DEFAULT_COMPILER)
    parser.add_argument("--receipt", type=Path)
    parser.add_argument("--wallpapers", type=Path, help="Verified staging directory for all built-in packs.")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "release")
    parser.add_argument("--qa-id")
    args = parser.parse_args()
    if args.receipt:
        path = build_installer(args.compiler, args.receipt, args.output_dir, qa_id=args.qa_id,
                               wallpapers=args.wallpapers)
    else:
        with tempfile.TemporaryDirectory(prefix="action75-release-") as scratch:
            receipt = build_release(Path(scratch) / "build")
            path = build_installer(args.compiler, receipt, args.output_dir, qa_id=args.qa_id,
                                   wallpapers=args.wallpapers)
    print(path)


if __name__ == "__main__":
    main()
