#!/usr/bin/env python3
"""Bundle the project's trusted Ubuntu x86-64 native client dependencies.

This is not an extractor for untrusted applications or a general linuxdeploy.
The caller must supply private copies of the two receipt-verified project ELFs.
Only dpkg-owned system libraries are passed to ldd; unit tests mock every tool.
"""

import hashlib
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
from urllib.parse import quote

BINARIES = ("gif-from-screen", "gif-from-screen-cli")
SYSTEM_ROOTS = (Path("/usr/lib"), Path("/lib"), Path("/lib64"))
LIBDIR = Path("/usr/lib/x86_64-linux-gnu")
XKB_ROOT = Path("/usr/share/X11/xkb")
DOC_ROOT = Path("/usr/share/doc")
COMMON_ROOT = Path("/usr/share/common-licenses")
CLIENT_CONFIG = Path("/usr/share/pipewire/client.conf")
CLIENT_TARGET = Path("usr/share/gifromscreen/pipewire/client.conf")
LICENSE_ROOT = Path("usr/share/licenses/gifromscreen/native")
GUI_SONAMES = (
    "libX11.so.6", "libX11-xcb.so.1", "libXcursor.so.1", "libXi.so.6",
    "libxkbcommon.so.0", "libxkbcommon-x11.so.0", "libwayland-client.so.0",
    "libwayland-cursor.so.0", "libwayland-egl.so.1",
)
PW_MODULES = ("protocol-native", "client-node", "client-device", "adapter", "metadata", "session-manager")
SPA_PLUGINS = ("support/libspa-support.so", "support/libspa-dbus.so", "support/libspa-journal.so", "audioconvert/libspa-audioconvert.so")
HOST_LIBRARIES = frozenset((
    "libc.so.6", "libm.so.6", "libpthread.so.0", "libdl.so.2", "librt.so.1",
    "libresolv.so.2", "libutil.so.1", "ld-linux-x86-64.so.2", "libanl.so.1",
    "libEGL.so.1", "libGL.so.1", "libGLX.so.0", "libGLdispatch.so.0",
    "libGLESv1_CM.so.1", "libGLESv2.so.2", "libvulkan.so.1", "libgbm.so.1",
))
MAX_FILES = 4096
MAX_LIBRARIES = 128
MAX_NATIVE_BYTES = 256 * 1024 * 1024
MAX_ELF_BYTES = 768 * 1024 * 1024
MAX_TEXT_BYTES = 2 * 1024 * 1024


def _command(arguments):
    environment = os.environ.copy()
    for key in ("LD_LIBRARY_PATH", "LD_PRELOAD", "LD_AUDIT"):
        environment.pop(key, None)
    environment["LC_ALL"] = "C"
    result = subprocess.run(arguments, check=True, capture_output=True, text=True,
                            env=environment, timeout=30)
    if len(result.stdout) > 4 * 1024 * 1024:
        raise ValueError("Native dependency tool output exceeded its bound")
    return result.stdout


def _sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _soname(name):
    if not re.fullmatch(r"[A-Za-z0-9_+.-]+", name) or name in (".", ".."):
        raise ValueError("Unsafe or unsupported library name: " + name)
    return name


def _host_library(name):
    return name in HOST_LIBRARIES or name.startswith(("libnvidia-", "libEGL_mesa.", "libGLX_mesa.")) or name.endswith("_dri.so")


def parse_ldd(output):
    """Parse known ldd forms; never reinterpret a missing/relative/unknown entry."""
    found = {}
    for raw in output.splitlines():
        line = raw.strip()
        if not line or line == "statically linked" or re.fullmatch(r"linux-vdso\.so\.1\s+\(0x[0-9a-fA-F]+\)", line):
            continue
        match = re.fullmatch(r"(\S+)\s+=>\s+(/.+?)\s+\(0x[0-9a-fA-F]+\)", line)
        if match:
            name, value = match.groups()
        else:
            match = re.fullmatch(r"(/\S+)\s+\(0x[0-9a-fA-F]+\)", line)
            if not match:
                raise ValueError("Missing or unsupported ldd dependency: " + line)
            value = match[1]
            name = Path(value).name
        _soname(name)
        path = Path(value)
        if name in found and found[name] != path:
            raise ValueError("Conflicting ldd paths for " + name)
        found[name] = path
    return found


