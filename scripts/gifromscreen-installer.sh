#!/bin/sh
# Install an official GifFromScreen release without administrator privileges.
set -eu
if ! command -v python3 >/dev/null 2>&1; then
    printf '%s\n' 'GifFromScreen requires Python 3.8 or newer to install.' >&2
    exit 1
fi
python3 - "$@" <<'PYTHON'
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request

REPOSITORY = "tiansongyu/gifromscreen"
RELEASES = "https://github.com/" + REPOSITORY + "/releases/download/"
API = "https://api.github.com/repos/" + REPOSITORY + "/releases/"


def check_platform():
    if sys.version_info < (3, 8):
        raise ValueError("Python 3.8 or newer is required")
    if platform.system() != "Linux" or platform.machine() not in ("x86_64", "amd64"):
        raise ValueError("prebuilt releases currently support Linux x86_64")
    libc, version = platform.libc_ver()
    if libc != "glibc" or tuple(map(int, version.split(".")[:2])) < (2, 35):
        raise ValueError("glibc 2.35 or newer is required (Ubuntu 22.04 or newer)")


def response(url):
    request = urllib.request.Request(url, headers={"User-Agent": "GifFromScreen-installer"})
    result = urllib.request.urlopen(request, timeout=60)
    if not result.url.startswith("https://"):
        result.close()
        raise ValueError("refusing an insecure download redirect")
    return result


def release_version(requested):
    if requested:
        tag = requested if requested.startswith("v") else "v" + requested
    else:
        with response(API + "latest") as source:
            tag = json.loads(source.read(1024 * 1024))["tag_name"]
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?", tag):
        raise ValueError("invalid release version: " + str(tag))
    return tag


def download(url, destination):
    with response(url) as source, destination.open("xb") as output:
        shutil.copyfileobj(source, output)


def verify_checksum(archive, checksum):
    fields = checksum.read_text(encoding="utf-8").split()
    if len(fields) != 2 or not re.fullmatch(r"[0-9a-fA-F]{64}", fields[0]):
        raise ValueError("invalid release checksum")
    if fields[1].lstrip("*") != archive.name:
        raise ValueError("checksum filename does not match the archive")
    digest = hashlib.sha256()
    with archive.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    if digest.hexdigest() != fields[0].lower():
        raise ValueError("SHA-256 mismatch; nothing was installed")


def extract_checked(archive, destination, root_name):
    with tarfile.open(archive, "r:gz") as source:
        members = source.getmembers()
        seen = set()
        for member in members:
            path = PurePosixPath(member.name)
            if (path.is_absolute() or not path.parts or ".." in path.parts
                    or path.parts[0] != root_name or path in seen
                    or not (member.isfile() or member.isdir())):
                raise ValueError("unsafe archive member: " + member.name)
            seen.add(path)
        for member in members:
            path = destination.joinpath(*PurePosixPath(member.name).parts)
            if member.isdir():
                path.mkdir(parents=True, exist_ok=True)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                with source.extractfile(member) as data, path.open("xb") as output:
                    shutil.copyfileobj(data, output)
                path.chmod(member.mode & 0o777)
    bundle = destination / root_name
    if not (bundle / "installer.py").is_file():
        raise ValueError("release archive is missing its package installer")
    return bundle


def main():
    parser = argparse.ArgumentParser(description="Install an official GifFromScreen Linux release.")
    parser.add_argument("--version", help="release version; defaults to the latest stable release")
    parser.add_argument("--prefix", default=os.environ.get("GIFROMSCREEN_PREFIX", str(Path.home() / ".local")),
                        help="absolute installation prefix (default: ~/.local)")
    args = parser.parse_args()
    check_platform()
    prefix = Path(args.prefix).expanduser()
    if not prefix.is_absolute():
        raise ValueError("--prefix must be an absolute path")
    tag = release_version(args.version)
    root_name = "gifromscreen-" + tag[1:] + "-linux-x86_64"
    filename = root_name + ".tar.gz"
    print("Downloading GifFromScreen " + tag + " for Linux x86_64...", flush=True)
    with tempfile.TemporaryDirectory(prefix="gifromscreen-download-") as scratch:
        work = Path(scratch)
        archive = work / filename
        checksum = work / (filename + ".sha256")
        base = RELEASES + tag + "/"
        download(base + filename, archive)
        download(base + filename + ".sha256", checksum)
        verify_checksum(archive, checksum)
        print("SHA-256 verified. Installing to " + str(prefix), flush=True)
        bundle = extract_checked(archive, work / "unpacked", root_name)
        subprocess.run([sys.executable, str(bundle / "installer.py"), "install", "--prefix", str(prefix)], check=True)
    print("\nInstalled GifFromScreen " + tag + ". Launch it from your applications menu or run:")
    print("  " + shlex.quote(str(prefix / "bin/gif-from-screen")))
    if str(prefix / "bin") not in os.environ.get("PATH", "").split(os.pathsep):
        print("Add " + str(prefix / "bin") + " to PATH to use gif-from-screen from any directory.")
    print("Uninstall: " + shlex.quote(str(prefix / "lib/gifromscreen/uninstall.sh"))
          + " --prefix " + shlex.quote(str(prefix)))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, tarfile.TarError, subprocess.CalledProcessError) as error:
        print("Installation failed: " + str(error), file=sys.stderr)
        print("Manual downloads: https://github.com/" + REPOSITORY + "/releases/latest", file=sys.stderr)
        sys.exit(1)
PYTHON
