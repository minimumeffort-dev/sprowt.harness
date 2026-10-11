"""Verify vendored skills against their lock manifest and optional upstream clones."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1] / "skills"


def verify(root=ROOT, sources=None):
    manifest = json.loads((root / "manifest.json").read_text())
    expected_paths = {f"{p['directory']}/{name}" for p in manifest["packages"] for name in p["files"]}
    actual_paths = {str(p.relative_to(root / "upstream")) for p in (root / "upstream").rglob("*") if p.is_file() or p.is_symlink()}
    if actual_paths != expected_paths:
        raise ValueError(f"upstream: missing {expected_paths - actual_paths}; extra {actual_paths - expected_paths}")
    count = 0
    for package in manifest["packages"]:
        base = root / "upstream" / package["directory"]
        expected = package["files"]
        source = sources.get(package["repository"]) if sources else None
        if source:
            revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True).strip()
            if revision != package["commit"]:
                raise ValueError(f"{source}: expected commit {package['commit']}, got {revision}")
        for relative, entry in expected.items():
            path = base / relative
            if path.is_symlink() or any(p.is_symlink() for p in path.parents if p.is_relative_to(root)):
                raise ValueError(f"{path}: upstream files must not be symlinks")
            content = path.read_bytes()
            if hashlib.sha256(content).hexdigest() != entry["sha256"]:
                raise ValueError(f"{path}: differs from the pinned upstream file")
            if source:
                original = subprocess.check_output(["git", "show", f"{package['commit']}:{entry['source']}"], cwd=source)
                if content != original:
                    raise ValueError(f"{path}: differs from upstream commit")
            count += 1
        if not (base / package["license"]).is_file():
            raise ValueError(f"{base}: license missing")
    return count


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sources", nargs=2, type=Path, metavar=("PONYTAIL", "CURSOR_PLUGINS"))
    args = parser.parse_args()
    sources = dict(zip(["https://github.com/dietrichgebert/ponytail", "https://github.com/cursor/plugins"], args.sources)) if args.sources else None
    print(f"Verified {verify(sources=sources)} pinned upstream files.")
