#!/usr/bin/env python3
"""Verify release provenance, extracted files, archive bytes, and target."""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import tarfile


FIXED_FILES = {
    "Cargo.lock",
    "rust-toolchain.toml",
    "spec/openapi.yaml",
    "docs/release.md",
    "docs/configuration.md",
    "docs/database.md",
    "docs/operations.md",
    "docs/analytics-reports.md",
    "docs/creation-allowance.md",
    "deploy/proxy/nginx.conf",
    "deploy/proxy/ferl-proxy-headers.conf",
    "deploy/proxy/static/index.html",
    "deploy/proxy/static/assets/example.js",
    "bin/ferl-server",
    "PROVENANCE.json",
}
ELF_MACHINES = {
    "aarch64-unknown-linux-gnu": 183,
    "x86_64-unknown-linux-gnu": 62,
}


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def safe_relative_path(value: str) -> PurePosixPath:
    path = PurePosixPath(value)
    if path.is_absolute() or not path.parts or ".." in path.parts or "." in path.parts:
        raise SystemExit(f"unsafe release inventory path: {value}")
    return path


def verify_archive_members(archive: Path, expected: dict[str, str], checksum_bytes: bytes) -> None:
    archive_files: dict[str, tarfile.TarInfo] = {}
    with tarfile.open(archive, "r:gz") as tar:
        seen_members: set[str] = set()
        for member in tar.getmembers():
            safe_relative_path(member.name.rstrip("/"))
            if member.name in seen_members:
                raise SystemExit(f"duplicate release archive member: {member.name}")
            seen_members.add(member.name)
            if member.isdir():
                continue
            if not member.isfile():
                raise SystemExit(f"unsupported non-file release archive member: {member.name}")
            archive_files[member.name] = member
        if set(archive_files) != set(expected) | {"SHA256SUMS"}:
            raise SystemExit("archive member list differs from the exact release inventory")
        for relative, digest in expected.items():
            stream = tar.extractfile(archive_files[relative])
            if stream is None or sha256(stream.read()) != digest:
                raise SystemExit(f"archive payload differs from its extracted/checksummed file: {relative}")
        checksum_member = tar.extractfile(archive_files["SHA256SUMS"])
        if checksum_member is None or checksum_member.read() != checksum_bytes:
            raise SystemExit("archive checksum inventory differs from the extracted release")


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
    sidecar_fields = sidecar.read_text().split()
    if len(sidecar_fields) != 2 or sidecar_fields[1] != archive.name:
        raise SystemExit("release archive checksum sidecar has an invalid format or filename")
    if sidecar_fields[0] != sha256(archive.read_bytes()):
        raise SystemExit("release archive checksum sidecar does not match archive bytes")

    provenance = json.loads((root / "PROVENANCE.json").read_text())
    if provenance.get("source_revision") != args.source_revision:
        raise SystemExit("archive source revision does not match the reviewed checkout")
    if type(provenance.get("source_tree_clean")) is not bool:
        raise SystemExit("release source-tree-clean provenance is not Boolean")
    if args.require_clean_source and provenance["source_tree_clean"] is not True:
        raise SystemExit("release verification requires a clean tracked/untracked source tree")
    if provenance.get("target") != args.target:
        raise SystemExit("archive target does not match the requested release target")
    if provenance.get("artifact") != archive.name:
        raise SystemExit("provenance artifact name does not match the release archive")
    if provenance.get("bit_for_bit_reproducibility_claimed") is not False:
        raise SystemExit("unexpected bit-for-bit reproducibility claim")
    if not isinstance(provenance.get("builder"), str) or not provenance["builder"]:
        raise SystemExit("release builder provenance is missing")
    if not isinstance(provenance.get("toolchain_observed"), str) or not provenance["toolchain_observed"]:
        raise SystemExit("observed build toolchain provenance is missing")
    target_arch = {
        "aarch64-unknown-linux-gnu": "arm64",
        "x86_64-unknown-linux-gnu": "amd64",
    }.get(args.target)
    if target_arch is None:
        raise SystemExit("release target is not in the supported Linux inventory")
    expected_builder = (
        "Rust/Cargo 1.99.1; pinned Debian Bookworm builder image "
        "sha256:48b7f435b8fc6378ce43b44f15e3a47fd8919b2825f835c39bd4bd3da50af5b8; "
        f"linux/{target_arch}"
    )
    if provenance["builder"] != expected_builder:
        raise SystemExit("release builder does not match the pinned Bookworm target builder")
    observed_toolchain = provenance["toolchain_observed"]
    if (
        "rustc 1.99.1 " not in observed_toolchain
        or "cargo 1.99.1 " not in observed_toolchain
        or f"host: {args.target}" not in observed_toolchain
        or "builder_os=debian/bookworm" not in observed_toolchain
    ):
        raise SystemExit("observed toolchain does not match the pinned Rust/Bookworm target")
    if provenance.get("openapi_path") != "spec/openapi.yaml":
        raise SystemExit("provenance OpenAPI path differs from the release inventory")

    expected: dict[str, str] = {}
    for line in (root / "SHA256SUMS").read_text().splitlines():
        digest, relative = line.split("  ", 1)
        safe_relative_path(relative)
        if relative in expected or len(digest) != 64 or any(
            byte not in "0123456789abcdef" for byte in digest
        ):
            raise SystemExit(f"invalid or duplicate release checksum entry: {relative}")
        expected[relative] = digest
    if not expected:
        raise SystemExit("empty release checksum inventory")

    root_files: set[str] = set()
    for path in root.rglob("*"):
        if path.is_symlink():
            raise SystemExit(f"release extraction contains a symlink: {path.relative_to(root)}")
        if path.is_file():
            root_files.add(path.relative_to(root).as_posix())
    if root_files != set(expected) | {"SHA256SUMS"}:
        raise SystemExit("extracted release has missing or unlisted files")
    for relative, digest in expected.items():
        path = root.joinpath(*safe_relative_path(relative).parts)
        if not path.is_file() or sha256(path.read_bytes()) != digest:
            raise SystemExit(f"release checksum failed: {relative}")

    openapi = (root / "spec/openapi.yaml").read_bytes()
    if openapi != args.openapi.read_bytes():
        raise SystemExit("release OpenAPI bytes differ from checked-in spec/openapi.yaml")
    if sha256(openapi) != provenance.get("openapi_sha256"):
        raise SystemExit("provenance OpenAPI checksum does not match packaged bytes")
    binary = root / "bin/ferl-server"
    if not binary.is_file() or not binary.stat().st_mode & 0o111:
        raise SystemExit("release ferl-server executable is missing or not executable")
    binary_bytes = binary.read_bytes()
    if sha256(binary_bytes) != provenance.get("binary_sha256"):
        raise SystemExit("provenance binary checksum does not match packaged executable")
    if binary_bytes[:4] != b"\x7fELF":
        raise SystemExit("release executable is not an ELF Linux binary")
    expected_machine = ELF_MACHINES.get(args.target)
    if expected_machine is None or int.from_bytes(binary_bytes[18:20], "little") != expected_machine:
        raise SystemExit("release ELF architecture does not match its provenance target")

    migrations = sorted(path.name for path in (root / "migrations").glob("*.sql"))
    repo_migrations_dir = args.openapi.resolve().parents[1] / "migrations"
    repo_migrations = sorted(path.name for path in repo_migrations_dir.glob("*.sql"))
    if not migrations or migrations != repo_migrations:
        raise SystemExit("shipped migrations differ from the complete checked-in SQL inventory")
    if migrations != provenance.get("embedded_migrations"):
        raise SystemExit("shipped migration inventory differs from provenance")
    if provenance.get("shipped_migrations") != [f"migrations/{name}" for name in migrations]:
        raise SystemExit("provenance shipped-migration paths do not match the release")
    if provenance.get("schema_version") != max(int(name[:4]) for name in migrations):
        raise SystemExit("provenance schema version does not match the shipped migrations")
    for migration in migrations:
        if (root / "migrations" / migration).read_bytes() != (
            repo_migrations_dir / migration
        ).read_bytes():
            raise SystemExit(f"shipped migration differs from checked-in source: {migration}")

    required_files = FIXED_FILES | {f"migrations/{name}" for name in migrations}
    if set(expected) != required_files:
        missing = sorted(required_files - set(expected))
        unexpected = sorted(set(expected) - required_files)
        raise SystemExit(f"release inventory mismatch; missing={missing}, unexpected={unexpected}")

    verify_archive_members(archive, expected, (root / "SHA256SUMS").read_bytes())

    print(f"verified archive={archive}")
    print(f"source_revision={args.source_revision}")
    print(f"source_tree_clean={str(provenance['source_tree_clean']).lower()}")
    print(f"target={args.target}")
    print(f"files={len(expected)}")
    print(f"openapi_sha256={sha256(openapi)}")
    print(f"binary_sha256={sha256(binary_bytes)}")


if __name__ == "__main__":
    main()