def _elf_information(path):
    info = path.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_ELF_BYTES:
        raise ValueError("ELF must be a bounded regular file: " + str(path))
    with path.open("rb") as source:
        header = source.read(64)
    if len(header) < 64 or header[:7] != b"\x7fELF\x02\x01\x01" or int.from_bytes(header[18:20], "little") != 62:
        raise ValueError("Expected an x86-64 little-endian ELF64: " + str(path))
    dynamic = _command(["readelf", "--dynamic", "--wide", str(path)])
    versions = _command(["readelf", "--version-info", "--wide", str(path)])
    required = sorted(set(re.findall(r"GLIBC_(\d+\.\d+)", versions)), key=lambda value: tuple(map(int, value.split("."))))
    if required and tuple(map(int, required[-1].split("."))) > (2, 35):
        raise ValueError("ELF exceeds the glibc 2.35 baseline: " + str(path))
    soname = re.search(r"\(SONAME\).*?\[(.*?)\]", dynamic)
    needed = re.findall(r"\(NEEDED\).*?\[(.*?)\]", dynamic)
    for name in needed:
        _soname(name)
    return {"needed": needed, "soname": _soname(soname[1]) if soname else None,
            "glibc_maximum_required": required[-1] if required else None}


def _system_file(path):
    if not path.is_absolute():
        raise ValueError("System library path is not absolute: " + str(path))
    real = path.resolve(strict=True)
    roots = tuple(root.resolve() for root in SYSTEM_ROOTS)
    if not real.is_file() or not any(real.is_relative_to(root) for root in roots):
        raise ValueError("Dependency is not a trusted system library: " + str(path))
    return real


def _loader_cache():
    result = {}
    for line in _command(["ldconfig", "-p"]).splitlines():
        match = re.fullmatch(r"\s*(\S+)\s+\(([^)]*)\)\s+=>\s+(/\S+)\s*", line)
        if match and "x86-64" in match[2] and "hwcap" not in match[2]:
            result.setdefault(_soname(match[1]), Path(match[3]))
    return result


def _destination(appdir, relative):
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError("Unsafe bundle destination")
    current = appdir
    for part in relative.parts[:-1]:
        current /= part
        if current.exists() or current.is_symlink():
            if not stat.S_ISDIR(current.lstat().st_mode):
                raise ValueError("Destination parent is not a private directory: " + str(current))
        else:
            current.mkdir()
    return appdir / relative


