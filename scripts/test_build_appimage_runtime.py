#!/usr/bin/env python3
"""Runtime build contracts; synthetic ELF/source bytes, no Docker or compiler."""

import argparse
from contextlib import redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import types
import unittest
from unittest.mock import patch

import build_appimage_runtime as builder


CONTAINER = "a" * 64
OTHER_CONTAINER = "b" * 64
TOKEN = "c" * 32
RECIPE_HASH = "d" * 64
IMAGE = "sha256:" + "e" * 64
PASS_LINE = "patched_runtime_cleanup=PASS external_symlinks=preserved fixtures=removed\n"


def runtime_header():
    value = bytearray(64)
    value[:7] = b"\x7fELF\x02\x01\x01"
    value[8:11] = b"AI\x02"
    value[18:20] = (62).to_bytes(2, "little")
    return bytes(value)


def source_record(data):
    return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(),
            "url": "https://example.invalid/synthetic-test-only"}


class IsolatedTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="gfs-runtime-wrapper-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        # Every test fails immediately if it accidentally reaches Docker or a
        # compiler. Specific tests replace these mocks with deterministic fakes.
        for name, target in (("run_owned", builder), ("run", builder.subprocess)):
            guard = patch.object(target, name, side_effect=AssertionError("no host process execution"))
            guard.start()
            self.addCleanup(guard.stop)


class SourceAndInventoryTests(IsolatedTests):
    def test_archive_pins_accept_only_exact_bytes_without_execution(self):
        data = b"synthetic source archive; never extracted"
        (self.root / "source.tar.gz").write_bytes(data)
        pins = {"archives": {"source.tar.gz": source_record(data)}}
        self.assertEqual(builder.verify_sources(self.root, pins), {"source.tar.gz": self.root / "source.tar.gz"})
        (self.root / "source.tar.gz").write_bytes(b"X" * len(data))
        with self.assertRaisesRegex(ValueError, "does not match"):
            builder.verify_sources(self.root, pins)

    def test_archive_hash_or_size_pin_tampering_is_rejected(self):
        data = b"original"
        (self.root / "source.tar.gz").write_bytes(data)
        for key, value in (("sha256", "0" * 64), ("bytes", len(data) + 1)):
            with self.subTest(key=key):
                record = source_record(data)
                record[key] = value
                with self.assertRaises(ValueError):
                    builder.verify_sources(self.root, {"archives": {"source.tar.gz": record}})

    def test_missing_link_directory_and_fifo_archives_are_rejected_before_hash(self):
        data = b"outside"
        outside = self.root / "original"
        outside.write_bytes(data)
        for kind in ("missing", "symlink", "directory", "fifo"):
            with self.subTest(kind=kind):
                source = self.root / (kind + ".tar.gz")
                if kind == "symlink":
                    source.symlink_to(outside)
                elif kind == "directory":
                    source.mkdir()
                elif kind == "fifo":
                    os.mkfifo(source)
                with patch.object(builder.portable, "sha256", side_effect=AssertionError("do not read special files")), \
                        self.assertRaises((ValueError, FileNotFoundError)):
                    builder.verify_sources(self.root, {"archives": {source.name: source_record(data)}})

    def test_archive_filename_cannot_escape_or_alias(self):
        for name in ("", ".", "..", "../source.tar.gz", "/source.tar.gz", "dir/source.tar.gz"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                builder.verify_sources(self.root, {"archives": {name: source_record(b"x")}})

    def test_source_manifest_requires_integer_version(self):
        recipe = self.root / "recipe"
        recipe.mkdir()
        for version in (True, "1", 0, 2, None):
            with self.subTest(version=version):
                (recipe / "sources.json").write_text(json.dumps({"format_version": version}))
                with patch.object(builder, "RECIPE", recipe), self.assertRaises(ValueError):
                    builder.source_pins()

    def test_inventory_hashes_nested_regular_files_without_executing(self):
        (self.root / "nested").mkdir()
        (self.root / "nested/runtime.o").write_bytes(b"not a compiled object")
        self.assertEqual(builder.inventory(self.root), {
            "nested/runtime.o": {"bytes": 21, "sha256": hashlib.sha256(b"not a compiled object").hexdigest()}})

    def test_inventory_rejects_links_fifo_and_socket(self):
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "file").write_bytes(b"not inventory")
        for kind in ("file_link", "directory_link", "dangling_link", "fifo", "socket"):
            with self.subTest(kind=kind):
                artifacts = self.root / kind
                artifacts.mkdir()
                item = artifacts / "unexpected"
                connection = None
                if kind == "file_link":
                    item.symlink_to(outside / "file")
                elif kind == "directory_link":
                    item.symlink_to(outside, target_is_directory=True)
                elif kind == "dangling_link":
                    item.symlink_to("absent")
                elif kind == "fifo":
                    os.mkfifo(item)
                else:
                    connection = socket.socket(socket.AF_UNIX)
                    connection.bind(str(item))
                try:
                    with self.assertRaisesRegex(ValueError, "regular files"):
                        builder.inventory(artifacts)
                finally:
                    if connection is not None:
                        connection.close()

    def test_inventory_byte_limit_precedes_hashing(self):
        path = self.root / "oversized-sparse-fixture"
        with path.open("wb") as target:
            target.truncate(512 * 1024 * 1024 + 1)
        with patch.object(builder.portable, "sha256", side_effect=AssertionError("must not hash oversized bytes")), \
                self.assertRaisesRegex(ValueError, "inventory budget"):
            builder.inventory(self.root)

    def test_inventory_root_itself_cannot_be_a_symlink(self):
        actual = self.root / "actual"
        actual.mkdir()
        (actual / "file").write_bytes(b"same bytes")
        alias = self.root / "alias"
        alias.symlink_to(actual, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "real directory"):
            builder.inventory(alias)


