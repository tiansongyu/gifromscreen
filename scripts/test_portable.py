#!/usr/bin/env python3
"""Inspect a real tarball and exercise installation only inside temporary paths."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

APP_ID = "io.github.tiansongyu.gifromscreen"
MANIFEST = Path("lib/gifromscreen/install-manifest.json")


def extract_checked(archive, destination):
    with tarfile.open(archive, "r:gz") as source:
        members = source.getmembers()
        seen = set()
        for member in members:
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts or member.name in seen or not (member.isdir() or member.isfile()):
                raise ValueError("unsafe/duplicate archive member: " + member.name)
            seen.add(member.name)
        for member in members:
            path = destination / member.name
            if member.isdir():
                path.mkdir(parents=True, exist_ok=True)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                with source.extractfile(member) as data, path.open("xb") as output:
                    shutil.copyfileobj(data, output)
                path.chmod(member.mode)
    roots = list(destination.iterdir())
    if len(roots) != 1:
        raise ValueError("archive must have one containing directory")
    return roots[0]


class PortableTests(unittest.TestCase):
    bundle = None

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="gifromscreen-install-test-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.prefix = self.root / "user prefix with spaces"

    def command(self, operation, success=True, bundle=None, prefix=None):
        result = subprocess.run([sys.executable, str((bundle or self.bundle) / "installer.py"), operation,
                                 "--prefix", str(prefix or self.prefix)], capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def test_archive_checksums_and_real_cli_export(self):
        subprocess.run(["sha256sum", "--check", "--quiet", "SHA256SUMS"], cwd=self.bundle, check=True)
        version = subprocess.check_output([str(self.bundle / "bin/gif-from-screen-cli"), "version"], text=True).strip()
        information = json.loads((self.bundle / "BUILD-INFO.json").read_text())
        self.assertEqual(version, information["package_version"])
        self.assertFalse(information["ffmpeg_bundled"])
        subprocess.run([str(self.bundle / "bin/gif-from-screen-cli"), "demo", str(self.root / "demo.gif")], check=True, capture_output=True)
        self.assertEqual((self.root / "demo.gif").read_bytes()[:6], b"GIF89a")

    def test_embedded_font_and_upstream_license_notices_are_present(self):
        licenses = self.bundle / "share/licenses/gifromscreen"
        inventory = json.loads((licenses / "THIRD-PARTY.json").read_text())
        font = next(package for package in inventory if package["name"] == "epaint_default_fonts")
        self.assertTrue(any(path.endswith("OFL.txt") for path in font["license_files"]))
        self.assertTrue(any(path.endswith("UFL.txt") for path in font["license_files"]))
        self.assertTrue(any(path.endswith("Hack-Regular.txt") for path in font["license_files"]))
        for package in inventory:
            self.assertTrue(package["license_files"])
            for name in package["license_files"]:
                self.assertTrue((licenses / name).is_file(), package["name"] + ": " + name)

    def test_install_space_prefix_launcher_and_idempotent_reinstall(self):
        self.command("install")
        binary = self.prefix / "bin/gif-from-screen-cli"
        self.assertTrue(binary.is_symlink())
        subprocess.run([str(binary), "--help"], check=True, capture_output=True)
        launcher = self.prefix / "share/applications" / (APP_ID + ".desktop")
        self.assertIn('Exec="' + str(self.prefix), launcher.read_text())
        if shutil.which("desktop-file-validate"):
            subprocess.run(["desktop-file-validate", str(launcher)], check=True)
        manifest_before = (self.prefix / MANIFEST).read_bytes()
        self.command("install")
        self.assertEqual((self.prefix / MANIFEST).read_bytes(), manifest_before)
        self.command("uninstall")
        self.assertFalse(binary.exists())
        self.assertFalse((self.prefix / MANIFEST).exists())

    def test_existing_foreign_binary_is_not_overwritten(self):
        foreign = self.prefix / "bin/gif-from-screen"
        foreign.parent.mkdir(parents=True)
        foreign.write_bytes(b"another application")
        self.command("install", success=False)
        self.assertEqual(foreign.read_bytes(), b"another application")
        self.assertFalse((self.prefix / "lib").exists())

    def test_existing_file_symlink_and_parent_symlink_are_rejected(self):
        foreign = self.root / "foreign"
        foreign.write_bytes(b"preserve")
        binary = self.prefix / "bin/gif-from-screen"
        binary.parent.mkdir(parents=True)
        binary.symlink_to(foreign)
        self.command("install", success=False)
        self.assertEqual(foreign.read_bytes(), b"preserve")
        binary.unlink()
        other = self.root / "elsewhere"
        other.mkdir()
        (self.prefix / "lib").symlink_to(other, target_is_directory=True)
        self.command("install", success=False)
        self.assertEqual(list(other.iterdir()), [])

    def test_modified_installed_file_blocks_uninstall_before_any_removal(self):
        self.command("install")
        readme = self.prefix / "lib/gifromscreen/README.txt"
        readme.write_text("user changes to preserve")
        before = (self.prefix / MANIFEST).read_bytes()
        self.command("uninstall", success=False)
        self.assertEqual(readme.read_text(), "user changes to preserve")
        self.assertEqual((self.prefix / MANIFEST).read_bytes(), before)
        self.assertTrue((self.prefix / "bin/gif-from-screen").exists())

    def test_uninstall_preserves_unknown_files_and_projects(self):
        self.command("install")
        project = self.prefix / "lib/gifromscreen/my-project.gfsproj"
        project.mkdir()
        (project / "user-data").write_bytes(b"keep")
        outside = self.root / "recording.gif"
        outside.write_bytes(b"keep outside")
        self.command("uninstall")
        self.assertEqual((project / "user-data").read_bytes(), b"keep")
        self.assertEqual(outside.read_bytes(), b"keep outside")
        self.assertFalse((self.prefix / "bin/gif-from-screen").exists())

    def test_traversal_in_owned_manifest_is_rejected(self):
        self.command("install")
        outside = self.root / "outside"
        outside.write_bytes(b"keep")
        manifest = self.prefix / MANIFEST
        document = json.loads(manifest.read_text())
        document["files"]["../outside"] = {"kind": "file", "sha256": hashlib.sha256(b"keep").hexdigest()}
        manifest.write_text(json.dumps(document))
        self.command("uninstall", success=False)
        self.assertEqual(outside.read_bytes(), b"keep")
        self.assertTrue((self.prefix / "bin/gif-from-screen").exists())

    def test_symlinked_owned_directory_blocks_uninstall(self):
        self.command("install")
        installed_bin = self.prefix / "lib/gifromscreen/bin"
        moved = self.root / "moved binaries"
        installed_bin.rename(moved)
        installed_bin.symlink_to(moved, target_is_directory=True)
        self.command("uninstall", success=False)
        self.assertTrue((moved / "gif-from-screen").exists())
        self.assertTrue((self.prefix / MANIFEST).exists())

    def test_checksum_tampering_is_rejected_before_install(self):
        copied = self.root / "tampered bundle"
        shutil.copytree(self.bundle, copied)
        (copied / "README.txt").write_text("tampered")
        self.command("install", bundle=copied, success=False)
        self.assertFalse(self.prefix.exists())

    def test_other_version_requires_explicit_uninstall(self):
        self.command("install")
        copied = self.root / "new version"
        shutil.copytree(self.bundle, copied)
        readme = copied / "README.txt"
        readme.write_text("new package content")
        checksum = copied / "SHA256SUMS"
        entries = checksum.read_text().splitlines()
        entries = [hashlib.sha256(readme.read_bytes()).hexdigest() + "  README.txt" if line.endswith("  README.txt") else line for line in entries]
        checksum.write_text("\n".join(entries) + "\n")
        self.command("install", bundle=copied, success=False)
        self.assertEqual((self.prefix / "lib/gifromscreen/README.txt").read_bytes(), (self.bundle / "README.txt").read_bytes())

    def test_desktop_special_character_prefix_is_escaped(self):
        self.prefix = self.root / 'prefix % " $ ` back\\slash'
        self.command("install")
        launcher = self.prefix / "share/applications" / (APP_ID + ".desktop")
        if shutil.which("desktop-file-validate"):
            subprocess.run(["desktop-file-validate", str(launcher)], check=True)
        content = launcher.read_text()
        self.assertIn("%%", content)
        self.command("uninstall")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    arguments = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="gifromscreen-archive-test-") as scratch:
        PortableTests.bundle = extract_checked(arguments.archive, Path(scratch))
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(PortableTests)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