class _Bundle:
    def __init__(self, appdir):
        self.appdir = appdir
        self.entries = {}
        self.packages = {}
        self.owner_cache = {}
        self.pending = []
        self.seen = set()
        self.native_bytes = 0
        self.host = set()
        self.special = {}

    def package_for(self, path):
        key = str(path)
        if key in self.owner_cache:
            return self.owner_cache[key]
        output = None
        for candidate in dict.fromkeys((path, path.resolve(strict=True))):
            try:
                output = _command(["dpkg-query", "-S", str(candidate)])
                break
            except subprocess.CalledProcessError:
                pass
        lines = [line for line in (output or "").splitlines() if ": " in line]
        if len(lines) != 1:
            raise ValueError("System source must have one unambiguous dpkg owner: " + key)
        owner = lines[0].rsplit(": ", 1)[0]
        if not re.fullmatch(r"[a-z0-9][a-z0-9+.-]*(?::[a-z0-9]+)?", owner):
            raise ValueError("Unsupported dpkg owner: " + owner)
        if owner not in self.packages:
            fmt = "${binary:Package}\t${Version}\t${source:Package}\t${source:Version}\n"
            fields = _command(["dpkg-query", "-W", "-f=" + fmt, owner]).strip().split("\t")
            if len(fields) != 4 or any(not field or "\n" in field for field in fields):
                raise ValueError("Missing package/source version for " + owner)
            self.packages[owner] = {
                "package": fields[0], "version": fields[1], "source_package": fields[2],
                "source_version": fields[3], "license_files": [],
                "source_reference": "https://launchpad.net/ubuntu/+source/" + quote(fields[2], safe="") + "/" + quote(fields[3], safe=""),
                "corresponding_source_collected": False,
            }
        self.owner_cache[key] = owner
        return owner

    def add(self, source, target, role, package=None, elf=None, runpath=None):
        real = source.resolve(strict=True)
        if not real.is_file():
            raise ValueError("Bundle source is not a regular file: " + str(source))
        digest = _sha(real)
        previous = self.entries.get(str(target))
        if previous:
            if previous["source_sha256"] != digest or previous["source_realpath"] != str(real):
                raise ValueError("Conflicting sources for " + str(target))
            return
        if len(self.entries) >= MAX_FILES:
            raise ValueError("Native bundle file-count limit exceeded")
        if role != "project-elf":
            self.native_bytes += real.stat().st_size
            if self.native_bytes > MAX_NATIVE_BYTES:
                raise ValueError("Native bundle exceeds its 256 MiB source budget")
        record = {"source": str(source), "source_realpath": str(real), "source_sha256": digest,
                  "target": str(target), "role": role, "package": package,
                  "source_byte_len": real.stat().st_size}
        if package:
            record.update({key: self.packages[package][key] for key in ("version", "source_package", "source_version")})
        if elf is not None:
            record.update(elf=elf, runpath=runpath)
            self.pending.append(real)
        self.entries[str(target)] = record

    def system_library(self, path, expected=None):
        real = _system_file(path)
        package = self.package_for(path)  # establish trust before any ldd call
        info = _elf_information(real)
        name = info["soname"] or path.name
        _soname(name)
        if expected is not None and name != expected:
            raise ValueError("Resolved ELF SONAME differs from " + expected)
        target, runpath = self.special.get(real, (Path("usr/lib") / name, "$ORIGIN"))
        self.add(path, target, "system-elf", package, info, runpath)

    def close_dependencies(self):
        while self.pending:
            path = self.pending.pop(0)
            if path in self.seen:
                continue
            self.seen.add(path)
            if len(self.seen) > MAX_LIBRARIES:
                raise ValueError("Native ELF count exceeds its bound")
            info = _elf_information(path)
            resolved = parse_ldd(_command(["ldd", str(path)]))
            if not set(info["needed"]).issubset(resolved):
                raise ValueError("ldd omitted required libraries for " + str(path))
            for name, dependency in resolved.items():
                _system_file(dependency)
                if _host_library(name):
                    self.host.add(name)
                else:
                    self.system_library(dependency, name)

    def resources(self):
        self.add(CLIENT_CONFIG, CLIENT_TARGET, "pipewire-config", self.package_for(CLIENT_CONFIG))
        root = XKB_ROOT.resolve(strict=True)
        if not root.is_dir():
            raise ValueError("XKB resource tree is missing")
        count = 0
        for directory, directories, files in os.walk(XKB_ROOT, followlinks=False):
            for name in directories:
                if (Path(directory) / name).is_symlink():
                    raise ValueError("XKB directory symlinks are not accepted")
            for name in sorted(files):
                source = Path(directory) / name
                if not source.resolve(strict=True).is_relative_to(root):
                    raise ValueError("XKB symlink escapes the resource tree")
                target = Path("usr/share/X11/xkb") / source.relative_to(XKB_ROOT)
                self.add(source, target, "xkb-data", self.package_for(source))
                count += 1
        if count == 0:
            raise ValueError("XKB resource tree is empty")

    def licenses(self):
        visited = set()
        while set(self.packages) - visited:
            for owner in sorted(set(self.packages) - visited):
                visited.add(owner)
                source = DOC_ROOT / owner.split(":", 1)[0] / "copyright"
                if source.stat().st_size > MAX_TEXT_BYTES:
                    raise ValueError("Oversized package copyright")
                data = source.read_bytes()
                target = LICENSE_ROOT / "packages" / owner.replace(":", "_") / "copyright"
                self.add(source, target, "copyright", owner)
                self.packages[owner]["license_files"].append(str(target))
                names = {name.rstrip(".") for name in re.findall(r"/usr/share/common-licenses/([A-Za-z0-9][A-Za-z0-9.+-]*)", data.decode("utf-8", errors="replace"))}
                for name in sorted(names):
                    common = COMMON_ROOT / name
                    if not common.resolve(strict=True).is_relative_to(COMMON_ROOT.resolve()) or common.stat().st_size > MAX_TEXT_BYTES:
                        raise ValueError("Invalid common-license reference: " + name)
                    common_target = LICENSE_ROOT / "common-licenses" / name
                    self.add(common, common_target, "common-license", self.package_for(common))
                    self.packages[owner]["license_files"].append(str(common_target))

    def install(self):
        for record in sorted(self.entries.values(), key=lambda entry: entry["target"]):
            source = Path(record["source_realpath"])
            if _sha(source) != record["source_sha256"]:
                raise ValueError("Source changed while planning: " + str(source))
            target = _destination(self.appdir, Path(record["target"]))
            if record["role"] != "project-elf":
                with source.open("rb") as data, target.open("xb") as output:
                    shutil.copyfileobj(data, output, 1024 * 1024)
            if "elf" in record:
                target.chmod(0o755)
                _command(["patchelf", "--set-rpath", record["runpath"], str(target)])
                after = _elf_information(target)
                if after["needed"] != record["elf"]["needed"]:
                    raise ValueError("ELF dependency list changed while setting RUNPATH")
                actual = _command(["patchelf", "--print-rpath", str(target)]).strip()
                if actual != record["runpath"]:
                    raise ValueError("Relative RUNPATH verification failed")
            else:
                target.chmod(0o644)
            record["patched_sha256"] = _sha(target)
            record["byte_len"] = target.stat().st_size