class RuntimeReceiptTests(IsolatedTests):
    def setUp(self):
        super().setUp()
        self.recipe = self.root / "recipe"
        self.recipe.mkdir()
        (self.recipe / "cleanup.patch").write_bytes(b"synthetic patch; never applied")
        self.pins = {"format_version": 1, "upstream_commit": "f" * 40,
                     "archives": {"source.tar.gz": source_record(b"source")}}
        (self.recipe / "sources.json").write_text(json.dumps(self.pins))
        for name, value in (("RECIPE", self.recipe), ("recipe_digest", lambda: RECIPE_HASH)):
            replacement = patch.object(builder, name, value)
            replacement.start()
            self.addCleanup(replacement.stop)
        self.directory = self.root / "build"
        self.artifacts = self.directory / "artifacts"
        self.artifacts.mkdir(parents=True)
        self.runtime = self.artifacts / "runtime-x86_64"
        self.runtime.write_bytes(runtime_header())
        (self.artifacts / "cleanup-test.log").write_text(PASS_LINE)
        self.materials = self.directory / "sources"
        self.materials.mkdir()
        (self.materials / "source.tar.gz").write_bytes(b"source")
        for path in self.recipe.iterdir():
            (self.materials / path.name).write_bytes(path.read_bytes())
        self.receipt = {"format_version": 1, "recipe_sha256": RECIPE_HASH,
                        "sources": self.pins, "patch_sha256": builder.portable.sha256(self.recipe / "cleanup.patch"),
                        "artifacts": builder.inventory(self.artifacts),
                        "source_materials": builder.inventory(self.materials)}
        self.write_receipt()

    def write_receipt(self, refresh_inventory=False):
        if refresh_inventory:
            self.receipt["artifacts"] = builder.inventory(self.artifacts)
        (self.directory / builder.RECEIPT).write_text(json.dumps(self.receipt))

    def test_valid_synthetic_header_and_bound_cleanup_evidence_are_accepted(self):
        runtime, receipt = builder.verify_runtime_build(self.directory)
        self.assertEqual(runtime, self.runtime)
        self.assertEqual(receipt, self.receipt)

    def test_recipe_source_pin_patch_and_receipt_version_changes_are_rejected(self):
        mutations = (("format_version", True), ("format_version", "1"),
                     ("recipe_sha256", "0" * 64), ("sources", {}), ("patch_sha256", "0" * 64))
        original = dict(self.receipt)
        for key, value in mutations:
            with self.subTest(key=key, value=value):
                self.receipt = {**original, key: value}
                self.write_receipt()
                with self.assertRaisesRegex(ValueError, "recipe changed"):
                    builder.verify_runtime_build(self.directory)
        self.receipt = original
        self.write_receipt()
        (self.recipe / "cleanup.patch").write_bytes(b"changed current patch")
        with self.assertRaisesRegex(ValueError, "recipe changed"):
            builder.verify_runtime_build(self.directory)

    def test_current_source_manifest_changes_invalidate_reuse(self):
        changed = {**self.pins, "upstream_commit": "a" * 40}
        (self.recipe / "sources.json").write_text(json.dumps(changed))
        with self.assertRaisesRegex(ValueError, "recipe changed"):
            builder.verify_runtime_build(self.directory)

    def test_modified_missing_or_unlisted_artifact_rejects_receipt(self):
        original = self.runtime.read_bytes()
        self.runtime.write_bytes(original + b"changed")
        with self.assertRaisesRegex(ValueError, "artifacts differ"):
            builder.verify_runtime_build(self.directory)
        self.runtime.unlink()
        with self.assertRaisesRegex(ValueError, "artifacts differ"):
            builder.verify_runtime_build(self.directory)
        self.runtime.write_bytes(original)
        (self.artifacts / "unlisted").write_bytes(b"extra")
        with self.assertRaisesRegex(ValueError, "artifacts differ"):
            builder.verify_runtime_build(self.directory)

    def test_same_hash_shape_receipt_cannot_replace_runtime_with_symlink(self):
        outside = self.root / "outside-runtime"
        self.runtime.rename(outside)
        self.runtime.symlink_to(outside)
        with self.assertRaisesRegex(ValueError, "regular files"):
            builder.verify_runtime_build(self.directory)

    def test_receipted_bad_elf_class_endianness_machine_or_appimage_magic_fail(self):
        invalid = [b"short"]
        for offset, value in ((0, 0), (4, 1), (5, 2), (6, 0), (8, ord("X")), (10, 1), (18, 183), (19, 1)):
            header = bytearray(runtime_header())
            header[offset] = value
            invalid.append(header)
        for data in invalid:
            with self.subTest(data=bytes(data[:20])):
                self.runtime.write_bytes(data)
                self.write_receipt(refresh_inventory=True)
                with self.assertRaisesRegex(ValueError, "type-2 AppImage ELF"):
                    builder.verify_runtime_build(self.directory)

    def test_cleanup_failure_remains_failure_even_with_fresh_artifact_receipt(self):
        for log in ("", "patched_runtime_cleanup=FAIL\n", "unrelated tests PASS\n"):
            with self.subTest(log=log):
                (self.artifacts / "cleanup-test.log").write_text(log)
                self.write_receipt(refresh_inventory=True)
                with self.assertRaisesRegex(ValueError, "cleanup regression"):
                    builder.verify_runtime_build(self.directory)

    def test_stale_cleanup_log_cannot_be_relabelled_without_inventory_change(self):
        (self.artifacts / "cleanup-test.log").write_text(PASS_LINE + "modified evidence\n")
        with self.assertRaisesRegex(ValueError, "artifacts differ"):
            builder.verify_runtime_build(self.directory)

    def test_retained_archive_patch_manifest_missing_or_extra_material_invalidates_receipt(self):
        for name in ("source.tar.gz", "cleanup.patch", "sources.json"):
            path = self.materials / name
            original = path.read_bytes()
            for missing in (False, True):
                with self.subTest(name=name, missing=missing):
                    if missing:
                        path.unlink()
                    else:
                        path.write_bytes(b"changed retained source evidence")
                    with self.assertRaisesRegex(ValueError, "source materials differ"):
                        builder.verify_runtime_build(self.directory)
                    path.write_bytes(original)
        (self.materials / "unlisted").write_bytes(b"unlisted retained file")
        with self.assertRaisesRegex(ValueError, "source materials differ"):
            builder.verify_runtime_build(self.directory)


