"""Bounded, owned Xvfb for package smoke tests; never reuses host DISPLAY."""

from contextlib import contextmanager
import math
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time


# Xorg's displayfd is a readiness notification, sent after screen/font/extension
# initialization, not merely socket selection. Five seconds is not a protocol
# deadline. Allow one longer, still bounded startup for the same child; this
# does not diagnose a particular slow runner. Never use a host display/respawn.
STARTUP_TIMEOUT_SECONDS = 30
MAX_STARTUP_TIMEOUT_SECONDS = 120
MAX_DISPLAY_BYTES = 32


def validate_startup_timeout(timeout):
    if (isinstance(timeout, bool) or not isinstance(timeout, (int, float))
            or not math.isfinite(timeout) or not 0 < timeout <= MAX_STARTUP_TIMEOUT_SECONDS):
        raise ValueError("Xvfb startup timeout must be finite and within (0, 120] seconds")


def stop_child(process):
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def read_display_number(server, timeout=STARTUP_TIMEOUT_SECONDS):
    validate_startup_timeout(timeout)
    started = time.monotonic()
    deadline = started + timeout
    data = b""

    def failure(reason, status):
        return RuntimeError(
            f"owned Xvfb startup failed: {reason}; PID {server.pid}; "
            f"elapsed={time.monotonic() - started:.3f}s; budget={timeout:g}s; "
            f"status={status!r}; displayfd bytes={len(data)}; prefix={data!r}"
        )

    if server.stdout is None:
        raise failure("no display-number pipe", server.poll())
    while b"\n" not in data:
        status = server.poll()
        if status is not None:
            raise failure("child exited before publishing a display", status)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise failure("deadline expired before a complete display number", status)
        try:
            readable = select.select([server.stdout], [], [], min(remaining, 0.1))[0]
        except InterruptedError:
            continue
        if not readable:
            continue
        if time.monotonic() >= deadline:
            raise failure("deadline expired before a complete display number", server.poll())
        chunk = os.read(server.stdout.fileno(), MAX_DISPLAY_BYTES - len(data))
        if not chunk:
            raise failure("display-number pipe reached EOF", server.poll())
        data += chunk
        if len(data) >= MAX_DISPLAY_BYTES:
            raise failure("oversized display number", server.poll())
    number = data[:-1] if data.endswith(b"\n") else b""
    if not number.isdigit() or int(number) > 65535:
        raise failure("invalid display number", server.poll())
    status = server.poll()
    if status is not None:
        raise failure("child exited after publishing a display", status)
    return ":" + number.decode("ascii")


@contextmanager
def owned_display(*, startup_timeout=STARTUP_TIMEOUT_SECONDS):
    validate_startup_timeout(startup_timeout)
    with tempfile.TemporaryDirectory(prefix="gfs-smoke-xvfb-") as scratch:
        with (Path(scratch) / "server.log").open("w+", encoding="utf-8", errors="replace") as log:
            started = time.monotonic()
            server = subprocess.Popen(
                ["Xvfb", "-displayfd", "1", "-screen", "0", "1024x768x24",
                 "-nolisten", "tcp", "-ac", "-noreset"],
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=log,
            )
            try:
                environment = os.environ.copy()
                environment["DISPLAY"] = read_display_number(server, timeout=startup_timeout)
                environment.pop("XAUTHORITY", None)
                # Verify a real connection before launching the application.
                # -noreset keeps probe disconnects from resetting the server.
                probe = subprocess.run(
                    ["xdotool", "getdisplaygeometry"], env=environment,
                    capture_output=True, text=True, timeout=3,
                )
                if probe.returncode or probe.stdout.strip() != "1024 768":
                    raise RuntimeError("owned display connection check failed: " + probe.stderr)
                print(
                    f"Owned Xvfb ready: PID {server.pid}, DISPLAY {environment['DISPLAY']}, "
                    f"startup={time.monotonic() - started:.3f}s", flush=True,
                )
                yield environment
            except Exception as error:
                log.flush()
                log.seek(0)
                raise RuntimeError(
                    f"{error}\nOwned Xvfb PID: {server.pid}; status: {server.poll()}; "
                    f"startup budget: {startup_timeout:g}s; server log:\n{log.read(65536)}"
                ) from error
            finally:
                stop_child(server)
                server.stdout.close()