def bundle_native(appdir: Path) -> dict:
    """Populate a fresh private AppDir; return evidence, never a distribution approval.

    The two usr/bin ELFs must already be trusted build-receipt copies, not hard
    links. A failed assembly may leave a partial caller-owned staging AppDir;
    existing native targets are never overwritten and no host files are changed.
    """
    appdir = Path(os.path.abspath(appdir))
    if appdir in (Path("/"), Path("/usr"), Path("/usr/local"), Path.home()):
        raise ValueError("Refusing a broad AppDir target")
    for directory in (*reversed(appdir.parents), appdir):
        if not stat.S_ISDIR(directory.lstat().st_mode):
            raise ValueError("AppDir ancestors must be non-symlink directories")
    plan = _Bundle(appdir)
    for name in BINARIES:
        binary = _destination(appdir, Path("usr/bin") / name)
        info = binary.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise ValueError("Project ELF must be a private non-symlink, non-hardlinked copy")
        elf = _elf_information(binary)
        binary.chmod(0o755)  # ldd must not silently fail on an archive's 0644 mode
        plan.add(binary, Path("usr/bin") / name, "project-elf", elf=elf, runpath="$ORIGIN/../lib")
    explicit = [(LIBDIR / "pipewire-0.3" / ("libpipewire-module-" + name + ".so"),
                 Path("usr/lib/pipewire-0.3") / ("libpipewire-module-" + name + ".so"), "$ORIGIN:$ORIGIN/..") for name in PW_MODULES]
    explicit += [(LIBDIR / "spa-0.2" / name, Path("usr/lib/spa-0.2") / name, "$ORIGIN/../..") for name in SPA_PLUGINS]
    plan.special = {source.resolve(strict=True): (target, runpath) for source, target, runpath in explicit}
    cache = _loader_cache()
    for name in ("libpipewire-0.3.so.0", *GUI_SONAMES):
        if name not in cache:
            raise ValueError("Required GUI/client library is absent: " + name)
        plan.system_library(cache[name], name)
    for source, _, _ in explicit:
        plan.system_library(source)
    plan.close_dependencies()
    plan.resources()
    plan.licenses()
    for record in plan.entries.values():
        target = _destination(appdir, Path(record["target"]))
        if record["role"] != "project-elf" and (target.exists() or target.is_symlink()):
            raise ValueError("Native target already exists: " + str(target))
    plan.install()
    return {
        "schema_version": 1, "glibc_baseline": "2.35", "ld_library_path_used": False,
        "redistribution_ready": False, "corresponding_source_collected": False,
        "source_gate": "Package ownership/version and notices are recorded, but corresponding source archives, distro patches and redistribution obligations still require separate verified material.",
        "files": sorted(plan.entries.values(), key=lambda entry: entry["target"]),
        "packages": sorted(plan.packages.values(), key=lambda entry: entry["package"]),
        "host_libraries": sorted(plan.host | HOST_LIBRARIES),
        "configuration": {"pipewire_config": str(CLIENT_TARGET), "pipewire_modules": "usr/lib/pipewire-0.3", "spa_plugins": "usr/lib/spa-0.2", "xkb_config_root": "usr/share/X11/xkb"},
        "limitations": ["No GPU loaders, vendor drivers, glibc/loader, fonts, FFmpeg or daemon are bundled.",
                        "X11 locale/Compose data, cursor themes, host display/Portal/PipeWire services and GPU drivers remain host resources.",
                        "Stock PipeWire client modules, DBus support and default-conditional journal logging are included; no daemon is bundled.",
                        "RUNPATH relocation and copied notices are not a legal redistribution guarantee."],
    }
