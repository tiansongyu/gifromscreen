"""Explicit lifecycle tests for CI's private display smoke harness."""

import os
import subprocess
import sys
import unittest
from unittest.mock import Mock, patch

from owned_xvfb import STARTUP_TIMEOUT_SECONDS, owned_display, read_display_number, stop_child


class StartupScenario:
    """Mechanical pipe/clock model, not evidence of X server readiness."""

    def __init__(self, chunks=(), exit_at=None, exit_status=7):
        self.now = 100.0
        self.started = self.now
        self.chunks = [(self.started + at, data) for at, data in chunks]
        self.exit_at = None if exit_at is None else self.started + exit_at
        self.exit_status = exit_status
        self.pid = 123456
        self.returncode = None
        self.stdout = Mock()
        self.stdout.fileno.return_value = 17
        self.waits = []
        self.reads = 0

    def poll(self):
        if self.exit_at is not None and self.now >= self.exit_at:
            self.returncode = self.exit_status
        return self.returncode

    def select(self, readers, writers, errors, timeout):
        assert readers == [self.stdout] and not writers and not errors
        assert 0 < timeout <= 0.1
        self.waits.append(timeout)
        assert len(self.waits) < 2000, "startup loop must remain bounded"
        until = self.now + timeout
        if self.chunks and self.chunks[0][0] <= until:
            self.now = max(self.now, self.chunks[0][0])
            return [self.stdout], [], []
        self.now = until
        return [], [], []

    def read(self, descriptor, size):
        assert descriptor == 17 and 0 < size <= 32
        self.reads += 1
        at, data = self.chunks.pop(0)
        assert at <= self.now
        if len(data) > size:
            self.chunks.insert(0, (at, data[size:]))
        return data[:size]

    def run(self, **options):
        with patch("owned_xvfb.time.monotonic", side_effect=lambda: self.now), \
                patch("owned_xvfb.select.select", side_effect=self.select), \
                patch("owned_xvfb.os.read", side_effect=self.read):
            return read_display_number(self, **options)


