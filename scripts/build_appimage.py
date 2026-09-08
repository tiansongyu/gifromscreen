#!/usr/bin/env python3
"""Assemble a local development AppImage from verified native portable binaries.

This does not publish an artifact. Native/runtime corresponding-source and
redistribution materials must be completed before enabling distributable builds.
"""

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import subprocess
import tempfile

import build_portable as portable
from appimage_tools import verify_tools
from portable_archive import extract_checked
from owned_process import run as run_owned


def verify_payload(bundle):
    # This format contains no links or special files, including unlisted empty
    # links. Verify before reading the manifest or copytree could follow one.
    for source in [bundle, *bundle.rglob("*")]:
        mode = source.lstat().st_mode
        if not (stat.S_ISREG(mode) or stat.S_ISDIR(mode)):
            raise ValueError("portable payload contains a link or special file: " + str(source))
    listed = set()
    for line in (bundle / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        digest, name = line.split("  ", 1)
        path = PurePosixPath(name)
        if (len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest)
                or path.is_absolute() or ".." in path.parts or str(path) != name
                or name in listed or name == "SHA256SUMS"):
            raise ValueError("invalid portable checksum entry: " + name)
        source = bundle / name
        if not source.is_file() or source.is_symlink() or portable.sha256(source) != digest:
            raise ValueError("portable checksum mismatch: " + name)
        listed.add(name)
    actual = {str(path.relative_to(bundle)) for path in bundle.rglob("*") if path.is_file()}
    if actual != listed | {"SHA256SUMS"}:
        raise ValueError("portable checksum inventory is incomplete")
    receipt = json.loads((bundle / "BUILD-INFO.json").read_text(encoding="utf-8"))
    if (not isinstance(receipt, dict) or type(receipt.get("schema_version")) is not int
            or receipt["schema_version"] != portable.BUILD_RECEIPT_VERSION
            or receipt.get("target") != portable.TARGET):
        raise ValueError("a current target-qualified portable build receipt is required")
    if receipt["source_tree_sha256"] != portable.tree_digest(["Cargo.toml", "Cargo.lock", "apps", "crates"]):
        raise ValueError("portable binaries do not describe the current Rust source tree")
    for name in portable.BINARIES:
        if portable.sha256(bundle / "bin" / name) != receipt["binaries"][name]:
            raise ValueError("portable binary differs from its build receipt: " + name)
    return receipt


def stage_payload(bundle, appdir):
    (appdir / "usr").mkdir(parents=True)
    for directory in ("bin", "share"):
        shutil.copytree(bundle / directory, appdir / "usr" / directory)
    launcher = appdir / "AppRun"
    shutil.copyfile(portable.ROOT / "packaging/appimage/AppRun", launcher)
    launcher.chmod(0o755)
    for name in portable.BINARIES:
        (appdir / "usr/bin" / name).chmod(0o755)
    desktop_name = portable.APP_ID + ".desktop"
    icon_name = portable.APP_ID + ".svg"
    (appdir / desktop_name).symlink_to("usr/share/applications/" + desktop_name)
    (appdir / icon_name).symlink_to("usr/share/icons/hicolor/scalable/apps/" + icon_name)
    (appdir / ".DirIcon").symlink_to(icon_name)


def file_inventory(appdir):
    files = {}
    for path in sorted(appdir.rglob("*")):
        name = str(path.relative_to(appdir))
        if path.is_symlink():
            link = os.readlink(path)
            if os.path.isabs(link):
                raise ValueError("AppDir links must be relocatable: " + name)
            try:
                target = path.resolve(strict=True)
            except (OSError, RuntimeError) as error:
                raise ValueError("AppDir link is broken or cyclic: " + name) from error
            if not target.is_relative_to(appdir.resolve()):
                raise ValueError("AppDir link escapes its payload: " + name)
            files[name] = {"symlink": link}
        elif path.is_file():
            files[name] = {"sha256": portable.sha256(path), "bytes": path.stat().st_size}
        elif not path.is_dir():
            raise ValueError("AppDir contains a special file: " + name)
    return files


