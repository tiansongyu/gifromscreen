#!/usr/bin/env python3
"""Verify pinned local AppImage tools without executing or downloading them."""

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys

MANIFEST = Path(__file__).resolve().parents[1] / "packaging/appimage/tools.json"
TARGET = "x86_64-unknown-linux-gnu"
TOOL_NAMES = ("appimagetool", "runtime")
MAX_TOOL_BYTES = 32 * 1024 * 1024
READ_BLOCK_BYTES = 128 * 1024


@dataclass(frozen=True)
class ToolPin:
    name: str
    filename: str
    version: str
    url: str
    source_repository: str
    source_commit: str
    byte_len: int
    sha256: str


def _load_pins():
    with MANIFEST.open("rb") as source:
        encoded = source.read(64 * 1024 + 1)
    if len(encoded) > 64 * 1024:
        raise ValueError("AppImage tool manifest is too large")
    document = json.loads(encoded)
    if (not isinstance(document, dict)
            or type(document.get("format_version")) is not int
            or document["format_version"] != 1
            or document.get("target") != TARGET):
        raise ValueError("Unsupported AppImage tool manifest version/target")
    records = document.get("tools")
    if not isinstance(records, dict) or set(records) != set(TOOL_NAMES):
        raise ValueError("AppImage tool manifest must contain exactly appimagetool and runtime")
    pins = []
    for name in TOOL_NAMES:
        record = records[name]
        if not isinstance(record, dict):
            raise ValueError("Invalid AppImage tool record: " + name)
        strings = ("filename", "version", "url", "source_repository", "source_commit", "sha256")
        if any(not isinstance(record.get(key), str) or not record[key] for key in strings):
            raise ValueError("Missing AppImage tool metadata: " + name)
        expected_filename = "appimagetool-x86_64.AppImage" if name == "appimagetool" else "runtime-x86_64"
        repository = "https://github.com/AppImage/" + ("appimagetool" if name == "appimagetool" else "type2-runtime")
        if (record["filename"] != expected_filename
                or not re.fullmatch(r"[0-9]+(?:\.[0-9]+)*", record["version"])
                or record["source_repository"] != repository
                or record["url"] != repository + "/releases/download/" + record["version"] + "/" + expected_filename
                or not re.fullmatch(r"[0-9a-f]{40}", record["source_commit"])
                or not re.fullmatch(r"[0-9a-f]{64}", record["sha256"])
                or type(record.get("byte_len")) is not int
                or not 64 <= record["byte_len"] <= MAX_TOOL_BYTES):
            raise ValueError("Invalid pinned AppImage tool identity: " + name)
        pins.append(ToolPin(name=name, **{key: record[key] for key in (*strings, "byte_len")}))
    return tuple(pins)


def _directory(directory):
    path = Path(os.path.abspath(os.fspath(directory)))
    # Reject redirected ancestors too; resolving first would hide a symlink.
    for parent in (*reversed(path.parents), path):
        if not stat.S_ISDIR(parent.lstat().st_mode):
            raise ValueError("AppImage tools require a real, non-symlink directory: " + str(parent))
    return path


def _identity(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def _verify_elf_header(header, path):
    # e_ident: ELF64, little-endian, version 1. e_machine 62 is AMD x86-64.
    # Both executable and PIE ELF types are accepted; the complete hash still
    # selects exactly one approved release, not arbitrary executables.
    if (len(header) < 64 or header[:7] != b"\x7fELF\x02\x01\x01"
            or int.from_bytes(header[16:18], "little") not in (2, 3)
            or int.from_bytes(header[18:20], "little") != 62
            or int.from_bytes(header[20:24], "little") != 1):
        raise ValueError("AppImage tool is not an x86_64 little-endian ELF64 executable: " + str(path))


def _verify_file(path, pin):
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode):
        raise ValueError("AppImage tool must be a regular non-symlink file: " + str(path))
    if before.st_size != pin.byte_len:
        raise ValueError("AppImage tool size mismatch: " + str(path))
    # O_NOFOLLOW closes the final-component symlink race, and O_NONBLOCK avoids
    # a FIFO replacement blocking before fstat can reject it. The tool directory
    # remains caller-owned; returned paths are not a lock against future writes.
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(descriptor, "rb") as source:
        opened = os.fstat(source.fileno())
        if not stat.S_ISREG(opened.st_mode) or _identity(opened) != _identity(before):
            raise ValueError("AppImage tool changed before verification: " + str(path))
        header = source.read(64)
        _verify_elf_header(header, path)
        digest = hashlib.sha256(header)
        total = len(header)
        while total <= pin.byte_len:
            block = source.read(min(READ_BLOCK_BYTES, pin.byte_len + 1 - total))
            if not block:
                break
            digest.update(block)
            total += len(block)
        after = os.fstat(source.fileno())
        current = path.lstat()
        if (total != pin.byte_len or _identity(before) != _identity(after)
                or not stat.S_ISREG(current.st_mode) or _identity(current) != _identity(before)):
            raise ValueError("AppImage tool changed during verification: " + str(path))
    if digest.hexdigest() != pin.sha256:
        raise ValueError("AppImage tool SHA256 mismatch: " + str(path))


def verify_tools(directory):
    """Return (appimagetool, runtime) absolute Paths only after both verify.

    Files are read only. This neither chmods nor executes tools, downloads an
    update, or proves reproducible compilation of the publisher's binaries.
    Callers must keep the verified directory private/stable through execution.
    """
    if sys.platform != "linux":
        raise ValueError("The pinned AppImage tools are Linux x86_64 executables")
    pins = _load_pins()
    base = _directory(directory)
    paths = tuple(base / pin.filename for pin in pins)
    for path, pin in zip(paths, pins):
        _verify_file(path, pin)
    return paths


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, help="existing directory containing the two pinned files")
    arguments = parser.parse_args()
    try:
        appimagetool, runtime = verify_tools(arguments.directory)
    except (OSError, ValueError, KeyError) as error:
        print("AppImage tool verification failed: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps({"verified": True, "appimagetool": str(appimagetool), "runtime": str(runtime)}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
