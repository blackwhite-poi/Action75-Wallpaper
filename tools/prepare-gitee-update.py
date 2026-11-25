"""Generate the Gitee update manifest for a verified GitHub Release installer."""

import argparse
import hashlib
import json
from pathlib import Path
import re
from urllib.request import Request, urlopen


REPO = "blackwhite-poi/Action75-Wallpaper"
MANIFEST = "Action75-Wallpaper-Update.json"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("installer", type=Path)
    args = parser.parse_args()
    installer = args.installer
    match = re.fullmatch(r"Action75-Wallpaper-Setup-(\d+\.\d+\.\d+)-x64\.exe", installer.name)
    if not match:
        parser.error("Expected a stable Action75 x64 installer filename")
    tag = f"v{match[1]}"
    request = Request(
        f"https://api.github.com/repos/{REPO}/releases/tags/{tag}",
        headers={"Accept": "application/vnd.github+json", "User-Agent": "Action75-release"},
    )
    with urlopen(request, timeout=20) as response:
        release = json.load(response)
    if release["tag_name"] != tag or release["draft"] or release["prerelease"]:
        parser.error("GitHub does not expose a matching stable release")
    matches = [asset for asset in release["assets"] if asset["name"] == installer.name]
    if len(matches) != 1 or matches[0]["state"] != "uploaded":
        parser.error("GitHub does not expose exactly one complete installer")
    asset = matches[0]
    with installer.open("rb") as stream:
        digest = f"sha256:{hashlib.file_digest(stream, 'sha256').hexdigest()}"
    size = installer.stat().st_size
    if (size, digest) != (asset.get("size"), asset.get("digest")):
        parser.error("Installer size or SHA-256 differs from the official GitHub asset")
    output = installer.with_name(MANIFEST)
    output.write_text(json.dumps({"tag_name": tag, "size": size, "digest": digest}) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
