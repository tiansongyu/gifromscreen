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
import runpy
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


def installed_version(prefix):
    receipt = prefix / "lib/gifromscreen/BUILD-INFO.json"
    if not receipt.exists():
        return None
    if receipt.is_symlink() or receipt.stat().st_size > 1024 * 1024:
        raise ValueError("invalid installed build receipt: " + str(receipt))
    version = json.loads(receipt.read_text(encoding="utf-8")).get("package_version")
    if not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("cannot identify the installed version; keep the existing installation")
    return version


def install_bundle(bundle, prefix):
    manifest = prefix / "lib/gifromscreen/install-manifest.json"
    if not manifest.exists():
        subprocess.run([sys.executable, str(bundle / "installer.py"), "install", "--prefix", str(prefix)], check=True)
        return
    # Use the newly verified package's ownership checks; never run arbitrary
    # code from an existing installation to remove files.
    package = runpy.run_path(str(bundle / "installer.py"))
    previous = package["load_manifest"](prefix)
    package["check_installed"](prefix, previous)
    planned = package["installation_files"](bundle, prefix)
    if package["manifest_records"](planned) == previous:
        print("The identical package is already installed; no files changed.")
        return
    for name in planned:
        if name not in previous and os.path.lexists(prefix / name):
            raise ValueError("refusing to overwrite an existing file: " + str(prefix / name))
    backup = Path(tempfile.mkdtemp(prefix="gifromscreen-upgrade-backup-"))
    try:
        for name in previous:
            path = PurePosixPath(name)
            if path.parts[:2] == ("lib", "gifromscreen"):
                relative = Path(*path.parts[2:])
                target = backup / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(prefix / name, target)
        package["verify_bundle"](backup)
    except Exception:
        shutil.rmtree(backup)
        raise
    try:
        package["uninstall"](prefix)
        try:
            package["install"](bundle, prefix)
        except Exception:
            package["install"](backup, prefix)
            raise
    except Exception as error:
        # Retain a verified copy if either replacement or rollback fails.
        raise OSError("upgrade failed; previous package retained at " + str(backup) + ": " + str(error)) from error
    shutil.rmtree(backup)


def main():
    parser = argparse.ArgumentParser(description="Install an official GifFromScreen Linux release.")
    parser.add_argument("--version", help="release version; defaults to the latest stable release")
    parser.add_argument("--allow-downgrade", action="store_true", help="explicitly allow replacing a newer installed version")
    parser.add_argument("--prefix", default=os.environ.get("GIFROMSCREEN_PREFIX", str(Path.home() / ".local")),
                        help="absolute installation prefix (default: ~/.local)")
    args = parser.parse_args()
    check_platform()
    prefix = Path(os.path.abspath(Path(args.prefix).expanduser()))
    if not Path(args.prefix).expanduser().is_absolute():
        raise ValueError("--prefix must be an absolute path")
    if prefix in (Path("/"), Path.home()) or any(ord(char) < 32 for char in str(prefix)):
        raise ValueError("choose a dedicated installation prefix")
    tag = release_version(args.version)
    current = installed_version(prefix)
    target_numbers = tuple(map(int, tag[1:].split("-", 1)[0].split(".")))
    if current and tuple(map(int, current.split("."))) > target_numbers and not args.allow_downgrade:
        print("Installed GifFromScreen " + current + " is newer than release " + tag + ". Keeping the installed version.")
        print("No files changed. Use --allow-downgrade only if you intend to install the older release.")
        return
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
        install_bundle(bundle, prefix)
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
