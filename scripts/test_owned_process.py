#!/usr/bin/env python3
"""Supervision tests create only their own child sessions; never signal host groups."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

from owned_process import OwnedProcess, run


def state(pid):
    try:
        with open("/proc/" + str(pid) + "/stat", "rb") as source:
            return source.read(4096).rpartition(b") ")[2].split()[0]
    except FileNotFoundError:
        return None


class OwnedProcessTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix="gfs-owned-process-test-")
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.log = self.enter_log()

    def enter_log(self):
        log = (self.root / "child.log").open("w+")
        self.addCleanup(log.close)
        return log

    def process(self, code):
        return OwnedProcess([sys.executable, "-c", code], stdout=self.log, stderr=self.log,
                            terminate_timeout=0.1, kill_timeout=1.0)

    def child_pid(self, path):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            if path.exists():
                content = path.read_text()
                if content:
                    return int(content)
            time.sleep(0.005)
        self.fail("owned child did not signal readiness")

    def test_normal_exit_is_observed_without_reaping_until_context_cleanup(self):
        with self.process("raise SystemExit(7)") as process:
            self.assertEqual(process.wait(3), 7)
            self.assertEqual(process.poll(), 7)
            self.assertEqual(state(process.pid), b"Z")
            self.assertEqual(os.getpgid(process.pid), process.pid)
        self.assertEqual(process.poll(), 7)
        with self.assertRaises(ChildProcessError):
            os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOWAIT | os.WNOHANG)
        process.close()  # Idempotent and never re-signals a reaped numeric PID.

    def test_leader_exits_first_but_ignoring_child_is_cleaned(self):
        ready = self.root / "grandchild.pid"
        code = (
            "import os,signal,time\n"
            "pid=os.fork()\n"
            "if pid==0:\n"
            " signal.signal(signal.SIGTERM,signal.SIG_IGN)\n"
            " with open(" + repr(str(ready)) + ", 'w') as f: f.write(str(os.getpid()))\n"
            " time.sleep(30)\n"
            "else: os._exit(0)\n"
        )
        with self.process(code) as process:
            child = self.child_pid(ready)
            self.assertEqual(os.getpgid(child), process.pid)
            self.assertEqual(process.wait(3), 0)
            self.assertEqual(state(process.pid), b"Z")
            self.assertNotIn(state(child), (None, b"Z", b"X"))
        self.assertIn(state(child), (None, b"Z", b"X"))

    def test_timeout_cleans_ignoring_leader_and_child(self):
        ready = self.root / "grandchild.pid"
        code = (
            "import os,signal,time\n"
            "signal.signal(signal.SIGTERM,signal.SIG_IGN)\n"
            "pid=os.fork()\n"
            "if pid==0:\n"
            " with open(" + repr(str(ready)) + ", 'w') as f: f.write(str(os.getpid()))\n"
            "time.sleep(30)\n"
        )
        with self.assertRaises(subprocess.TimeoutExpired):
            with self.process(code) as process:
                child = self.child_pid(ready)
                self.assertIsNone(process.poll())
                process.wait(0.02)
        self.assertEqual(process.poll(), -9)
        self.assertIn(state(child), (None, b"Z", b"X"))

    def test_exception_in_body_still_cleans_owned_group(self):
        with self.assertRaisesRegex(ValueError, "intentional"):
            with self.process("import time; time.sleep(30)") as process:
                raise ValueError("intentional")
        self.assertIn(process.poll(), (-15, -9))
        self.assertIsNone(state(process.pid))

    def test_custom_group_session_preexec_and_pipes_are_rejected_before_spawn(self):
        for override in ({"start_new_session": False}, {"start_new_session": True},
                         {"process_group": 0}, {"preexec_fn": None},
                         {"stdout": subprocess.PIPE}, {"stderr": subprocess.PIPE},
                         {"stdin": subprocess.PIPE}):
            with self.subTest(override=override):
                with self.assertRaises(ValueError):
                    OwnedProcess(["this-must-never-be-executed"], **override)

    def test_run_returns_exit_status_and_optional_check_raises_after_reaping(self):
        command = [sys.executable, "-c", "raise SystemExit(4)"]
        self.assertEqual(run(command, timeout=3, stdout=self.log, stderr=self.log), 4)
        with self.assertRaises(subprocess.CalledProcessError) as caught:
            run(command, timeout=3, check=True, stdout=self.log, stderr=self.log)
        self.assertEqual(caught.exception.returncode, 4)

    def test_nonfinite_timeouts_cannot_disable_bounded_supervision(self):
        for value in (float("nan"), float("inf"), float("-inf"), -1, "1"):
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    OwnedProcess(["must-not-execute"], terminate_timeout=value)
                with self.assertRaises(ValueError):
                    OwnedProcess(["must-not-execute"], kill_timeout=value)
                with self.process("raise SystemExit(0)") as process:
                    with self.assertRaises(ValueError):
                        process.wait(value)

    def test_externally_reaped_leader_never_causes_a_signal_to_its_old_numeric_group(self):
        process = self.process("raise SystemExit(0)")
        self.assertEqual(process.wait(3), 0)
        # Deliberately bypass OwnedProcess, as an accidental direct Popen.wait
        # would. Popen also records the status, avoiding a false ResourceWarning.
        self.assertEqual(process._process.wait(timeout=1), 0)
        with patch("owned_process.os.killpg") as signal_group:
            with self.assertRaisesRegex(RuntimeError, "reaped externally"):
                process.close()
            signal_group.assert_not_called()


if __name__ == "__main__":
    unittest.main()
