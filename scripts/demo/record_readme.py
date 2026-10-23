#!/usr/bin/env python3
"""Record the real desktop workflow on an owned Xvfb display.

Requires Xvfb, xdotool, ImageMagick import, FFmpeg, Pillow, Tk and DejaVu fonts.
The background/animation is original illustrative content; app pixels are not
reconstructed. Every interaction is recorded in order, with no time compression.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time

from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from owned_xvfb import owned_display, stop_child


def record(binary, output, version):
    output.mkdir(parents=True, exist_ok=False)
    state = output / "scene.json"
    events = []
    started = None

    def scene(visible, caption):
        state.write_text(json.dumps({"scene": visible, "caption": caption}), encoding="utf-8")
        if started is not None:
            events.append({"seconds": round(time.monotonic() - started, 3), "caption": caption})

    scene(False, "01  /  Open the recorder")
    with owned_display() as environment:
        environment.update(
            XDG_CONFIG_HOME=str(output / "config"),
            XDG_DATA_HOME=str(output / "data"),
            XDG_STATE_HOME=str(output / "state"),
            XDG_SESSION_TYPE="x11", LIBGL_ALWAYS_SOFTWARE="1",
            LANG="en_US.UTF-8", LC_ALL="en_US.UTF-8",
        )
        environment.pop("WAYLAND_DISPLAY", None)

        def run(*args):
            return subprocess.run(args, env=environment, check=True, capture_output=True,
                                  text=True, timeout=15).stdout.strip()

        def pointer(x, y, click=False):
            run("xdotool", "mousemove", str(x), str(y))
            time.sleep(0.15)
            if click:
                run("xdotool", "click", "1")

        def shot(name):
            run("import", "-display", environment["DISPLAY"], "-window", "root", str(output / (name + ".png")))

        def wait_for(predicate, description, timeout=20):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if app.poll() is not None:
                    raise RuntimeError("Application exited; inspect app.log")
                if predicate():
                    return
                time.sleep(0.1)
            raise RuntimeError("Timed out waiting for " + description)

        with (output / "app.log").open("w") as app_log, (output / "capture.log").open("w") as capture_log:
            fixture = subprocess.Popen(
                [sys.executable, str(Path(__file__).with_name("scene.py")), str(state), version],
                env=environment, stdout=app_log, stderr=app_log,
            )
            app = recorder = None
            try:
                time.sleep(0.5)
                run("xsetroot", "-cursor_name", "left_ptr")
                app = subprocess.Popen([str(binary)], cwd=output, env=environment,
                                       stdout=app_log, stderr=app_log)
                time.sleep(3)
                window = run("xdotool", "search", "--name", "^GifFromScreen$").splitlines()[-1]
                run("xdotool", "windowmove", window, "252", "292", "windowfocus", window)
                pointer(1023, 767)
                time.sleep(0.5)
                recorder = subprocess.Popen([
                    "ffmpeg", "-hide_banner", "-loglevel", "warning", "-y",
                    "-f", "x11grab", "-video_size", "1024x768", "-framerate", "20",
                    "-i", environment["DISPLAY"], "-c:v", "ffv1", str(output / "workflow.mkv"),
                ], stdin=subprocess.PIPE, stdout=capture_log, stderr=capture_log, env=environment)
                started = time.monotonic()
                shot("launcher")
                time.sleep(2)
                scene(True, "02  /  Capture a little motion")
                pointer(318, 410, True)
                time.sleep(0.7)
                run("xdotool", "windowfocus", window)
                pointer(303, 648, True)
                run("xdotool", "key", "ctrl+a", "type", "--clearmodifiers", "20")
                run("xdotool", "key", "Return")
                time.sleep(0.4)
                shot("recorder-ready")
                pointer(529, 648, True)
                pointer(1023, 767)
                time.sleep(5.7)
                shot("recording")
                pointer(587, 648, True)
                wait_for(lambda: "WIDTH=960" in run("xdotool", "getwindowgeometry", "--shell", window), "the editor")
                run("xdotool", "windowmove", window, "32", "64", "windowsize", window, "960", "632")
                scene(True, "03  /  Inspect and replay your frames")
                time.sleep(0.5)
                pointer(136, 661, True)
                pointer(1023, 767)
                time.sleep(3)
                shot("editor")
                pointer(136, 661, True)
                pointer(280, 595, True)
                time.sleep(0.7)
                scene(True, "04  /  Export your GIF")
                pointer(933, 89, True)
                time.sleep(0.6)
                run("xdotool", "windowfocus", window)
                pointer(277, 302, True)
                run("xdotool", "key", "ctrl+a", "type", "--clearmodifiers", "motion-study.gif")
                pointer(98, 608, True)
                pointer(1023, 767)
                wait_for(lambda: (output / "motion-study.gif").is_file(), "the exported GIF", timeout=30)
                time.sleep(1)
                shot("export")
                # Return to the same recorded project, then replay the result.
                pointer(600, 300)
                run("xdotool", "click", "--repeat", "16", "--delay", "35", "4")
                time.sleep(0.6)
                pointer(70, 136, True)
                time.sleep(0.5)
                pointer(136, 661, True)
                pointer(1023, 767)
                scene(True, "05  /  A moment, ready to share")
                time.sleep(3)
                shot("final")
                recorder.communicate(b"q", timeout=10)
                if recorder.returncode:
                    raise RuntimeError("FFmpeg capture failed; inspect capture.log")
                recorder = None
            finally:
                if recorder is not None:
                    recorder.communicate(b"q", timeout=10)
                if app is not None:
                    stop_child(app)
                stop_child(fixture)

    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "warning", "-y", "-i", str(output / "workflow.mkv"),
        "-filter_complex", "[0:v]split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle",
        "-loop", "0", str(output / "workflow.gif"),
    ], check=True, timeout=120)
    with Image.open(output / "workflow.gif") as gif:
        duration_ms = 0
        for index in range(gif.n_frames):
            gif.seek(index)
            gif.load()
            duration_ms += gif.info["duration"]
        metadata = {"width": gif.width, "height": gif.height, "frames": gif.n_frames,
                    "duration_ms": duration_ms, "loop": gif.info.get("loop")}
    (output / "provenance.json").write_text(json.dumps({
        "version": version, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "display": "owned Xvfb, X11, 1024x768", "recording_fps": 20,
        "events": events, "gif": metadata,
        "gif_sha256": hashlib.sha256((output / "workflow.gif").read_bytes()).hexdigest(),
    }, indent=2) + "\n", encoding="utf-8")
    print(output / "workflow.gif", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path, help="New directory; existing paths are refused")
    parser.add_argument("--version", required=True, help="Version of the supplied binary, for the demo caption")
    args = parser.parse_args()
    record(args.binary.resolve(strict=True), args.output_dir.resolve(), args.version)
