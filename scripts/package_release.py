#!/usr/bin/env python3
"""Create a timestamp-normalized ferl release archive and checksums."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import uuid


ROOT = Path(__file__).resolve().parents[1]
RELEASE_FILES = (
    Path("Cargo.lock"),
    Path("rust-toolchain.toml"),
    Path("spec/openapi.yaml"),
    Path("docs/release.md"),
    Path("docs/configuration.md"),
    Path("docs/database.md"),
    Path("docs/operations.md"),
    Path("docs/analytics-reports.md"),
    Path("docs/creation-allowance.md"),
    Path("deploy/proxy/nginx.conf"),
    Path("deploy/proxy/ferl-proxy-headers.conf"),
    Path("deploy/proxy/static/index.html"),
    Path("deploy/proxy/static/assets/example.js"),
)
BUILDERS = {
    "aarch64-unknown-linux-gnu": (
        "docker.io/library/rust@sha256:32f7944ed00949c44d3944a0a852b3e5ea862a610a38b43ac92a33aa87256b75",
        "linux/arm64",
        "arm64",
    ),
    "x86_64-unknown-linux-gnu": (
        "docker.io/library/rust@sha256:8bf7188ae46ad73b801c8d405eedc13ce63a7852b562885743a56890723549a8",
        "linux/amd64",
        "amd64",
    ),
}


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def canonical_json_sha256(value: dict) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return sha256(encoded)


def git_output(*arguments: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(ROOT), *arguments],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def fail_if_changed(revision: str, tree: str) -> None:
    if git_output("rev-parse", "HEAD") != revision:
        raise SystemExit("source revision changed while preparing the release")
    if git_output("rev-parse", "HEAD^{tree}") != tree:
        raise SystemExit("committed source tree changed while preparing the release")
    if git_output("status", "--porcelain", "--untracked-files=normal"):
        raise SystemExit("release packaging requires a clean tracked/untracked source tree")


def load_build_receipt(path: Path, revision: str, tree: str) -> tuple[dict, Path, str, str]:
    try:
        receipt = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit("release build receipt is missing or invalid") from error
    if not isinstance(receipt, dict):
        raise SystemExit("release build receipt is not a JSON object")
    target = receipt.get("target")
    if not isinstance(target, str) or target not in BUILDERS:
        raise SystemExit("release build receipt names an unsupported target")
    image, platform, architecture = BUILDERS[target]
    if receipt.get("source_revision") != revision or receipt.get("source_tree") != tree:
        raise SystemExit("release build receipt is not bound to the current committed source tree")
    if receipt.get("source_snapshot_verified") is not True:
        raise SystemExit("release build receipt lacks committed source snapshot verification")
    if receipt.get("builder_image") != image or receipt.get("builder_platform") != platform:
        raise SystemExit("release build receipt does not identify the pinned target builder")
    if receipt.get("build_command") != (
        f"cargo build --locked --release --target {target} --bin ferl-server"
    ):
        raise SystemExit("release build receipt does not identify the production build command")
    observation = receipt.get("builder_observation")
    if not isinstance(observation, dict) or any(
        observation.get(field) != receipt.get(field)
        for field in [
            "target",
            "builder_image",
            "builder_platform",
            "build_command",
            "binary_sha256",
        ]
    ):
        raise SystemExit("builder-observed invocation does not match the release receipt")
    invocation = receipt.get("build_invocation")
    if not isinstance(invocation, str) or not invocation:
        raise SystemExit("release build receipt has no invocation identifier")
    relative_value = receipt.get("binary_relative_path")
    if not isinstance(relative_value, str):
        raise SystemExit("release build receipt executable path is missing")
    relative = Path(relative_value)
    if relative.is_absolute() or ".." in relative.parts:
        raise SystemExit("release build receipt has an unsafe executable path")
    binary = (path.resolve().parent / relative).resolve(strict=True)
    try:
        binary.relative_to(path.resolve().parent)
    except ValueError:
        raise SystemExit("release executable is outside the recorded build output")
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise SystemExit("release build receipt executable is absent or not executable")
    binary_bytes = binary.read_bytes()
    if sha256(binary_bytes) != receipt.get("binary_sha256"):
        raise SystemExit("release executable bytes differ from the observed build receipt")
    observed_toolchain = receipt.get("toolchain_observed")
    toolchain_path = path.resolve().parent / "toolchain.txt"
    if not isinstance(observed_toolchain, str) or not observed_toolchain:
        raise SystemExit("release build receipt lacks an observed toolchain")
    if toolchain_path.read_text() != observed_toolchain:
        raise SystemExit("observed builder record differs from the release build receipt")
    if (
        "rustc 1.99.1 " not in observed_toolchain
        or "cargo 1.99.1 " not in observed_toolchain
        or f"host: {target}" not in observed_toolchain
        or "builder_os=debian/bookworm" not in observed_toolchain
    ):
        raise SystemExit("observed toolchain does not match pinned Rust/Bookworm target")
    builder = (
        f"Rust/Cargo 1.99.1; pinned Debian Bookworm builder image sha256:{image.split('@sha256:', 1)[1]}; "
        f"linux/{architecture}"
    )
    return receipt, binary, target, builder


def committed_migrations(revision: str) -> list[Path]:
    paths = git_output("ls-tree", "-r", "--name-only", revision, "--", "migrations/").splitlines()
    if not paths or any(not path.endswith(".sql") for path in paths):
        raise SystemExit("committed source has a missing or unexpected migration inventory")
    return [ROOT / path for path in sorted(paths)]


def committed_bytes(revision: str, relative: str) -> bytes:
    return subprocess.run(
        ["git", "-C", str(ROOT), "show", f"{revision}:{relative}"],
        check=True,
        capture_output=True,
    ).stdout


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--build-manifest", type=Path, required=True)
    parser.add_argument("--source-snapshot", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()

    actual_revision = git_output("rev-parse", "HEAD")
    actual_tree = git_output("rev-parse", "HEAD^{tree}")
    fail_if_changed(actual_revision, actual_tree)
    receipt, binary, target, builder = load_build_receipt(
        args.build_manifest, actual_revision, actual_tree
    )
    source_root = args.source_snapshot.resolve(strict=True)
    if not (source_root / "Cargo.toml").is_file() or not (source_root / "Cargo.lock").is_file():
        raise SystemExit("committed release source snapshot is incomplete")
    output_dir = args.output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    version = "0.1.0"
    cargo_manifest = (source_root / "Cargo.toml").read_text()
    for line in cargo_manifest.splitlines():
        if line.startswith("version = "):
            version = line.split('"')[1]
            break

    with tempfile.TemporaryDirectory(prefix="ferl-release-stage-", dir=output_dir) as temporary:
        stage = Path(temporary)
        (stage / "bin").mkdir()
        shutil.copyfile(binary, stage / "bin/ferl-server")
        os.chmod(stage / "bin/ferl-server", 0o755)
        (stage / "migrations").mkdir()
        committed_paths = committed_migrations(actual_revision)
        migrations = [source_root / "migrations" / path.name for path in committed_paths]
        if sorted(path.name for path in (source_root / "migrations").glob("*.sql")) != sorted(
            path.name for path in committed_paths
        ):
            raise SystemExit("source snapshot migrations differ from committed Git inventory")
        for migration in migrations:
            relative = f"migrations/{migration.name}"
            if migration.read_bytes() != committed_bytes(actual_revision, relative):
                raise SystemExit(f"source snapshot differs from committed Git content: {relative}")
            shutil.copyfile(migration, stage / "migrations" / migration.name)
        for relative in RELEASE_FILES:
            destination = stage / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            source = source_root / relative
            if source.read_bytes() != committed_bytes(actual_revision, relative.as_posix()):
                raise SystemExit(f"source snapshot differs from committed Git content: {relative}")
            shutil.copyfile(source, destination)

        source_spec = (source_root / "spec/openapi.yaml").read_bytes()
        packaged_spec = (stage / "spec/openapi.yaml").read_bytes()
        if source_spec != packaged_spec:
            raise SystemExit("packaged OpenAPI differs from the checked-in normative file")

        fail_if_changed(actual_revision, actual_tree)
        toolchain = receipt["toolchain_observed"].strip()
        provenance = {
            "artifact": f"ferl-server-{version}-{target}.tar.gz",
            "source_revision": actual_revision,
            "source_tree": actual_tree,
            "source_tree_clean": True,
            "target": target,
            "builder": builder,
            "builder_image": receipt["builder_image"],
            "build_invocation": receipt["build_invocation"],
            "build_command": receipt["build_command"],
            "build_receipt": receipt,
            "build_receipt_sha256": canonical_json_sha256(receipt),
            "toolchain_observed": toolchain,
            "schema_version": max(int(path.name[:4]) for path in migrations),
            "embedded_migrations": [path.name for path in migrations],
            "shipped_migrations": [f"migrations/{path.name}" for path in migrations],
            "openapi_path": "spec/openapi.yaml",
            "openapi_sha256": sha256(packaged_spec),
            "binary_sha256": sha256((stage / "bin/ferl-server").read_bytes()),
            "bit_for_bit_reproducibility_claimed": False,
        }
        (stage / "PROVENANCE.json").write_text(
            json.dumps(provenance, indent=2, sort_keys=True) + "\n"
        )

        payload = sorted(
            path for path in stage.rglob("*") if path.is_file() and path.name != "SHA256SUMS"
        )
        checksum_lines = []
        for path in payload:
            relative = path.relative_to(stage).as_posix()
            checksum_lines.append(f"{sha256(path.read_bytes())}  {relative}")
        (stage / "SHA256SUMS").write_text("\n".join(checksum_lines) + "\n")

        archive_name = f"ferl-server-{version}-{target}.tar.gz"
        archive = output_dir / archive_name
        source_date_epoch = int(os.environ.get("SOURCE_DATE_EPOCH", "0"))
        temporary_archive = output_dir / f".{archive.name}.{uuid.uuid4().hex}.tmp"
        with temporary_archive.open("xb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=source_date_epoch) as compressed:
                with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as tar:
                    for path in sorted(stage.rglob("*")):
                        relative = path.relative_to(stage).as_posix()
                        info = tar.gettarinfo(str(path), arcname=relative)
                        info.uid = 0
                        info.gid = 0
                        info.uname = ""
                        info.gname = ""
                        info.mtime = source_date_epoch
                        if path.is_file():
                            with path.open("rb") as stream:
                                tar.addfile(info, stream)
                        else:
                            tar.addfile(info)

        os.replace(temporary_archive, archive)

    archive_digest = sha256(archive.read_bytes())
    sidecar = output_dir / f"{archive.name}.sha256"
    temporary_sidecar = output_dir / f".{sidecar.name}.{uuid.uuid4().hex}.tmp"
    temporary_sidecar.write_text(f"{archive_digest}  {archive.name}\n")
    os.replace(temporary_sidecar, sidecar)
    print(f"artifact={archive}")
    print(f"sha256={archive_digest}")
    print(f"source_revision={actual_revision}")
    print(f"source_tree={actual_tree}")
    print(f"build_invocation={receipt['build_invocation']}")
    print(f"target={target}")


if __name__ == "__main__":
    main()
