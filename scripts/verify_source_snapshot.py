#!/usr/bin/env python3
"""Check that a release build snapshot is byte-for-byte its committed Git tree."""

import argparse
import os
from pathlib import Path, PurePosixPath
import stat
import subprocess


def git(repository: Path, *arguments: str) -> bytes:
    return subprocess.run(
        ["git", "-C", str(repository), *arguments],
        check=True,
        capture_output=True,
    ).stdout


def committed_entries(repository: Path, revision: str) -> dict[str, tuple[bytes, bytes]]:
    entries: dict[str, tuple[bytes, bytes]] = {}
    for record in git(repository, "ls-tree", "-r", "-z", revision).split(b"\0"):
        if not record:
            continue
        metadata, raw_path = record.split(b"\t", 1)
        mode, kind, object_id = metadata.split()
        if kind != b"blob" or mode not in (b"100644", b"100755", b"120000"):
            raise SystemExit("release source snapshot contains an unsupported Git object")
        relative = raw_path.decode("utf-8")
        path = PurePosixPath(relative)
        if path.is_absolute() or ".." in path.parts or path.as_posix() != relative:
            raise SystemExit("committed release source contains a noncanonical path")
        contents = git(repository, "cat-file", "blob", object_id.decode("ascii"))
        entries[relative] = (mode, contents)
    if not entries:
        raise SystemExit("committed release source tree is empty")
    return entries


def snapshot_entries(snapshot: Path) -> set[str]:
    if not snapshot.is_dir() or snapshot.is_symlink():
        raise SystemExit("release source snapshot is not a real directory")
    entries: set[str] = set()
    for path in snapshot.rglob("*"):
        if path.is_symlink() or path.is_file():
            entries.add(path.relative_to(snapshot).as_posix())
        elif not path.is_dir():
            raise SystemExit("release source snapshot contains a special filesystem entry")
    return entries


def verify_snapshot(repository: Path, revision: str, snapshot: Path, require_read_only: bool = False) -> None:
    expected = committed_entries(repository.resolve(strict=True), revision)
    snapshot = snapshot.resolve(strict=True)
    actual_paths = snapshot_entries(snapshot)
    if actual_paths != set(expected):
        missing = sorted(set(expected) - actual_paths)
        unexpected = sorted(actual_paths - set(expected))
        raise SystemExit(f"release source snapshot inventory mismatch; missing={missing}, unexpected={unexpected}")
    for relative, (mode, expected_bytes) in expected.items():
        path = snapshot.joinpath(*PurePosixPath(relative).parts)
        if mode == b"120000":
            if not path.is_symlink() or os.readlink(path).encode() != expected_bytes:
                raise SystemExit(f"release source snapshot symlink differs from Git: {relative}")
        elif path.is_symlink() or not path.is_file() or path.read_bytes() != expected_bytes:
            raise SystemExit(f"release source snapshot bytes differ from Git: {relative}")
        elif bool(path.stat().st_mode & stat.S_IXUSR) != (mode == b"100755"):
            raise SystemExit(f"release source snapshot executable mode differs from Git: {relative}")
    if require_read_only:
        for path in [snapshot, *snapshot.rglob("*")]:
            if path.is_symlink():
                continue
            if path.stat().st_mode & (stat.S_IWUSR | stat.S_IWGRP | stat.S_IWOTH):
                raise SystemExit("release source snapshot is writable during its build")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", type=Path, required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--snapshot", type=Path, required=True)
    parser.add_argument("--require-read-only", action="store_true")
    args = parser.parse_args()
    verify_snapshot(args.repository, args.revision, args.snapshot, args.require_read_only)
    tree = git(args.repository, "rev-parse", f"{args.revision}^{{tree}}").decode().strip()
    print(f"verified_source_snapshot={args.snapshot}")
    print(f"source_revision={args.revision}")
    print(f"source_tree={tree}")


if __name__ == "__main__":
    main()
