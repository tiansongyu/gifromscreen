#!/usr/bin/env python3
"""Build the cleanup-corrected AppImage runtime from verified local source archives.

The SDK base is digest-pinned. APK identities are retained but their repositories
are not hermetic. No runtime/source artifact is published by this script.
"""

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import tempfile
import uuid

import build_portable as portable
from owned_process import run as run_owned
from portable_archive import extract_checked

RECIPE = portable.ROOT / "packaging/appimage/runtime"
LABEL = "io.github.tiansongyu.gifromscreen.runtime-build"
RECEIPT = "RUNTIME-BUILD.json"


def recipe_digest():
    return portable.tree_digest(["packaging/appimage/runtime", "scripts/build_appimage_runtime.py", "scripts/owned_process.py"])


def source_pins():
    document = json.loads((RECIPE / "sources.json").read_text(encoding="utf-8"))
    if type(document.get("format_version")) is not int or document["format_version"] != 1:
        raise ValueError("unknown runtime source manifest version")
    return document


def verify_sources(directory, pins):
    paths = {}
    for name, record in pins["archives"].items():
        if PurePosixPath(name).name != name or name in ("", ".", ".."):
            raise ValueError("invalid source archive filename")
        source = directory / name
        info = source.lstat()
        if (not stat.S_ISREG(info.st_mode) or info.st_size != record["bytes"]
                or info.st_size > 32 * 1024 * 1024 or portable.sha256(source) != record["sha256"]):
            raise ValueError("runtime source archive does not match its pin: " + name)
        paths[name] = source
    return paths


def inventory(directory):
    if not stat.S_ISDIR(directory.lstat().st_mode):
        raise ValueError("runtime artifacts must be a real directory, not a symlink")
    files = {}
    total = 0
    for path in sorted(directory.rglob("*")):
        mode = path.lstat().st_mode
        if stat.S_ISDIR(mode):
            continue
        if not stat.S_ISREG(mode):
            raise ValueError("runtime build outputs must be regular files: " + str(path))
        total += path.stat().st_size
        if len(files) >= 8192 or total > 512 * 1024 * 1024:
            raise ValueError("runtime build material exceeded its inventory budget")
        files[str(path.relative_to(directory))] = {"sha256": portable.sha256(path), "bytes": path.stat().st_size}
    return files


def query(arguments):
    # Docker may return a valid ID on stdout and an unrelated warning on stderr.
    # Never turn the warning into an invalid identifier and lose cleanup ownership.
    with tempfile.TemporaryFile(mode="w+") as log, tempfile.TemporaryFile(mode="w+") as errors:
        status = run_owned(arguments, stdout=log, stderr=errors, timeout=30)
        log.seek(0)
        errors.seek(0)
        result = log.read(1024 * 1024 + 1)
        diagnostic = errors.read(65536)
        if status:
            raise RuntimeError("Container command failed: " + result[:65536] + diagnostic)
        if len(result) > 1024 * 1024:
            raise ValueError("container metadata exceeded its bound")
        return result.strip()


def logged(arguments, path, timeout, **kwargs):
    with path.open("w") as log:
        run_owned(arguments, stdout=log, stderr=log, check=True, timeout=timeout, **kwargs)


def inspect_owned(container, token):
    if not re.fullmatch(r"[0-9a-f]{64}", container):
        raise ValueError("invalid owned container identity")
    result = json.loads(query(["docker", "inspect", container]))[0]
    if result["Id"] != container or result["Config"]["Labels"].get(LABEL) != token:
        raise ValueError("container ownership receipt changed; refusing lifecycle operation")
    return result


def stop_owned(container, token):
    details = inspect_owned(container, token)
    if details["State"]["Running"]:
        query(["docker", "stop", "--time", "3", container])
    if inspect_owned(container, token)["State"]["Running"]:
        raise RuntimeError("owned runtime build container did not stop")
    query(["docker", "rm", container])


def verify_runtime_build(directory):
    """Return a local built runtime only while recipe and all materials match.

    A local receipt prevents accidental reuse, not a signature authenticating
    attacker-controlled metadata. Keep the build directory private and stable.
    """
    directory = directory.absolute()
    receipt = json.loads((directory / RECEIPT).read_text(encoding="utf-8"))
    if (type(receipt.get("format_version")) is not int or receipt["format_version"] != 1
            or receipt.get("recipe_sha256") != recipe_digest()
            or receipt.get("sources") != source_pins()
            or receipt.get("patch_sha256") != portable.sha256(RECIPE / "cleanup.patch")):
        raise ValueError("runtime build recipe changed; rebuild the runtime from source")
    actual = inventory(directory / "artifacts")
    if actual != receipt["artifacts"]:
        raise ValueError("runtime build artifacts differ from their receipt")
    if inventory(directory / "sources") != receipt["source_materials"]:
        raise ValueError("runtime source materials differ from their receipt")
    runtime = directory / "artifacts/runtime-x86_64"
    with runtime.open("rb") as source:
        header = source.read(64)
    if (len(header) != 64 or header[:7] != b"\x7fELF\x02\x01\x01"
            or header[8:11] != b"AI\x02" or int.from_bytes(header[18:20], "little") != 62):
        raise ValueError("built runtime is not an x86_64 type-2 AppImage ELF")
    if "patched_runtime_cleanup=PASS" not in (directory / "artifacts/cleanup-test.log").read_text(encoding="utf-8"):
        raise ValueError("patched runtime cleanup regression did not pass")
    return runtime, receipt


