#!/usr/bin/env python3
"""Measure the real desktop incremental recording worker on an owned Xvfb.

This measures application capture/writing and project recovery, not GUI render
FPS or physical display latency. The numbered/color-changing source reports its
own submitted cadence; Xvfb and Tk are not a physical display qualification.
"""

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import time

SCRIPT = Path(__file__).resolve()
ROOT = SCRIPT.parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from owned_xvfb import read_display_number, stop_child

TEST = "recording_perf_tests::owned_xvfb_desktop_incremental_recording"
REMOVED_CONNECTION_ENV = (
    "XAUTHORITY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "SESSION_MANAGER",
    "DBUS_SESSION_BUS_ADDRESS", "DBUS_SYSTEM_BUS_ADDRESS", "DBUS_STARTER_ADDRESS",
    "DBUS_STARTER_BUS_TYPE", "PIPEWIRE_REMOTE", "PIPEWIRE_RUNTIME_DIR",
)


def private_json(path, data):
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    path.chmod(0o600)


@contextmanager
def display(directory, width, height):
    with (directory / "xvfb.log").open("w") as log:
        server = subprocess.Popen([
            "Xvfb", "-displayfd", "1", "-screen", "0", f"{width}x{height}x24",
            "-nolisten", "tcp", "-ac", "-noreset",
        ], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=log)
        try:
            environment = os.environ.copy()
            for name in REMOVED_CONNECTION_ENV:
                environment.pop(name, None)
            environment["DISPLAY"] = read_display_number(server)
            (directory / "runtime").mkdir(mode=0o700)
            environment.update(
                XDG_SESSION_TYPE="x11", XDG_CONFIG_HOME=str(directory / "config"),
                XDG_DATA_HOME=str(directory / "data"), XDG_STATE_HOME=str(directory / "state"),
                GFS_RECORDING_PERF_DIR=str(directory), LIBGL_ALWAYS_SOFTWARE="1",
                XDG_RUNTIME_DIR=str(directory / "runtime"),
            )
            yield environment, server.pid
        finally:
            stop_child(server)
            server.stdout.close()