class ContainerOwnershipTests(IsolatedTests):
    @staticmethod
    def details(container=CONTAINER, token=TOKEN, running=False):
        return {"Id": container, "Config": {"Labels": {builder.LABEL: token}},
                "State": {"Running": running, "ExitCode": 0}}

    def test_inspect_requires_exact_full_id_before_any_process(self):
        for identity in ("", "a" * 12, "a" * 63, "A" * 64, "a" * 65,
                         CONTAINER + "\nwarning", "--latest", "foreign-name"):
            with self.subTest(identity=identity), patch.object(builder, "query") as query, self.assertRaises(ValueError):
                builder.inspect_owned(identity, TOKEN)
            query.assert_not_called()

    def test_inspect_accepts_only_matching_id_and_exact_label(self):
        with patch.object(builder, "query", return_value=json.dumps([self.details()])) as query:
            self.assertEqual(builder.inspect_owned(CONTAINER, TOKEN), self.details())
            query.assert_called_once_with(["docker", "inspect", CONTAINER])
        for details in (self.details(container=OTHER_CONTAINER), self.details(token="different"),
                        {**self.details(), "Config": {"Labels": {"unrelated": TOKEN}}}):
            with self.subTest(details=details), patch.object(builder, "query", return_value=json.dumps([details])), \
                    self.assertRaisesRegex(ValueError, "ownership receipt"):
                builder.inspect_owned(CONTAINER, TOKEN)

    def test_cleanup_revalidates_ownership_before_stop_and_remove(self):
        replies = [json.dumps([self.details(running=True)]), CONTAINER,
                   json.dumps([self.details(running=False)]), CONTAINER]
        with patch.object(builder, "query", side_effect=replies) as query:
            builder.stop_owned(CONTAINER, TOKEN)
        self.assertEqual([call.args[0] for call in query.call_args_list], [
            ["docker", "inspect", CONTAINER], ["docker", "stop", "--time", "3", CONTAINER],
            ["docker", "inspect", CONTAINER], ["docker", "rm", CONTAINER]])

    def test_cleanup_never_stops_foreign_container_or_removes_after_label_change(self):
        for changed_after_stop in (False, True):
            replies = ([json.dumps([self.details(running=True)]), CONTAINER]
                       if changed_after_stop else [])
            replies.append(json.dumps([self.details(token="foreign")]))
            with self.subTest(changed_after_stop=changed_after_stop), \
                    patch.object(builder, "query", side_effect=replies) as query, self.assertRaises(ValueError):
                builder.stop_owned(CONTAINER, TOKEN)
            operations = [call.args[0][1] for call in query.call_args_list]
            self.assertNotIn("rm", operations)
            self.assertEqual("stop" in operations, changed_after_stop)

    def test_cleanup_does_not_force_remove_container_that_remains_running(self):
        replies = [json.dumps([self.details(running=True)]), CONTAINER,
                   json.dumps([self.details(running=True)])]
        with patch.object(builder, "query", side_effect=replies) as query, self.assertRaises(RuntimeError):
            builder.stop_owned(CONTAINER, TOKEN)
        self.assertNotIn("rm", [call.args[0][1] for call in query.call_args_list])

    def test_metadata_size_is_bounded(self):
        def produce(_arguments, **kwargs):
            kwargs["stdout"].write("x" * (1024 * 1024 + 1))

        with patch.object(builder, "run_owned", side_effect=produce), self.assertRaises(ValueError):
            builder.query(["docker", "inspect", CONTAINER])

    def test_create_stderr_does_not_contaminate_returned_container_identity(self):
        def produce(_arguments, **kwargs):
            kwargs["stderr"].write("Docker diagnostic warning, not a container ID\n")
            kwargs["stdout"].write(CONTAINER + "\n")

        with patch.object(builder, "run_owned", side_effect=produce):
            self.assertEqual(builder.query(["docker", "create", "synthetic-image"]), CONTAINER)

    def test_failed_create_never_returns_an_id_from_its_output(self):
        def produce(_arguments, **kwargs):
            kwargs["stdout"].write(CONTAINER + "\n")
            kwargs["stderr"].write("creation failed\n")
            return 1

        with patch.object(builder, "run_owned", side_effect=produce), \
                self.assertRaisesRegex(RuntimeError, "creation failed"):
            builder.query(["docker", "create", "synthetic-image"])


