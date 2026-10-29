#!/usr/bin/env python3
"""Create a timestamp-normalized ferl release archive and checksums."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import tarfile
import tempfile


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


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--source-tree-clean", choices=("true", "false"), required=True)
    parser.add_argument("--builder", required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()

    binary = args.binary.resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise SystemExit(f"release executable is absent or not executable: {binary}")
    output_dir = args.output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    version = "0.1.0"
    cargo_manifest = (ROOT / "Cargo.toml").read_text()
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
        migrations = sorted((ROOT / "migrations").glob("*.sql"))
        if not migrations:
            raise SystemExit("no checked-in migration SQL found")
        for migration in migrations:
            shutil.copyfile(migration, stage / "migrations" / migration.name)
        for relative in RELEASE_FILES:
            destination = stage / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / relative, destination)

        source_spec = (ROOT / "spec/openapi.yaml").read_bytes()
        packaged_spec = (stage / "spec/openapi.yaml").read_bytes()
        if source_spec != packaged_spec:
            raise SystemExit("packaged OpenAPI differs from the checked-in normative file")

        toolchain_path = next(
            (parent / "toolchain.txt" for parent in binary.parents if (parent / "toolchain.txt").is_file()),
            None,
        )
        toolchain = toolchain_path.read_text().strip() if toolchain_path else args.builder
        provenance = {
            "artifact": f"ferl-server-{version}-{args.target}.tar.gz",
            "source_revision": args.source_revision,
            "source_tree_clean": args.source_tree_clean == "true",
            "target": args.target,
            "builder": args.builder,
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

        archive_name = f"ferl-server-{version}-{args.target}.tar.gz"
        archive = output_dir / archive_name
        source_date_epoch = int(os.environ.get("SOURCE_DATE_EPOCH", "0"))
        with archive.open("wb") as raw:
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

    archive_digest = sha256(archive.read_bytes())
    (output_dir / f"{archive.name}.sha256").write_text(
        f"{archive_digest}  {archive.name}\n"
    )
    print(f"artifact={archive}")
    print(f"sha256={archive_digest}")
    print(f"source_revision={args.source_revision}")
    print(f"target={args.target}")


if __name__ == "__main__":
    main()
