#!/usr/bin/env python3
"""Exercise the standalone installer without network access or host installation."""

import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import types
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("gifromscreen-installer.sh")
CODE = SCRIPT.read_text().split("<<'PYTHON'\n", 1)[1].rsplit("\nPYTHON", 1)[0]
installer = types.ModuleType("release_installer")
exec(compile(CODE, str(SCRIPT), "exec"), installer.__dict__)


class ReleaseInstallerTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="gfs-bootstrap-test-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.archive = self.root / "gifromscreen-0.1.0-linux-x86_64.tar.gz"
        self.root_name = self.archive.name.removesuffix(".tar.gz")
        self.checksum = self.root / (self.archive.name + ".sha256")

    def make_archive(self, entries):
        with tarfile.open(self.archive, "w:gz") as archive:
            for name, kind in entries:
                member = tarfile.TarInfo(name)
                if kind == "symlink":
                    member.type = tarfile.SYMTYPE
                    member.linkname = "/tmp/outside"
                elif kind == "hardlink":
                    member.type = tarfile.LNKTYPE
                    member.linkname = self.root_name + "/installer.py"
                elif kind == "fifo":
                    member.type = tarfile.FIFOTYPE
                else:
                    data = b"# synthetic installer\n"
                    member.size = len(data)
                    archive.addfile(member, io.BytesIO(data))
                    continue
                archive.addfile(member)

    def test_shell_help_does_not_download_or_install(self):
        result = subprocess.run(["sh", str(SCRIPT), "--help"], text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--prefix", result.stdout)

    def test_requested_version_is_normalized_and_invalid_tags_are_rejected(self):
        self.assertEqual(installer.release_version("0.1.0"), "v0.1.0")
        self.assertEqual(installer.release_version("v0.1.0-preview.1"), "v0.1.0-preview.1")
        for value in ("../main", "1.0", "v1.0.0/extra", "1.0.0;touch bad"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                installer.release_version(value)

    def test_latest_release_uses_api_tag(self):
        with patch.object(installer, "response", return_value=io.BytesIO(json.dumps({"tag_name": "v0.1.0"}).encode())):
            self.assertEqual(installer.release_version(None), "v0.1.0")

    def test_platform_rejects_unsupported_architecture_and_old_glibc(self):
        for system, machine, libc in (("Darwin", "x86_64", ("", "")), ("Linux", "aarch64", ("glibc", "2.35")),
                                      ("Linux", "x86_64", ("glibc", "2.34")), ("Linux", "x86_64", ("musl", "1.2"))):
            with self.subTest(system=system, machine=machine, libc=libc), \
                    patch.object(installer.platform, "system", return_value=system), \
                    patch.object(installer.platform, "machine", return_value=machine), \
                    patch.object(installer.platform, "libc_ver", return_value=libc), self.assertRaises(ValueError):
                installer.check_platform()

    def test_checksum_accepts_matching_archive(self):
        self.archive.write_bytes(b"verified package")
        expected = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        self.checksum.write_text(expected + "  " + self.archive.name + "\n")
        installer.verify_checksum(self.archive, self.checksum)

    def test_checksum_rejects_corruption_wrong_filename_and_malformed_data(self):
        self.archive.write_bytes(b"corrupted")
        for content in ("0" * 64 + "  " + self.archive.name, "0" * 64 + "  another.tar.gz", "not a checksum"):
            with self.subTest(content=content):
                self.checksum.write_text(content)
                with self.assertRaises(ValueError):
                    installer.verify_checksum(self.archive, self.checksum)

    def test_extraction_rejects_traversal_links_duplicates_and_extra_roots(self):
        cases = [("../outside", "file"), ("/tmp/outside", "file"), ("other/file", "file")]
        cases += [(self.root_name + "/link", kind) for kind in ("symlink", "hardlink", "fifo")]
        for bad in cases:
            with self.subTest(bad=bad):
                self.make_archive([(self.root_name + "/installer.py", "file"), bad])
                destination = self.root / "unpacked"
                with self.assertRaises(ValueError):
                    installer.extract_checked(self.archive, destination, self.root_name)
                self.assertFalse(destination.exists(), "preflight must finish before extraction")
        self.make_archive([(self.root_name + "/installer.py", "file")] * 2)
        with self.assertRaises(ValueError):
            installer.extract_checked(self.archive, self.root / "duplicates", self.root_name)

    def test_valid_archive_extracts_into_one_directory(self):
        self.make_archive([(self.root_name + "/installer.py", "file"), (self.root_name + "/bin/app", "file")])
        bundle = installer.extract_checked(self.archive, self.root / "unpacked", self.root_name)
        self.assertTrue((bundle / "bin/app").is_file())

    def test_corrupt_download_never_runs_package_installer(self):
        def fake_download(url, destination):
            destination.write_bytes(b"corrupt" if not url.endswith(".sha256") else
                                    ("0" * 64 + "  " + self.archive.name).encode())
        with patch.object(installer, "check_platform"), patch.object(installer, "download", side_effect=fake_download), \
                patch.object(installer.sys, "argv", ["installer", "--version", "0.1.0", "--prefix", str(self.root / "prefix")]), \
                patch.object(installer.subprocess, "run") as run, self.assertRaises(ValueError):
            installer.main()
        run.assert_not_called()

    def test_success_delegates_to_bundle_installer_with_space_in_prefix(self):
        self.make_archive([(self.root_name + "/installer.py", "file")])
        data = self.archive.read_bytes()
        checksum = (hashlib.sha256(data).hexdigest() + "  " + self.archive.name).encode()
        prefix = self.root / "user prefix"
        def fake_download(url, destination):
            destination.write_bytes(checksum if url.endswith(".sha256") else data)
        with patch.object(installer, "check_platform"), patch.object(installer, "download", side_effect=fake_download), \
                patch.object(installer.sys, "argv", ["installer", "--version", "v0.1.0", "--prefix", str(prefix)]), \
                patch.object(installer.subprocess, "run") as run:
            installer.main()
        self.assertEqual(run.call_args.args[0][-3:], ["install", "--prefix", str(prefix)])
        self.assertTrue(run.call_args.kwargs["check"])


if __name__ == "__main__":
    unittest.main()
