"""Safety and input checks for the benchmark harness; no X server or GUI starts."""

import contextlib
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("recording_perf", Path(__file__).with_name("recording_perf.py"))
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


class HarnessTests(unittest.TestCase):
    def test_display_is_owned_and_cannot_inherit_host_connections(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            server = mock.Mock(pid=27182)
            inherited = {"HOME": "/existing-home", "DISPLAY": ":host", "WAYLAND_DISPLAY": "host-wayland", "WAYLAND_SOCKET": "7", "DBUS_SESSION_BUS_ADDRESS": "host-bus", "XAUTHORITY": "/host/authority"}
            with mock.patch.dict(os.environ, inherited, clear=True), \
                 mock.patch.object(benchmark.subprocess, "Popen", return_value=server) as spawn, \
                 mock.patch.object(benchmark, "read_display_number", return_value=":431"), \
                 mock.patch.object(benchmark, "stop_child") as stop:
                with benchmark.display(directory, 1920, 1080) as (environment, pid):
                    self.assertEqual(pid, server.pid)
                    self.assertEqual(environment["DISPLAY"], ":431")
                    self.assertEqual(environment["HOME"], inherited["HOME"])
                    self.assertEqual(environment["XDG_RUNTIME_DIR"], str(directory / "runtime"))
                    for name in benchmark.REMOVED_CONNECTION_ENV:
                        self.assertNotIn(name, environment)
                    self.assertIn("1920x1080x24", spawn.call_args.args[0])
                    self.assertIn("-displayfd", spawn.call_args.args[0])
                    self.assertEqual(os.environ["DISPLAY"], ":host")
                stop.assert_called_once_with(server)
                server.stdout.close.assert_called_once()

    def test_startup_failure_cleans_only_the_launched_server(self):
        with tempfile.TemporaryDirectory() as scratch:
            server = mock.Mock(pid=27182)
            with mock.patch.object(benchmark.subprocess, "Popen", return_value=server) as spawn, \
                 mock.patch.object(benchmark, "read_display_number", side_effect=RuntimeError("timeout")), \
                 mock.patch.object(benchmark, "stop_child") as stop:
                with self.assertRaisesRegex(RuntimeError, "timeout"):
                    with benchmark.display(Path(scratch), 1920, 1080):
                        self.fail("startup must not succeed")
                self.assertEqual(spawn.call_count, 1)
                stop.assert_called_once_with(server)
                server.stdout.close.assert_called_once()

    def test_invalid_dimensions_rates_and_durations_fail_before_creating_output(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch) / "must-not-exist"
            for option, value in [("--width", "0"), ("--height", "4321"), ("--fps", "61"), ("--seconds", "0"), ("--seconds", "nan"), ("--seconds", "inf"), ("--seconds", "0.0001")]:
                with self.subTest(option=option, value=value), \
                     mock.patch("sys.argv", ["recording_perf.py", "--output-dir", str(directory), option, value]), \
                     contextlib.redirect_stderr(io.StringIO()):
                    with self.assertRaises(SystemExit) as result:
                        benchmark.main()
                    self.assertEqual(result.exception.code, 2)
                    self.assertFalse(directory.exists())

    def test_existing_output_is_refused_without_starting_a_process(self):
        with tempfile.TemporaryDirectory() as scratch, \
             mock.patch("sys.argv", ["recording_perf.py", "--output-dir", scratch]), \
             mock.patch.object(benchmark.subprocess, "Popen") as spawn:
            with self.assertRaises(FileExistsError):
                benchmark.main()
            spawn.assert_not_called()

    def test_insufficient_capacity_fails_before_build_or_display(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch) / "new-output"
            with mock.patch("sys.argv", ["recording_perf.py", "--output-dir", str(directory)]), \
                 mock.patch.object(benchmark.shutil, "disk_usage", return_value=mock.Mock(free=0)), \
                 mock.patch.object(benchmark, "test_binary") as build, \
                 mock.patch.object(benchmark.subprocess, "Popen") as spawn:
                with self.assertRaisesRegex(RuntimeError, "insufficient free disk"):
                    benchmark.main()
                build.assert_not_called()
                spawn.assert_not_called()

    def test_binary_hash_is_content_based(self):
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / "binary"
            path.write_bytes(b"owned fixture")
            self.assertEqual(benchmark.binary_digest(path), hashlib.sha256(b"owned fixture").hexdigest())


if __name__ == "__main__":
    unittest.main()
