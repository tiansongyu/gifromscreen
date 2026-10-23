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
import runpy
import shutil
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

    def make_bundle(self, version):
        bundle = self.root / ("bundle-" + version)
        bundle.mkdir()
        packaging = SCRIPT.parent.parent / "packaging/linux"
        names = ["installer.py", "install.sh", "uninstall.sh"]
        for name in names:
            shutil.copy2(packaging / name, bundle / name)
        app_id = "io.github.tiansongyu.gifromscreen"
        files = {
            "bin/gif-from-screen": "#!/bin/sh\nexit 0\n",
            "bin/gif-from-screen-cli": "#!/bin/sh\nexit 0\n",
            "share/applications/" + app_id + ".desktop": "[Desktop Entry]\nExec=gif-from-screen\nIcon=" + app_id + "\n",
            "share/icons/hicolor/scalable/apps/" + app_id + ".svg": "<svg/>\n",
            "BUILD-INFO.json": json.dumps({"package_version": version}),
        }
        for name, content in files.items():
            path = bundle / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        checksums = []
        for path in sorted(bundle.rglob("*")):
            if path.is_file():
                checksums.append(hashlib.sha256(path.read_bytes()).hexdigest() + "  " + path.relative_to(bundle).as_posix())
        (bundle / "SHA256SUMS").write_text("\n".join(checksums) + "\n")
        return bundle

    def test_newer_installed_version_is_kept_without_downloading(self):
        prefix = self.root / "prefix"
        receipt = prefix / "lib/gifromscreen/BUILD-INFO.json"
        receipt.parent.mkdir(parents=True)
        receipt.write_text(json.dumps({"package_version": "0.1.2"}))
        with patch.object(installer, "check_platform"), patch.object(installer, "download") as download, \
                patch.object(installer, "release_version", return_value="v0.1.0"), \
                patch.object(installer.sys, "argv", ["installer", "--prefix", str(prefix)]):
            installer.main()
        download.assert_not_called()
        self.assertEqual(installer.installed_version(prefix), "0.1.2")

    def test_upgrade_preserves_projects_and_replaces_only_verified_owned_files(self):
        old = self.make_bundle("0.1.0")
        new = self.make_bundle("0.1.3")
        prefix = self.root / "user prefix"
        runpy.run_path(str(old / "installer.py"))["install"](old, prefix)
        project = prefix / "lib/gifromscreen/personal.gfsproj"
        project.write_text("keep this project")
        installer.install_bundle(new, prefix)
        self.assertEqual(installer.installed_version(prefix), "0.1.3")
        self.assertEqual(project.read_text(), "keep this project")
        installer.install_bundle(new, prefix)

    def test_upgrade_rejects_modified_owned_files_without_removal(self):
        old = self.make_bundle("0.1.0")
        new = self.make_bundle("0.1.3")
        prefix = self.root / "prefix"
        runpy.run_path(str(old / "installer.py"))["install"](old, prefix)
        binary = prefix / "lib/gifromscreen/bin/gif-from-screen"
        binary.write_text("modified binary")
        with self.assertRaisesRegex(ValueError, "changed"):
            installer.install_bundle(new, prefix)
        self.assertEqual(installer.installed_version(prefix), "0.1.0")
        self.assertEqual(binary.read_text(), "modified binary")

    def test_failed_upgrade_restores_previous_verified_package(self):
        old = self.make_bundle("0.1.0")
        new = self.make_bundle("0.1.3")
        prefix = self.root / "prefix"
        package = runpy.run_path(str(new / "installer.py"))
        package["install"](old, prefix)
        original_install = package["install"]
        def fail_new(bundle, destination):
            if bundle == new:
                raise OSError("injected installation failure")
            original_install(bundle, destination)
        package["install"] = fail_new
        backup = self.root / "backup"
        backup.mkdir()
        with patch.object(installer.runpy, "run_path", return_value=package), \
                patch.object(installer.tempfile, "mkdtemp", return_value=str(backup)), \
                self.assertRaisesRegex(OSError, "upgrade failed"):
            installer.install_bundle(new, prefix)
        self.assertEqual(installer.installed_version(prefix), "0.1.0")
        self.assertTrue((prefix / "bin/gif-from-screen").exists())

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
    unittest.main(buffer=True)
