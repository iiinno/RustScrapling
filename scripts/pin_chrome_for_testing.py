#!/usr/bin/env python3
"""Pin a Chrome for Testing version for `rustscrapling install` (Plan C-05).

Downloads each platform archive over verified HTTPS (streamed, not stored),
computes SHA-256 and size, and prepends the entry to
crates/rustscrapling-cli/src/chrome_for_testing.json.

  python3 scripts/pin_chrome_for_testing.py            # current Stable
  python3 scripts/pin_chrome_for_testing.py 156.0.8078.80
"""
import datetime
import hashlib
import json
import pathlib
import sys
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "crates/rustscrapling-cli/src/chrome_for_testing.json"
BASE = "https://googlechromelabs.github.io/chrome-for-testing/"
PLATFORMS = ("linux64", "mac-arm64", "mac-x64", "win64", "win32")


def main() -> int:
    if len(sys.argv) > 1:
        listing = json.load(urllib.request.urlopen(BASE + "known-good-versions-with-downloads.json"))
        entry = next(v for v in listing["versions"] if v["version"] == sys.argv[1])
        channel = None
    else:
        listing = json.load(urllib.request.urlopen(BASE + "last-known-good-versions-with-downloads.json"))
        entry, channel = listing["channels"]["Stable"], "Stable"
    downloads = {}
    for item in entry["downloads"]["chrome"]:
        if item["platform"] not in PLATFORMS:
            continue
        digest, size = hashlib.sha256(), 0
        with urllib.request.urlopen(item["url"]) as response:
            while chunk := response.read(1 << 20):
                digest.update(chunk)
                size += len(chunk)
        downloads[item["platform"]] = {"url": item["url"], "sha256": digest.hexdigest(), "size": size}
        print(item["platform"], digest.hexdigest(), size, file=sys.stderr)
    manifest = json.loads(MANIFEST.read_text())
    manifest["versions"] = [v for v in manifest["versions"] if v["version"] != entry["version"]]
    manifest["versions"].insert(0, {"version": entry["version"], "channel": channel,
                                    "recorded": datetime.date.today().isoformat(), "downloads": downloads})
    MANIFEST.write_text(json.dumps(manifest, indent=1) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
