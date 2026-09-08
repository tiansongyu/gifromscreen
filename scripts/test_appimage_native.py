"""Small synthetic fixtures: no test passes an untrusted ELF to ldd."""

from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import appimage_native as native


def fixture_elf(path, soname=None, needed=()):
    header = bytearray(64)
    header[:7] = b"\x7fELF\x02\x01\x01"
    header[18:20] = (62).to_bytes(2, "little")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(header + json.dumps({"soname": soname, "needed": list(needed)}).encode())
    return path


class DependencyParsingTests(unittest.TestCase):
    def test_absolute_dependencies_and_loader(self):
        self.assertEqual(native.parse_ldd(
            "\tlinux-vdso.so.1 (0xdeadbeef)\n"
            "libX11.so.6 => /lib/x86_64-linux-gnu/libX11.so.6 (0xabcd)\n"
            "/lib64/ld-linux-x86-64.so.2 (0x1234)\n"), {
                "libX11.so.6": Path("/lib/x86_64-linux-gnu/libX11.so.6"),
                "ld-linux-x86-64.so.2": Path("/lib64/ld-linux-x86-64.so.2"),
            })

    def test_missing_relative_unsafe_or_unknown_dependencies_rejected(self):
        for line in ("libmissing.so => not found", "libx.so => relative/libx.so (0x12)",
                     "../libx.so => /usr/lib/libx.so (0x12)", "unexpected loader warning",
                     "libx.so => /usr/lib/libx.so (0x12)\nlibx.so => /lib/other.so (0x34)"):
            with self.subTest(line=line), self.assertRaises(ValueError):
                native.parse_ldd(line)

    def test_host_graphics_and_glibc_are_not_bundled_but_libgcc_is(self):
        for name in ("libc.so.6", "ld-linux-x86-64.so.2", "libGLdispatch.so.0",
                     "libvulkan.so.1", "libnvidia-glcore.so.535", "iris_dri.so"):
            self.assertTrue(native._host_library(name))
        for name in ("libgcc_s.so.1", "libpipewire-0.3.so.0", "libX11.so.6"):
            self.assertFalse(native._host_library(name))

    def test_stock_client_modules_and_default_conditional_support_are_explicit(self):
        self.assertEqual(set(native.PW_MODULES), {
            "protocol-native", "client-node", "client-device", "adapter", "metadata", "session-manager"})
        self.assertEqual(set(native.SPA_PLUGINS), {
            "support/libspa-support.so", "support/libspa-dbus.so",
            "support/libspa-journal.so", "audioconvert/libspa-audioconvert.so"})
        self.assertEqual(str(native.CLIENT_TARGET), "usr/share/gifromscreen/pipewire/client.conf")

    def test_tool_environment_does_not_leak_dynamic_loader_overrides(self):
        with patch.dict(os.environ, {"LD_LIBRARY_PATH": "/bad", "LD_PRELOAD": "/bad.so", "LD_AUDIT": "/audit.so"}), \
                patch.object(native.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "ok")) as run:
            self.assertEqual(native._command(["readelf", "--version"]), "ok")
        environment = run.call_args.kwargs["env"]
        self.assertFalse(any(key in environment for key in ("LD_LIBRARY_PATH", "LD_PRELOAD", "LD_AUDIT")))
        self.assertEqual(environment["LC_ALL"], "C")
        self.assertEqual(run.call_args.kwargs["timeout"], 30)

    def test_invalid_elf_is_rejected_before_external_tools(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "not-an-elf"
            source.write_bytes(b"not executable")
            with patch.object(native, "_command") as command, self.assertRaises(ValueError):
                native._elf_information(source)
            command.assert_not_called()

    def test_newer_glibc_is_rejected_without_running_elf(self):
        with tempfile.TemporaryDirectory() as directory:
            source = fixture_elf(Path(directory) / "fixture")
            with patch.object(native, "_command", side_effect=["", "Name: GLIBC_2.36"]), \
                    self.assertRaisesRegex(ValueError, "glibc 2.35"):
                native._elf_information(source)


class PathAndEvidenceTests(unittest.TestCase):
    def test_destination_rejects_escape_and_symlink_parent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "outside").mkdir()
            (root / "usr").symlink_to(root / "outside", target_is_directory=True)
            for path in (Path("../escape"), Path("/absolute"), Path("usr/lib/libx.so")):
                with self.subTest(path=path), self.assertRaises(ValueError):
                    native._destination(root, path)

    def test_private_project_files_cannot_be_symlinks_or_hardlinks(self):
        for symlink in (False, True):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                original = fixture_elf(root / "source")
                appdir = root / "AppDir"
                destination = appdir / "usr/bin" / native.BINARIES[0]
                destination.parent.mkdir(parents=True)
                if symlink:
                    destination.symlink_to(original)
                else:
                    os.link(original, destination)
                with patch.object(native, "_command") as command, self.assertRaisesRegex(ValueError, "private"):
                    native.bundle_native(appdir)
                command.assert_not_called()

    def test_dependency_ownership_is_checked_before_ldd(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = fixture_elf(root / "unowned.so", "unowned.so")
            with patch.object(native, "SYSTEM_ROOTS", (root,)), \
                    patch.object(native, "_command", side_effect=subprocess.CalledProcessError(1, ["dpkg-query"])) as command, \
                    self.assertRaisesRegex(ValueError, "dpkg owner"):
                native._Bundle(root).system_library(source)
            self.assertTrue(all(call.args[0][0] == "dpkg-query" for call in command.call_args_list))

    def test_source_changed_between_plan_and_copy_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.write_bytes(b"initial")
            plan = native._Bundle(root)
            plan.add(source, Path("usr/share/copied"), "resource")
            source.write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "Source changed"):
                plan.install()
            self.assertFalse((root / "usr/share/copied").exists())

    def test_conflicting_sources_for_one_soname_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first, second = root / "first", root / "second"
            first.write_bytes(b"first")
            second.write_bytes(b"second")
            plan = native._Bundle(root)
            plan.add(first, Path("usr/lib/libx.so"), "resource")
            with self.assertRaisesRegex(ValueError, "Conflicting sources"):
                plan.add(second, Path("usr/lib/libx.so"), "resource")

    def test_empty_xkb_tree_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "client.conf"
            config.write_text("# config\n")
            xkb = root / "xkb"
            xkb.mkdir()
            with patch.object(native, "XKB_ROOT", xkb), patch.object(native, "CLIENT_CONFIG", config), \
                    patch.object(native._Bundle, "package_for", return_value=None), \
                    self.assertRaisesRegex(ValueError, "empty"):
                native._Bundle(root).resources()


