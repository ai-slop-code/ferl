#!/usr/bin/env python3
"""Regression controls for release archive verification."""

import hashlib
import io
import runpy
import subprocess
import sys
import tempfile
import tarfile
import unittest
from pathlib import Path


VERIFY = runpy.run_path(str(Path(__file__).with_name("verify-release.py")))


class ReleaseArchiveVerificationTests(unittest.TestCase):
    def test_archive_payload_must_match_extracted_checksum_inventory(self) -> None:
        expected_payload = b"verified extracted payload"
        checksum_inventory = b"checksum inventory from extracted release\n"
        with tempfile.TemporaryDirectory(prefix="ferl-release-verifier-") as temporary:
            archive = Path(temporary) / "fixture.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
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

    def test_package_provenance_cannot_name_another_git_revision(self) -> None:
        script = Path(__file__).with_name("package_release.py")
        root = script.parent.parent
        with tempfile.TemporaryDirectory(prefix="ferl-package-provenance-") as temporary:
            result = subprocess.run(
                [
                    sys.executable,
                    str(script),
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
            self.assertIn("source revision does not match", result.stderr)


if __name__ == "__main__":
    unittest.main()
