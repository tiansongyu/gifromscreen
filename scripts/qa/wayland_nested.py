#!/usr/bin/python3
"""Private GNOME/PipeWire acceptance lab. Never replaces or restarts host services.

start creates one owned Xvfb + private bus and bounded supervised child lifetime.
status/exec/stop operate on an existing lab; they never start a replacement lab.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from xml.sax.saxutils import escape

SCRIPT_DIR = Path(__file__).resolve().parent
LOG_LIMIT = 2 * 1024 * 1024
MAX_LIFETIME_SECONDS = 4 * 60 * 60
STOP = threading.Event()
STOP_SIGNAL = None
REMOVED_ENV = (
    "DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET",
    "DBUS_SESSION_BUS_ADDRESS", "DBUS_SYSTEM_BUS_ADDRESS", "DBUS_STARTER_ADDRESS",
    "DBUS_STARTER_BUS_TYPE", "SESSION_MANAGER", "XDG_SESSION_ID", "GNOME_SETUP_DISPLAY",
    "PIPEWIRE_REMOTE", "PIPEWIRE_CONFIG_NAME", "PIPEWIRE_CONFIG_PREFIX",
    "GDK_BACKEND", "QT_QPA_PLATFORM", "G_DEBUG", "GNOME_SHELL_SESSION_MODE",
)


def private_write(path, text):
    with open(path, "w", encoding="utf-8") as stream:
        stream.write(text)
    os.chmod(path, 0o600)


def json_write(path, value):
    temporary = path.with_name(path.name + ".pending")
    private_write(temporary, json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def validate_lifetime(seconds):
    if not 1 <= seconds <= MAX_LIFETIME_SECONDS:
        raise ValueError(f"lifetime must be 1–{MAX_LIFETIME_SECONDS} seconds")


def valid_lab(value):
    path = Path(value).absolute()
    if path.parent != Path(tempfile.gettempdir()) or not path.name.startswith("gfs-wayland-qa."):
        raise ValueError("Not an allocated gfs-wayland-qa temporary directory")
    info = path.lstat()
    if path.is_symlink() or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError("Lab directory must be owned by this user and mode 0700")
    if read_json(path / "instance.json")["kind"] != "gifromscreen-wayland-qa-v1":
        raise ValueError("Unrecognized lab instance")
    return path


def private_environment(lab, inherited):
    environment = {key: value for key, value in inherited.items() if key not in REMOVED_ENV}
    for name, directory in {
        "XDG_RUNTIME_DIR": "runtime", "XDG_CONFIG_HOME": "config",
        "XDG_CONFIG_DIRS": "config-system", "XDG_CACHE_HOME": "cache",
        "XDG_DATA_HOME": "data", "XDG_STATE_HOME": "state",
        "PIPEWIRE_RUNTIME_DIR": "runtime", "PIPEWIRE_CONFIG_DIR": "pipewire",
        "MEDIA_SESSION_CONFIG_DIR": "media-session",
    }.items():
        environment[name] = str(lab / directory)
    environment.update({
        "XDG_DATA_DIRS": "/usr/share", "XDG_CURRENT_DESKTOP": "GNOME",
        "XDG_SESSION_DESKTOP": "gnome", "XDG_SESSION_TYPE": "wayland",
        "GSETTINGS_BACKEND": "memory", "NO_AT_BRIDGE": "1", "GTK_A11Y": "none",
        "PULSE_SERVER": f"unix:{lab}/runtime/no-pulse-server",
        "LIBGL_ALWAYS_SOFTWARE": "1", "GALLIUM_DRIVER": "llvmpipe",
        "VK_ICD_FILENAMES": "/usr/share/vulkan/icd.d/lvp_icd.x86_64.json",
        "FONTCONFIG_FILE": str(lab / "fontconfig.conf"),
        "FONTCONFIG_PATH": str(lab / "config/fontconfig"),
        "LP_NUM_THREADS": "2", "RAYON_NUM_THREADS": "4",
        "MUTTER_DEBUG_DUMMY_MODE_SPECS": "1280x720",
        "PYTHONDONTWRITEBYTECODE": "1",
    })
    return environment


def prepare(lab, app):
    for directory in ("runtime", "config", "config-system", "cache", "data", "state",
                      "pipewire", "media-session", "output", "logs", "bin"):
        (lab / directory).mkdir(mode=0o700)
    for source, destination in (
        ("pipewire.conf", "pipewire/pipewire.conf"),
        ("media-session.conf", "media-session/media-session.conf"),
        ("client.conf", "pipewire/client.conf"),
        ("client.conf", "pipewire/client-rt.conf"),
        ("client.conf", "media-session/client.conf"),
    ):
        private_write(lab / destination, (SCRIPT_DIR / source).read_text(encoding="utf-8"))
    prepare_fontconfig(lab)
    private_write(lab / "private-bus.conf", f"""<busconfig>
  <type>session</type>
  <listen>unix:path={escape(str(lab / 'runtime/bus'))}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/>
  </policy>