class MockAssembly:
    """All ELF tools are synthetic; fixture bytes must never reach system ldd."""

    def __init__(self, root):
        self.root = root
        self.appdir = root / "AppDir"
        self.libdir = root / "system/lib"
        self.xkb = root / "xkb"
        self.docs = root / "doc"
        self.common = root / "common"
        self.config = root / "client.conf"
        self.ldd_calls = []
        self.runpaths = {}
        self.trusted = set()
        self.cache = {}
        self.config.write_text("context.modules = [ # exact stock fixture\n]\n")
        (self.xkb / "symbols").mkdir(parents=True)
        (self.xkb / "symbols/us").write_text("xkb_symbols \"basic\" {};\n")
        (self.docs / "native-fixture").mkdir(parents=True)
        (self.docs / "native-fixture/copyright").write_text(
            "Copyright: complete fixture notice\nLicense: see /usr/share/common-licenses/MIT\n")
        self.common.mkdir()
        (self.common / "MIT").write_text("Complete fixture license body.\n")
        self.core = self.library("libpipewire-0.3.so.0", needed=("libgcc_s.so.1", "libc.so.6"))
        self.gcc = self.library("libgcc_s.so.1", needed=("libc.so.6",))
        self.libc = self.library("libc.so.6")
        self.library("libX11.so.6", needed=("libc.so.6",))
        for name in native.BINARIES:
            self.trusted.add(fixture_elf(self.appdir / "usr/bin" / name, needed=(self.core.name,)).resolve())
        for name in native.PW_MODULES:
            self.library("libpipewire-module-" + name + ".so", "pipewire-0.3", needed=(self.core.name,))
        for name in native.SPA_PLUGINS:
            self.library(Path(name).name, str(Path("spa-0.2") / Path(name).parent), needed=("libc.so.6",))

    def library(self, name, directory="", needed=()):
        source = fixture_elf(self.libdir / directory / name, name, needed)
        self.trusted.add(source.resolve())
        self.cache[name] = source
        return source

    def command(self, arguments):
        tool = arguments[0]
        if tool == "ldconfig":
            return "\n".join(name + " (libc6,x86-64) => " + str(path) for name, path in self.cache.items())
        if tool == "dpkg-query":
            if arguments[1] == "-S":
                return "native-fixture: " + arguments[2] + "\n"
            return "native-fixture\t1.0-1\tnative-source\t1.0-1\n"
        path = Path(arguments[-1])
        if tool == "patchelf":
            if arguments[1] == "--set-rpath":
                self.runpaths[path] = arguments[2]
                return ""
            return self.runpaths[path] + "\n"
        info = json.loads(path.read_bytes()[64:])
        if tool == "readelf":
            if arguments[1] == "--version-info":
                return "Name: GLIBC_2.35\n"
            return "\n".join("(NEEDED) Shared library: [" + name + "]" for name in info["needed"]) + (
                "\n(SONAME) Library soname: [" + info["soname"] + "]" if info["soname"] else "")
        if tool == "ldd":
            if path.resolve() not in self.trusted:
                raise AssertionError("ldd received a non-trusted source")
            self.ldd_calls.append(path)
            # Include transitive resolutions exactly as real ldd does.
            names = set(info["needed"])
            if self.core.name in names:
                names.update(("libgcc_s.so.1", "libc.so.6"))
            return "\n".join(name + " => " + str(self.cache[name]) + " (0x1234)" for name in sorted(names))
        raise AssertionError("Unexpected tool: " + tool)

    def patched(self):
        stack = ExitStack()
        for name, value in (("LIBDIR", self.libdir), ("SYSTEM_ROOTS", (self.libdir,)),
                            ("XKB_ROOT", self.xkb), ("DOC_ROOT", self.docs), ("COMMON_ROOT", self.common),
                            ("CLIENT_CONFIG", self.config), ("GUI_SONAMES", ("libX11.so.6",))):
            stack.enter_context(patch.object(native, name, value))
        stack.enter_context(patch.object(native, "_command", side_effect=self.command))
        return stack


