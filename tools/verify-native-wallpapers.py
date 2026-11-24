"""Check importable native packs without executing their DLLs."""

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import struct
import subprocess

from release_payload import ROOT, digest, inventory, wallpaper_abi, wallpaper_files


ENTRY_POINTS = {1: "Action75GetWallpaper", 2: "Action75GetWallpaper12"}
IMPORT_MODULE = re.compile(r"^[ \t]+([A-Za-z0-9_.-]+\.dll)[ \t]*$", re.MULTILINE | re.IGNORECASE)
EXPORT = re.compile(r"^[ \t]+\d+ +[0-9A-Fa-f]+ +[0-9A-Fa-f]{8} +([^\s=]+)(.*)$", re.MULTILINE)
LEGACY_GRAPHICS = re.compile(r"^(?:d3d(?:8|9|10|11)|d3dx|ddraw)", re.IGNORECASE)
SEPARATE_RUNTIME = re.compile(r"^(?:vcruntime|msvcp|concrt|msvcr\d|ucrtbased|lib(?:gcc|stdc\+\+|winpthread))",
                              re.IGNORECASE)


def find_dumpbin():
    if installed := shutil.which("dumpbin.exe"):
        return Path(installed)
    installer = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)"))
    vswhere = installer / "Microsoft Visual Studio/Installer/vswhere.exe"
    if vswhere.is_file():
        found = subprocess.check_output([
            vswhere, "-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-find", r"VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe",
        ], encoding="utf-8").splitlines()
        if found:
            return Path(found[0])
    raise FileNotFoundError("Use the Visual Studio x64 tools or pass --dumpbin")


def package_file(directory, value):
    if not isinstance(value, str) or not value or "\\" in value:
        raise ValueError(f"Invalid package relative path: {value!r}")
    relative = PurePosixPath(value)
    path = directory.joinpath(*relative.parts)
    if relative.is_absolute() or ".." in relative.parts or ":" in value or path.resolve() != path:
        raise ValueError(f"Package path must stay inside the pack: {value}")
    if not path.is_file():
        raise FileNotFoundError(path)
    return path


def pe_metadata(dumpbin, path):
    with path.open("rb") as stream:
        dos = stream.read(64)
        if len(dos) != 64 or dos[:2] != b"MZ":
            raise ValueError(f"Not a Windows PE image: {path}")
        stream.seek(struct.unpack_from("<I", dos, 60)[0])
        header = stream.read(26)
    if (len(header) != 26 or header[:4] != b"PE\0\0"
            or struct.unpack_from("<H", header, 4)[0] != 0x8664
            or not struct.unpack_from("<H", header, 22)[0] & 0x2000
            or struct.unpack_from("<H", header, 24)[0] != 0x20B):
        raise ValueError(f"Expected a Windows x64 PE32+ DLL: {path}")
    output = subprocess.check_output(
        [dumpbin, "/NOLOGO", "/EXPORTS", "/IMPORTS", path], encoding="utf-8", errors="replace")
    return ({name: suffix.strip() for name, suffix in EXPORT.findall(output)},
            sorted({name.casefold() for name in IMPORT_MODULE.findall(output)}))


def material_platforms(path):
    # Effekseer 1.80.8 eMCB stores a platform list, authored bytes, then platform chunks.
    data = path.read_bytes()
    if len(data) < 24 or data[:4] != b"eMCB":
        raise ValueError(f"Invalid Effekseer material cache: {path}")
    count = struct.unpack_from("<I", data, 16)[0]
    offset = 20 + 4 * count
    if not 0 < count <= 256 or offset + 4 > len(data):
        raise ValueError(f"Invalid material platform list: {path}")
    declared = set(struct.unpack_from(f"<{count}I", data, 20))
    original_size = struct.unpack_from("<I", data, offset)[0]
    offset += 4 + original_size
    chunks = set()
    while offset + 8 <= len(data):
        platform, size = struct.unpack_from("<II", data, offset)
        if not size or platform in chunks:
            raise ValueError(f"Invalid material platform chunk: {path}")
        chunks.add(platform)
        offset += 8 + size
    if offset != len(data) or declared != chunks:
        raise ValueError(f"Incomplete material platform chunks: {path}")
    return sorted(chunks)


