"""Assemble the built-in library from the independently checked author builds."""

import argparse
import json
from pathlib import Path
import runpy
import shutil
import tempfile

from release_payload import ROOT, digest, inventory, wallpaper_abi, wallpaper_files


CHECK = runpy.run_path(str(ROOT / "tools/verify-native-wallpapers.py"))


def stage(sdk_output, destination):
    sdk_output = sdk_output.resolve(strict=True)
    destination = destination.resolve()
    if destination.exists():
        raise FileExistsError(destination)
    settings = inventory()
    aliases = settings.get("nativeBuildAliases", {})
    if not set(aliases).issubset(settings["wallpapers"]):
        raise ValueError("An author build alias must refer to a declared built-in pack")
    prebuilt = set(settings.get("prebuiltNativeWallpapers", ()))
    if not prebuilt.issubset(settings["wallpapers"]) or prebuilt.intersection(aliases):
        raise ValueError("A prebuilt pack must be a declared built-in without a build alias")
    results = {}
    for path in sdk_output.glob("*-pe.json"):
        receipt = json.loads(path.read_bytes())
        if receipt["errors"]:
            raise ValueError(f"Author build verification failed: {path}")
        for row in receipt["verified"]:
            if row["abi"] != 2:
                continue
            if row["id"] in results:
                raise ValueError(f"Duplicate author build for {row['id']}")
            source = Path(row["package"]).resolve(strict=True)
            if not source.is_relative_to(sdk_output):
                raise ValueError(f"Author pack is outside the verified build: {source}")
            results[row["id"]] = (source, row)

    dumpbin = CHECK["find_dumpbin"]()
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".action75-native-", dir=destination.parent) as scratch:
        temporary = Path(scratch) / "library"
        temporary.mkdir()
        for pack_id in sorted(settings["wallpapers"]):
            build_id = aliases.get(pack_id, pack_id)
            baseline = ROOT / "wallpapers-native" / pack_id
            if pack_id in prebuilt:
                source, receipt = baseline, None
            else:
                if build_id not in results:
                    raise ValueError(f"No passed author build for {pack_id} ({build_id})")
                source, receipt = results[build_id]
            target = temporary / pack_id
            target.mkdir()
            names = wallpaper_files(settings, pack_id, source if build_id == pack_id else baseline)
            if build_id == pack_id:
                for name in names:
                    file = CHECK["package_file"](source, name)
                    path = target / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(file, path)
                    if receipt is not None and digest(path) != receipt["assetsSha256"].get(name):
                        raise ValueError(f"Author build asset changed: {file}")
            else:
                manifest = json.loads((baseline / "manifest.json").read_bytes())
                entry = manifest["entry"]
                for name in names:
                    path = target / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    if name == "manifest.json":
                        manifest.update(abi=2, renderer="d3d12")
                        path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
                    elif name == entry:
                        shutil.copy2(CHECK["package_file"](source, receipt["entry"]), path)
                        if digest(path) != receipt["binarySha256"]:
                            raise ValueError(f"Author build DLL changed: {build_id}")
                    else:
                        shutil.copy2(CHECK["package_file"](baseline, name), path)
            CHECK["check_pack"](target, wallpaper_abi(settings, pack_id), dumpbin, declared=names,
                                expected_id=pack_id, baseline=baseline)
        temporary.rename(destination)
    return destination


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk-output", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    print(stage(args.sdk_output, args.output))
