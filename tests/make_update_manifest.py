"""Build a SHA-256 manifest for Rust release assets."""

import argparse
import hashlib
import json
import tomllib
from pathlib import Path
from urllib.parse import quote


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--directory", type=Path, required=True)
    args = parser.parse_args()
    version = args.version.removeprefix("v")
    package = tomllib.loads(Path("Cargo.toml").read_text())["package"]
    if version != package["version"]:
        parser.error("release tag must match Cargo.toml package version")
    if len(args.repository.split("/")) != 2:
        parser.error("repository must be OWNER/REPO")
    assets = {}
    for path in sorted(args.directory.glob("nezha-agent-rust-*")):
        if not path.is_file():
            continue
        target = path.name.removeprefix("nezha-agent-rust-").removesuffix(".exe")
        if target in assets:
            parser.error(f"duplicate target: {target}")
        assets[target] = {
            "url": (f"https://github.com/{args.repository}/releases/download/"
                    f"{quote(args.version)}/{quote(path.name)}"),
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        }
    if not assets:
        parser.error("no Rust agent release assets found")
    output = args.directory / "manifest.json"
    output.write_text(json.dumps({"version": version, "assets": assets}, indent=2) + "\n")
    print(f"wrote {output} with {len(assets)} targets")


if __name__ == "__main__":
    main()
