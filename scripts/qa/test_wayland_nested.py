"""Portable harness safety tests; no GNOME, portal, PipeWire or X server starts."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import time
import unittest

MODULE = Path(__file__).with_name("wayland_nested.py")
SPEC = importlib.util.spec_from_file_location("wayland_nested", MODULE)
lab_module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(lab_module)


class HarnessTests(unittest.TestCase):
    def test_environment_preserves_home_and_replaces_all_connection_names(self):
        lab = Path("/tmp/gfs-wayland-qa.example")
        original = {"HOME": "/original/home", "DISPLAY": ":0", "WAYLAND_DISPLAY": "wayland-0",
                    "DBUS_SESSION_BUS_ADDRESS": "unix:path=/real/user/bus",
                    "DBUS_SYSTEM_BUS_ADDRESS": "unix:path=/real/system/bus",
                    "PIPEWIRE_REMOTE": "real-remote", "PATH": os.environ["PATH"]}
        environment = lab_module.private_environment(lab, original)
        self.assertEqual(environment["HOME"], original["HOME"])
        self.assertEqual(original["DISPLAY"], ":0")
        for key in lab_module.REMOVED_ENV:
            self.assertNotIn(key, environment)
        for key in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME",
                    "XDG_STATE_HOME", "XDG_CONFIG_DIRS", "PIPEWIRE_RUNTIME_DIR",
                    "PIPEWIRE_CONFIG_DIR", "MEDIA_SESSION_CONFIG_DIR", "FONTCONFIG_FILE"):
            self.assertTrue(environment[key].startswith(str(lab) + "/"), key)

    def test_configs_cannot_activate_hardware_or_realtime_managers(self):
        for name in ("pipewire.conf", "media-session.conf", "client.conf"):
            config = (MODULE.parent / name).read_text()
            for forbidden in ("api.alsa", "api.v4l2", "api.bluez", "api.libcamera",
                              "libpipewire-module-rt", "pulse-bridge", "logind"):
                self.assertNotIn(forbidden, config)
        self.assertIn("default = [ flatpak portal suspend-node policy-node ]",
                      (MODULE.parent / "media-session.conf").read_text())

    def test_new_files_are_private_and_stale_or_foreign_labs_are_rejected(self):
        with tempfile.TemporaryDirectory(prefix="gfs-wayland-qa.") as directory:
            lab = Path(directory)
            lab_module.json_write(lab / "instance.json", {"kind": "gifromscreen-wayland-qa-v1"})
            self.assertEqual(lab_module.valid_lab(directory), lab)
            self.assertEqual((lab / "instance.json").stat().st_mode & 0o077, 0)
            os.chmod(lab, 0o755)
            with self.assertRaises(ValueError):
                lab_module.valid_lab(directory)
        with self.assertRaises((ValueError, FileNotFoundError)):
            lab_module.valid_lab("/tmp/does-not-own")

    def test_supervisor_cleans_only_registered_child_and_keeps_start_identity(self):
        with tempfile.TemporaryDirectory(prefix="gfs-wayland-qa.") as directory:
            lab = Path(directory)
            (lab / "output").mkdir()
            (lab / "logs").mkdir()
            children = lab_module.Children(lab, dict(os.environ))
            process = children.start("fixture-test", ["/usr/bin/python3", "-u", "-c",
                "import time; print('fixture ready', flush=True); time.sleep(30)"])
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                path = lab / "logs/fixture-test.log"
                if path.exists() and "fixture ready" in path.read_text():
                    break
                time.sleep(0.01)
            else:
                self.fail("live child output was not drained promptly")
            before = json.loads((lab / "children.json").read_text())[0]
            children.close()
            after = json.loads((lab / "children.json").read_text())[0]
            self.assertIsNotNone(process.poll())
            self.assertEqual(before["start_ticks"], after["start_ticks"])
            self.assertFalse(after["alive"])
            self.assertIsNotNone(after["exit_code"])

    def test_closing_noncritical_app_does_not_stop_the_lab_service(self):
        with tempfile.TemporaryDirectory(prefix="gfs-wayland-qa.") as directory:
            lab = Path(directory)
            (lab / "output").mkdir()
            (lab / "logs").mkdir()
            children = lab_module.Children(lab, dict(os.environ))
            service = children.start("service-test", ["/usr/bin/python3", "-c", "import time; time.sleep(30)"])
            app = children.start("app-test", ["/usr/bin/python3", "-c", "pass"], critical=False)
            try:
                app.wait(timeout=3)
                children.check()
                self.assertIsNone(service.poll())
            finally:
                children.close()
            self.assertIsNotNone(service.poll())


if __name__ == "__main__":
    unittest.main()