class BuildLifecycleTests(IsolatedTests):
    def setUp(self):
        super().setUp()
        self.recipe = self.root / "recipe"
        self.recipe.mkdir()
        for name in ("cleanup.patch", "build-runtime.sh", "cleanup-test.c", "Dockerfile", "install-dependencies.sh"):
            (self.recipe / name).write_text("synthetic recipe " + name)
        self.source_dir = self.root / "archives"
        self.source_dir.mkdir()
        self.pins = {"format_version": 1, "upstream_commit": "f" * 40,
                     "base_image": "alpine:synthetic@" + IMAGE, "archives": {}}
        (self.recipe / "Dockerfile").write_text("FROM " + self.pins["base_image"] + "\n")
        for name in ("type2-runtime-dd6cebed.tar.gz", "fuse-3.15.0.tar.xz", "squashfuse-0.5.2.tar.gz"):
            data = ("synthetic source " + name).encode()
            (self.source_dir / name).write_bytes(data)
            self.pins["archives"][name] = source_record(data)
        (self.recipe / "sources.json").write_text(json.dumps(self.pins))
        self.output = self.root / "output"
        self.commands = []
        self.running = False
        self.create_reply = CONTAINER
        self.label = TOKEN
        self.fail_start = False
        self.extracted_archive = None
        for name, value in (("RECIPE", self.recipe), ("recipe_digest", lambda: RECIPE_HASH),
                            ("extract_checked", self.extract), ("logged", self.logged), ("query", self.query)):
            mocker = patch.object(builder, name, value)
            mocker.start()
            self.addCleanup(mocker.stop)
        for target, name, value in ((builder.os, "getuid", lambda: 1000), (builder.os, "getgid", lambda: 1000),
                                    (builder.os, "uname", lambda: types.SimpleNamespace(machine="x86_64")),
                                    (builder.uuid, "uuid4", lambda: types.SimpleNamespace(hex=TOKEN))):
            mocker = patch.object(target, name, value)
            mocker.start()
            self.addCleanup(mocker.stop)
        compile_mock = patch.object(builder.subprocess, "run", return_value=subprocess.CompletedProcess([], 0))
        compile_mock.start()
        self.addCleanup(compile_mock.stop)

    def extract(self, archive, unpack):
        self.extracted_archive = archive
        work = unpack / "synthetic-runtime"
        (work / "src/runtime").mkdir(parents=True)
        (work / "patches/libfuse").mkdir(parents=True)
        (work / "patches/libfuse/mount.c.diff").write_text("synthetic patch")
        return work

    def query(self, command):
        self.commands.append(command)
        if command[1] == "create":
            return self.create_reply
        if command[1] == "inspect":
            return json.dumps([ContainerOwnershipTests.details(token=self.label, running=self.running)])
        if command[1] == "stop":
            self.running = False
            return CONTAINER
        if command[1] == "rm":
            return CONTAINER
        raise AssertionError("Unexpected mocked Docker query")

    def logged(self, command, _path, _timeout):
        self.commands.append(command)
        if command[1] == "build":
            Path(command[command.index("--iidfile") + 1]).write_text(IMAGE)
            return
        if command[1] != "start":
            raise AssertionError("Unexpected mocked Docker log operation")
        if self.fail_start:
            self.running = True
            raise subprocess.TimeoutExpired(command, 300)
        (self.output / "artifacts/runtime-x86_64").write_bytes(runtime_header())
        (self.output / "artifacts/cleanup-test.log").write_text(PASS_LINE)

    def invoke(self):
        with redirect_stdout(io.StringIO()):
            builder.build(argparse.Namespace(source_dir=self.source_dir, output_dir=self.output))

    def test_valid_build_uses_exact_created_id_and_never_names_or_global_cleanup(self):
        self.invoke()
        create = next(command for command in self.commands if command[1] == "create")
        self.assertIn(builder.LABEL + "=" + TOKEN, create)
        self.assertIn(IMAGE, create)
        self.assertEqual(create[create.index("--network") + 1], "none")
        self.assertEqual(create[create.index("--user") + 1], "1000:1000")
        self.assertIn("--read-only", create)
        self.assertEqual([command for command in self.commands if command[1] == "start"],
                         [["docker", "start", "--attach", CONTAINER]])
        self.assertEqual([command for command in self.commands if command[1] == "rm"], [["docker", "rm", CONTAINER]])
        receipt = json.loads((self.output / builder.RECEIPT).read_text())
        self.assertFalse(receipt["redistribution_ready"])
        self.assertFalse(receipt["corresponding_source_complete"])
        self.assertEqual(self.extracted_archive, self.output / "sources/type2-runtime-dd6cebed.tar.gz")
        self.assertEqual(receipt["source_materials"], builder.inventory(self.output / "sources"))

    def test_contaminated_create_identity_never_reaches_lifecycle_operations(self):
        self.create_reply = "warning\n" + CONTAINER
        with self.assertRaisesRegex(ValueError, "container identity"):
            self.invoke()
        self.assertFalse(any(command[1] in ("inspect", "start", "stop", "rm") for command in self.commands))
        self.assertFalse((self.output / builder.RECEIPT).exists())

    def test_foreign_label_after_create_never_starts_stops_or_removes_container(self):
        self.label = "foreign"
        with self.assertRaisesRegex(ValueError, "ownership receipt"):
            self.invoke()
        self.assertFalse(any(command[1] in ("start", "stop", "rm") for command in self.commands))

    def test_build_timeout_stops_only_owned_container_and_never_writes_success_receipt(self):
        self.fail_start = True
        with self.assertRaises(subprocess.TimeoutExpired):
            self.invoke()
        lifecycle = [command for command in self.commands if command[1] in ("stop", "rm")]
        self.assertEqual(lifecycle, [["docker", "stop", "--time", "3", CONTAINER], ["docker", "rm", CONTAINER]])
        self.assertFalse((self.output / builder.RECEIPT).exists())

    def test_changed_recipe_during_build_still_cleans_up_but_cannot_issue_receipt(self):
        with patch.object(builder, "recipe_digest", side_effect=[RECIPE_HASH, "0" * 64]), \
                self.assertRaisesRegex(ValueError, "recipe changed while building"):
            self.invoke()
        self.assertIn(["docker", "rm", CONTAINER], self.commands)
        self.assertFalse((self.output / builder.RECEIPT).exists())

    def test_sdk_dockerfile_must_match_manifest_before_any_docker_call(self):
        (self.recipe / "Dockerfile").write_text("FROM unrelated:latest\n")
        with self.assertRaisesRegex(ValueError, "SDK base"):
            self.invoke()
        self.assertFalse(self.commands)
        self.assertFalse(self.output.exists())

    def test_tampered_copied_archive_fails_second_pin_check_before_extraction(self):
        original_copy = builder.shutil.copyfile

        def corrupt_copy(source, target):
            result = original_copy(source, target)
            if Path(target).name == "type2-runtime-dd6cebed.tar.gz":
                data = Path(target).read_bytes()
                Path(target).write_bytes(b"X" * len(data))
            return result

        with patch.object(builder.shutil, "copyfile", side_effect=corrupt_copy), \
                self.assertRaisesRegex(ValueError, "does not match its pin"):
            self.invoke()
        self.assertIsNone(self.extracted_archive)
        self.assertFalse(self.commands)
        self.assertFalse((self.output / builder.RECEIPT).exists())

    def test_source_material_change_during_build_cleans_up_but_cannot_issue_receipt(self):
        original_logged = self.logged

        def mutate_material(command, path, timeout):
            original_logged(command, path, timeout)
            if command[1] == "start":
                (self.output / "sources/cleanup.patch").write_bytes(b"changed after compilation")

        with patch.object(builder, "logged", side_effect=mutate_material), \
                self.assertRaisesRegex(ValueError, "recipe changed while building"):
            self.invoke()
        self.assertIn(["docker", "rm", CONTAINER], self.commands)
        self.assertFalse((self.output / builder.RECEIPT).exists())


if __name__ == "__main__":
    unittest.main()
