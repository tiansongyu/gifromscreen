#!/usr/bin/env python3
"""Exercise a locally built AppImage via its runtime/launcher on an owned Xvfb."""

import argparse
import ctypes
import ctypes.util
from contextlib import nullcontext
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time

from owned_xvfb import owned_display
from owned_process import OwnedProcess


class MessageData(ctypes.Union):
    _fields_ = [("bytes", ctypes.c_char * 20), ("longs", ctypes.c_long * 5)]


class ClientMessage(ctypes.Structure):
    _fields_ = [("type", ctypes.c_int), ("serial", ctypes.c_ulong),
                ("send_event", ctypes.c_int), ("display", ctypes.c_void_p),
                ("window", ctypes.c_ulong), ("message_type", ctypes.c_ulong),
                ("format", ctypes.c_int), ("data", MessageData)]


class XEvent(ctypes.Union):
    _fields_ = [("client", ClientMessage), ("padding", ctypes.c_long * 24)]


def close_owned_window(window, group, environment):
    properties = subprocess.check_output(
        ["xprop", "-id", str(window), "_NET_WM_PID", "WM_PROTOCOLS"],
        env=environment, text=True, timeout=3,
    )
    match = re.search(r"_NET_WM_PID\(CARDINAL\) = (\d+)", properties)
    if not match or os.getpgid(int(match.group(1))) != group or "WM_DELETE_WINDOW" not in properties:
        raise ValueError("visible window does not belong to the owned AppImage process group")
    # Standard WM close, never XDestroyWindow. The library talks only to the
    # private display supplied by owned_display, and the target was just checked.
    library = ctypes.CDLL(ctypes.util.find_library("X11"))
    library.XOpenDisplay.argtypes = [ctypes.c_char_p]
    library.XOpenDisplay.restype = ctypes.c_void_p
    library.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
    library.XInternAtom.restype = ctypes.c_ulong
    library.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long, ctypes.POINTER(XEvent)]
    library.XFlush.argtypes = [ctypes.c_void_p]
    library.XCloseDisplay.argtypes = [ctypes.c_void_p]
    display = library.XOpenDisplay(environment["DISPLAY"].encode())
    if not display:
        raise RuntimeError("could not open the owned display for normal close")
    try:
        protocols = library.XInternAtom(display, b"WM_PROTOCOLS", 1)
        delete = library.XInternAtom(display, b"WM_DELETE_WINDOW", 1)
        if not protocols or not delete:
            raise ValueError("close protocol disappeared")
        event = XEvent()
        event.client.type = 33
        event.client.window = window
        event.client.message_type = protocols
        event.client.format = 32
        event.client.data.longs[0] = delete
        if not library.XSendEvent(display, window, 0, 0, ctypes.byref(event)):
            raise RuntimeError("WM_DELETE_WINDOW could not be queued")
        library.XFlush(display)
    finally:
        library.XCloseDisplay(display)


def desktop(artifact, scratch, environment):
    with (scratch / "desktop.log").open("w+") as log, OwnedProcess(
        [str(artifact)], cwd=scratch, env=environment, stdout=log, stderr=log,
    ) as process:
        try:
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError("AppImage exited before displaying its window")
                result = subprocess.run(["xdotool", "search", "--onlyvisible", "--name", "^GifFromScreen$"],
                                        env=environment, capture_output=True, text=True, timeout=2)
                if result.returncode == 0 and result.stdout.strip():
                    close_owned_window(int(result.stdout.splitlines()[0]), process.pid, environment)
                    if process.wait(timeout=10) != 0:
                        raise RuntimeError("AppImage did not exit successfully after normal window close")
                    return
                time.sleep(0.1)
            raise RuntimeError("AppImage did not display a native window before the deadline")
        except Exception as error:
            log.flush()
            log.seek(0)
            raise RuntimeError(str(error) + "\n" + log.read(65536)) from error


def cli(artifact, arguments, scratch, environment):
    # Regular-file output avoids waiting indefinitely on an orphan's inherited
    # pipe if the runtime fails. The owner retains the leader until group cleanup.
    with tempfile.TemporaryFile(mode="w+") as log, OwnedProcess(
        [str(artifact), "--cli", *arguments], cwd=scratch, env=environment,
        stdout=log, stderr=log,
    ) as process:
        status = process.wait(timeout=30)
        log.seek(0)
        output = log.read(65536)
        if status != 0:
            raise RuntimeError("AppImage CLI failed: " + output)
        return output.strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("appimage", type=Path)
    parser.add_argument("--mode", choices=("extract", "fuse"), default="extract")
    parser.add_argument("--evidence-dir", type=Path, help="retain logs/GIFs in a new directory instead of temporary cleanup")
    arguments = parser.parse_args()
    artifact = arguments.appimage.resolve(strict=True)
    if arguments.evidence_dir:
        arguments.evidence_dir.mkdir(parents=True, exist_ok=False)
    workspace = (nullcontext(str(arguments.evidence_dir.absolute())) if arguments.evidence_dir
                 else tempfile.TemporaryDirectory(prefix="gfs-appimage-check-"))
    with workspace as temporary, owned_display() as environment:
        scratch = Path(temporary)
        for name in ("config", "data", "state", "cache", "runtime", "temp"):
            (scratch / name).mkdir(mode=0o700)
        environment.update(
            XDG_CONFIG_HOME=str(scratch / "config"), XDG_DATA_HOME=str(scratch / "data"),
            XDG_STATE_HOME=str(scratch / "state"), XDG_CACHE_HOME=str(scratch / "cache"),
            XDG_RUNTIME_DIR=str(scratch / "runtime"), TMPDIR=str(scratch / "temp"),
            XDG_SESSION_TYPE="x11",
            WGPU_BACKEND="gl", LIBGL_ALWAYS_SOFTWARE="1",
        )
        environment.pop("WAYLAND_DISPLAY", None)
        environment.pop("NO_CLEANUP", None)
        environment.pop("APPIMAGE_EXTRACT_AND_RUN", None)
        if arguments.mode == "extract":
            environment["APPIMAGE_EXTRACT_AND_RUN"] = "1"
        version = cli(artifact, ["version"], scratch, environment)
        if not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+].*)?", version):
            raise ValueError("AppImage CLI returned an invalid package version")
        output = scratch / "sample output.gif"
        cli(artifact, ["demo", str(output)], scratch, environment)
        info = json.loads(subprocess.check_output(
            ["ffprobe", "-v", "error", "-show_entries", "stream=codec_name,width,height,nb_frames",
             "-of", "json", str(output)], text=True, timeout=10))
        stream = info["streams"][0]
        if stream["codec_name"] != "gif" or int(stream["nb_frames"]) < 2:
            raise ValueError("AppRun demo did not create the expected animated GIF")
        subprocess.run(["ffmpeg", "-v", "error", "-i", str(output), "-f", "null", "-"],
                       check=True, capture_output=True, timeout=30)
        desktop(artifact, scratch, environment)
        leftovers = list((scratch / "temp").iterdir())
        if leftovers:
            raise ValueError("AppImage runtime files remained after normal exit: " + str(leftovers))
        report = {"version": version, "cli_gif": stream, "mode": arguments.mode,
                  "native_window": "normal close, exit 0", "runtime_cleanup": "complete",
                  "scope": "owned Xvfb, software GL; not Wayland/hardware acceptance"}
        (scratch / "result.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(report))


if __name__ == "__main__":
    main()
