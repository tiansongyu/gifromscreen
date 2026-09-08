"""Bounded, owned Xvfb for package smoke tests; never reuses host DISPLAY."""

from contextlib import contextmanager
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time


def stop_child(process):
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def read_display_number(server, timeout=5):
    deadline = time.monotonic() + timeout
    data = b""
    while b"\n" not in data:
        if server.poll() is not None:
            raise RuntimeError(f"owned Xvfb exited during startup: {server.returncode}")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError("owned Xvfb did not publish a display within five seconds")
        if not select.select([server.stdout], [], [], min(remaining, 0.1))[0]:
            continue
        chunk = os.read(server.stdout.fileno(), 32 - len(data))
        if not chunk:
            raise RuntimeError("owned Xvfb closed its display-number pipe")
        data += chunk
        if len(data) >= 32:
            raise RuntimeError("owned Xvfb returned an oversized display number")
    number = data.strip()
    if not number.isdigit() or int(number) > 65535:
        raise RuntimeError("owned Xvfb returned an invalid display number")
    return ":" + number.decode("ascii")


@contextmanager
def owned_display():
    with tempfile.TemporaryDirectory(prefix="gfs-smoke-xvfb-") as scratch:
        with (Path(scratch) / "server.log").open("w+") as log:
            server = subprocess.Popen(
                ["Xvfb", "-displayfd", "1", "-screen", "0", "1024x768x24",
                 "-nolisten", "tcp", "-ac", "-noreset"],
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=log,
            )
            try:
                environment = os.environ.copy()
                environment["DISPLAY"] = read_display_number(server)
                environment.pop("XAUTHORITY", None)
                # Verify a real connection before launching the application.
                # -noreset keeps probe disconnects from resetting the server.
                probe = subprocess.run(
                    ["xdotool", "getdisplaygeometry"], env=environment,
                    capture_output=True, text=True, timeout=3,
                )
                if probe.returncode or probe.stdout.strip() != "1024 768":
                    raise RuntimeError("owned display connection check failed: " + probe.stderr)
                print(f"Owned Xvfb ready: PID {server.pid}, DISPLAY {environment['DISPLAY']}", flush=True)
                yield environment
            except Exception as error:
                log.flush()
                log.seek(0)
                raise RuntimeError(
                    f"{error}\nOwned Xvfb status: {server.poll()}; server log:\n{log.read(65536)}"
                ) from error
            finally:
                stop_child(server)
                server.stdout.close()
