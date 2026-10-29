#!/usr/bin/env python3
"""Verify the packaged file inventory, checksums, target, and normative spec."""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import tarfile


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--openapi", type=Path, required=True)
    parser.add_argument("--require-clean-source", action="store_true")
    args = parser.parse_args()
    root = args.root.resolve(strict=True)
    archive = args.archive.resolve(strict=True)
    sidecar = archive.with_name(archive.name + ".sha256")
    if sidecar.read_text().split()[0] != sha256(archive.read_bytes()):
        raise SystemExit("release archive checksum sidecar does not match archive bytes")
    provenance = json.loads((root / "PROVENANCE.json").read_text())
    if provenance["source_revision"] != args.source_revision:
        raise SystemExit("archive source revision does not match the reviewed checkout")
    if args.require_clean_source and provenance.get("source_tree_clean") is not True:
        raise SystemExit("release verification requires a clean tracked/untracked source tree")
    if provenance["target"] != args.target:
        raise SystemExit("archive target does not match the requested release target")
    if provenance["bit_for_bit_reproducibility_claimed"] is not False:
        raise SystemExit("unexpected bit-for-bit reproducibility claim")

    expected: dict[str, str] = {}
    for line in (root / "SHA256SUMS").read_text().splitlines():
        digest, relative = line.split("  ", 1)
        path = PurePosixPath(relative)
        if path.is_absolute() or ".." in path.parts or relative in expected:
            raise SystemExit(f"unsafe or duplicate release inventory path: {relative}")
        expected[relative] = digest
    if not expected:
        raise SystemExit("empty release checksum inventory")
    for relative, digest in expected.items():
        path = root.joinpath(*PurePosixPath(relative).parts)
        if not path.is_file() or sha256(path.read_bytes()) != digest:
            raise SystemExit(f"release checksum failed: {relative}")

    openapi = (root / "spec/openapi.yaml").read_bytes()
    if openapi != args.openapi.read_bytes():
        raise SystemExit("release OpenAPI bytes differ from checked-in spec/openapi.yaml")
    if sha256(openapi) != provenance["openapi_sha256"]:
        raise SystemExit("provenance OpenAPI checksum does not match packaged bytes")
    binary = root / "bin/ferl-server"
    if not binary.is_file() or not binary.stat().st_mode & 0o111:
        raise SystemExit("release ferl-server executable is missing or not executable")
    if sha256(binary.read_bytes()) != provenance["binary_sha256"]:
        raise SystemExit("provenance binary checksum does not match packaged executable")
    binary_bytes = binary.read_bytes()
    if binary_bytes[:4] != b"\x7fELF":
        raise SystemExit("release executable is not an ELF Linux binary")
    machine = int.from_bytes(binary_bytes[18:20], "little")
    expected_machine = {
        "aarch64-unknown-linux-gnu": 183,
        "x86_64-unknown-linux-gnu": 62,
    }.get(args.target)
    if machine != expected_machine:
        raise SystemExit("release ELF architecture does not match its provenance target")

    migrations = sorted(path.name for path in (root / "migrations").glob("*.sql"))
    if migrations != provenance["embedded_migrations"]:
        raise SystemExit("shipped migration inventory differs from provenance")
    repo_migrations = args.openapi.resolve().parents[1] / "migrations"
    for migration in migrations:
        if (root / "migrations" / migration).read_bytes() != (repo_migrations / migration).read_bytes():
            raise SystemExit(f"shipped migration differs from checked-in source: {migration}")
    with tarfile.open(archive, "r:gz") as tar:
        names = {member.name for member in tar.getmembers() if member.isfile()}
    expected_archive = set(expected) | {"SHA256SUMS"}
    if names != expected_archive:
        raise SystemExit("archive member list differs from the verified extracted inventory")

    print(f"verified archive={archive}")
    print(f"source_revision={args.source_revision}")
    print(f"source_tree_clean={str(provenance['source_tree_clean']).lower()}")
    print(f"target={args.target}")
    print(f"files={len(expected)}")
    print(f"openapi_sha256={sha256(openapi)}")
    print(f"binary_sha256={sha256(binary.read_bytes())}")


if __name__ == "__main__":
    main()