def fixture(directory):
    import signal
    import tkinter as tk

    config = json.loads((directory / "configuration.json").read_text())
    root = tk.Tk()
    root.overrideredirect(True)
    root.geometry(f"{config['width']}x{config['height']}+0+0")
    canvas = tk.Canvas(root, highlightthickness=0)
    canvas.pack(fill="both", expand=True)
    label = canvas.create_text(48, 48, anchor="nw", font=("DejaVu Sans", 32), fill="white")
    marker = canvas.create_rectangle(0, 0, 24, config["height"], fill="white", outline="")
    period = 1 / config["fps"]
    deadline = time.perf_counter()
    samples = []
    number = 0
    skipped_slots = 0

    def tick():
        nonlocal number, deadline, skipped_slots
        started = time.perf_counter()
        number += 1
        color = f"#{number & 255:02x}{(number >> 8) & 255:02x}{number * 73 & 255:02x}"
        canvas.configure(background=color)
        canvas.itemconfigure(label, text=f"Owned source frame {number}")
        left = (number * 19) % max(1, config["width"] - 24)
        canvas.coords(marker, left, 0, left + 24, config["height"])
        root.update_idletasks()
        active = (directory / "recording-started").exists() and not (directory / "recording-finished").exists()
        samples.append((time.perf_counter_ns(), active, number))
        if number == 1:
            (directory / "source-ready").touch(exist_ok=True)
        deadline += period
        if deadline < started:
            missed = int((started - deadline) / period) + 1
            skipped_slots += missed
            deadline += missed * period
        root.after(max(1, round((deadline - time.perf_counter()) * 1000)), tick)

    def stop(_number, _frame):
        root.after_idle(root.destroy)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    root.after_idle(tick)
    try:
        root.mainloop()
    finally:
        active = [sample[0] for sample in samples if sample[1]]
        intervals = sorted((after - before) / 1e6 for before, after in zip(active, active[1:]))
        def percentile(p):
            return intervals[min(len(intervals) - 1, max(0, (len(intervals) * p + 99) // 100 - 1))] if intervals else None
        fps = (len(active) - 1) * 1e9 / (active[-1] - active[0]) if len(active) > 1 else None
        private_json(directory / "source.json", {
            "scope": "Tk submitted updates on owned Xvfb, not physical presentation",
            "source_updates": number, "active_updates": len(active), "active_fps": fps,
            "interval_ms": {"p50": percentile(50), "p95": percentile(95), "p99": percentile(99)},
            "skipped_schedule_slots": skipped_slots, "samples_ns_active_number": samples,
        })


def test_binary(profile):
    command = ["cargo", "test", "--locked", "--no-run", "-p", "gif-from-screen", "--bin", "gif-from-screen", "--message-format=json", "--quiet"]
    if profile == "release":
        command.append("--release")
    result = subprocess.run(command, cwd=ROOT, check=True, capture_output=True, text=True)
    artifacts = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
    executables = [item["executable"] for item in artifacts if item.get("reason") == "compiler-artifact" and item.get("executable") and item.get("profile", {}).get("test")]
    if len(executables) != 1:
        raise RuntimeError("expected one compiled desktop test executable")
    return Path(executables[0])


def binary_digest(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def environment_metadata():
    cpu = "unknown"
    try:
        cpu = next(line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name"))
    except (OSError, StopIteration):
        pass
    return {
        "os": platform.platform(), "kernel": platform.release(), "architecture": platform.machine(),
        "cpu": cpu, "ram_bytes": os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES"),
        "renderer_scope": "owned Xvfb software X server; application GUI renderer is not started",
        "gpu_driver": "not exercised by this capture/writer benchmark",
        "physical_display_refresh_hz": None,
        "refresh_scope": "source callback scheduling only; physical display refresh/presentation unknown",
    }


def dirty_diff_digest():
    digest = hashlib.sha256(subprocess.check_output(["git", "diff", "HEAD", "--binary"], cwd=ROOT))
    names = subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=ROOT).split(b"\0")
    for name in sorted(name for name in names if name):
        digest.update(name)
        path = ROOT / os.fsdecode(name)
        if path.is_file():
            digest.update(bytes.fromhex(binary_digest(path)))
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True, help="new directory; existing paths are refused")
    parser.add_argument("--width", type=int, default=1920)
    parser.add_argument("--height", type=int, default=1080)
    parser.add_argument("--fps", type=int, default=60)
    parser.add_argument("--seconds", type=float, default=5)
    parser.add_argument("--profile", choices=("release", "debug"), default="release")
    parser.add_argument("--test-binary", type=Path, help="already compiled desktop test binary; profile is then externally unverified")
    parser.add_argument("--fixture", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    directory = args.output_dir.absolute()
    if args.fixture:
        fixture(directory)
        return 0
    if not (1 <= args.width <= 7680 and 1 <= args.height <= 4320 and 1 <= args.fps <= 60 and 0 < args.seconds <= 600):
        parser.error("dimensions must be 1..7680×1..4320, FPS 1..60, duration (0,600] seconds")
    if args.seconds * 1000 < 1:
        parser.error("duration must be at least 1 millisecond")
    directory.mkdir(mode=0o700, parents=True, exist_ok=False)
    directory.chmod(0o700)
    # Budget the worst-case raw input despite current compression/deduplication.
    raw_budget = args.width * args.height * 4 * args.fps * args.seconds
    required = int(raw_budget * 1.2) + 512 * 1024 * 1024
    available = shutil.disk_usage(directory).free
    if available < required:
        raise RuntimeError(f"insufficient free disk: need conservative {required} bytes, have {available}")
    binary = args.test_binary.absolute() if args.test_binary else test_binary(args.profile)
    configuration = dict(width=args.width, height=args.height, fps=args.fps, duration_ms=round(args.seconds * 1000))
    with display(directory, args.width, args.height) as (environment, pid):
        configuration.update(display=environment["DISPLAY"], xvfb_pid=pid)
        private_json(directory / "configuration.json", configuration)
        with (directory / "source.log").open("w") as source_log, (directory / "test.log").open("w") as test_log:
            source = subprocess.Popen([sys.executable, str(SCRIPT), "--fixture", "--output-dir", str(directory)], env=environment, stdout=source_log, stderr=source_log)
            try:
                deadline = time.monotonic() + 15
                while not (directory / "source-ready").exists():
                    if source.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError("dynamic source did not become ready; inspect source.log")
                    time.sleep(0.02)
                time.sleep(0.25)
                result = subprocess.run([str(binary), "--ignored", "--exact", TEST, "--nocapture"], env=environment, cwd=directory, stdout=test_log, stderr=test_log, timeout=max(120, args.seconds * 3 + 60))
            finally:
                stop_child(source)
    report = {
        "schema": 1, "physical_display_qualified": False,
        "scope": "desktop incremental capture/writer worker on owned Xvfb; excludes GUI rendering",
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "dirty_files": subprocess.check_output(["git", "status", "--short"], cwd=ROOT, text=True).splitlines(),
        "dirty_diff_sha256": dirty_diff_digest(), "environment": environment_metadata(),
        "test_binary": str(binary), "binary_sha256": binary_digest(binary),
        "profile": args.profile if args.test_binary is None else "external-unverified",
        "configuration": configuration, "conservative_disk_preflight_bytes": required,
        "recording": json.loads((directory / "recording.json").read_text()) if (directory / "recording.json").exists() else None,
        "source": json.loads((directory / "source.json").read_text()) if (directory / "source.json").exists() else None,
        "test_exit": result.returncode,
    }
    private_json(directory / "report.json", report)
    summary = {key: value for key, value in report.items() if key not in ("dirty_files", "source")}
    if summary["recording"]:
        summary["recording"] = {key: value for key, value in summary["recording"].items() if key != "capture_pts_us"}
    print(json.dumps(summary, indent=2))
    print("Dynamic source:", {key: value for key, value in (report["source"] or {}).items() if key != "samples_ns_active_number"})
    print("Full result:", directory / "report.json")
    return result.returncode


if __name__ == "__main__":
    sys.exit(main())