</busconfig>
""")
    frozen = lab / "bin/gif-from-screen"
    shutil.copyfile(app, frozen)
    os.chmod(frozen, 0o700)
    digest = hashlib.sha256()
    with open(frozen, "rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=SCRIPT_DIR, text=True).strip()
    json_write(lab / "instance.json", {"kind": "gifromscreen-wayland-qa-v1", "app": str(frozen), "original_app": str(app),
               "sha256": digest.hexdigest(), "revision": revision, "uid": os.getuid()})


def prepare_fontconfig(lab):
    private_write(lab / "fontconfig.conf", f"""<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <dir>/usr/share/fonts</dir>
  <cachedir>{escape(str(lab / 'cache/fontconfig'))}</cachedir>
  <cachedir>/var/cache/fontconfig</cachedir>
  <alias><family>sans-serif</family><prefer><family>DejaVu Sans</family></prefer></alias>
  <alias><family>serif</family><prefer><family>DejaVu Serif</family></prefer></alias>
  <alias><family>monospace</family><prefer><family>DejaVu Sans Mono</family></prefer></alias>
</fontconfig>
""")


def process_identity(pid):
    try:
        # stat's comm may contain spaces: fields after its final ')' start at state.
        data = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return {"pid": pid, "start_ticks": data[19], "process_group": int(data[2])}
    except (OSError, IndexError, ValueError):
        return {"pid": pid, "exited": True}


class Children:
    def __init__(self, lab, environment, record="children.json"):
        self.lab, self.environment = lab, environment
        self.entries, self.pumps = [], []
        self.identities = {}
        self.record = record
        self.noncritical = set()

    def start(self, name, command, environment=None, critical=True):
        process = subprocess.Popen(command, cwd=self.lab / "output",
                                   env=environment or self.environment,
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT)
        self.entries.append((name, command, process))
        self.identities[process.pid] = process_identity(process.pid)
        if not critical:
            self.noncritical.add(process.pid)
        self.save()
        pump = threading.Thread(target=self.drain, args=(name, process.stdout), daemon=True)
        pump.start()
        self.pumps.append(pump)
        return process

    def drain(self, name, source):
        written = 0
        with source, open(self.lab / "logs" / f"{name}.log", "wb") as destination:
            os.chmod(destination.name, 0o600)
            while True:
                block = source.read1(4096)
                if not block:
                    break
                remaining = LOG_LIMIT - written
                if remaining > 0:
                    destination.write(block[:remaining])
                    destination.flush()
                    written += min(len(block), remaining)
            if written == LOG_LIMIT:
                destination.write(b"\n[log limit reached; further output was drained]\n")

    def save(self):
        json_write(self.lab / self.record, [dict(name=name, command=command,
                   **self.identities[process.pid], alive=process.poll() is None, exit_code=process.poll())
                   for name, command, process in self.entries])

    def check(self):
        if STOP.is_set() or (self.lab / "stop.request").exists():
            raise InterruptedError("lab stop requested")
        for name, _, process in self.entries:
            if process.poll() is not None and process.pid not in self.noncritical:
                raise RuntimeError(f"{name} exited with status {process.returncode}; see logs/{name}.log")

    def close(self):
        for _, _, process in reversed(self.entries):
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)
        for pump in self.pumps:
            pump.join(timeout=2)
        self.save()


def query(command, environment, timeout=3):
    try:
        result = subprocess.run(command, env=environment, capture_output=True, text=True,
                                stdin=subprocess.DEVNULL, timeout=timeout)
        return result.returncode == 0, result.stdout + result.stderr
    except subprocess.TimeoutExpired:
        return False, "query timed out"


def owns_name(name, environment):
    success, output = query(["gdbus", "call", "--session", "--dest", "org.freedesktop.DBus",
                            "--object-path", "/org/freedesktop/DBus", "--method",
                            "org.freedesktop.DBus.NameHasOwner", name], environment)
    return success and "true" in output


def wait_ready(children, label, predicate, timeout):
    print(f"WAIT {label}", flush=True)
    deadline = time.monotonic() + timeout
    while True:
        children.check()
        if predicate():
            print(f"READY {label}", flush=True)
            return
        if time.monotonic() >= deadline:
            raise TimeoutError(f"readiness deadline for {label} expired; no replacement process was launched")
        STOP.wait(0.1)


def session_environment(lab, environment):
    keys = set(private_environment(lab, {}).keys()) | {"DISPLAY", "XAUTHORITY",
           "DBUS_SESSION_BUS_ADDRESS", "DBUS_SYSTEM_BUS_ADDRESS", "WAYLAND_DISPLAY", "GDK_BACKEND"}
    return {key: environment[key] for key in keys if key in environment}


def inner(args):
    lab = valid_lab(args.lab)
    environment = dict(os.environ)
    environment["DBUS_SYSTEM_BUS_ADDRESS"] = environment["DBUS_SESSION_BUS_ADDRESS"]
    os.environ["DBUS_SYSTEM_BUS_ADDRESS"] = environment["DBUS_SESSION_BUS_ADDRESS"]
    children = Children(lab, environment)
    status = {"phase": "starting", "supervisor": process_identity(os.getpid()),
              "display": environment["DISPLAY"], "lab": str(lab)}
    json_write(lab / "status.json", status)
    try:
        children.start("pipewire", ["pipewire", "-c", str(lab / "pipewire/pipewire.conf")])
        wait_ready(children, "private PipeWire", lambda: (lab / "runtime/pipewire-0").is_socket(), args.startup_timeout)
        children.start("media-session", ["pipewire-media-session", "-c", "media-session.conf"])
        children.start("gnome-shell", ["gnome-shell", "--nested", "--wayland", "--no-x11",
                        "--sm-disable", "--wayland-display=gfs-qa-wayland"])
        wait_ready(children, "nested GNOME ScreenCast", lambda:
                   (lab / "runtime/gfs-qa-wayland").is_socket()
                   and owns_name("org.gnome.Mutter.ScreenCast", environment), args.startup_timeout)
        environment.update({"WAYLAND_DISPLAY": "gfs-qa-wayland", "GDK_BACKEND": "wayland"})
        json_write(lab / "session-env.json", session_environment(lab, environment))
        children.start("fixture", ["/usr/bin/python3", str(SCRIPT_DIR / "wayland-fixture.py")], critical=False)
        children.start("permission-store", ["/usr/libexec/xdg-permission-store"])
        children.start("portal-gnome", ["/usr/libexec/xdg-desktop-portal-gnome", "--verbose"])
        wait_ready(children, "GNOME portal backend", lambda: owns_name(
                   "org.freedesktop.impl.portal.desktop.gnome", environment), args.startup_timeout)
        children.start("portal", ["/usr/libexec/xdg-desktop-portal", "--verbose"])
        wait_ready(children, "desktop portal", lambda: owns_name(
                   "org.freedesktop.portal.Desktop", environment), args.startup_timeout)
        for prop in ("AvailableSourceTypes", "AvailableCursorModes"):
            success, output = query(["gdbus", "call", "--session", "--dest", "org.freedesktop.portal.Desktop",
                                    "--object-path", "/org/freedesktop/portal/desktop", "--method",
                                    "org.freedesktop.DBus.Properties.Get", "org.freedesktop.portal.ScreenCast", prop], environment)
            private_write(lab / "logs" / f"{prop}.txt", output)
            if not success:
                raise RuntimeError(f"ScreenCast property {prop} is unavailable: {output}")
        success, graph = query(["pw-dump", "-r", "pipewire-0"], environment)
        private_write(lab / "logs/pipewire-initial.json", graph)
        if not success:
            raise RuntimeError("private PipeWire graph could not be inspected")
        forbidden = ("api.alsa.", "api.v4l2.", "api.bluez5.", "api.libcamera.")
        if any(token in graph for token in forbidden):
            raise RuntimeError("hardware discovery unexpectedly appeared in the private graph")
        children.start("gifromscreen", [args.app], critical=False)
        status.update(phase="ready", wayland_display=environment["WAYLAND_DISPLAY"])
        json_write(lab / "status.json", status)
        print("LAB_READY " + json.dumps(status), flush=True)
        deadline = time.monotonic() + args.seconds
        while time.monotonic() < deadline:
            children.check()
            STOP.wait(0.2)
        status.update(phase="stopped", reason="bounded acceptance lifetime ended")
    except InterruptedError as error:
        status.update(phase="stopped", reason=str(error))
    except Exception as error:
        status.update(phase="failed", reason=str(error))
        print("LAB_FAILED " + str(error), flush=True)
    finally:
        children.close()
        status["cleanup_complete"] = all(process.poll() is not None for _, _, process in children.entries)
        json_write(lab / "status.json", status)
    return 1 if status["phase"] == "failed" else 0


def start(args):
    app = Path(args.app).resolve(strict=True)
    for executable in ("xvfb-run", "xauth", "dbus-run-session", "gnome-shell", "pipewire",
                       "pipewire-media-session", "pw-dump", "gdbus"):
        if shutil.which(executable) is None:
            raise RuntimeError(f"missing prerequisite: {executable}")
    lab = Path(tempfile.mkdtemp(prefix="gfs-wayland-qa."))
    os.chmod(lab, 0o700)
    prepare(lab, app)
    json_write(lab / "outer-supervisor.json", dict(parent_pid=os.getppid(),
               command=sys.argv, **process_identity(os.getpid())))
    environment = private_environment(lab, os.environ)
    command = ["xvfb-run", "--auto-servernum", "--auth-file", str(lab / "Xauthority"),
               "--error-file", str(lab / "logs/xvfb.log"),
               "--server-args=-screen 0 1440x1000x24 +extension GLX +extension RENDER -nolisten tcp",
               "dbus-run-session", "--config-file", str(lab / "private-bus.conf"), "--",
               "/usr/bin/python3", str(Path(__file__).resolve()), "inner", "--lab", str(lab),
               "--app", str(lab / "bin/gif-from-screen"), "--seconds", str(args.seconds),
               "--startup-timeout", str(args.startup_timeout)]
    print(f"LAB_CREATED {lab}", flush=True)
    process = subprocess.Popen(command, env=environment, start_new_session=True)
    json_write(lab / "launcher.json", dict(command=command, **process_identity(process.pid)))
    deadline = time.monotonic() + args.startup_timeout * 4 + args.seconds + 30
    stop_deadline = None
    while process.poll() is None:
        if STOP.is_set():
            private_write(lab / "stop.request", f"outer supervisor signal {STOP_SIGNAL}; parent={os.getppid()}\n")
            stop_deadline = stop_deadline or time.monotonic() + 20
        if time.monotonic() > min(deadline, stop_deadline or deadline):
            # start_new_session made a dedicated process group: only descendants
            # of this launcher can belong to it. Record concrete identities and
            # verify them again before signaling; never match a program name.
            members = []
            for entry in Path("/proc").iterdir():
                if entry.name.isdigit():
                    identity = process_identity(int(entry.name))
                    if identity.get("process_group") == process.pid:
                        members.append(identity)
            json_write(lab / "forced-cleanup.json", members)
            for identity in reversed(members):
                if process_identity(identity["pid"]) == identity:
                    try:
                        os.kill(identity["pid"], signal.SIGTERM)
                    except ProcessLookupError:
                        pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                for identity in reversed(members):
                    if process_identity(identity["pid"]) == identity:
                        try:
                            os.kill(identity["pid"], signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                process.wait(timeout=5)
            break
        time.sleep(0.2)
    print(f"LAB_EXIT {lab} status={process.returncode}", flush=True)
    return process.returncode


def existing(args):
    lab = valid_lab(args.lab)
    if args.action == "status":
        print(json.dumps(read_json(lab / "status.json"), indent=2))
        return 0
    if args.action == "stop":
        private_write(lab / "stop.request", "explicit scoped stop\n")
        return 0
    status = read_json(lab / "status.json")
    if status["phase"] != "ready" or process_identity(status["supervisor"]["pid"]) != status["supervisor"]:
        raise RuntimeError("the recorded lab supervisor is not live and ready; refusing stale connection details")
    environment = private_environment(lab, os.environ)
    environment.update(read_json(lab / "session-env.json"))
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        raise ValueError("exec requires a command after --")
    return subprocess.run(command, env=environment, cwd=lab / "output", timeout=30).returncode


def lab_alive(lab):
    status = read_json(lab / "status.json")
    return status["phase"] == "ready" and process_identity(status["supervisor"]["pid"]) == status["supervisor"]


def run_app(args):
    """Attach one supervised app to a live lab, never relaunch any lab service."""
    lab = valid_lab(args.lab)
    if not lab_alive(lab):
        raise RuntimeError("lab is not live and ready")
    prepare_fontconfig(lab)
    environment = private_environment(lab, os.environ)
    environment.update(read_json(lab / "session-env.json"))
    app = Path(args.app).resolve(strict=True)
    name = f"extra-app-{os.getpid()}"
    frozen = lab / "bin" / name
    frozen.parent.mkdir(mode=0o700, exist_ok=True)
    with open(app, "rb") as source, open(frozen, "xb") as destination:
        shutil.copyfileobj(source, destination, 1024 * 1024)
    os.chmod(frozen, 0o700)
    digest = hashlib.sha256()
    with open(frozen, "rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    children = Children(lab, environment, record=f"{name}-children.json")
    app_status = {"supervisor": process_identity(os.getpid()), "app": str(frozen),
                  "original_app": str(app), "sha256": digest.hexdigest(), "name": name, "phase": "running"}
    try:
        children.start(name, [str(frozen)])
        json_write(lab / f"{name}.json", app_status)
        print("APP_READY " + json.dumps(app_status), flush=True)
        deadline = time.monotonic() + args.seconds
        while time.monotonic() < deadline and lab_alive(lab):
            if (lab / f"{name}.stop").exists():
                break
            children.check()
            STOP.wait(0.2)
    except (InterruptedError, RuntimeError) as error:
        print(f"APP_EXIT {error}", flush=True)
    finally:
        children.close()
        app_status["phase"] = "stopped"
        app_status["cleanup_complete"] = all(process.poll() is not None for _, _, process in children.entries)
        json_write(lab / f"{name}.json", app_status)
    return 0


def main():
    os.umask(0o077)
    def request_stop(number, _frame):
        global STOP_SIGNAL
        STOP_SIGNAL = number
        STOP.set()
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, request_stop)
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="action", required=True)
    for action in ("start", "inner"):
        child = subcommands.add_parser(action)
        child.add_argument("--app", required=True)
        child.add_argument("--seconds", type=int, default=1200)
        child.add_argument("--startup-timeout", type=int, default=45)
        if action == "inner":
            child.add_argument("--lab", required=True)
    for action in ("status", "stop", "exec"):
        child = subcommands.add_parser(action)
        child.add_argument("lab")
        if action == "exec":
            child.add_argument("command", nargs=argparse.REMAINDER)
    child = subcommands.add_parser("run-app")
    child.add_argument("lab")
    child.add_argument("--app", required=True)
    child.add_argument("--seconds", type=int, default=1200)
    args = parser.parse_args()
    if args.action == "run-app":
        validate_lifetime(args.seconds)
        return run_app(args)
    if args.action in ("start", "inner"):
        validate_lifetime(args.seconds)
        if not 1 <= args.startup_timeout <= 120:
            raise ValueError("startup timeout must be 1–120 seconds")
        return start(args) if args.action == "start" else inner(args)
    return existing(args)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        print(f"ERROR: {error}", file=sys.stderr)
        sys.exit(1)
