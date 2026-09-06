#!/usr/bin/env python3
"""Launch the packaged desktop under an existing display (CI uses Xvfb)."""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile
import time

from test_portable import extract_checked


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    arguments = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="gifromscreen-desktop-smoke-") as scratch:
        bundle = extract_checked(arguments.archive, Path(scratch))
        environment = os.environ.copy()
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
                    result = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(process.pid)], capture_output=True, text=True)
                    if result.returncode == 0 and result.stdout.strip():
                        print("Packaged desktop displayed a visible X11 window.")
                        return
                    time.sleep(0.1)
                output.seek(0)
                raise RuntimeError("desktop did not show a window:\n" + output.read())
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


if __name__ == "__main__":
    main()
