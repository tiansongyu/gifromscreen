#!/usr/bin/env python3
"""Launch the packaged desktop under an existing display (CI uses Xvfb)."""

import argparse
from contextlib import nullcontext
import os
from pathlib import Path
import subprocess
import tempfile
import time

from portable_archive import extract_checked
from owned_xvfb import owned_display, stop_child


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--owned-xvfb", action="store_true", help="start one bounded owned Xvfb; ignore host DISPLAY")
    arguments = parser.parse_args()
    display = owned_display() if arguments.owned_xvfb else nullcontext(os.environ.copy())
    with display as environment:
        smoke(arguments.archive, environment)


def smoke(archive, environment):
    with tempfile.TemporaryDirectory(prefix="gifromscreen-desktop-smoke-") as scratch:
        bundle = extract_checked(archive, Path(scratch))
        environment = environment.copy()
        environment.pop("WAYLAND_DISPLAY", None)
        environment["XDG_SESSION_TYPE"] = "x11"
        environment["WGPU_BACKEND"] = "gl"
        environment["LIBGL_ALWAYS_SOFTWARE"] = "1"
        environment["XDG_CONFIG_HOME"] = str(Path(scratch) / "config")
        environment["XDG_DATA_HOME"] = str(Path(scratch) / "data")
        environment["XDG_STATE_HOME"] = str(Path(scratch) / "state")
        with (Path(scratch) / "desktop.log").open("w+") as output:
            process = subprocess.Popen([str(bundle / "bin/gif-from-screen")], cwd=scratch, env=environment, stdout=output, stderr=output)
            try:
                for _ in range(100):
                    if process.poll() is not None:
                        output.seek(0)
                        raise RuntimeError("desktop exited before showing a window:\n" + output.read())
                    result = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(process.pid)], env=environment, capture_output=True, text=True, timeout=2)
                    if result.returncode == 0 and result.stdout.strip():
                        print("Packaged desktop displayed a visible X11 window.")
                        return
                    time.sleep(0.1)
                output.seek(0)
                raise RuntimeError("desktop did not show a window:\n" + output.read())
            finally:
                stop_child(process)


if __name__ == "__main__":
    main()