class AssemblyTests(unittest.TestCase):
    def test_full_mock_closure_resources_runpaths_and_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture = MockAssembly(Path(directory))
            with fixture.patched():
                evidence = native.bundle_native(fixture.appdir)
            self.assertFalse(evidence["redistribution_ready"])
            self.assertFalse(evidence["corresponding_source_collected"])
            self.assertFalse(evidence["ld_library_path_used"])
            files = {entry["target"]: entry for entry in evidence["files"]}
            self.assertNotIn("usr/lib/libc.so.6", files)
            self.assertIn("libc.so.6", evidence["host_libraries"])
            self.assertIn("usr/lib/libgcc_s.so.1", files)
            self.assertEqual(len(fixture.ldd_calls), len(set(fixture.ldd_calls)))
            self.assertEqual((fixture.appdir / native.CLIENT_TARGET).read_bytes(), fixture.config.read_bytes())
            for relative, record in files.items():
                target = fixture.appdir / relative
                self.assertEqual(record["patched_sha256"], hashlib.sha256(target.read_bytes()).hexdigest())
                if record["package"]:
                    self.assertEqual(record["source_package"], "native-source")
                if "elf" in record:
                    self.assertTrue(record["runpath"].startswith("$ORIGIN"))
                    self.assertNotIn(str(fixture.root), record["runpath"])
                    self.assertEqual(target.stat().st_mode & 0o777, 0o755)
            self.assertEqual(files["usr/bin/gif-from-screen"]["runpath"], "$ORIGIN/../lib")
            self.assertEqual(files["usr/lib/pipewire-0.3/libpipewire-module-client-node.so"]["runpath"], "$ORIGIN:$ORIGIN/..")
            self.assertEqual(files["usr/lib/spa-0.2/support/libspa-dbus.so"]["runpath"], "$ORIGIN/../..")
            self.assertEqual(files["usr/lib/libgcc_s.so.1"]["runpath"], "$ORIGIN")
            notices = evidence["packages"][0]["license_files"]
            self.assertEqual(len(notices), 2)
            self.assertEqual((fixture.appdir / notices[0]).read_bytes(), (fixture.docs / "native-fixture/copyright").read_bytes())
            self.assertEqual((fixture.appdir / notices[1]).read_bytes(), (fixture.common / "MIT").read_bytes())

    def test_existing_native_target_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture = MockAssembly(Path(directory))
            occupied = fixture.appdir / "usr/lib/libgcc_s.so.1"
            occupied.parent.mkdir(parents=True)
            occupied.write_bytes(b"user-owned")
            with fixture.patched(), self.assertRaisesRegex(ValueError, "already exists"):
                native.bundle_native(fixture.appdir)
            self.assertEqual(occupied.read_bytes(), b"user-owned")
            self.assertFalse(fixture.runpaths)

    def test_missing_direct_dependency_cannot_produce_success_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture = MockAssembly(Path(directory))
            original = fixture.command

            def omitted(arguments):
                return "" if arguments[0] == "ldd" else original(arguments)

            with fixture.patched(), patch.object(native, "_command", side_effect=omitted), \
                    self.assertRaisesRegex(ValueError, "omitted required"):
                native.bundle_native(fixture.appdir)
            self.assertFalse(fixture.runpaths)


if __name__ == "__main__":
    unittest.main()