class OwnedDisplayTests(unittest.TestCase):
    def test_real_child_pipe_failure_is_bounded_and_reaped(self):
        # Retain the original real-pipe/child regression alongside the new
        # deterministic clock model; mocks do not establish OS pipe cleanup.
        for output in [b"not-a-number\n", b"1" * 32, b""]:
            with self.subTest(output=output):
                process = subprocess.Popen(
                    [sys.executable, "-c", f"import os,time; os.write(1,{output!r}); time.sleep(5)"],
                    stdout=subprocess.PIPE,
                )
                try:
                    with self.assertRaises(RuntimeError):
                        read_display_number(process, timeout=0.1)
                finally:
                    stop_child(process)
                    process.stdout.close()
                self.assertIsNotNone(process.poll())

    def test_display_protocol_is_bounded_and_rejects_bad_output(self):
        for output in [b"not-a-number\n", b"1" * 32, b"", b"65536\n", b"2\n3\n",
                       b"2\nnoise", b"-1\n", b" 2\n", b"2\r\n", b"\xff\n"]:
            with self.subTest(output=output):
                with self.assertRaises(RuntimeError):
                    StartupScenario([(0.0, output)]).run(timeout=0.1)

    def test_delayed_publication_past_the_old_deadline_uses_the_same_live_child(self):
        scenario = StartupScenario([(0.1, b"4"), (8.0, b"2\n")])
        with patch("owned_xvfb.subprocess.Popen") as spawn:
            self.assertEqual(scenario.run(), ":42")
        spawn.assert_not_called()
        self.assertIsNone(scenario.poll())
        self.assertEqual(scenario.reads, 2)
        self.assertAlmostEqual(scenario.now - scenario.started, 8.0)
        self.assertEqual(STARTUP_TIMEOUT_SECONDS, 30)

    def test_alive_but_silent_child_expires_and_reports_identity_and_budget(self):
        scenario = StartupScenario()
        with self.assertRaises(RuntimeError) as caught:
            scenario.run(timeout=0.25)
        message = str(caught.exception)
        for text in ["deadline expired", "PID 123456", "elapsed=0.250s", "budget=0.25s",
                     "status=None", "displayfd bytes=0", "prefix=b''"]:
            self.assertIn(text, message)
        self.assertIsNone(scenario.poll())
        self.assertAlmostEqual(scenario.now - scenario.started, 0.25)
        self.assertEqual(scenario.reads, 0)

    def test_partial_number_cannot_extend_the_absolute_deadline(self):
        scenario = StartupScenario([(0.1, b"4"), (0.2, b"2"), (0.5, b"\n")])
        with self.assertRaisesRegex(RuntimeError, "deadline expired.*bytes=2; prefix=b'42'"):
            scenario.run(timeout=0.3)
        self.assertAlmostEqual(scenario.now - scenario.started, 0.3)
        self.assertEqual(scenario.reads, 2)

    def test_early_exit_is_reported_before_waiting_out_the_deadline(self):
        scenario = StartupScenario(exit_at=0.15, exit_status=19)
        with self.assertRaisesRegex(RuntimeError, "child exited before.*status=19"):
            scenario.run(timeout=30)
        self.assertLess(scenario.now - scenario.started, 0.3)
        self.assertEqual(scenario.reads, 0)

    def test_output_observed_after_deadline_does_not_turn_a_timeout_into_success(self):
        scenario = StartupScenario([(0.0, b"42\n")])

        def delayed_select(*args):
            scenario.now = scenario.started + 2.0
            return [scenario.stdout], [], []

        scenario.select = delayed_select
        with self.assertRaisesRegex(RuntimeError, "deadline expired.*elapsed=2.000s.*budget=1s"):
            scenario.run(timeout=1)
        self.assertEqual(scenario.reads, 0)

    def test_empty_stdout_eof_is_not_a_live_process_timeout(self):
        scenario = StartupScenario([(0.1, b"")])
        with self.assertRaisesRegex(RuntimeError, "pipe reached EOF.*status=None.*bytes=0"):
            scenario.run(timeout=30)
        self.assertAlmostEqual(scenario.now - scenario.started, 0.1)
        self.assertIsNone(scenario.poll())

    def test_no_stdout_pipe_and_unbounded_timeouts_are_rejected_before_waiting(self):
        scenario = StartupScenario()
        scenario.stdout = None
        with self.assertRaisesRegex(RuntimeError, "no display-number pipe"):
            scenario.run()
        for timeout in [0, -1, 121, float("nan"), float("inf"), "5", True]:
            with self.subTest(timeout=timeout), patch("owned_xvfb.subprocess.Popen") as spawn:
                with self.assertRaises(ValueError):
                    with owned_display(startup_timeout=timeout):
                        self.fail("invalid deadline cannot start a server")
                spawn.assert_not_called()

    def test_startup_failure_cleans_exactly_one_owned_child_without_probe_or_respawn(self):
        server = Mock(pid=987654)
        server.poll.return_value = None
        with patch("owned_xvfb.subprocess.Popen", return_value=server) as spawn, \
                patch("owned_xvfb.read_display_number", side_effect=RuntimeError("deadline expired")) as read, \
                patch("owned_xvfb.subprocess.run") as probe, \
                patch("owned_xvfb.stop_child") as stop:
            with self.assertRaisesRegex(RuntimeError, "deadline expired[\\s\\S]*PID: 987654"):
                with owned_display(startup_timeout=7):
                    self.fail("failed startup must not yield any environment")
            spawn.assert_called_once()
            self.assertEqual(spawn.call_args.args[0][:3], ["Xvfb", "-displayfd", "1"])
            read.assert_called_once_with(server, timeout=7)
            probe.assert_not_called()
            stop.assert_called_once_with(server)
            server.stdout.close.assert_called_once()

    def test_owned_server_ignores_host_environment_and_is_reaped_on_error(self):
        servers = []
        real_popen = subprocess.Popen

        def start(*args, **kwargs):
            process = real_popen(*args, **kwargs)
            if args[0][0] == "Xvfb":
                servers.append(process)
            return process

        with patch.dict(os.environ, {"DISPLAY": "invalid-host-display", "XAUTHORITY": "/invalid-host-auth"}):
            with patch("owned_xvfb.subprocess.Popen", side_effect=start):
                with self.assertRaisesRegex(RuntimeError, "injected failure.*", msg="failure must include server diagnostics"):
                    with owned_display() as environment:
                        self.assertTrue(environment["DISPLAY"].startswith(":"))
                        self.assertNotIn("XAUTHORITY", environment)
                        self.assertEqual(os.environ["DISPLAY"], "invalid-host-display")
                        raise RuntimeError("injected failure")
            self.assertEqual(len(servers), 1)
            self.assertIsNotNone(servers[0].poll())

    def test_overlapping_owned_servers_do_not_reuse_each_others_display(self):
        with owned_display() as first, owned_display() as second:
            self.assertNotEqual(first["DISPLAY"], second["DISPLAY"])


if __name__ == "__main__":
    unittest.main()
