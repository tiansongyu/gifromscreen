"""Explicit lifecycle tests for CI's private display smoke harness."""

import os
import subprocess
import sys
import unittest
from unittest.mock import patch

from owned_xvfb import owned_display, read_display_number, stop_child


class OwnedDisplayTests(unittest.TestCase):
    def test_display_protocol_is_bounded_and_rejects_bad_output(self):
        for output in [b"not-a-number\n", b"1" * 32, b""]:
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
