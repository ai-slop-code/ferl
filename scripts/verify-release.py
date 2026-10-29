#!/usr/bin/env python3
"""Verify release provenance, extracted files, archive bytes, and target."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import tempfile


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
BUILDER_DIGESTS = {
    "aarch64-unknown-linux-gnu": "32f7944ed00949c44d3944a0a852b3e5ea862a610a38b43ac92a33aa87256b75",
    "x86_64-unknown-linux-gnu": "8bf7188ae46ad73b801c8d405eedc13ce63a7852b562885743a56890723549a8",
}


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_stream(stream) -> str:
    digest = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
        digest.update(chunk)
    return digest.hexdigest()


def safe_relative_path(value: str) -> PurePosixPath:
    path = PurePosixPath(value)
    if (
        not value
        or "\\" in value
        or path.is_absolute()
        or not path.parts
        or path.as_posix() != value
        or any(part in ("", ".", "..") for part in value.split("/"))
    ):
        raise SystemExit(f"unsafe release inventory path: {value}")
    return path


def archive_members(tar: tarfile.TarFile) -> tuple[dict[str, tarfile.TarInfo], set[str]]:
    files: dict[str, tarfile.TarInfo] = {}
    directories: set[str] = set()
    seen: set[str] = set()
    for member in tar.getmembers():
        path = safe_relative_path(member.name)
        relative = path.as_posix()
        if relative in seen:
            raise SystemExit(f"duplicate normalized release archive member: {relative}")
        seen.add(relative)
        if member.isdir():
            directories.add(relative)
        elif member.isfile():
            files[relative] = member
        else:
            raise SystemExit(f"unsupported non-file release archive member: {relative}")
    return files, directories


def parse_checksum_inventory(checksum_bytes: bytes) -> dict[str, str]:
    try:
        text = checksum_bytes.decode("ascii")
    except UnicodeDecodeError as error:
        raise SystemExit("release checksum inventory is not ASCII") from error
    expected: dict[str, str] = {}
    for line in text.splitlines():
        if "  " not in line:
            raise SystemExit("release checksum inventory has an invalid line")
        digest, relative = line.split("  ", 1)
        safe_relative_path(relative)
        if relative in expected or len(digest) != 64 or any(
            byte not in "0123456789abcdef" for byte in digest
        ):
            raise SystemExit(f"invalid or duplicate release checksum entry: {relative}")
        expected[relative] = digest
    if not expected:
        raise SystemExit("empty release checksum inventory")
    return expected


def git_output(repository: Path, *arguments: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repository), *arguments],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def verify_source_checkout(repository: Path, revision: str) -> str:
    actual_revision = git_output(repository, "rev-parse", "HEAD")
    if actual_revision != revision:
        raise SystemExit("release source revision does not match the current checkout")
    if git_output(repository, "status", "--porcelain", "--untracked-files=normal"):
        raise SystemExit("release verification requires a clean tracked/untracked source tree")
    return git_output(repository, "rev-parse", "HEAD^{tree}")


def expected_source_files(repository: Path, revision: str) -> set[str]:
    tracked = git_output(repository, "ls-tree", "-r", "--name-only", revision, "--", "migrations/")
    migrations = {line for line in tracked.splitlines() if line.endswith(".sql")}
    if not migrations or len(migrations) != len(tracked.splitlines()):
        raise SystemExit("committed source has a missing or unexpected migration inventory")
    return (FIXED_FILES - {"bin/ferl-server", "PROVENANCE.json"}) | migrations


def verify_migration_provenance(provenance: dict, source_files: set[str]) -> list[str]:
    migrations = sorted(
        path.split("/", 1)[1] for path in source_files if path.startswith("migrations/")
    )
    if not migrations or any(
        len(name) < 5 or not name[:4].isdigit() or not name.endswith(".sql")
        for name in migrations
    ):
        raise SystemExit("committed migration filenames do not have versioned SQL names")
    if migrations != provenance.get("embedded_migrations"):
        raise SystemExit("provenance migrations differ from the committed source inventory")
    if provenance.get("shipped_migrations") != [f"migrations/{name}" for name in migrations]:
        raise SystemExit("provenance shipped-migration paths do not match the committed source")
    if provenance.get("schema_version") != max(int(name[:4]) for name in migrations):
        raise SystemExit("provenance schema version does not match the committed migrations")
    return migrations


def verify_provenance(provenance: dict, archive_name: str, revision: str, target: str) -> None:
    if not isinstance(provenance, dict):
        raise SystemExit("release provenance is not a JSON object")
    if provenance.get("source_revision") != revision:
        raise SystemExit("archive source revision does not match the reviewed checkout")
    if provenance.get("source_tree_clean") is not True:
        raise SystemExit("release source-tree-clean provenance must be true")
    if provenance.get("target") != target:
        raise SystemExit("archive target does not match the requested release target")
    if provenance.get("artifact") != archive_name:
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
    }.get(target)
    if target_arch is None or target not in BUILDER_DIGESTS:
        raise SystemExit("release target is not in the supported Linux inventory")
    expected_builder = (
        "Rust/Cargo 1.99.1; pinned Debian Bookworm builder image "
        f"sha256:{BUILDER_DIGESTS[target]}; linux/{target_arch}"
    )
    if provenance["builder"] != expected_builder:
        raise SystemExit("release builder does not match the pinned Bookworm target builder")
    observed_toolchain = provenance["toolchain_observed"]
    if (
        "rustc 1.99.1 " not in observed_toolchain
        or "cargo 1.99.1 " not in observed_toolchain
        or f"host: {target}" not in observed_toolchain
        or "builder_os=debian/bookworm" not in observed_toolchain
    ):
        raise SystemExit("observed toolchain does not match the pinned Rust/Bookworm target")
    if provenance.get("openapi_path") != "spec/openapi.yaml":
        raise SystemExit("provenance OpenAPI path differs from the release inventory")
    receipt = provenance.get("build_receipt")
    if not isinstance(receipt, dict):
        raise SystemExit("release provenance lacks the observed build receipt")
    receipt_digest = hashlib.sha256(
        json.dumps(receipt, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    if provenance.get("build_receipt_sha256") != receipt_digest:
        raise SystemExit("release build receipt checksum does not match provenance")
    expected_image = (
        "docker.io/library/rust@sha256:" + BUILDER_DIGESTS[target]
    )
    observation = receipt.get("builder_observation")
    observed_toolchain_receipt = receipt.get("toolchain_observed")
    if (
        receipt.get("source_revision") != revision
        or receipt.get("source_tree") != provenance.get("source_tree")
        or receipt.get("source_snapshot_verified") is not True
        or receipt.get("target") != target
        or receipt.get("builder_image") != expected_image
        or provenance.get("builder_image") != expected_image
        or receipt.get("builder_platform") != f"linux/{target_arch}"
        or receipt.get("build_invocation") != provenance.get("build_invocation")
        or receipt.get("build_command")
        != f"cargo build --locked --release --target {target} --bin ferl-server"
        or receipt.get("build_command") != provenance.get("build_command")
        or not isinstance(observed_toolchain_receipt, str)
        or observed_toolchain_receipt.strip() != provenance["toolchain_observed"]
        or receipt.get("binary_sha256") != provenance.get("binary_sha256")
        or not isinstance(observation, dict)
        or any(
            observation.get(field) != receipt.get(field)
            for field in [
                "target",
                "builder_image",
                "builder_platform",
                "build_command",
                "binary_sha256",
            ]
        )
    ):
        raise SystemExit("release build receipt is not bound to the committed source, binary, target, and builder")


def preverify_archive(
    archive: Path,
    source_revision: str,
    target: str,
    openapi_source: Path,
    archive_stream,
) -> tuple[dict[str, tarfile.TarInfo], dict[str, str], bytes]:
    repository = openapi_source.resolve(strict=True).parents[1]
    archive_digest = sha256_stream(archive_stream)
    archive_stream.seek(0)
    sidecar = archive.with_name(archive.name + ".sha256")
    sidecar_fields = sidecar.read_text().split()
    if (
        len(sidecar_fields) != 2
        or sidecar_fields[1] != archive.name
        or len(sidecar_fields[0]) != 64
        or any(byte not in "0123456789abcdef" for byte in sidecar_fields[0])
        or sidecar_fields[0] != archive_digest
    ):
        raise SystemExit("release archive checksum sidecar does not match archive bytes")

    with tarfile.open(fileobj=archive_stream, mode="r:gz") as tar:
        files, directories = archive_members(tar)
        source_tree = verify_source_checkout(repository, source_revision)
        if "SHA256SUMS" not in files or "PROVENANCE.json" not in files:
            raise SystemExit("archive lacks its checksum inventory or provenance")
        sums_stream = tar.extractfile(files["SHA256SUMS"])
        provenance_stream = tar.extractfile(files["PROVENANCE.json"])
        if sums_stream is None or provenance_stream is None:
            raise SystemExit("archive inventory files cannot be read")
        checksum_bytes = sums_stream.read()
        expected = parse_checksum_inventory(checksum_bytes)
        if set(files) != set(expected) | {"SHA256SUMS"}:
            raise SystemExit("archive member list differs from its checksum inventory")
        if set(expected) != FIXED_FILES | {
            f"migrations/{Path(path).name}"
            for path in expected_source_files(repository, source_revision)
            if path.startswith("migrations/")
        }:
            raise SystemExit("archive checksum inventory differs from the exact committed release inventory")
        expected_directories = {
            parent.as_posix()
            for name in set(expected) | {"SHA256SUMS"}
            for parent in PurePosixPath(name).parents
            if parent.as_posix() != "."
        }
        if directories != expected_directories:
            raise SystemExit("archive contains missing or unexpected directory members")
        for relative, digest in expected.items():
            stream = tar.extractfile(files[relative])
            if stream is None or sha256(stream.read()) != digest:
                raise SystemExit(f"archive payload checksum mismatch: {relative}")
        provenance = json.loads(provenance_stream.read())
        verify_provenance(provenance, archive.name, source_revision, target)
        if provenance.get("source_tree") != source_tree:
            raise SystemExit("provenance source tree does not match the committed Git tree")
        if provenance.get("openapi_sha256") != sha256(openapi_source.read_bytes()):
            raise SystemExit("provenance OpenAPI checksum does not match checked-in bytes")
        if expected["spec/openapi.yaml"] != sha256(openapi_source.read_bytes()):
            raise SystemExit("archive OpenAPI bytes differ from checked-in spec/openapi.yaml")

        source_files = expected_source_files(repository, source_revision)
        for relative in source_files:
            committed = subprocess.run(
                ["git", "-C", str(repository), "show", f"{source_revision}:{relative}"],
                check=True,
                capture_output=True,
            ).stdout
            stream = tar.extractfile(files[relative])
            if stream is None or stream.read() != committed:
                raise SystemExit(f"release source file differs from committed Git content: {relative}")

        binary_stream = tar.extractfile(files["bin/ferl-server"])
        if binary_stream is None:
            raise SystemExit("release executable is absent from archive")
        binary_bytes = binary_stream.read()
        if sha256(binary_bytes) != provenance.get("binary_sha256"):
            raise SystemExit("provenance binary checksum does not match archive payload")
        if binary_bytes[:4] != b"\x7fELF":
            raise SystemExit("release executable is not an ELF Linux binary")
        expected_machine = ELF_MACHINES.get(target)
        if expected_machine is None or int.from_bytes(binary_bytes[18:20], "little") != expected_machine:
            raise SystemExit("release ELF architecture does not match its provenance target")
        verify_migration_provenance(provenance, source_files)
    return files, expected, checksum_bytes


def safe_extract_archive(
    archive: Path,
    destination: Path,
    source_revision: str,
    target: str,
    openapi_source: Path,
) -> None:
    archive = archive.resolve(strict=True)
    destination = destination.absolute()
    if destination.exists() or destination.is_symlink():
        raise SystemExit("release extraction destination must not already exist")
    destination.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=f".{destination.name}.extracting-", dir=destination.parent))
    try:
        with archive.open("rb") as archive_stream:
            initial_digest = sha256_stream(archive_stream)
            archive_stream.seek(0)
            _files, _, _ = preverify_archive(
                archive,
                source_revision,
                target,
                openapi_source,
                archive_stream,
            )
            archive_stream.seek(0)
            with tarfile.open(fileobj=archive_stream, mode="r:gz") as tar:
                staged_files, directories = archive_members(tar)
                for directory in sorted(directories, key=lambda value: (value.count("/"), value)):
                    staging.joinpath(*safe_relative_path(directory).parts).mkdir(parents=True, exist_ok=True)
                for relative, member in staged_files.items():
                    target_path = staging.joinpath(*safe_relative_path(relative).parts)
                    target_path.parent.mkdir(parents=True, exist_ok=True)
                    stream = tar.extractfile(member)
                    if stream is None:
                        raise SystemExit(f"archive member cannot be read: {relative}")
                    with target_path.open("xb") as output:
                        shutil.copyfileobj(stream, output)
                    target_path.chmod(member.mode & 0o777)
            archive_stream.seek(0)
            if sha256_stream(archive_stream) != initial_digest:
                raise SystemExit("release archive bytes changed during safe extraction")
        if destination.exists() or destination.is_symlink():
            raise SystemExit("release extraction destination appeared during extraction")
        os.rename(staging, destination)
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def verify_archive_members(archive: Path, expected: dict[str, str], checksum_bytes: bytes) -> None:
    with tarfile.open(archive, "r:gz") as tar:
        archive_files, directories = archive_members(tar)
        if set(archive_files) != set(expected) | {"SHA256SUMS"}:
            raise SystemExit("archive member list differs from the exact release inventory")
        expected_directories = {
            parent.as_posix()
            for name in set(expected) | {"SHA256SUMS"}
            for parent in PurePosixPath(name).parents
            if parent.as_posix() != "."
        }
        if directories != expected_directories:
            raise SystemExit("archive directory members differ from the exact release inventory")
        for relative, digest in expected.items():
            stream = tar.extractfile(archive_files[relative])
            if stream is None or sha256(stream.read()) != digest:
                raise SystemExit(f"archive payload differs from its extracted/checksummed file: {relative}")
        checksum_member = tar.extractfile(archive_files["SHA256SUMS"])
        if checksum_member is None or checksum_member.read() != checksum_bytes:
            raise SystemExit("archive checksum inventory differs from the extracted release")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path)
    parser.add_argument("--extract-to", type=Path)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--openapi", type=Path, required=True)
    parser.add_argument("--require-clean-source", action="store_true")
    args = parser.parse_args()
    archive = args.archive.resolve(strict=True)
    if args.extract_to is not None:
        safe_extract_archive(
            archive,
            args.extract_to,
            args.source_revision,
            args.target,
            args.openapi,
        )
        print(f"preverified_and_extracted={args.extract_to}")
        return
    if args.root is None:
        parser.error("--root is required unless --extract-to is selected")
    root = args.root.resolve(strict=True)
    repository = args.openapi.resolve(strict=True).parents[1]
    source_tree = verify_source_checkout(repository, args.source_revision)
    sidecar = archive.with_name(archive.name + ".sha256")
    sidecar_fields = sidecar.read_text().split()
    if len(sidecar_fields) != 2 or sidecar_fields[1] != archive.name:
        raise SystemExit("release archive checksum sidecar has an invalid format or filename")
    if sidecar_fields[0] != sha256(archive.read_bytes()):
        raise SystemExit("release archive checksum sidecar does not match archive bytes")

    provenance = json.loads((root / "PROVENANCE.json").read_text())
    verify_provenance(provenance, archive.name, args.source_revision, args.target)
    if provenance.get("source_tree") != source_tree:
        raise SystemExit("provenance source tree does not match the committed Git tree")
    if args.require_clean_source and provenance["source_tree_clean"] is not True:
        raise SystemExit("release verification requires a clean tracked/untracked source tree")

    expected = parse_checksum_inventory((root / "SHA256SUMS").read_bytes())

    root_files: set[str] = set()
    root_directories: set[str] = set()
    for path in root.rglob("*"):
        if path.is_symlink():
            raise SystemExit(f"release extraction contains a symlink: {path.relative_to(root)}")
        if path.is_file():
            root_files.add(path.relative_to(root).as_posix())
        elif path.is_dir():
            root_directories.add(path.relative_to(root).as_posix())
        else:
            raise SystemExit(f"release extraction contains a special member: {path.relative_to(root)}")
    if root_files != set(expected) | {"SHA256SUMS"}:
        raise SystemExit("extracted release has missing or unlisted files")
    expected_directories = {
        parent.as_posix()
        for name in set(expected) | {"SHA256SUMS"}
        for parent in PurePosixPath(name).parents
        if parent.as_posix() != "."
    }
    if root_directories != expected_directories:
        raise SystemExit("extracted release has missing or unexpected directories")
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

    source_files = expected_source_files(repository, args.source_revision)
    repo_migrations = sorted(path.split("/", 1)[1] for path in source_files if path.startswith("migrations/"))
    migrations = sorted(path.name for path in (root / "migrations").glob("*.sql"))
    if not migrations or migrations != repo_migrations:
        raise SystemExit("shipped migrations differ from the complete committed Git inventory")
    verify_migration_provenance(provenance, source_files)
    for migration in migrations:
        committed = subprocess.run(
            ["git", "-C", str(repository), "show", f"{args.source_revision}:migrations/{migration}"],
            check=True,
            capture_output=True,
        ).stdout
        if (root / "migrations" / migration).read_bytes() != committed:
            raise SystemExit(f"shipped migration differs from committed source: {migration}")

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
