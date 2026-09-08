#!/usr/bin/env python3
"""Small synthetic envelope tests, not substitutes for the pinned release hashes."""

from contextlib import ExitStack
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import appimage_tools as tools


def fixture_elf(machine=62):
    header = bytearray(64)
    header[:7] = b"\x7fELF\x02\x01\x01"
    header[16:18] = (3).to_bytes(2, "little")
    header[18:20] = machine.to_bytes(2, "little")
    header[20:24] = (1).to_bytes(4, "little")
    return bytes(header) + b"synthetic non-executable test payload" * 5


class ToolVerificationTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix="gifromscreen-tools-test-")
        self.addCleanup(scratch.cleanup)
        self.directory = Path(scratch.name)
        self.payload = fixture_elf()
        self.real_pins = tools._load_pins()
        self.pins = tuple(replace(pin, byte_len=len(self.payload),
                                  sha256=hashlib.sha256(self.payload).hexdigest())
                          for pin in self.real_pins)
        for pin in self.pins:
            (self.directory / pin.filename).write_bytes(self.payload)
        patches = ExitStack()
        self.addCleanup(patches.close)
        patches.enter_context(patch.object(tools, "_load_pins", return_value=self.pins))
        self.execute = patches.enter_context(patch.object(subprocess, "Popen", side_effect=AssertionError("tools must never execute")))

    def first(self):
        return self.directory / self.pins[0].filename

    def test_valid_small_envelopes_return_absolute_paths_without_executing_or_chmod(self):
        self.first().chmod(0o644)
        before = self.first().stat()
        actual = tools.verify_tools(str(self.directory))
        self.assertEqual(actual, tuple(self.directory / pin.filename for pin in self.pins))
        self.assertTrue(all(path.is_absolute() for path in actual))
        self.assertEqual(self.first().stat().st_mode, before.st_mode)
        self.assertEqual(self.first().read_bytes(), self.payload)
        self.execute.assert_not_called()

    def test_published_manifest_binds_both_named_versions_sizes_and_full_source_commits(self):
        first, second = self.real_pins
        self.assertEqual((first.version, first.byte_len), ("1.9.1", 15092216))
        self.assertEqual((second.version, second.byte_len), ("20251108", 944632))
        self.assertEqual(first.sha256, "ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0")
        self.assertEqual(second.sha256, "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d")
        self.assertEqual(first.source_commit, "8c8c91f762b412a19f4e8d2c4b35afb98f2d7c81")
        self.assertEqual(second.source_commit, "dd6cebedcbddde9c82f89b011e8e1d40b6e43868")

    def test_same_size_tampering_fails_hash_without_running_either_tool(self):
        data = self.payload[:-1] + bytes([self.payload[-1] ^ 1])
        self.first().write_bytes(data)
        with self.assertRaisesRegex(ValueError, "SHA256 mismatch"):
            tools.verify_tools(self.directory)
        self.execute.assert_not_called()

    def test_truncated_and_oversized_files_fail_before_hashing(self):
        for data in (self.payload[:-1], self.payload + b"x"):
            with self.subTest(length=len(data)):
                self.first().write_bytes(data)
                with patch.object(tools.hashlib, "sha256", side_effect=AssertionError("size must reject first")):
                    with self.assertRaisesRegex(ValueError, "size mismatch"):
                        tools.verify_tools(self.directory)

    def test_missing_second_tool_rejects_the_whole_pair(self):
        (self.directory / self.pins[1].filename).unlink()
        with self.assertRaises(FileNotFoundError):
            tools.verify_tools(self.directory)
        self.execute.assert_not_called()

    def test_file_symlink_and_symlinked_directory_are_rejected(self):
        source = self.directory / "original-tool"
        self.first().rename(source)
        self.first().symlink_to(source.name)
        with self.assertRaisesRegex(ValueError, "regular non-symlink"):
            tools.verify_tools(self.directory)
        self.first().unlink()
        source.rename(self.first())
        alias = self.directory / "alias"
        alias.symlink_to(self.directory, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "non-symlink directory"):
            tools.verify_tools(alias)

    def test_directory_and_fifo_never_become_executable_inputs(self):
        self.first().unlink()
        self.first().mkdir()
        with self.assertRaisesRegex(ValueError, "regular non-symlink"):
            tools.verify_tools(self.directory)
        self.first().rmdir()
        os.mkfifo(self.first())
        with self.assertRaisesRegex(ValueError, "regular non-symlink"):
            tools.verify_tools(self.directory)
        self.execute.assert_not_called()

    def test_wrong_architecture_is_rejected_even_with_matching_mock_hash(self):
        for machine in (3, 40, 183):
            with self.subTest(machine=machine):
                data = fixture_elf(machine)
                self.first().write_bytes(data)
                pin = replace(self.pins[0], byte_len=len(data), sha256=hashlib.sha256(data).hexdigest())
                with patch.object(tools, "_load_pins", return_value=(pin, self.pins[1])):
                    with self.assertRaisesRegex(ValueError, "x86_64 little-endian ELF64"):
                        tools.verify_tools(self.directory)

    def test_non_elf_or_wrong_class_and_byte_order_are_rejected(self):
        for index, replacement in ((0, 0), (4, 1), (5, 2), (6, 0), (20, 0)):
            with self.subTest(index=index):
                data = bytearray(self.payload)
                data[index] = replacement
                self.first().write_bytes(data)
                pin = replace(self.pins[0], sha256=hashlib.sha256(data).hexdigest())
                with patch.object(tools, "_load_pins", return_value=(pin, self.pins[1])):
                    with self.assertRaisesRegex(ValueError, "x86_64 little-endian ELF64"):
                        tools.verify_tools(self.directory)


class ToolManifestTests(unittest.TestCase):
    def test_invalid_inventory_paths_size_and_digest_are_rejected(self):
        original = json.loads(tools.MANIFEST.read_text(encoding="utf-8"))
        with tempfile.TemporaryDirectory(prefix="gifromscreen-tool-manifest-") as scratch:
            path = Path(scratch) / "tools.json"
            for key, value in (("filename", "../runtime"), ("byte_len", True),
                               ("byte_len", tools.MAX_TOOL_BYTES + 1),
                               ("sha256", "0" * 63), ("source_commit", "latest"),
                               ("url", "https://example.com/replacement")):
                with self.subTest(field=key, value=value):
                    changed = json.loads(json.dumps(original))
                    changed["tools"]["runtime"][key] = value
                    path.write_text(json.dumps(changed), encoding="utf-8")
                    with patch.object(tools, "MANIFEST", path):
                        with self.assertRaisesRegex(ValueError, "Invalid pinned"):
                            tools._load_pins()
            original["tools"].pop("runtime")
            path.write_text(json.dumps(original), encoding="utf-8")
            with patch.object(tools, "MANIFEST", path):
                with self.assertRaisesRegex(ValueError, "exactly appimagetool and runtime"):
                    tools._load_pins()


if __name__ == "__main__":
    unittest.main()
