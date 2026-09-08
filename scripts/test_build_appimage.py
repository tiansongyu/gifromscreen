#!/usr/bin/env python3
"""AppDir contract tests with tiny payloads; no real AppImage/tool/GPU acceptance."""

import argparse
from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import types
import unittest
from unittest.mock import patch

import build_appimage as builder
import build_appimage_runtime as runtime_builder


SOURCE_DIGEST = "a" * 64


class AppImageBuilderTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix="gifromscreen-appdir-test-")
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        source = patch.object(builder.portable, "tree_digest", return_value=SOURCE_DIGEST)
        source.start()
        self.addCleanup(source.stop)
        self.bundle = self.make_bundle(self.root / "portable fixture")
        self.appdir = self.root / "AppDir with spaces"

    def make_bundle(self, destination):
        for name in builder.portable.BINARIES:
            path = destination / "bin" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"synthetic payload, not an ELF: " + name.encode())
        share = destination / "share"
        desktop = share / "applications" / (builder.portable.APP_ID + ".desktop")
        desktop.parent.mkdir(parents=True)
        desktop.write_text("[Desktop Entry]\nType=Application\nName=Fixture\nExec=gif-from-screen\n"
                           "Icon=" + builder.portable.APP_ID + "\n", encoding="utf-8")
        icon = share / "icons/hicolor/scalable/apps" / (builder.portable.APP_ID + ".svg")
        icon.parent.mkdir(parents=True)
        icon.write_text('<svg xmlns="http://www.w3.org/2000/svg"/>\n', encoding="utf-8")
        license_file = share / "licenses/gifromscreen/LICENSE-MIT"
        license_file.parent.mkdir(parents=True)
        license_file.write_text("Synthetic license fixture, not a distribution notice.\n", encoding="utf-8")
        receipt = {
            "schema_version": builder.portable.BUILD_RECEIPT_VERSION,
            "toolchain": builder.portable.TOOLCHAIN,
            "target": builder.portable.TARGET,
            "profile": builder.portable.PROFILE,
            "path_remap": builder.portable.PATH_REMAP,
            "rustc": "rustc 1.88.0 (synthetic fixture)",
            "cargo": "cargo 1.88.0 (synthetic fixture)",
            "source_tree_sha256": SOURCE_DIGEST,
            "cargo_lock_sha256": "b" * 64,
            "source_date_epoch": 1700000000,
            "package_version": "0.1.0",
            "binaries": {name: builder.portable.sha256(destination / "bin" / name)
                         for name in builder.portable.BINARIES},
        }
        self.write_receipt(receipt, destination)
        return destination

    def receipt(self):
        return json.loads((self.bundle / "BUILD-INFO.json").read_text(encoding="utf-8"))

    def checksums(self, bundle=None):
        bundle = bundle or self.bundle
        names = sorted(path for path in bundle.rglob("*") if path.is_file()
                       and path.name != "SHA256SUMS")
        (bundle / "SHA256SUMS").write_text("".join(
            builder.portable.sha256(path) + "  " + path.relative_to(bundle).as_posix() + "\n"
            for path in names), encoding="utf-8")

    def write_receipt(self, receipt, bundle=None):
        bundle = bundle or self.bundle
        (bundle / "BUILD-INFO.json").write_text(json.dumps(receipt), encoding="utf-8")
        self.checksums(bundle)

    def test_complete_payload_and_binary_receipt_are_accepted_without_executing(self):
        with patch.object(builder.subprocess, "run", side_effect=AssertionError("no execution")):
            self.assertEqual(builder.verify_payload(self.bundle), self.receipt())

    def test_unlisted_regular_file_and_missing_listed_file_are_rejected(self):
        extra = self.bundle / "unlisted.txt"
        extra.write_bytes(b"not in the inventory")
        with self.assertRaisesRegex(ValueError, "inventory is incomplete"):
            builder.verify_payload(self.bundle)
        extra.unlink()
        (self.bundle / "bin" / builder.portable.BINARIES[0]).unlink()
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            builder.verify_payload(self.bundle)

    def test_modified_file_and_rehashed_binary_with_stale_receipt_are_rejected(self):
        binary = self.bundle / "bin" / builder.portable.BINARIES[0]
        binary.write_bytes(b"different fake executable")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            builder.verify_payload(self.bundle)
        self.checksums()
        with self.assertRaisesRegex(ValueError, "differs from its build receipt"):
            builder.verify_payload(self.bundle)

    def test_different_current_source_tree_cannot_be_relabelled(self):
        receipt = self.receipt()
        receipt["source_tree_sha256"] = "c" * 64
        self.write_receipt(receipt)
        with self.assertRaisesRegex(ValueError, "current Rust source tree"):
            builder.verify_payload(self.bundle)

    def test_receipt_requires_exact_schema_type_and_target(self):
        original = self.receipt()
        for key, value in (("schema_version", None), ("schema_version", 0),
                           ("schema_version", True), ("schema_version", "1"),
                           ("target", "aarch64-unknown-linux-gnu")):
            with self.subTest(field=key, value=value):
                receipt = dict(original)
                if value is None:
                    receipt.pop(key)
                else:
                    receipt[key] = value
                self.write_receipt(receipt)
                with self.assertRaises(ValueError):
                    builder.verify_payload(self.bundle)

    def test_checksum_names_and_duplicates_cannot_escape_or_alias(self):
        checksums = self.bundle / "SHA256SUMS"
        original = checksums.read_text(encoding="utf-8")
        entries = ("../outside", "/absolute", "bin/../bin/gif-from-screen",
                   "./BUILD-INFO.json", "bin//gif-from-screen", "", "SHA256SUMS")
        for name in entries:
            with self.subTest(name=name):
                checksums.write_text(original + "0" * 64 + "  " + name + "\n", encoding="utf-8")
                with self.assertRaises(ValueError):
                    builder.verify_payload(self.bundle)
        checksums.write_text(original + original.splitlines()[0] + "\n", encoding="utf-8")
        with self.assertRaises(ValueError):
            builder.verify_payload(self.bundle)

    def test_payload_rejects_unlisted_links_and_special_files_not_just_regular_files(self):
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "must-not-bundle.txt").write_bytes(b"foreign bytes")
        for kind in ("directory_link", "dangling_link", "fifo"):
            with self.subTest(kind=kind):
                bundle = self.make_bundle(self.root / kind)
                unwanted = bundle / "share" / "unlisted"
                if kind == "directory_link":
                    unwanted.symlink_to(outside, target_is_directory=True)
                elif kind == "dangling_link":
                    unwanted.symlink_to("absent")
                else:
                    os.mkfifo(unwanted)
                with self.assertRaises(ValueError):
                    builder.verify_payload(bundle)

    def test_checksum_file_itself_cannot_be_a_symlink(self):
        checksums = self.bundle / "SHA256SUMS"
        outside = self.root / "outside-checksums"
        checksums.rename(outside)
        checksums.symlink_to(outside)
        with self.assertRaises(ValueError):
            builder.verify_payload(self.bundle)

    def test_stage_preserves_payload_bytes_and_creates_only_relative_internal_links(self):
        builder.verify_payload(self.bundle)
        builder.stage_payload(self.bundle, self.appdir)
        expected = {
            builder.portable.APP_ID + ".desktop": "usr/share/applications/" + builder.portable.APP_ID + ".desktop",
            builder.portable.APP_ID + ".svg": "usr/share/icons/hicolor/scalable/apps/" + builder.portable.APP_ID + ".svg",
            ".DirIcon": builder.portable.APP_ID + ".svg",
        }
        for name, target in expected.items():
            path = self.appdir / name
            self.assertTrue(path.is_symlink())
            self.assertEqual(os.readlink(path), target)
            self.assertTrue(path.resolve(strict=True).is_relative_to(self.appdir))
        for name in builder.portable.BINARIES:
            staged = self.appdir / "usr/bin" / name
            self.assertEqual(staged.read_bytes(), (self.bundle / "bin" / name).read_bytes())
            self.assertEqual(staged.stat().st_mode & 0o777, 0o755)
        self.assertEqual((self.appdir / "AppRun").stat().st_mode & 0o777, 0o755)
        self.assertFalse((self.appdir / "installer.py").exists())
        inventory = builder.file_inventory(self.appdir)
        self.assertEqual(inventory[".DirIcon"], {"symlink": expected[".DirIcon"]})
        self.assertEqual(inventory["usr/bin/gif-from-screen"]["sha256"],
                         builder.portable.sha256(self.bundle / "bin/gif-from-screen"))

    def test_inventory_rejects_external_and_absolute_internal_links(self):
        builder.stage_payload(self.bundle, self.appdir)
        outside = self.root / "foreign"
        outside.write_bytes(b"foreign")
        link = self.appdir / "unexpected-link"
        for target in (outside, "../foreign", self.appdir / "usr/bin/gif-from-screen"):
            with self.subTest(target=str(target)):
                link.symlink_to(target)
                try:
                    with self.assertRaises(ValueError):
                        builder.file_inventory(self.appdir)
                finally:
                    link.unlink()

    def test_inventory_rejects_dangling_links_cycles_and_fifo(self):
        builder.stage_payload(self.bundle, self.appdir)
        link = self.appdir / "invalid"
        for target in ("missing", "invalid"):
            with self.subTest(target=target):
                link.symlink_to(target)
                try:
                    with self.assertRaises((ValueError, OSError, RuntimeError)):
                        builder.file_inventory(self.appdir)
                finally:
                    link.unlink()
        os.mkfifo(link)
        with self.assertRaises(ValueError):
            builder.file_inventory(self.appdir)

    def stage_fake_launchers(self):
        builder.stage_payload(self.bundle, self.appdir)
        for relative in ("usr/share/gifromscreen/pipewire", "usr/lib/pipewire-0.3",
                         "usr/lib/spa-0.2", "usr/share/X11/xkb"):
            (self.appdir / relative).mkdir(parents=True, exist_ok=True)
        (self.appdir / "usr/share/gifromscreen/pipewire/client.conf").write_text(
            "# private synthetic configuration\n", encoding="utf-8")
        for name in builder.portable.BINARIES:
            body = '#!/bin/sh\nset -eu\ntest -r "$PIPEWIRE_CONFIG_DIR/$PIPEWIRE_CONFIG_NAME"\n'
            body += 'test -d "$PIPEWIRE_MODULE_DIR"\ntest -d "$SPA_PLUGIN_DIR"\n'
            body += 'test -d "$XKB_CONFIG_ROOT"\nprintf \'%s\\000\' ' + repr(name)
            body += ' "$PWD" "${PATH-ABSENT}" "${LD_LIBRARY_PATH-ABSENT}"'
            body += ' "$PIPEWIRE_CONFIG_DIR" "$PIPEWIRE_CONFIG_NAME" "$PIPEWIRE_MODULE_DIR"'
            body += ' "$SPA_PLUGIN_DIR" "$XKB_CONFIG_ROOT" "$@"\nexit "${GFS_TEST_EXIT-0}"\n'
            path = self.appdir / "usr/bin" / name
            path.write_text(body, encoding="utf-8")
            path.chmod(0o755)

    def test_apprun_preserves_arguments_cwd_and_host_loader_path_for_desktop_and_cli(self):
        self.stage_fake_launchers()
        cwd = self.root / "caller working directory"
        cwd.mkdir()
        arguments = ["relative project.gfsproj", "", "--flag=value", 'quotes " % $ `', "line1\nline2"]
        for use_cli in (False, True):
            with self.subTest(cli=use_cli):
                environment = os.environ.copy()
                environment.update(PATH="/usr/bin:/bin:/nonexistent path with spaces",
                                   LD_LIBRARY_PATH="/nonexistent-host-library-choice",
                                   APPDIR="/poisoned/not-the-application",
                                   PIPEWIRE_CONFIG_DIR="/old/config", PIPEWIRE_CONFIG_NAME="old.conf",
                                   PIPEWIRE_MODULE_DIR="/old/modules", SPA_PLUGIN_DIR="/old/spa",
                                   XKB_CONFIG_ROOT="/old/xkb", GFS_TEST_EXIT="23")
                command = [str(self.appdir / "AppRun"), *(["--cli"] if use_cli else []), *arguments]
                result = subprocess.run(command, cwd=cwd, env=environment, capture_output=True, timeout=5)
                self.assertEqual(result.returncode, 23, result.stderr)
                fields = result.stdout.decode().split("\0")
                self.assertEqual(fields[-1], "")
                self.assertEqual(fields[:-1], [builder.portable.BINARIES[int(use_cli)], str(cwd),
                    environment["PATH"], environment["LD_LIBRARY_PATH"],
                    str(self.appdir / "usr/share/gifromscreen/pipewire"), "client.conf",
                    str(self.appdir / "usr/lib/pipewire-0.3"), str(self.appdir / "usr/lib/spa-0.2"),
                    str(self.appdir / "usr/share/X11/xkb"), *arguments])

    def test_apprun_does_not_create_ld_library_path_when_unset(self):
        self.stage_fake_launchers()
        environment = os.environ.copy()
        environment.pop("LD_LIBRARY_PATH", None)
        environment.pop("GFS_TEST_EXIT", None)
        result = subprocess.run([str(self.appdir / "AppRun"), "--cli"], env=environment,
                                capture_output=True, timeout=5, check=True)
        self.assertEqual(result.stdout.decode().split("\0")[3], "ABSENT")

    def arguments(self, development_only):
        return argparse.Namespace(archive=self.root / "input.tar.gz", tools_dir=self.root / "tools",
                                  runtime_build=self.root / "runtime build",
                                  output_dir=self.root / "new output", development_only=development_only)

    def test_distribution_is_blocked_before_tool_verification_or_output_writes(self):
        arguments = self.arguments(False)
        with patch.object(builder, "verify_tools", side_effect=AssertionError("do not verify/execute")):
            with self.assertRaisesRegex(ValueError, "only --development-only"):
                builder.build(arguments)
        self.assertFalse(arguments.output_dir.exists())

    def test_tool_verification_failure_does_not_execute_or_create_output(self):
        arguments = self.arguments(True)
        with patch.object(builder, "verify_tools", side_effect=ValueError("unverified tool")):
            with patch.object(builder, "verify_runtime_build", side_effect=AssertionError("tools must verify first")):
                with patch.object(builder.subprocess, "run", side_effect=AssertionError("no execution")):
                    with self.assertRaisesRegex(ValueError, "unverified tool"):
                        builder.build(arguments)
        self.assertFalse(arguments.output_dir.exists())

    def _mocked_development_assembly(self, mutate_payload=False):
        # This checks orchestration only: these bytes are NOT accepted by the
        # real verify_tools and the output is NOT a valid AppImage.
        arguments = self.arguments(True)
        arguments.archive.write_bytes(b"mock archive")
        tool = self.root / "mock-appimagetool"
        reference_runtime = self.root / "upstream-reference-runtime"
        runtime = self.root / "patched-runtime"
        tool.write_bytes(b"never execute")
        reference_runtime.write_bytes(b"known upstream reference; never passed to packager")
        runtime.write_bytes(b"mock patched runtime; never execute")
        runtime_receipt = {"format_version": 1, "recipe_sha256": "e" * 64,
                           "redistribution_ready": False, "synthetic_test_only": True}
        calls = []

        def run(command, **kwargs):
            calls.append((command, kwargs))
            self.assertEqual(command[0], "desktop-file-validate")
            return subprocess.CompletedProcess(command, 0)

        def run_owned(command, **kwargs):
            calls.append((command, kwargs))
            self.assertEqual(command[0], str(tool))
            self.assertEqual(command[1:5], ["--appimage-extract-and-run", "--no-appstream",
                                           "--runtime-file", str(runtime)])
            self.assertEqual(kwargs["timeout"], 180)
            self.assertEqual(kwargs["env"]["ARCH"], "x86_64")
            self.assertNotIn("NO_CLEANUP", kwargs["env"])
            self.assertNotIn("VERSION", kwargs["env"])
            self.assertTrue(Path(kwargs["env"]["TMPDIR"]).is_dir())
            Path(command[-1]).write_bytes(b"orchestration test, not a real AppImage")
            if mutate_payload:
                (Path(command[-2]) / "AppRun").write_text("changed by packager")
            return 0

        native = types.SimpleNamespace(bundle_native=lambda appdir: {"mock_only": True})
        with patch.object(builder, "verify_tools", return_value=(tool, reference_runtime)), \
                patch.object(builder, "verify_runtime_build", return_value=(runtime, runtime_receipt)) as verify_runtime:
            with patch.object(builder, "extract_checked", return_value=self.bundle):
                with patch.dict("sys.modules", {"appimage_native": native}):
                    with patch.object(builder.subprocess, "run", side_effect=run):
                        with patch.object(builder, "run_owned", side_effect=run_owned):
                            with patch.dict(os.environ, {"VERSION": "must-not-rewrite-desktop"}), \
                                    redirect_stdout(io.StringIO()) as output:
                                builder.build(arguments)
        result = json.loads(output.getvalue())
        self.assertFalse(result["redistribution_ready"])
        metadata = json.loads((Path(result["appdir"]) / "BUILD-INFO.json").read_text())
        self.assertTrue(metadata["development_only"])
        self.assertFalse(metadata["redistribution_ready"])
        verify_runtime.assert_called_once_with(arguments.runtime_build)
        self.assertEqual(metadata["runtime_build"], runtime_receipt)
        self.assertEqual(metadata["tools"], {tool.name: builder.portable.sha256(tool),
                                           runtime.name: builder.portable.sha256(runtime)})
        self.assertNotIn(reference_runtime.name, metadata["tools"])
        self.assertEqual(len(calls), 2)
        self.assertEqual(Path(result["appimage"]).stat().st_mode & 0o777, 0o755)
        self.assertTrue(Path(result["appimage"] + ".sha256").is_file())

    def test_mocked_development_assembly_passes_explicit_local_runtime_without_downloading(self):
        self._mocked_development_assembly()

    def test_packager_cannot_silently_change_the_inventoried_payload(self):
        with self.assertRaisesRegex(ValueError, "packager changed the verified AppDir"):
            self._mocked_development_assembly(mutate_payload=True)

    def test_cli_requires_verified_runtime_build_argument(self):
        command = ["build_appimage.py", "fixture.tar.gz", "--output-dir", str(self.root / "output"), "--development-only"]
        with patch("sys.argv", command), patch.object(builder, "build") as build:
            with redirect_stdout(io.StringIO()), patch("sys.stderr", new_callable=io.StringIO) as error:
                with self.assertRaises(SystemExit) as result:
                    builder.main()
            self.assertEqual(result.exception.code, 2)
            self.assertIn("--runtime-build", error.getvalue())
            build.assert_not_called()
        with patch("sys.argv", [*command, "--runtime-build", str(self.root / "verified build")]), \
                patch.object(builder, "build") as build:
            builder.main()
        self.assertEqual(build.call_args.args[0].runtime_build, self.root / "verified build")

    def make_runtime_build(self, arguments):
        # These bounded synthetic bytes exercise the real receipt validator;
        # they are not a compiled runtime and are never executed or packaged.
        artifacts = arguments.runtime_build / "artifacts"
        artifacts.mkdir(parents=True)
        header = bytearray(64)
        header[:7] = b"\x7fELF\x02\x01\x01"
        header[8:11] = b"AI\x02"
        header[18:20] = (62).to_bytes(2, "little")
        (artifacts / "runtime-x86_64").write_bytes(header)
        (artifacts / "cleanup-test.log").write_text("patched_runtime_cleanup=PASS\n")
        materials = arguments.runtime_build / "sources"
        materials.mkdir()
        (materials / "source.tar.gz").write_bytes(b"synthetic retained source")
        recipe = self.root / "runtime recipe"
        recipe.mkdir()
        (recipe / "cleanup.patch").write_bytes(b"synthetic patch, never applied")
        pins = {"format_version": 1, "upstream_commit": "f" * 40, "archives": {}}
        (recipe / "sources.json").write_text(json.dumps(pins))
        receipt = {"format_version": 1, "recipe_sha256": SOURCE_DIGEST, "sources": pins,
                   "patch_sha256": builder.portable.sha256(recipe / "cleanup.patch"),
                   "source_materials": runtime_builder.inventory(materials),
                   "artifacts": runtime_builder.inventory(artifacts)}
        (arguments.runtime_build / runtime_builder.RECEIPT).write_text(json.dumps(receipt))
        return recipe, receipt

    def test_invalid_runtime_receipt_or_artifact_is_rejected_before_any_output_or_execution(self):
        arguments = self.arguments(True)
        recipe, receipt = self.make_runtime_build(arguments)
        receipt_file = arguments.runtime_build / runtime_builder.RECEIPT
        runtime = arguments.runtime_build / "artifacts/runtime-x86_64"
        original_runtime = runtime.read_bytes()
        for invalid in ("recipe", "runtime", "source-material", "cleanup"):
            with self.subTest(invalid=invalid):
                candidate = dict(receipt)
                if invalid == "recipe":
                    candidate["recipe_sha256"] = "0" * 64
                elif invalid == "runtime":
                    runtime.write_bytes(original_runtime + b"unexpected runtime bytes")
                elif invalid == "source-material":
                    (arguments.runtime_build / "sources/unlisted").write_bytes(b"unlisted")
                else:
                    (arguments.runtime_build / "artifacts/cleanup-test.log").write_text("patched_runtime_cleanup=FAIL\n")
                    candidate["artifacts"] = runtime_builder.inventory(arguments.runtime_build / "artifacts")
                receipt_file.write_text(json.dumps(candidate))
                native = types.SimpleNamespace(bundle_native=lambda _appdir: self.fail("native must not run"))
                with patch.object(builder, "verify_tools", return_value=(self.root / "packager", self.root / "reference-runtime")), \
                        patch.object(runtime_builder, "RECIPE", recipe), \
                        patch.object(builder, "extract_checked", side_effect=AssertionError("must not extract")), \
                        patch.object(builder.subprocess, "run", side_effect=AssertionError("must not execute")), \
                        patch.object(builder, "run_owned", side_effect=AssertionError("must not package")), \
                        patch.dict("sys.modules", {"appimage_native": native}), self.assertRaises(ValueError):
                    builder.build(arguments)
                self.assertFalse(arguments.output_dir.exists())
                runtime.write_bytes(original_runtime)
                unlisted = arguments.runtime_build / "sources/unlisted"
                if unlisted.exists():
                    unlisted.unlink()

    def test_missing_runtime_build_does_not_fall_back_to_reference_runtime(self):
        arguments = self.arguments(True)
        with patch.object(builder, "verify_tools", return_value=(self.root / "packager", self.root / "reference-runtime")), \
                patch.object(builder, "extract_checked", side_effect=AssertionError("must not extract")), \
                patch.object(builder.subprocess, "run", side_effect=AssertionError("must not execute")), \
                patch.object(builder, "run_owned", side_effect=AssertionError("must not package")), \
                self.assertRaises(FileNotFoundError):
            builder.build(arguments)
        self.assertFalse(arguments.output_dir.exists())


if __name__ == "__main__":
    unittest.main()
