#!/usr/bin/env python3
"""Per-user installation with exact-file ownership and no recursive deletion."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import sys

APP_ID = "io.github.tiansongyu.gifromscreen"
PAYLOAD = Path("lib/gifromscreen")
MANIFEST = PAYLOAD / "install-manifest.json"
DESKTOP = Path("share/applications") / (APP_ID + ".desktop")
ICON = Path("share/icons/hicolor/scalable/apps") / (APP_ID + ".svg")
BINARIES = ("gif-from-screen", "gif-from-screen-cli")


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def exists(path):
    return os.path.lexists(path)


def regular(path):
    return stat.S_ISREG(path.lstat().st_mode)


def safe_path(prefix, relative):
    """Reject traversal and symlinks anywhere in the destination ancestry."""
    relative = PurePosixPath(str(relative))
    if relative.is_absolute() or not relative.parts or any(part in (".", "..") for part in relative.parts):
        raise ValueError("unsafe relative path: " + str(relative))
    path = prefix.joinpath(*relative.parts)
    for candidate in reversed(path.parents):
        if candidate.is_symlink():
            raise ValueError("refusing symlinked installation directory: " + str(candidate))
        if exists(candidate) and not candidate.is_dir():
            raise ValueError("installation parent is not a directory: " + str(candidate))
    return path


def owned_path(relative):
    relative = Path(relative)
    return relative in (DESKTOP, ICON, *(Path("bin") / name for name in BINARIES)) or PAYLOAD in relative.parents


def verify_bundle(bundle):
    checksums = bundle / "SHA256SUMS"
    if not checksums.is_file() or checksums.is_symlink():
        raise ValueError("run the installer from a complete, extracted portable package")
    files = {}
    for line in checksums.read_text(encoding="utf-8").splitlines():
        expected, separator, name = line.partition("  ")
        if not separator or len(expected) != 64 or any(char not in "0123456789abcdef" for char in expected):
            raise ValueError("invalid package checksum entry")
        path = safe_path(bundle, name)
        if name in files or not regular(path) or digest(path) != expected:
            raise ValueError("package checksum mismatch: " + name)
        files[name] = {"kind": "file", "sha256": expected}
    required = ["installer.py", "install.sh", "uninstall.sh", str(DESKTOP), str(ICON)]
    required.extend("bin/" + name for name in BINARIES)
    if any(name not in files for name in required):
        raise ValueError("package is missing a required installation file")
    files["SHA256SUMS"] = {"kind": "file", "sha256": digest(checksums)}
    return files


def desktop_exec(path):
    # The desktop-file string layer is decoded before Exec argument quoting.
    value = str(path).replace("%", "%%")
    for character in ("\\", '"', "$", "`"):
        value = value.replace(character, "\\" + character)
    return '"' + value.replace("\\", "\\\\") + '"'


def installation_files(bundle, prefix):
    files = {}
    for name, record in verify_bundle(bundle).items():
        files[str(PAYLOAD / name)] = dict(record, source=str(bundle / name))
    for name in BINARIES:
        files["bin/" + name] = {"kind": "symlink", "target": "../lib/gifromscreen/bin/" + name}
    files[str(ICON)] = {
        "kind": "file", "sha256": digest(bundle / ICON), "source": str(bundle / ICON),
    }
    launcher = (bundle / DESKTOP).read_text(encoding="utf-8")
    launcher = launcher.replace("Exec=gif-from-screen\n", "Exec=" + desktop_exec(prefix / PAYLOAD / "bin/gif-from-screen") + "\n")
    launcher = launcher.replace("Icon=" + APP_ID + "\n", "Icon=" + str(prefix / ICON).replace("\\", "\\\\") + "\n")
    data = launcher.encode("utf-8")
    files[str(DESKTOP)] = {"kind": "file", "sha256": hashlib.sha256(data).hexdigest(), "data": data}
    return files


def manifest_records(files):
    return {name: {key: value for key, value in record.items() if key not in ("source", "data")} for name, record in files.items()}


def load_manifest(prefix):
    path = safe_path(prefix, MANIFEST)
    if not exists(path):
        return None
    if not regular(path):
        raise ValueError("installation manifest is not a regular file")
    document = json.loads(path.read_text(encoding="utf-8"))
    if document.get("application") != APP_ID or document.get("format") != 1 or document.get("prefix") != str(prefix):
        raise ValueError("existing installation has a different owner or prefix")
    records = document.get("files")
    if not isinstance(records, dict) or not records:
        raise ValueError("invalid installation file manifest")
    for name, record in records.items():
        safe_path(prefix, name)
        if not isinstance(record, dict) or not owned_path(name) or name == str(MANIFEST) or record.get("kind") not in ("file", "symlink"):
            raise ValueError("manifest contains a file outside the application's namespaces")
    return records


def check_installed(prefix, records, allow_missing=False):
    for name, record in records.items():
        path = safe_path(prefix, name)
        if allow_missing and not exists(path):
            continue
        if record["kind"] == "symlink":
            valid = path.is_symlink() and os.readlink(path) == record.get("target")
        else:
            valid = exists(path) and regular(path) and digest(path) == record.get("sha256")
        if not valid:
            raise ValueError("installed file was changed; nothing was removed: " + str(path))


def ensure_parents(path, created):
    for parent in reversed(path.parents):
        if not exists(parent):
            parent.mkdir()
            created.append(parent)
        elif parent.is_symlink() or not parent.is_dir():
            raise ValueError("unsafe installation parent: " + str(parent))


def install(bundle, prefix):
    files = installation_files(bundle, prefix)
    records = manifest_records(files)
    previous = load_manifest(prefix)
    if previous is not None:
        check_installed(prefix, previous)
        if previous == records:
            print("The identical package is already installed; no files changed.")
            return
        raise ValueError("another GifFromScreen version is installed; uninstall it before installing this package")
    # Complete preflight before creating any destination.
    for name in (*files, str(MANIFEST)):
        if exists(safe_path(prefix, name)):
            raise ValueError("refusing to overwrite an existing file: " + str(prefix / name))
    created_files = []
    created_directories = []
    try:
        for name, record in files.items():
            path = safe_path(prefix, name)
            ensure_parents(path, created_directories)
            if record["kind"] == "symlink":
                os.symlink(record["target"], path)
            else:
                mode = 0o755 if name.startswith(str(PAYLOAD / "bin") + "/") or name.endswith((".sh", "/installer.py")) else 0o644
                descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, mode)
                created_files.append(path)
                with os.fdopen(descriptor, "wb") as destination:
                    if "data" in record:
                        destination.write(record["data"])
                    else:
                        with open(record["source"], "rb") as source:
                            shutil.copyfileobj(source, destination)
                continue
            created_files.append(path)
        manifest = safe_path(prefix, MANIFEST)
        document = {"format": 1, "application": APP_ID, "prefix": str(prefix), "files": records}
        with manifest.open("x", encoding="utf-8") as destination:
            created_files.append(manifest)
            json.dump(document, destination, indent=2, sort_keys=True)
            destination.write("\n")
    except Exception:
        # Only names successfully created by this invocation are rolled back.
        for path in reversed(created_files):
            if exists(path):
                path.unlink()
        for path in reversed(created_directories):
            try:
                path.rmdir()
            except OSError:
                pass
        raise
    print("Installed GifFromScreen in " + str(prefix))
    print("Desktop launcher: " + str(prefix / DESKTOP))
    print("Uninstall: " + str(prefix / PAYLOAD / "uninstall.sh") + " --prefix " + str(prefix))


def uninstall(prefix):
    records = load_manifest(prefix)
    if records is None:
        raise ValueError("no GifFromScreen installation manifest at " + str(prefix / MANIFEST))
    check_installed(prefix, records, allow_missing=True)
    directories = set()
    for name in (*records, str(MANIFEST)):
        path = safe_path(prefix, name)
        for parent in path.parents:
            if parent == prefix:
                break
            directories.add(parent)
        if exists(path):
            path.unlink()
    # rmdir removes empty directories only. Unknown files and projects survive.
    for path in sorted(directories, key=lambda value: len(value.parts), reverse=True):
        try:
            path.rmdir()
        except OSError:
            pass
    print("Removed unchanged GifFromScreen application files. Projects and user data were preserved.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("install", "uninstall"))
    parser.add_argument("--prefix", type=Path, default=Path.home() / ".local")
    arguments = parser.parse_args()
    prefix = Path(os.path.abspath(arguments.prefix.expanduser()))
    if prefix in (Path("/"), Path.home()) or any(ord(char) < 32 for char in str(prefix)):
        raise ValueError("choose a dedicated installation prefix, not a root/home directory or control-character path")
    safe_path(prefix, MANIFEST)
    if arguments.operation == "install":
        install(Path(__file__).absolute().parent, prefix)
    else:
        uninstall(prefix)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError) as error:
        print("Installation error: " + str(error), file=sys.stderr)
        sys.exit(1)
