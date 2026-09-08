#!/usr/bin/env python3
"""Build and package the native Linux binaries without downloading packagers."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
TOOLCHAIN = "1.88.0"
TARGET = "x86_64-unknown-linux-gnu"
BINARIES = ("gif-from-screen", "gif-from-screen-cli")
APP_ID = "io.github.tiansongyu.gifromscreen"
BUILD_RECEIPT_VERSION = 1
PROFILE = "release"
PATH_REMAP = "/usr/src/gifromscreen"


def run(arguments, **kwargs):
    return subprocess.check_output(arguments, cwd=ROOT, text=True, **kwargs).strip()


def sha256(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def tree_digest(pathspecs):
    paths = run(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z", "--", *pathspecs]).split("\0")
    result = hashlib.sha256()
    for name in sorted(set(filter(None, paths))):
        path = ROOT / name
        result.update(name.encode() + b"\0")
        result.update((sha256(path) if path.is_file() else "missing").encode() + b"\0")
    return result.hexdigest()


def build_binaries(cargo, target_directory, epoch, skip_build):
    # Even an empty encoded value takes precedence over RUSTFLAGS in Cargo.
    # Refuse it rather than recording a path remap which did not take effect.
    if "CARGO_ENCODED_RUSTFLAGS" in os.environ:
        raise ValueError("unset CARGO_ENCODED_RUSTFLAGS before packaging; it overrides the recorded RUSTFLAGS path remap")
    binary_directory = target_directory / TARGET / PROFILE
    receipt_path = binary_directory / "gifromscreen-build.json"
    identity = {
        "schema_version": BUILD_RECEIPT_VERSION,
        "toolchain": TOOLCHAIN,
        "rustc": run(["rustc", "+" + TOOLCHAIN, "--version"]),
        "cargo": run([*cargo, "--version"]),
        "profile": PROFILE,
        "target": TARGET,
        "path_remap": PATH_REMAP,
    }
    source_hash = tree_digest(["Cargo.toml", "Cargo.lock", "apps", "crates"])
    binaries = {name: binary_directory / name for name in BINARIES}
    if skip_build:
        receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
        if not isinstance(receipt, dict):
            raise ValueError("invalid build receipt; omit --skip-build")
        for key, expected in identity.items():
            if type(receipt.get(key)) is not type(expected) or receipt[key] != expected:
                raise ValueError("build receipt " + key + " differs from the requested build; omit --skip-build")
        if receipt["source_tree_sha256"] != source_hash:
            raise ValueError("Rust source changed since the recorded build; omit --skip-build")
        for name, path in binaries.items():
            if sha256(path) != receipt["binaries"][name]:
                raise ValueError("binary differs from its build receipt: " + name)
        return binaries, receipt
    environment = os.environ.copy()
    environment["SOURCE_DATE_EPOCH"] = str(epoch)
    environment["CARGO_TARGET_DIR"] = str(target_directory)
    environment["RUSTFLAGS"] = "--remap-path-prefix=" + str(ROOT) + "=" + PATH_REMAP
    receipt = {
        **identity,
        "source_revision": run(["git", "rev-parse", "HEAD"]),
        "source_dirty": bool(run(["git", "status", "--porcelain", "--", "Cargo.toml", "Cargo.lock", "apps", "crates"])),
        "source_tree_sha256": source_hash,
        "cargo_lock_sha256": sha256(ROOT / "Cargo.lock"),
        "source_date_epoch": epoch,
    }
    # An explicit target also overrides CARGO_BUILD_TARGET / build.target. Read
    # only its target-qualified output; never re-label an old host-path binary.
    subprocess.run([*cargo, "build", "--locked", "--release", "--target", TARGET,
                    "-p", BINARIES[0], "-p", BINARIES[1]], cwd=ROOT, env=environment, check=True)
    if tree_digest(["Cargo.toml", "Cargo.lock", "apps", "crates"]) != source_hash:
        raise ValueError("Rust sources changed while building; repeat against a stable worktree")
    receipt["binaries"] = {name: sha256(path) for name, path in binaries.items()}
    write_json(receipt_path, receipt)
    return binaries, receipt


def dependency_packages(metadata):
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    pending = [package["id"] for package in packages.values() if package["name"] in BINARIES and package["source"] is None]
    visited = set()
    while pending:
        package_id = pending.pop()
        if package_id in visited:
            continue
        visited.add(package_id)
        for dependency in nodes[package_id]["deps"]:
            if any(kind["kind"] != "dev" for kind in dependency["dep_kinds"]):
                pending.append(dependency["pkg"])
    return sorted((packages[value] for value in visited if packages[value]["source"]), key=lambda value: (value["name"], value["version"]))


def license_files(package):
    root = Path(package["manifest_path"]).parent
    selected = set()
    if package["license_file"]:
        selected.add(root / package["license_file"])
    for path in root.rglob("*"):
        if not path.is_file() or path.is_symlink():
            continue
        relative = path.relative_to(root)
        if len(relative.parts) > 4:
            continue
        name = path.name.lower()
        if any(token in name for token in ("license", "licence", "copying", "copyright", "notice")) or name in ("ofl.txt", "ufl.txt", "dep5") or ("fonts" in relative.parts and path.suffix == ".txt"):
            selected.add(path)
    return root, sorted(selected)


def embedded_font_license(destination):
    root = ROOT / "apps/desktop/assets/fonts"
    provenance = json.loads((root / "sources.json").read_text(encoding="utf-8"))
    if type(provenance.get("format_version")) is not int or provenance["format_version"] != 1:
        raise ValueError("unsupported embedded font provenance")
    records = provenance.get("files")
    if (not isinstance(records, list) or len(records) != 2
            or {record.get("file") for record in records} != {"NotoSansCJKsc-Regular.otf", "OFL.txt"}):
        raise ValueError("incomplete embedded font source inventory")
    for record in records:
        source = root / record["file"]
        if (source.is_symlink() or not source.is_file() or type(record.get("bytes")) is not int
                or not 0 < record["bytes"] <= 20_000_000 or source.stat().st_size != record["bytes"]
                or sha256(source) != record.get("sha256")):
            raise ValueError("embedded font/source license checksum mismatch: " + record["file"])
    font = next(record for record in records if record["file"].endswith(".otf"))
    relative = Path("fonts/noto-cjk-2.004")
    target = destination / relative
    target.mkdir(parents=True)
    hashes = {}
    for name in ("OFL.txt", "COPYRIGHT.txt", "sources.json", "README.md"):
        source = root / name
        if source.is_symlink() or not source.is_file() or source.stat().st_size > 64 * 1024:
            raise ValueError("missing or invalid embedded font notice: " + name)
        shutil.copyfile(source, target / name)
        hashes[str(relative / name)] = sha256(target / name)
    return {"name": "Noto Sans CJK SC", "version": "2.004", "license": "OFL-1.1",
            "repository": font["repository"], "authors": ["Adobe"], "source": font["source_url"],
            "source_kind": "embedded-font", "source_commit": font["commit"],
            "source_sha256": font["sha256"], "source_byte_len": font["bytes"], "face_index": font["face_index"],
            "font_modified": font["modified"], "license_files": [str(relative / name) for name in ("OFL.txt", "COPYRIGHT.txt")],
            "provenance_file": str(relative / "sources.json"), "notice_sha256": hashes}


def collect_licenses(metadata, destination):
    shutil.copytree(ROOT / "packaging/licenses", destination)
    sources = json.loads((destination / "upstream/sources.json").read_text(encoding="utf-8"))
    for name, record in sources.items():
        if sha256(destination / "upstream" / name) != record["sha256"]:
            raise ValueError("vendored upstream license checksum mismatch: " + name)
    inventory = []
    for package in dependency_packages(metadata):
        root, selected = license_files(package)
        identifier = package["name"] + "-" + package["version"]
        record = {key: package[key] for key in ("name", "version", "license", "repository", "authors", "source")}
        record["license_files"] = []
        for source in selected:
            relative = source.relative_to(root)
            target = destination / "dependencies" / identifier / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            record["license_files"].append(str(target.relative_to(destination)))
        expression = package["license"] or ""
        if "Apache-2.0" in expression:
            # Choose the Apache alternative when monorepo crate archives omit
            # their shared LICENSE. Preserve other AND obligations (font files).
            record["license_files"].append("LICENSE-APACHE")
            record["license_choice"] = "Apache-2.0 alternative; additional AND obligations retained"
        elif expression == "CC0-1.0":
            record["license_files"].append("upstream/CC0-1.0.txt")
        elif identifier == "cookie-factory-0.3.3":
            record["license_files"].extend(["upstream/cookie-factory-0.3.3-MIT.txt", "upstream/cookie-factory-0.3.3-copyright.txt"])
        if not record["license_files"]:
            raise ValueError("missing distributable license text for " + identifier + ": " + expression)
        inventory.append(record)
    inventory.append(embedded_font_license(destination))
    write_json(destination / "THIRD-PARTY.json", inventory)
    text = "Third-party normal/build dependencies (Cargo.lock-resolved) and embedded font inventory\n\n"
    for record in inventory:
        text += record["name"] + " " + record["version"] + " — " + str(record["license"]) + "\n"
        text += "  " + str(record["repository"] or record["source"]) + "\n"
        text += "  " + ", ".join(record["license_files"]) + "\n"
    (destination / "THIRD-PARTY.txt").write_text(text, encoding="utf-8")
    return len(inventory)


def elf_information(binary, maximum):
    with binary.open("rb") as source:
        if source.read(4) != b"\x7fELF":
            raise ValueError("not an ELF executable: " + str(binary))
    dynamic = run(["readelf", "--dynamic", "--wide", str(binary)])
    versions = run(["readelf", "--version-info", "--wide", str(binary)])
    required = sorted(set(re.findall(r"GLIBC_(\d+\.\d+)", versions)), key=lambda value: tuple(map(int, value.split("."))))
    if required and tuple(map(int, required[-1].split("."))) > tuple(map(int, maximum.split("."))):
        raise ValueError("ELF requires glibc " + required[-1] + ", above requested baseline " + maximum)
    linkage = run(["ldd", str(binary)])
    if "not found" in linkage:
        raise ValueError("runtime dependency is unavailable:\n" + linkage)
    return {"glibc_maximum_required": required[-1] if required else None, "needed_libraries": re.findall(r"Shared library: \[(.*?)\]", dynamic)}


def write_archive(bundle, output, epoch):
    with output.open("xb") as destination:
        with gzip.GzipFile(filename="", mode="wb", compresslevel=9, fileobj=destination, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
                for path in [bundle, *sorted(bundle.rglob("*"))]:
                    information = archive.gettarinfo(str(path), arcname=str(path.relative_to(bundle.parent)))
                    information.uid = information.gid = 0
                    information.uname = information.gname = ""
                    information.mtime = epoch
                    information.mode = 0o755 if path.is_dir() or path.parent.name == "bin" or path.name in ("install.sh", "uninstall.sh", "installer.py") else 0o644
                    if path.is_file():
                        with path.open("rb") as source:
                            archive.addfile(information, source)
                    else:
                        archive.addfile(information)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "target/packages")
    parser.add_argument("--target-dir", type=Path, default=ROOT / "target/portable-build")
    parser.add_argument("--skip-build", action="store_true", help="reuse only binaries matching a previous build receipt and source digest")
    parser.add_argument("--max-glibc", default="2.35")
    arguments = parser.parse_args()
    if sys.platform != "linux" or os.uname().machine != "x86_64":
        raise ValueError("this package target currently requires a native Linux x86_64 builder")
    cargo = ["cargo", "+" + TOOLCHAIN]
    locked_hash = sha256(ROOT / "Cargo.lock")
    metadata = json.loads(run([*cargo, "metadata", "--locked", "--format-version", "1", "--filter-platform", TARGET]))
    if sha256(ROOT / "Cargo.lock") != locked_hash:
        raise ValueError("Cargo.lock changed while reading dependency metadata")
    version = next(package["version"] for package in metadata["packages"] if package["name"] == BINARIES[0])
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH", run(["git", "log", "-1", "--format=%ct"])))
    binaries, receipt = build_binaries(cargo, arguments.target_dir.absolute(), epoch, arguments.skip_build)
    if receipt["cargo_lock_sha256"] != locked_hash:
        raise ValueError("build receipt and dependency inventory refer to different Cargo.lock files")
    name = "gifromscreen-" + version + "-linux-x86_64"
    output_directory = arguments.output_dir.absolute()
    output_directory.mkdir(parents=True, exist_ok=True)
    output = output_directory / (name + ".tar.gz")
    checksum = output.with_name(output.name + ".sha256")
    if output.exists() or checksum.exists():
        raise ValueError("package output already exists; choose another --output-dir")
    package_paths = ["packaging", "scripts", "docs/PACKAGING.md", ".github/workflows/portable.yml"]
    package_source_hash = tree_digest(package_paths)
    with tempfile.TemporaryDirectory(prefix="gifromscreen-package-") as scratch:
        bundle = Path(scratch) / name
        (bundle / "bin").mkdir(parents=True)
        for binary_name, source in binaries.items():
            shutil.copyfile(source, bundle / "bin" / binary_name)
        for filename in ("README.txt", "install.sh", "uninstall.sh", "installer.py"):
            shutil.copyfile(ROOT / "packaging/linux" / filename, bundle / filename)
        for directory, suffix in (("applications", ".desktop"), ("icons/hicolor/scalable/apps", ".svg")):
            destination = bundle / "share" / directory / (APP_ID + suffix)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / "packaging/linux" / (APP_ID + suffix), destination)
        count = collect_licenses(metadata, bundle / "share/licenses/gifromscreen")
        if tree_digest(package_paths) != package_source_hash:
            raise ValueError("packaging sources changed while assembling the archive")
        information = dict(receipt)
        information.update({
            "package_version": version,
            "package_git_dirty": bool(run(["git", "status", "--porcelain"])),
            "packaging_tree_sha256": package_source_hash,
            "elf": {binary_name: elf_information(path, arguments.max_glibc) for binary_name, path in binaries.items()},
            "glibc_baseline_limit": arguments.max_glibc,
            "ffmpeg_bundled": False,
            "dependency_inventory_count": count,
        })
        write_json(bundle / "BUILD-INFO.json", information)
        lines = [sha256(path) + "  " + str(path.relative_to(bundle)) for path in sorted(bundle.rglob("*")) if path.is_file()]
        (bundle / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")
        write_archive(bundle, output, epoch)
    value = sha256(output)
    checksum.write_text(value + "  " + output.name + "\n", encoding="ascii")
    print(json.dumps({"archive": str(output), "sha256": value, "bytes": output.stat().st_size, "dependency_count": count}))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, StopIteration, subprocess.CalledProcessError) as error:
        print("Package build failed: " + str(error), file=sys.stderr)
        sys.exit(1)
