"""Check backend isolation and reject windows that die during initialization."""

from contextlib import ExitStack
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import smoke_portable_desktop as smoke


class StartupSmokeTests(unittest.TestCase):
    def exercise(self, backend="auto", exit_at=None, visible=True):
        clock = SimpleNamespace(now=0.0)
        process = Mock()
        process.pid = 123
        process.poll.side_effect = lambda: 1 if exit_at is not None and clock.now >= exit_at else None
        environment = {"WGPU_BACKEND": "gl", "WAYLAND_DISPLAY": "not-the-test-display"}

        def sleep(seconds):
            clock.now += seconds

        with ExitStack() as stack:
            stack.enter_context(patch.object(smoke, "extract_checked", return_value=Path("unused-bundle")))
            launch = stack.enter_context(patch.object(smoke.subprocess, "Popen", return_value=process))
            stack.enter_context(patch.object(smoke.subprocess, "run", return_value=SimpleNamespace(
                returncode=0 if visible else 1, stdout="42\n" if visible else "")))
            stop = stack.enter_context(patch.object(smoke, "stop_child"))
            stack.enter_context(patch.object(smoke.time, "monotonic", side_effect=lambda: clock.now))
            stack.enter_context(patch.object(smoke.time, "sleep", side_effect=sleep))
            try:
                smoke.smoke(Path("unused.tar.gz"), environment, backend)
            finally:
                stop.assert_called_once_with(process)
                self.assertEqual(environment["WGPU_BACKEND"], "gl")
                self.assertIn("WAYLAND_DISPLAY", environment)
        return launch.call_args.kwargs["env"], clock.now

    def test_default_exercises_automatic_backend_and_waits_for_a_stable_window(self):
        environment, elapsed = self.exercise()
        self.assertNotIn("WGPU_BACKEND", environment)
        self.assertNotIn("WAYLAND_DISPLAY", environment)
        self.assertEqual(environment["XDG_SESSION_TYPE"], "x11")
        self.assertGreaterEqual(elapsed, 1.0)

    def test_explicit_backend_is_used_only_for_the_child(self):
        environment, _ = self.exercise(backend="vulkan")
        self.assertEqual(environment["WGPU_BACKEND"], "vulkan")

    def test_window_that_appears_then_exits_during_startup_is_a_failure(self):
        with self.assertRaisesRegex(RuntimeError, "desktop exited"):
            self.exercise(exit_at=0.2)

    def test_no_window_times_out_and_cleans_up_the_owned_child(self):
        with self.assertRaisesRegex(RuntimeError, "did not show a window"):
            self.exercise(visible=False)


if __name__ == "__main__":
    unittest.main()
