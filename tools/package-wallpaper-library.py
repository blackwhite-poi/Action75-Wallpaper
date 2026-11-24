"""Stage a verified local copy of the wallpaper library and its ZIPs."""

import argparse
import json
from pathlib import Path, PurePosixPath
import runpy
import shutil
import tempfile
from urllib.parse import quote, unquote, urlsplit
from zipfile import ZIP_DEFLATED, ZipFile

from release_payload import ROOT, digest, inventory, wallpaper_abi, wallpaper_files


CHECK = runpy.run_path(str(ROOT / "tools/verify-native-wallpapers.py"))
EXTENSION = "meniscus-ferrofluid-v1"


def prepare(template, output, builtin_dir, ferrofluid, dumpbin, baseline=None):
    template = template.resolve(strict=True)
    output = output.resolve()
    if output.exists():
        raise FileExistsError(output)
    catalog = json.loads((template / "catalog.json").read_bytes())
    inventory_data = inventory()
    builtins = inventory_data["wallpapers"]
    items = catalog["wallpapers"]
    ids = [item["id"] for item in items]
    if len(ids) != len({pack_id.casefold() for pack_id in ids}) or set(ids) != {*builtins, EXTENSION}:
        raise ValueError("Library catalog must cover each built-in and the ferrofluid extension exactly once")
    assets = []
    used_names = set()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".action75-library-", dir=output.parent) as temp:
        staged = Path(temp) / "library"
        (staged / "packs").mkdir(parents=True)
        for item in items:
            pack_id = item["id"]
            pack = ferrofluid if pack_id == EXTENSION else builtin_dir / pack_id
            original = None
            if baseline is not None:
                original = (baseline / "examples/native-wallpapers/ferrofluid/Ferrofluid"
                            if pack_id == EXTENSION else baseline / "wallpapers-native" / pack_id)
            declared = wallpaper_files(inventory_data, pack_id, pack) if pack_id in builtins else None
            evidence = CHECK["check_pack"](pack, wallpaper_abi(inventory_data, pack_id), dumpbin,
                                            declared=declared, expected_id=pack_id, baseline=original)
            pack_name = PurePosixPath(unquote(urlsplit(item["download"]).path)).name
            if (not pack_name.endswith(".zip") or any(char in pack_name for char in '\\:*?"<>|')
                    or pack_name.casefold() in used_names):
                raise ValueError(f"Invalid or duplicate archive name: {pack_name}")
            used_names.add(pack_name.casefold())
            original_preview = CHECK["package_file"](template, item["preview"])
            preview = staged / item["preview"]
            preview.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(original_preview, preview)
            archive_path = staged / "packs" / pack_name
            with ZipFile(archive_path, "w", ZIP_DEFLATED, compresslevel=9) as archive:
                for relative, expected in sorted(evidence["assetsSha256"].items()):
                    source = CHECK["package_file"](pack.resolve(), relative)
                    archive.write(source, f"{pack_id}/{relative}")
                    if digest(source) != expected:
                        raise ValueError(f"Pack asset changed while staging: {source}")
            with ZipFile(archive_path) as archive:
                manifest = json.loads(archive.read(f"{pack_id}/manifest.json"))
                if manifest["id"] != pack_id or archive.testzip() is not None:
                    raise ValueError(f"Invalid library ZIP: {archive_path}")
            item.update(download=f"packs/{quote(pack_name)}", size=archive_path.stat().st_size,
                        sha256=digest(archive_path))
            assets.append({"id": pack_id, "file": f"packs/{pack_name}", "bytes": item["size"],
                           "sha256": item["sha256"], "binarySha256": evidence["binarySha256"],
                           "previewSha256": digest(preview)})
        (staged / "catalog.json").write_text(json.dumps(catalog, ensure_ascii=False, indent=2) + "\n",
                                             encoding="utf-8")
        (staged / "verification.json").write_text(json.dumps({"packs": assets}, ensure_ascii=False, indent=2) + "\n",
                                                  encoding="utf-8")
        staged.rename(output)
    return output


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--template", type=Path, required=True, help="Existing local catalog and preview directory.")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--builtins", type=Path, default=ROOT / "wallpapers-native")
    parser.add_argument("--ferrofluid", type=Path, default=ROOT / "examples/native-wallpapers/ferrofluid/Ferrofluid")
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--dumpbin", type=Path)
    args = parser.parse_args()
    print(prepare(args.template, args.output, args.builtins, args.ferrofluid,
                  args.dumpbin or CHECK["find_dumpbin"](), args.baseline))
