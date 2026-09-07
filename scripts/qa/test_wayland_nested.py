"""Portable harness safety tests; no GNOME, portal, PipeWire or X server starts."""
import importlib.util
import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from types import SimpleNamespace
from unittest import mock

MODULE = Path(__file__).with_name("wayland_nested.py")
SPEC = importlib.util.spec_from_file_location("wayland_nested", MODULE)
lab_module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(lab_module)


class HarnessTests(unittest.TestCase):
    def test_four_hour_lifetime_is_bounded_without_weakening_startup_checks(self):
        lab_module.validate_lifetime(1)
        lab_module.validate_lifetime(14400)
        for invalid in (0, -1, 14401, 2**63):
            with self.assertRaises(ValueError):
                lab_module.validate_lifetime(invalid)

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

    def test_x11_environment_and_attachments_cannot_inherit_wayland_or_host_sessions(self):
        with tempfile.TemporaryDirectory(prefix="gfs-wayland-qa.") as directory:
            lab = Path(directory)
            inherited = {"HOME": "/original/home", "DISPLAY": ":0",
                         "WAYLAND_DISPLAY": "host-wayland", "WAYLAND_SOCKET": "9",
                         "GDK_BACKEND": "wayland", "DBUS_SESSION_BUS_ADDRESS": "host-bus"}
            environment = lab_module.private_environment(lab, inherited, "x11")
            self.assertEqual(environment["HOME"], "/original/home")
            self.assertEqual(environment["XDG_SESSION_TYPE"], "x11")
            self.assertEqual(environment["GDK_BACKEND"], "x11")
            for key in ("DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DBUS_SESSION_BUS_ADDRESS"):
                self.assertNotIn(key, environment)
            environment.update(DISPLAY=":93", XAUTHORITY=str(lab / "Xauthority"),
                               DBUS_SESSION_BUS_ADDRESS=f"unix:path={lab}/runtime/bus")
            lab_module.json_write(lab / "instance.json", {"display_server": "x11"})
            saved = lab_module.session_environment(lab, environment, "x11")
            saved["WAYLAND_DISPLAY"] = "injected-wrong-backend"
            saved["WAYLAND_SOCKET"] = "8"
            lab_module.json_write(lab / "session-env.json", saved)
            attached = lab_module.attached_environment(lab, inherited)
            self.assertEqual(attached["DISPLAY"], ":93")
            self.assertEqual(attached["XDG_SESSION_TYPE"], "x11")
            self.assertEqual(attached["GDK_BACKEND"], "x11")
            self.assertNotIn("WAYLAND_DISPLAY", attached)
            self.assertNotIn("WAYLAND_SOCKET", attached)
            self.assertEqual(attached["HOME"], "/original/home")
            saved["XDG_SESSION_TYPE"] = "wayland"
            lab_module.json_write(lab / "session-env.json", saved)
            with self.assertRaises(RuntimeError):
                lab_module.attached_environment(lab, inherited)

    def test_legacy_lab_attachments_default_to_wayland(self):
        with tempfile.TemporaryDirectory(prefix="gfs-wayland-qa.") as directory:
            lab = Path(directory)
            lab_module.json_write(lab / "instance.json", {"kind": "gifromscreen-wayland-qa-v1"})
            environment = lab_module.private_environment(lab, {})
            environment.update(DISPLAY=":93", WAYLAND_DISPLAY="gfs-qa-wayland", GDK_BACKEND="wayland")
            lab_module.json_write(lab / "session-env.json", environment)
            attached = lab_module.attached_environment(lab, {"HOME": "/original/home"})
            self.assertEqual(attached["XDG_SESSION_TYPE"], "wayland")
            self.assertEqual(attached["WAYLAND_DISPLAY"], "gfs-qa-wayland")
            self.assertEqual(attached["GDK_BACKEND"], "wayland")

    def test_x11_window_manager_requires_private_live_xvfb_identity_and_authority(self):
        with tempfile.TemporaryDirectory(prefix="gfs-wayland-qa.") as directory:
            lab = Path(directory)
            lab_module.private_write(lab / "Xauthority", "private fixture")
            launcher = {"pid": 3201, "start_ticks": "11", "process_group": 3201}
            server = {"pid": 3202, "start_ticks": "12", "process_group": 3201}
            lab_module.json_write(lab / "launcher.json", launcher)
            environment = {"DISPLAY": ":93", "XAUTHORITY": str(lab / "Xauthority")}
            def identity(pid):
                return launcher if pid == launcher["pid"] else {
                    "pid": pid, "start_ticks": "10", "process_group": launcher["pid"]}
            with mock.patch.object(lab_module, "process_identity", side_effect=identity), \
                 mock.patch.object(lab_module, "group_processes", return_value=[server]), \
                 mock.patch.object(lab_module, "process_command", return_value=["/usr/bin/Xvfb", ":93", "-nolisten", "tcp"]):
                self.assertEqual(lab_module.private_xvfb_identity(lab, environment)["pid"], 3202)
                with self.assertRaises(RuntimeError):
                    lab_module.private_xvfb_identity(lab, dict(environment, XAUTHORITY="/host/Xauthority"))
                self.assertIsNone(lab_module.private_xvfb_identity(lab, dict(environment, DISPLAY=":0")))
                with self.assertRaises(RuntimeError):
                    lab_module.private_xvfb_identity(lab, dict(environment, DISPLAY="host:93"))
            with mock.patch.object(lab_module, "process_identity", return_value={"pid": 3201, "exited": True}):
                with self.assertRaises(RuntimeError):
                    lab_module.private_xvfb_identity(lab, environment)

    def test_x11_readiness_requires_gnome_wm_and_private_bus_owner(self):
        responses = [(True, "_NET_SUPPORTING_WM_CHECK(WINDOW): window id # 0x20"),
                     (True, '_NET_WM_NAME(UTF8_STRING) = "GNOME Shell"')]
        with mock.patch.object(lab_module, "query", side_effect=responses), \
             mock.patch.object(lab_module, "owns_name", return_value=True):
            self.assertTrue(lab_module.x11_window_manager_ready({}))
        with mock.patch.object(lab_module, "query", return_value=(True, "window id # 0x0")):
            self.assertFalse(lab_module.x11_window_manager_ready({}))
        with mock.patch.object(lab_module, "query", side_effect=responses), \
             mock.patch.object(lab_module, "owns_name", return_value=False):
            self.assertFalse(lab_module.x11_window_manager_ready({}))

    def test_x11_inner_starts_no_media_or_portal_services(self):
        started = []
        class FakeProcess:
            ended = False
            def poll(self):
                return 0 if self.ended else None
        class FakeChildren:
            def __init__(self, lab, environment):
                self.lab, self.environment, self.entries = lab, environment, []
            def start(self, name, command, critical=True):
                started.append((name, command, dict(self.environment)))
                self.entries.append((name, command, FakeProcess()))
            def check(self):
                pass
            def close(self):
                for _, _, process in self.entries:
                    process.ended = True
        with tempfile.TemporaryDirectory(prefix="gfs-wayland-qa.") as directory:
            lab = Path(directory)
            lab_module.json_write(lab / "instance.json", {"kind": "gifromscreen-wayland-qa-v1", "display_server": "x11"})
            environment = lab_module.private_environment(lab, {}, "x11")
            environment.update(DISPLAY=":93", DBUS_SESSION_BUS_ADDRESS=f"unix:path={lab}/runtime/bus")
            args = SimpleNamespace(lab=str(lab), display_server="x11", app="/test/app", seconds=0, startup_timeout=1)
            with mock.patch.dict(os.environ, environment, clear=True), \
                 mock.patch.object(lab_module, "Children", FakeChildren), \
                 mock.patch.object(lab_module, "private_xvfb_identity", return_value={"pid": 3202}), \
                 mock.patch.object(lab_module, "x11_window_manager_ready", return_value=True), \
                 mock.patch.object(lab_module, "prepare_portals") as portals, \
                 contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(lab_module.inner(args), 0)
                portals.assert_not_called()
            self.assertEqual([name for name, _, _ in started], ["gnome-shell", "fixture", "gifromscreen"])
            self.assertEqual(started[0][1], ["gnome-shell", "--x11", "--sm-disable"])
            for _, _, environment in started:
                self.assertEqual(environment["XDG_SESSION_TYPE"], "x11")
                self.assertEqual(environment["GDK_BACKEND"], "x11")
                self.assertNotIn("WAYLAND_DISPLAY", environment)
            status = lab_module.read_json(lab / "status.json")
            self.assertEqual(status["actual_display_server"], "x11")
            self.assertTrue(status["cleanup_complete"])

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
