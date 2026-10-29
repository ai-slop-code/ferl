#!/usr/bin/env python3
"""Regression controls for release archive verification."""

import hashlib
import io
import json
import runpy
import subprocess
import sys
import tempfile
import tarfile
import unittest
from pathlib import Path


VERIFY = runpy.run_path(str(Path(__file__).with_name("verify-release.py")))
PACKAGE = runpy.run_path(str(Path(__file__).with_name("package_release.py")))
SOURCE_SNAPSHOT = runpy.run_path(str(Path(__file__).with_name("verify_source_snapshot.py")))


class ReleaseArchiveVerificationTests(unittest.TestCase):
    def test_archive_payload_must_match_extracted_checksum_inventory(self) -> None:
        expected_payload = b"verified extracted payload"
        checksum_inventory = b"checksum inventory from extracted release\n"
        with tempfile.TemporaryDirectory(prefix="ferl-release-verifier-") as temporary:
            archive = Path(temporary) / "fixture.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                directory = tarfile.TarInfo("bin")
                directory.type = tarfile.DIRTYPE
                tar.addfile(directory)
                for name, payload in [
                    ("bin/ferl-server", b"different bytes in archive"),
                    ("SHA256SUMS", checksum_inventory),
                ]:
                    member = tarfile.TarInfo(name)
                    member.size = len(payload)
                    tar.addfile(member, io.BytesIO(payload))

            with self.assertRaisesRegex(SystemExit, "archive payload differs"):
                VERIFY["verify_archive_members"](
                    archive,
                    {"bin/ferl-server": hashlib.sha256(expected_payload).hexdigest()},
                    checksum_inventory,
                )

    def test_archive_rejects_traversal_and_link_members(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ferl-release-verifier-") as temporary:
            archive = Path(temporary) / "unsafe.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                member = tarfile.TarInfo("../outside")
                member.size = 1
                tar.addfile(member, io.BytesIO(b"x"))

            with self.assertRaisesRegex(SystemExit, "unsafe release inventory path"):
                VERIFY["verify_archive_members"](archive, {}, b"")

    def test_archive_rejects_normalized_aliases_and_special_members(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ferl-release-verifier-") as temporary:
            archive = Path(temporary) / "aliases.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                for name in ["bin/./ferl-server", "bin/ferl-server"]:
                    member = tarfile.TarInfo(name)
                    member.size = 1
                    tar.addfile(member, io.BytesIO(b"x"))
            with self.assertRaisesRegex(SystemExit, "unsafe release inventory path"):
                VERIFY["verify_archive_members"](archive, {}, b"")

            archive = Path(temporary) / "duplicate.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                for _ in range(2):
                    member = tarfile.TarInfo("bin/ferl-server")
                    member.size = 1
                    tar.addfile(member, io.BytesIO(b"x"))
            with self.assertRaisesRegex(SystemExit, "duplicate normalized"):
                VERIFY["verify_archive_members"](archive, {}, b"")

            archive = Path(temporary) / "link.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                member = tarfile.TarInfo("bin/ferl-server")
                member.type = tarfile.SYMTYPE
                member.linkname = "../../outside"
                tar.addfile(member)
            with self.assertRaisesRegex(SystemExit, "unsupported non-file"):
                VERIFY["verify_archive_members"](archive, {}, b"")

            archive = Path(temporary) / "special.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                member = tarfile.TarInfo("device")
                member.type = tarfile.CHRTYPE
                tar.addfile(member)
            with self.assertRaisesRegex(SystemExit, "unsupported non-file"):
                VERIFY["verify_archive_members"](archive, {}, b"")

            archive = Path(temporary) / "unexpected-directory.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                directory = tarfile.TarInfo("unexpected")
                directory.type = tarfile.DIRTYPE
                tar.addfile(directory)
                sums = b""
                member = tarfile.TarInfo("SHA256SUMS")
                member.size = len(sums)
                tar.addfile(member, io.BytesIO(sums))
            with self.assertRaisesRegex(SystemExit, "directory members"):
                VERIFY["verify_archive_members"](archive, {}, sums)

    def test_unsafe_archive_is_rejected_before_extraction_writes_anything(self) -> None:
        repository = Path(__file__).resolve().parents[1]
        revision = subprocess.run(
            ["git", "-C", str(repository), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        with tempfile.TemporaryDirectory(prefix="ferl-safe-extract-") as temporary:
            directory = Path(temporary)
            archive = directory / "unsafe.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                member = tarfile.TarInfo("../outside")
                member.size = 1
                tar.addfile(member, io.BytesIO(b"x"))
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            archive.with_name(archive.name + ".sha256").write_text(f"{digest}  {archive.name}\n")
            destination = directory / "extracted"
            with self.assertRaisesRegex(SystemExit, "unsafe release inventory path"):
                VERIFY["safe_extract_archive"](
                    archive,
                    destination,
                    revision,
                    "aarch64-unknown-linux-gnu",
                    repository / "spec/openapi.yaml",
                )
            self.assertFalse(destination.exists())
            self.assertFalse((directory.parent / "outside").exists())

    def test_committed_migration_inventory_cannot_be_forged_to_match_package(self) -> None:
        repository = Path(__file__).resolve().parents[1]
        revision = subprocess.run(
            ["git", "-C", str(repository), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        source_files = VERIFY["expected_source_files"](repository, revision)
        migrations = sorted(
            path.split("/", 1)[1]
            for path in source_files
            if path.startswith("migrations/")
        )
        provenance = {
            "embedded_migrations": migrations,
            "shipped_migrations": [f"migrations/{name}" for name in migrations],
            "schema_version": max(int(name[:4]) for name in migrations),
        }
        self.assertEqual(VERIFY["verify_migration_provenance"](provenance, source_files), migrations)
        provenance["embedded_migrations"] = migrations[1:]
        provenance["shipped_migrations"] = [f"migrations/{name}" for name in migrations[1:]]
        provenance["schema_version"] = max(int(name[:4]) for name in migrations[1:])
        with self.assertRaisesRegex(SystemExit, "committed source inventory"):
            VERIFY["verify_migration_provenance"](provenance, source_files)

    def test_package_provenance_cannot_be_supplied_as_caller_arguments(self) -> None:
        script = Path(__file__).with_name("package_release.py")
        root = script.parent.parent
        with tempfile.TemporaryDirectory(prefix="ferl-package-provenance-") as temporary:
            result = subprocess.run(
                [
                    sys.executable,
                    str(script),
                    "--build-manifest",
                    str(Path(temporary) / "build-receipt.json"),
                    "--source-snapshot",
                    str(root),
                    "--binary",
                    str(Path(temporary) / "absent-binary"),
                    "--target",
                    "aarch64-unknown-linux-gnu",
                    "--source-revision",
                    "not-the-checkout-head",
                    "--source-tree-clean",
                    "true",
                    "--builder",
                    "test-only",
                    "--output-dir",
                    temporary,
                ],
                cwd=root,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("unrecognized arguments", result.stderr)

    def test_build_receipt_rejects_executable_substitution(self) -> None:
        repository = Path(__file__).resolve().parents[1]
        revision = subprocess.run(
            ["git", "-C", str(repository), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        tree = subprocess.run(
            ["git", "-C", str(repository), "rev-parse", "HEAD^{tree}"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        target = "aarch64-unknown-linux-gnu"
        image = "docker.io/library/rust@sha256:32f7944ed00949c44d3944a0a852b3e5ea862a610a38b43ac92a33aa87256b75"
        toolchain = "rustc 1.99.1 test\ncargo 1.99.1 test\nhost: aarch64-unknown-linux-gnu\nbuilder_os=debian/bookworm\n"
        with tempfile.TemporaryDirectory(prefix="ferl-build-receipt-") as temporary:
            output = Path(temporary)
            binary = output / "target/aarch64-unknown-linux-gnu/release/ferl-server"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"first executable")
            binary.chmod(0o755)
            (output / "toolchain.txt").write_text(toolchain)
            receipt = {
                "build_invocation": "focused-control",
                "source_revision": revision,
                "source_tree": tree,
                "source_snapshot_verified": True,
                "target": target,
                "builder_image": image,
                "builder_platform": "linux/arm64",
                "build_command": f"cargo build --locked --release --target {target} --bin ferl-server",
                "binary_relative_path": "target/aarch64-unknown-linux-gnu/release/ferl-server",
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "toolchain_observed": toolchain,
            }
            receipt["builder_observation"] = {
                key: receipt[key]
                for key in ["target", "builder_image", "builder_platform", "build_command", "binary_sha256"]
            }
            manifest = output / "build-receipt.json"
            manifest.write_text(json.dumps(receipt))
            PACKAGE["load_build_receipt"](manifest, revision, tree)
            binary.write_bytes(b"substituted executable")
            with self.assertRaisesRegex(SystemExit, "bytes differ"):
                PACKAGE["load_build_receipt"](manifest, revision, tree)

    def test_build_source_snapshot_must_match_every_committed_blob(self) -> None:
        repository = Path(__file__).resolve().parents[1]
        revision = subprocess.run(
            ["git", "-C", str(repository), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        archive = subprocess.run(
            ["git", "-C", str(repository), "archive", "--format=tar", revision],
            check=True,
            capture_output=True,
        ).stdout
        with tempfile.TemporaryDirectory(prefix="ferl-source-snapshot-") as temporary:
            snapshot = Path(temporary) / "source"
            snapshot.mkdir()
            subprocess.run(
                ["tar", "-xf", "-", "-C", str(snapshot)],
                input=archive,
                check=True,
                capture_output=True,
            )
            SOURCE_SNAPSHOT["verify_snapshot"](repository, revision, snapshot)
            (snapshot / "Cargo.toml").write_bytes(b"caller-substituted source")
            with self.assertRaisesRegex(SystemExit, "bytes differ from Git"):
                SOURCE_SNAPSHOT["verify_snapshot"](repository, revision, snapshot)


if __name__ == "__main__":
    unittest.main()