def build(arguments):
    if os.uname().machine != "x86_64" or os.getuid() == 0:
        raise ValueError("use a non-root Linux x86_64 builder with Docker access")
    pins = source_pins()
    if (RECIPE / "Dockerfile").read_text(encoding="utf-8").splitlines()[0] != "FROM " + pins["base_image"]:
        raise ValueError("runtime SDK base does not match the pinned source manifest")
    sources = verify_sources(arguments.source_dir.absolute(), pins)
    digest = recipe_digest()
    output = arguments.output_dir.absolute()
    if "," in str(output):
        raise ValueError("Docker bind-mount output paths may not contain commas")
    output.mkdir(parents=True, exist_ok=False)
    artifacts = output / "artifacts"
    artifacts.mkdir()
    materials = output / "sources"
    materials.mkdir()
    for name, source in sources.items():
        shutil.copyfile(source, materials / name)
    for source in RECIPE.iterdir():
        if source.is_file():
            shutil.copyfile(source, materials / source.name)
    sources = verify_sources(materials, pins)
    source_materials = inventory(materials)
    with tempfile.TemporaryDirectory(prefix="gfs-runtime-source-") as temporary:
        scratch = Path(temporary)
        unpack = scratch / "unpack"
        unpack.mkdir()
        work = extract_checked(sources["type2-runtime-dd6cebed.tar.gz"], unpack)
        subprocess.run(["git", "apply", "--check", str(RECIPE / "cleanup.patch")], cwd=work, check=True)
        subprocess.run(["git", "apply", str(RECIPE / "cleanup.patch")], cwd=work, check=True)
        patch_hash = portable.sha256(RECIPE / "cleanup.patch")
        version = pins["upstream_commit"] + "-gifromscreen-" + patch_hash[:12]
        (work / "src/runtime/version").write_text(version + "\n", encoding="ascii")
        for name in ("build-runtime.sh", "cleanup-test.c"):
            shutil.copyfile(RECIPE / name, work / name)
        context = scratch / "sdk"
        context.mkdir()
        for name in ("Dockerfile", "install-dependencies.sh"):
            shutil.copyfile(RECIPE / name, context / name)
        for name in ("fuse-3.15.0.tar.xz", "squashfuse-0.5.2.tar.gz"):
            shutil.copyfile(sources[name], context / name)
        shutil.copyfile(work / "patches/libfuse/mount.c.diff", context / "mount.c.diff")
        image_file = scratch / "image.id"
        print("Building digest-pinned runtime SDK; see " + str(output / "sdk-build.log"), flush=True)
        logged(["docker", "build", "--platform", "linux/amd64", "--iidfile", str(image_file), str(context)],
               output / "sdk-build.log", 900)
        image = image_file.read_text(encoding="ascii").strip()
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
            raise ValueError("Docker did not return an immutable SDK image ID")
        token = uuid.uuid4().hex
        command = ["docker", "create", "--platform", "linux/amd64", "--init",
                   "--label", LABEL + "=" + token, "--user", f"{os.getuid()}:{os.getgid()}",
                   "--network", "none", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                   "--read-only", "--pids-limit", "128", "--memory", "2g", "--cpus", "2",
                   "--tmpfs", "/tmp:rw,exec,nosuid,nodev,mode=1777,size=536870912",
                   "--mount", f"type=bind,src={work},dst=/ws,readonly",
                   "--mount", f"type=bind,src={artifacts},dst=/out",
                   "--workdir", "/ws", image, "bash", "/ws/build-runtime.sh"]
        container = query(command)
        try:
            inspect_owned(container, token)
            print("Compiling and testing patched runtime in owned container " + container[:12], flush=True)
            logged(["docker", "start", "--attach", container], output / "runtime-build.log", 300)
            details = inspect_owned(container, token)
            portable.write_json(output / "container-result.json", details)
            if details["State"]["Running"] or details["State"]["ExitCode"] != 0:
                raise RuntimeError("runtime build container failed; inspect runtime-build.log")
        finally:
            stop_owned(container, token)
        if recipe_digest() != digest or inventory(materials) != source_materials:
            raise ValueError("runtime recipe changed while building; repeat with stable sources")
        receipt = {"format_version": 1, "recipe_sha256": digest, "sources": pins,
                   "patch_sha256": patch_hash, "version": version, "sdk_image_id": image,
                   "network_during_runtime_compilation": False, "runtime_compile_uid": os.getuid(),
                   "redistribution_ready": False, "corresponding_source_complete": False,
                   "source_materials": source_materials,
                   "artifacts": inventory(artifacts)}
        portable.write_json(output / RECEIPT, receipt)
    runtime, _ = verify_runtime_build(output)
    print(json.dumps({"runtime": str(runtime), "sha256": portable.sha256(runtime),
                      "receipt": str(output / RECEIPT), "redistribution_ready": False}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, default=portable.ROOT / "target/runtime-sources")
    parser.add_argument("--output-dir", type=Path, required=True)
    build(parser.parse_args())


if __name__ == "__main__":
    main()
