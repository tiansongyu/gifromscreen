#!/usr/bin/env python3
"""Bounded build-receipt regressions; Cargo compilation is mocked, never downloaded."""

import json
import os
import shutil
from contextlib import ExitStack
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import build_portable as portable


class BuildReceiptTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix="gifromscreen-build-receipt-")
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.target = self.root / "build cache with spaces"
        self.cargo = ["cargo", "+" + portable.TOOLCHAIN]
        self.epoch = 1234567890
        (self.root / "Cargo.lock").write_text("locked fixture\n", encoding="utf-8")
        self.source_digest = "a" * 64
        self.builds = []
        patches = ExitStack()
        self.addCleanup(patches.close)
        patches.enter_context(patch.object(portable, "ROOT", self.root))
        patches.enter_context(patch.object(portable, "tree_digest", side_effect=lambda _: self.source_digest))
        patches.enter_context(patch.object(portable, "run", side_effect=self.query))
        patches.enter_context(patch.object(portable.subprocess, "run", side_effect=self.compile))
        patches.enter_context(patch.dict(os.environ, {}, clear=True))

    def query(self, arguments, **kwargs):
        if arguments == ["rustc", "+" + portable.TOOLCHAIN, "--version"]:
            return "rustc 1.88.0 (test compiler)"
        if arguments == [*self.cargo, "--version"]:
            return "cargo 1.88.0 (test compiler)"
        if arguments == ["git", "rev-parse", "HEAD"]:
            return "b" * 40
        if arguments[:3] == ["git", "status", "--porcelain"]:
            return ""
        self.fail("unexpected build query: " + repr(arguments))

    def compile(self, arguments, *, cwd, env, check):
        self.assertEqual(cwd, self.root)
        self.assertTrue(check)
        self.assertEqual(arguments[:3], [*self.cargo, "build"])
        self.assertIn("--locked", arguments)
        self.assertIn("--release", arguments)
        # Model Cargo's target precedence. Without an explicit --target an
        # external override writes elsewhere, leaving old release/ bins intact.
        target = (arguments[arguments.index("--target") + 1]
                  if "--target" in arguments else env.get("CARGO_BUILD_TARGET"))
        directory = Path(env["CARGO_TARGET_DIR"])
        if target:
            directory /= target
        directory /= "release"
        directory.mkdir(parents=True, exist_ok=True)
        for name in portable.BINARIES:
            (directory / name).write_bytes(b"new compiled fixture: " + name.encode())
        self.builds.append((list(arguments), dict(env)))
        return subprocess.CompletedProcess(arguments, 0)

    def build(self, skip=False):
        return portable.build_binaries(self.cargo, self.target, self.epoch, skip)

    def receipt_path(self):
        return self.target / portable.TARGET / portable.PROFILE / "gifromscreen-build.json"

    def rewrite_receipt(self, receipt):
        self.receipt_path().write_text(json.dumps(receipt), encoding="utf-8")

    def test_explicit_target_ignores_external_override_and_never_relabels_old_host_bins(self):
        old_directory = self.target / "release"
        old_directory.mkdir(parents=True)
        for name in portable.BINARIES:
            (old_directory / name).write_bytes(b"stale host binary")
        with patch.dict(os.environ, {"CARGO_BUILD_TARGET": "aarch64-unknown-linux-gnu"}):
            binaries, receipt = self.build()
        arguments, environment = self.builds[0]
        self.assertEqual(arguments[arguments.index("--target") + 1], portable.TARGET)
        self.assertEqual(receipt["target"], portable.TARGET)
        self.assertEqual(receipt["schema_version"], portable.BUILD_RECEIPT_VERSION)
        self.assertEqual(receipt["toolchain"], portable.TOOLCHAIN)
        self.assertEqual(environment["RUSTFLAGS"],
                         "--remap-path-prefix=" + str(self.root) + "=" + portable.PATH_REMAP)
        self.assertEqual(environment["SOURCE_DATE_EPOCH"], str(self.epoch))
        for name, path in binaries.items():
            self.assertEqual(path, self.target / portable.TARGET / "release" / name)
            self.assertEqual(path.read_bytes(), b"new compiled fixture: " + name.encode())
            self.assertEqual(receipt["binaries"][name], portable.sha256(path))
            self.assertEqual((old_directory / name).read_bytes(), b"stale host binary")
        self.assertFalse((old_directory / "gifromscreen-build.json").exists())
        self.assertFalse((self.target / "aarch64-unknown-linux-gnu").exists())

    def test_matching_skip_reuses_only_target_qualified_artifacts(self):
        binaries, receipt = self.build()
        with patch.dict(os.environ, {"CARGO_BUILD_TARGET": "unrelated-target"}):
            reused, previous = self.build(skip=True)
        self.assertEqual(reused, binaries)
        self.assertEqual(previous, receipt)
        self.assertEqual(len(self.builds), 1)

    def test_skip_rejects_mismatched_schema_toolchain_target_profile_and_remap(self):
        _, original = self.build()
        mismatches = {
            "schema_version": [None, True, 0, 2, "1"],
            "toolchain": [None, "1.98.0"],
            "target": [None, "aarch64-unknown-linux-gnu"],
            "profile": [None, "debug"],
            "path_remap": [None, "/some/other/path"],
            "rustc": [None, "rustc 1.98.0 (other compiler)"],
            "cargo": [None, "cargo 1.98.0 (other compiler)"],
        }
        for key, values in mismatches.items():
            for value in values:
                with self.subTest(field=key, value=value):
                    changed = dict(original)
                    if value is None:
                        changed.pop(key)
                    else:
                        changed[key] = value
                    self.rewrite_receipt(changed)
                    with self.assertRaisesRegex(ValueError, key + ".*omit --skip-build"):
                        self.build(skip=True)
        self.assertEqual(len(self.builds), 1)

    def test_old_receipt_without_schema_is_not_grandfathered(self):
        _, receipt = self.build()
        receipt.pop("schema_version")
        receipt.pop("toolchain")
        self.rewrite_receipt(receipt)
        with self.assertRaisesRegex(ValueError, "schema_version.*omit --skip-build"):
            self.build(skip=True)

    def test_invalid_top_level_receipt_is_rejected(self):
        self.build()
        for document in ([], None, "unexpected"):
            with self.subTest(document=document):
                self.rewrite_receipt(document)
                with self.assertRaisesRegex(ValueError, "invalid build receipt"):
                    self.build(skip=True)

    def test_changed_source_or_binary_cannot_be_reused(self):
        binaries, _ = self.build()
        self.source_digest = "c" * 64
        with self.assertRaisesRegex(ValueError, "Rust source changed"):
            self.build(skip=True)
        self.source_digest = "a" * 64
        binaries[portable.BINARIES[0]].write_bytes(b"modified binary")
        with self.assertRaisesRegex(ValueError, "binary differs"):
            self.build(skip=True)

    def test_encoded_rustflags_including_empty_cannot_suppress_the_declared_remap(self):
        for flags in ("", "-C\x1fopt-level=0", "--remap-path-prefix=/wrong=/claimed"):
            with self.subTest(flags=flags):
                with patch.dict(os.environ, {"CARGO_ENCODED_RUSTFLAGS": flags}):
                    with self.assertRaisesRegex(ValueError, "unset CARGO_ENCODED_RUSTFLAGS"):
                        self.build()
        self.assertEqual(self.builds, [])
        self.assertFalse(self.receipt_path().exists())

    def test_failed_compilation_never_records_existing_binaries_as_a_new_build(self):
        self.build()
        previous = self.receipt_path().read_bytes()
        with patch.object(portable.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "cargo")):
            with self.assertRaises(subprocess.CalledProcessError):
                self.build()
        self.assertEqual(self.receipt_path().read_bytes(), previous)

    def test_source_mutation_during_build_does_not_publish_receipt(self):
        with patch.object(portable, "tree_digest", side_effect=["a" * 64, "d" * 64]):
            with self.assertRaisesRegex(ValueError, "Rust sources changed while building"):
                self.build()
        self.assertFalse(self.receipt_path().exists())


class EmbeddedFontNoticeTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix="gifromscreen-font-notices-")
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.fonts = portable.ROOT / "apps/desktop/assets/fonts"
        self.destination = self.root / "licenses"

    def test_license_collector_includes_verified_embedded_font_notices_without_duplicating_font_binary(self):
        with patch.object(portable, "dependency_packages", return_value=[]), \
                patch.object(portable, "run", side_effect=AssertionError("no external command")):
            self.assertEqual(portable.collect_licenses({}, self.destination), 1)
        inventory = json.loads((self.destination / "THIRD-PARTY.json").read_text())
        font = inventory[0]
        self.assertEqual(font["name"], "Noto Sans CJK SC")
        self.assertEqual(font["version"], "2.004")
        self.assertEqual(font["source_kind"], "embedded-font")
        self.assertEqual(font["license"], "OFL-1.1")
        self.assertEqual(font["source_commit"], "523d033d6cb47f4a80c58a35753646f5c3608a78")
        self.assertEqual(font["source_sha256"], "2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b")
        self.assertEqual(font["source_byte_len"], 16_437_364)
        self.assertEqual(font["face_index"], 0)
        self.assertFalse(font["font_modified"])
        copied = self.destination / "fonts/noto-cjk-2.004"
        self.assertEqual({path.name for path in copied.iterdir()}, {"OFL.txt", "COPYRIGHT.txt", "sources.json", "README.md"})
        for name in font["notice_sha256"]:
            self.assertEqual(portable.sha256(self.destination / name), font["notice_sha256"][name])
            self.assertEqual((self.destination / name).read_bytes(), (self.fonts / Path(name).name).read_bytes())
        self.assertIn("Noto Sans CJK SC 2.004", (self.destination / "THIRD-PARTY.txt").read_text())

    def private_sources(self):
        root = self.root / "source"
        source = root / "apps/desktop/assets/fonts"
        shutil.copytree(self.fonts, source)
        return root, source

    def test_changed_font_or_ofl_cannot_be_packaged_with_stale_source_evidence(self):
        root, source = self.private_sources()
        for name in ("NotoSansCJKsc-Regular.otf", "OFL.txt"):
            with self.subTest(name=name):
                path = source / name
                original = path.read_bytes()
                changed = bytearray(original)
                changed[-1] ^= 1
                path.write_bytes(changed)
                with patch.object(portable, "ROOT", root), self.assertRaisesRegex(ValueError, "checksum mismatch"):
                    portable.embedded_font_license(self.destination)
                self.assertFalse(self.destination.exists())
                path.write_bytes(original)

    def test_missing_or_redirected_font_notice_is_rejected(self):
        root, source = self.private_sources()
        font = source / "NotoSansCJKsc-Regular.otf"
        font.unlink()
        font.symlink_to(self.fonts / font.name)
        with patch.object(portable, "ROOT", root), self.assertRaisesRegex(ValueError, "checksum mismatch"):
            portable.embedded_font_license(self.destination)
        self.assertFalse(self.destination.exists())
        font.unlink()
        shutil.copyfile(self.fonts / font.name, font)
        (source / "COPYRIGHT.txt").unlink()
        with patch.object(portable, "ROOT", root), self.assertRaisesRegex(ValueError, "font notice"):
            portable.embedded_font_license(self.destination)


if __name__ == "__main__":
    unittest.main()
