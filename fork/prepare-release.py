#!/usr/bin/env python3
"""Prepare a new release-matched fork in a NEW worktree; never push or deploy."""
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import urllib.request

ROOT = Path(__file__).resolve().parent.parent


def git(*args, cwd=ROOT, check=True):
    return subprocess.run(["git", *args], cwd=cwd, check=check, text=True, capture_output=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag", nargs="?")
    parser.add_argument("directory", nargs="?", type=Path)
    parser.add_argument("--check", action="store_true", help="Read the newest upstream release without changing anything")
    args = parser.parse_args()
    metadata = json.loads((ROOT / "fork/release.json").read_text())
    if args.check:
        request = urllib.request.Request("https://api.github.com/repos/paradedb/paradedb/releases/latest", headers={"User-Agent": "pointsy-paradedb-fork"})
        with urllib.request.urlopen(request, timeout=30) as response:
            latest = json.load(response)["tag_name"]
        print(json.dumps({"pinned": metadata["upstreamTag"], "latest": latest, "updateAvailable": latest != metadata["upstreamTag"]}))
        return
    if not args.tag or not re.fullmatch(r"v\d+\.\d+\.\d+", args.tag) or not args.directory:
        parser.error("Provide a stable upstream tag vX.Y.Z and a new worktree directory")
    destination = args.directory.resolve()
    if destination.exists():
        parser.error("The destination must not already exist")
    git("fetch", "--no-tags", "https://github.com/paradedb/paradedb.git", f"refs/tags/{args.tag}:refs/tags/{args.tag}")
    commit = git("rev-parse", f"{args.tag}^{{commit}}").stdout.strip()
    branch = f"fork/pg18-{args.tag}-r1"
    git("worktree", "add", "-b", branch, str(destination), commit)
    applied = git("apply", "--3way", "--index", str(ROOT / "fork/physical-failover.patch"), cwd=destination, check=False)
    if applied.returncode:
        raise SystemExit(f"Patch conflict: review {destination}; nothing was pushed.\n{applied.stderr}")
    # Copy only fork-owned additions. Never replace upstream SQL, lockfiles or docs.
    for relative in ("fork", "tests/physical_failover"):
        shutil.copytree(ROOT / relative, destination / relative)
    for relative in ("docker/Dockerfile.experimental-ha", ".github/workflows/experimental-physical-failover.yml"):
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / relative, target)
    metadata.update(upstreamTag=args.tag, upstreamCommit=commit, extensionVersion=args.tag[1:], forkRevision=1)
    (destination / "fork/release.json").write_text(json.dumps(metadata, indent=2) + "\n")
    for name in (".gitignore", ".dockerignore"):
        path = destination / name
        text = path.read_text() if path.exists() else ""
        if "artifacts/physical-failover/" not in text.splitlines():
            path.write_text(text.rstrip() + "\nartifacts/physical-failover/\n")
    print(f"Prepared {branch} at {destination}. Review and commit; then push only this fork branch to run CI. No push, release, or deployment was performed.")


if __name__ == "__main__":
    main()