def build(arguments):
    if not arguments.development_only:
        raise ValueError("AppImage redistribution materials are not complete; only --development-only local builds are enabled")
    tool, runtime = verify_tools(arguments.tools_dir)
    source_hash = portable.tree_digest(["packaging", "scripts"])
    output = arguments.output_dir.absolute()
    # Reserve a fresh directory; never overwrite an existing user's package.
    output.mkdir(parents=True, exist_ok=False)
    appdir = output / "GifFromScreen.AppDir"
    with tempfile.TemporaryDirectory(prefix="gfs-appimage-input-") as scratch:
        bundle = extract_checked(arguments.archive, Path(scratch))
        receipt = verify_payload(bundle)
        stage_payload(bundle, appdir)
    # Imported only after validating the package. The native helper works on
    # these trusted build outputs and the builder's local package-manager libs.
    from appimage_native import bundle_native
    native = bundle_native(appdir)
    (appdir / "DEVELOPMENT-NOTICE.txt").write_text(
        "LOCAL DEVELOPMENT BUILD — NOT READY FOR REDISTRIBUTION\n\n"
        "This AppImage adds native libraries and an AppImage runtime to the portable payload.\n"
        "The portable payload's original notice about unbundled system libraries applies\n"
        "to its tar.gz format, not to this development AppImage. See BUILD-INFO.json\n"
        "and the native dependency inventory for this bundle's actual contents.\n"
        "Corresponding-source and runtime/static-library distribution materials must\n"
        "be completed before publishing this format. No automatic desktop installation,\n"
        "permissions bypass, updater, or system-service changes are performed.\n",
        encoding="utf-8",
    )
    metadata = {
        "format_version": 1,
        "development_only": True,
        "redistribution_ready": False,
        "portable_archive_sha256": portable.sha256(arguments.archive),
        "portable_build": receipt,
        "packaging_tree_sha256": source_hash,
        "native": native,
        "tools": {path.name: portable.sha256(path) for path in (tool, runtime)},
        "required_host_services": ["graphics loader/driver", "display server", "desktop portals", "PipeWire server for Wayland"],
        "ffmpeg_bundled": False,
    }
    portable.write_json(appdir / "BUILD-INFO.json", metadata)
    portable.write_json(appdir / "PAYLOAD.json", file_inventory(appdir))
    subprocess.run(["desktop-file-validate", str(appdir / (portable.APP_ID + ".desktop"))], check=True)
    epoch = receipt["source_date_epoch"]
    for path in [appdir, *appdir.rglob("*")]:
        os.utime(path, (epoch, epoch), follow_symlinks=False)
    if portable.tree_digest(["packaging", "scripts"]) != source_hash:
        raise ValueError("packaging sources changed during assembly; repeat in a new output directory")
    artifact = output / ("gifromscreen-" + receipt["package_version"] + "-development-x86_64.AppImage")
    # The verified tool owns its extract-and-run lifecycle, so FUSE is not
    # required on the builder. No runtime download is delegated to appimagetool.
    tool.chmod(0o755)
    environment = os.environ.copy()
    environment.pop("NO_CLEANUP", None)
    environment.update(ARCH="x86_64", SOURCE_DATE_EPOCH=str(epoch))
    with tempfile.TemporaryDirectory(prefix="gfs-appimage-tool-") as tooling:
        # Concurrent builds must not share the tool runtime's extracted cache.
        environment["TMPDIR"] = tooling
        run_owned([str(tool), "--appimage-extract-and-run", "--no-appstream",
                   "--runtime-file", str(runtime), str(appdir), str(artifact)],
                  env=environment, check=True, timeout=180)
    artifact.chmod(0o755)
    digest = portable.sha256(artifact)
    (output / (artifact.name + ".sha256")).write_text(digest + "  " + artifact.name + "\n", encoding="ascii")
    print(json.dumps({"appimage": str(artifact), "sha256": digest,
                      "redistribution_ready": False, "appdir": str(appdir)}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--tools-dir", type=Path, default=portable.ROOT / "target/appimage-tools")
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--development-only", action="store_true")
    build(parser.parse_args())


if __name__ == "__main__":
    main()