def check_pack(path, abi, dumpbin, *, declared=None, expected_id=None, baseline=None):
    directory = path.resolve(strict=True)
    manifest = json.loads(package_file(directory, "manifest.json").read_bytes())
    pack_id = manifest["id"]
    if not isinstance(pack_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", pack_id):
        raise ValueError(f"Invalid pack ID: {pack_id!r}")
    if expected_id is not None and expected_id != pack_id:
        raise ValueError(f"Expected ID {expected_id}, found {pack_id}")
    renderer = manifest.get("renderer")
    if (manifest.get("version") != 1 or manifest.get("kind") != "native"
            or type(manifest.get("abi")) is not int or manifest["abi"] != abi
            or (abi == 2 and renderer != "d3d12") or (abi == 1 and renderer not in (None, "d3d11"))):
        raise ValueError(f"{pack_id}: expected native ABI {abi}, found {manifest.get('abi')}/{renderer}")
    entry = package_file(directory, manifest["entry"])
    referenced = {"manifest.json", entry.relative_to(directory).as_posix()}
    for key in ("settings", "preview"):
        if key in manifest:
            referenced.add(package_file(directory, manifest[key]).relative_to(directory).as_posix())
    if declared is not None:
        if not referenced.issubset(declared):
            raise ValueError(f"{pack_id}: assets missing from release-assets.json: {sorted(referenced - set(declared))}")
        assets = {name: digest(package_file(directory, name)) for name in declared}
    else:
        assets = {name: digest(package_file(directory, name))
                  for file in sorted(directory.rglob("*")) if file.is_file()
                  for name in [file.relative_to(directory).as_posix()]}
    if {file for file in assets if file.casefold().endswith(".dll")} != {manifest["entry"]}:
        raise ValueError(f"{pack_id}: pack must contain just its one entry DLL")
    if {file for file in assets if PurePosixPath(file).name.casefold() == "manifest.json"} != {"manifest.json"}:
        raise ValueError(f"{pack_id}: nested importable manifest in pack")
    if len(assets) != len({name.casefold() for name in assets}):
        raise ValueError(f"{pack_id}: duplicate asset names under Windows filename rules")
    exports, imports = pe_metadata(dumpbin, entry)
    if exports.get(ENTRY_POINTS[abi]) != "" or ENTRY_POINTS[3 - abi] in exports:
        raise ValueError(f"{pack_id}: expected the ABI {abi} entry point, found {exports}")
    if abi == 2 and (legacy := [module for module in imports if LEGACY_GRAPHICS.match(module)]):
        raise ValueError(f"{pack_id}: legacy graphics imports: {legacy}")
    materials = {}
    if abi == 2:
        binary = entry.read_bytes().lower()
        for symbol in ("D3D11On12CreateDevice", "D3D11CreateDevice", "d3d11on12.dll", "d3d11.dll"):
            if any(symbol.lower().encode(encoding) in binary for encoding in ("ascii", "utf-16-le")):
                raise ValueError(f"{pack_id}: DLL contains a legacy graphics loader reference: {symbol}")
        for name in assets:
            if name.casefold().endswith(".efkmatd"):
                platforms = material_platforms(package_file(directory, name))
                if 3 not in platforms:  # Effekseer CompiledMaterialPlatformType::DirectX12
                    raise ValueError(f"{pack_id}: stale material cache without a DX12 chunk: {name}")
                materials[name] = platforms
    system = Path(os.environ["SystemRoot"]) / "System32"
    unexpected = [
        module for module in imports
        if SEPARATE_RUNTIME.match(module) or not (
            module.startswith(("api-ms-win-", "ext-ms-win-")) or (system / module).is_file()
        )
    ]
    if unexpected:
        raise ValueError(f"{pack_id}: dependencies outside the Windows runtime: {unexpected}")
    result = {"id": pack_id, "package": str(directory), "abi": abi, "renderer": renderer or "d3d11",
              "entry": manifest["entry"], "exports": sorted(exports), "imports": imports,
              "binarySha256": digest(entry), "assetsSha256": assets, "materialPlatforms": materials}
    if baseline is not None:
        check_baseline(directory, manifest, assets, baseline)
        result["baseline"] = str(baseline.resolve())
    return result


def check_baseline(directory, manifest, assets, baseline):
    baseline = baseline.resolve(strict=True)
    previous = json.loads(package_file(baseline, "manifest.json").read_bytes())
    if previous["id"] != manifest["id"]:
        raise ValueError(f"Changed package ID: {previous['id']} → {manifest['id']}")
    if "settings" in previous:
        old = json.loads(package_file(baseline, previous["settings"]).read_bytes())
        new = json.loads(package_file(directory, manifest["settings"]).read_bytes())
        if old != new:
            raise ValueError(f"{previous['id']}: published settings schema or defaults changed")
    # A converted shader is a build output. Original image, model and effect assets stay byte-identical.
    runtime_assets = {".png", ".jpg", ".jpeg", ".webp", ".dds", ".tga", ".bmp", ".gif",
                      ".efk", ".efkefc", ".efkmodel", ".efkmat", ".glb", ".gltf", ".mp4", ".webm"}
    for path in baseline.rglob("*"):
        if path.is_file() and path.suffix.casefold() in runtime_assets:
            name = path.relative_to(baseline).as_posix()
            if assets.get(name) != digest(path):
                raise ValueError(f"{previous['id']}: original asset changed or was omitted: {name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dumpbin", type=Path)
    parser.add_argument("--builtins", action="store_true", help="Check all packs declared in release-assets.json.")
    parser.add_argument("--library-dir", type=Path, default=ROOT / "wallpapers-native")
    parser.add_argument("--pack", type=Path, action="append", default=[], help="Additional ABI 2 pack directory.")
    parser.add_argument("--compare", nargs=2, type=Path, action="append", default=[],
                        metavar=("PACK", "ORIGINAL"), help="Verify a candidate against an original pack.")
    parser.add_argument("--legacy-fixture", type=Path, action="append", default=[], help="Explicit ABI 1 test fixture.")
    parser.add_argument("--report", type=Path, required=True)
    options = parser.parse_args()
    dumpbin = options.dumpbin or find_dumpbin()
    entries = [(path, 2, {}) for path in options.pack]
    entries.extend((path, 2, {"baseline": baseline}) for path, baseline in options.compare)
    entries.extend((path, 1, {}) for path in options.legacy_fixture)
    if options.builtins:
        assets = inventory()
        declared = {key: wallpaper_files(assets, key, options.library_dir / key) for key in assets["wallpapers"]}
        if len(declared) != len({pack_id.casefold() for pack_id in declared}):
            raise ValueError("Duplicate built-in pack IDs under Windows filename rules")
        entries.extend((options.library_dir / pack_id, wallpaper_abi(assets, pack_id),
                        {"declared": names, "expected_id": pack_id})
                       for pack_id, names in sorted(declared.items()))
    if not entries:
        parser.error("Provide --builtins, --pack, --compare or --legacy-fixture")
    checked = []
    errors = []
    seen = set()
    for path, abi, arguments in entries:
        try:
            receipt = check_pack(path, abi, dumpbin, **arguments)
            key = (abi, receipt["id"].casefold())
            if key in seen:
                raise ValueError(f"Duplicate ABI {abi} pack ID: {receipt['id']}")
            seen.add(key)
            checked.append(receipt)
        except (KeyError, ValueError, OSError, subprocess.CalledProcessError) as error:
            errors.append({"path": str(path), "expectedAbi": abi, "error": str(error)})
    report = {"dumpbin": str(dumpbin), "verified": checked, "errors": errors}
    options.report.parent.mkdir(parents=True, exist_ok=True)
    options.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"{len(checked)} native packs verified, {len(errors)} errors; {options.report}")
    return bool(errors)


if __name__ == "__main__":
    raise SystemExit(main())
