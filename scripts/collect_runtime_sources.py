#!/usr/bin/env python3
"""Collect exact APK sources for linked libraries and actually used runtime headers.

APKBUILD is retained as data, never sourced/evaluated. Downloads come from its
recorded aports commit or Alpine's release distfiles mirror, and every source
input must match its APKBUILD SHA512. Reuse caches are rehashed against fresh
pinned recipes and share the download byte budget. This is not a redistribution approval or
an attempt to recreate the complete Alpine compiler/SDK build environment.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

MAX_METADATA = 16 * 1024 * 1024
MAX_RECIPE = 512 * 1024
MAX_FILE = 128 * 1024 * 1024
MAX_TOTAL = 256 * 1024 * 1024
MAX_ORIGINS = 16
MAX_SOURCE_FILES = 512
APORTS = "https://raw.githubusercontent.com/alpinelinux/aports"
DISTFILES = "https://distfiles.alpinelinux.org/distfiles"
SAFE_NAME = re.compile(r"[A-Za-z0-9_+.-]+")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        # Do not contact a redirect target before checking it. These two fixed
        # official source endpoints need no cross-host redirect to work.
        raise ValueError("Source redirects are not permitted: " + new_url)


def open_source(request, timeout):
    return urllib.request.build_opener(NoRedirect()).open(request, timeout=timeout)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read_bounded(path, maximum=MAX_METADATA):
    if path.is_symlink() or not path.is_file():
        raise ValueError("Expected a regular input file: " + str(path))
    with path.open("rb") as source:
        content = source.read(maximum + 1)
    if len(content) > maximum:
        raise ValueError("Input exceeded its byte limit: " + str(path))
    return content


def safe_name(name):
    if not isinstance(name, str) or not SAFE_NAME.fullmatch(name) or name in (".", ".."):
        raise ValueError("Unsafe source/package filename: " + repr(name))
    return name


def parse_apk_installed(content):
    packages = {}
    for paragraph in content.split("\n\n"):
        fields = {}
        for line in paragraph.splitlines():
            if len(line) >= 2 and line[1] == ":" and line[0] in "PVALoc":
                if line[0] in fields:
                    raise ValueError("Duplicate APK identity field")
                fields[line[0]] = line[2:]
        if not fields:
            continue
        if any(not fields.get(key) for key in "PVALoc"):
            raise ValueError("APK identity is missing name/version/origin/commit/license")
        if not re.fullmatch(r"[0-9a-f]{40}", fields["c"]):
            raise ValueError("APK has no exact aports commit")
        for key in "PVo":
            safe_name(fields[key])
        package = {
            "package": fields["P"], "version": fields["V"], "architecture": fields["A"],
            "license_expression": fields["L"], "origin": fields["o"], "aports_commit": fields["c"],
        }
        identity = package["package"] + "-" + package["version"]
        if identity in packages:
            raise ValueError("Duplicate installed APK identity")
        packages[identity] = package
    return packages


def source_plan(apk_text, owners_text, links_text):
    packages = parse_apk_installed(apk_text)
    links = set(filter(None, links_text.splitlines()))
    if not links or len(links) > 256 or any(not line.startswith(("/usr/", "/lib/")) for line in links):
        raise ValueError("Invalid or oversized link-input path inventory")
    owners = {}
    for line in owners_text.splitlines():
        if not line.startswith("/"):
            continue
        parts = line.split(" | ")
        if len(parts) != 3 or parts[0] in owners:
            raise ValueError("Invalid/duplicate package ownership record")
        original, resolved, description = parts
        match = re.fullmatch(re.escape(original) + r" is owned by (\S+)", description)
        if match:
            if match[1] not in packages:
                raise ValueError("Link owner absent from installed APK database: " + match[1])
            owners[original] = (resolved, packages[match[1]])
        elif description.startswith("ERROR: ") and "Could not find owner package" in description:
            owners[original] = (resolved, None)
        else:
            raise ValueError("Unrecognized ownership evidence: " + description)
    if not links.issubset(owners):
        raise ValueError("Link input has no recorded ownership result")
    origins = {}
    unowned = []
    headers = {}
    for original, (resolved, package) in owners.items():
        if original not in links:
            if package:
                key = (package["origin"], package["aports_commit"])
                item = headers.setdefault(key, {"origin": key[0], "aports_commit": key[1],
                                               "role": "header-only", "packages": [], "header_inputs": []})
                if package not in item["packages"]:
                    item["packages"].append(package)
                item["header_inputs"].append({"path": original, "resolved_path": resolved})
            continue
        if package is None:
            unowned.append({"path": original, "resolved_path": resolved})
            continue
        key = (package["origin"], package["aports_commit"])
        item = origins.setdefault(key, {"origin": key[0], "aports_commit": key[1],
                                       "role": "linked-library", "packages": [], "linked_inputs": []})
        if package not in item["packages"]:
            item["packages"].append(package)
        item["linked_inputs"].append({"path": original, "resolved_path": resolved})
    if len(set(origins) | set(headers)) > MAX_ORIGINS:
        raise ValueError("Too many source origins for this bounded runtime collector")
    for item in origins.values():
        item["packages"].sort(key=lambda record: record["package"])
        item["linked_inputs"].sort(key=lambda record: record["path"])
    header_origins = [headers[key] for key in sorted(headers) if key not in origins]
    for item in header_origins:
        item["packages"].sort(key=lambda record: record["package"])
        item["header_inputs"].sort(key=lambda record: record["path"])
    return {"origins": [origins[key] for key in sorted(origins)],
            "non_apk_link_inputs": sorted(unowned, key=lambda record: record["path"]),
            "header_origins": header_origins,
            "header_only_packages": [package for item in header_origins for package in item["packages"]]}


def parse_recipe(content, versions):
    # Read literal release declarations/checksums only; shell expansion and
    # function bodies are never evaluated, even for a pinned APKBUILD.
    version = re.search(r"(?m)^pkgver=(['\"]?)([A-Za-z0-9_.]+)\1[ \t]*(?:#.*)?$", content)
    release = re.search(r"(?m)^pkgrel=([0-9]+)[ \t]*(?:#.*)?$", content)
    if not version or not release or set(versions) != {version[2] + "-r" + release[1]}:
        raise ValueError("APKBUILD release does not match the installed link owner")
    blocks = re.findall(r"(?ms)^sha512sums=([\"'])(.*?)\1\s*$", content)
    if len(blocks) != 1:
        raise ValueError("Expected one literal SHA512 source inventory")
    sources = {}
    for line in blocks[0][1].splitlines():
        if not line.strip():
            continue
        fields = line.split()
        if len(fields) != 2 or not re.fullmatch(r"[0-9a-f]{128}", fields[0]):
            raise ValueError("Unverifiable APKBUILD source checksum")
        name = safe_name(fields[1])
        if name in sources:
            raise ValueError("Duplicate source checksum name")
        sources[name] = fields[0]
    if not sources or len(sources) > MAX_SOURCE_FILES:
        raise ValueError("Empty or oversized source inventory")
    return sources


class Fetcher:
    def __init__(self, timeout=600, maximum=MAX_TOTAL, max_file_bytes=MAX_FILE):
        if type(max_file_bytes) is not int or not 0 < max_file_bytes <= MAX_TOTAL:
            raise ValueError(f"max-file-bytes must be a positive integer no greater than {MAX_TOTAL}")
        self.deadline = time.monotonic() + timeout
        self.maximum = maximum
        self.max_file_bytes = max_file_bytes
        self.received = 0
        self.accounted = 0
        self.reused_bytes = 0

    def check_size(self, size, maximum):
        if size > maximum:
            raise ValueError(f"Source size {size} exceeds per-file limit {maximum} bytes")
        remaining = self.maximum - self.accounted
        if size > remaining:
            raise ValueError(f"Source size {size} exceeds remaining total byte budget {remaining} of {self.maximum}")

    def fetch(self, url, target, expected_sha512=None, maximum=None):
        maximum = self.max_file_bytes if maximum is None else min(maximum, self.max_file_bytes)
        parsed = urllib.parse.urlsplit(url)
        allowed = {"raw.githubusercontent.com", "distfiles.alpinelinux.org"}
        if parsed.scheme != "https" or parsed.hostname not in allowed or parsed.username or parsed.password:
            raise ValueError("Source URL is not an allowed HTTPS source")
        if time.monotonic() >= self.deadline:
            raise TimeoutError("Source collection deadline exceeded")
        request = urllib.request.Request(url, headers={"User-Agent": "gifromscreen-source-collector/1"})
        try:
            response = open_source(request, timeout=min(20, self.deadline - time.monotonic()))
        except urllib.error.HTTPError as error:
            if error.code == 404:
                error.close()
                return None
            raise
        temporary = None
        try:
            with response:
                final = urllib.parse.urlsplit(response.geturl())
                if final.scheme != "https" or final.hostname not in allowed:
                    raise ValueError("Source redirect escaped the allowed HTTPS hosts")
                length = response.headers.get("Content-Length")
                if length:
                    self.check_size(int(length), maximum)
                fd, temporary = tempfile.mkstemp(prefix=".source-part-", dir=target.parent)
                size = 0
                sha256 = hashlib.sha256()
                sha512 = hashlib.sha512()
                with os.fdopen(fd, "wb") as output:
                    while True:
                        if time.monotonic() >= self.deadline:
                            raise TimeoutError("Source collection deadline exceeded")
                        block = response.read(min(128 * 1024, maximum - size + 1,
                                                  self.maximum - self.accounted + 1))
                        if not block:
                            break
                        size += len(block)
                        self.received += len(block)
                        self.accounted += len(block)
                        if size > maximum or self.accounted > self.maximum:
                            raise ValueError("Source download exceeds its byte budget")
                        sha256.update(block)
                        sha512.update(block)
                        output.write(block)
                if expected_sha512 is not None and sha512.hexdigest() != expected_sha512:
                    raise ValueError("SHA512 mismatch for " + target.name)
                if target.exists() or target.is_symlink():
                    raise ValueError("Source destination already exists")
                os.link(temporary, target)
                return {"file": target.name, "url": url, "resolved_url": response.geturl(),
                        "bytes": size, "sha256": sha256.hexdigest(), "sha512": sha512.hexdigest(),
                        "declared_sha512": expected_sha512}
        finally:
            if temporary is not None:
                Path(temporary).unlink()  # Only this invocation's exact temporary file.


class ReuseKit:
    """Cache index only: fresh pinned APKBUILD checksums remain authoritative."""

    def __init__(self, directory):
        self.directory = directory.absolute()
        self._directories(self.directory)
        encoded = read_bounded(self.directory / "SOURCE-INVENTORY.json")
        document = json.loads(encoded)
        if not isinstance(document, dict) or type(document.get("format_version")) is not int or document["format_version"] != 1:
            raise ValueError("Unsupported reuse source inventory")
        self.inventory_sha256 = digest(encoded)
        self.origins = {}
        records = document.get("origins", []) + document.get("header_origins", [])
        if len(records) > MAX_ORIGINS:
            raise ValueError("Reuse inventory has too many origins")
        for record in records:
            key = (safe_name(record["origin"]), record["aports_commit"])
            if not re.fullmatch(r"[0-9a-f]{40}", key[1]) or key in self.origins:
                raise ValueError("Invalid/duplicate reuse origin commit")
            files = record["files"]
            if not isinstance(files, list) or len(files) > MAX_SOURCE_FILES + 1:
                raise ValueError("Invalid reuse file inventory")
            indexed = {}
            for item in files:
                name = safe_name(item["file"])
                if name in indexed:
                    raise ValueError("Duplicate reuse filename")
                indexed[name] = item
            self.origins[key] = (record["packages"], indexed)

    @staticmethod
    def _directories(directory):
        for item in (*reversed(directory.parents), directory):
            if not stat.S_ISDIR(item.lstat().st_mode):
                raise ValueError("Reuse kit directory must not be a symlink")

    def copy(self, origin, name, checksum, urls, target, fetcher):
        cached = self.origins.get((origin["origin"], origin["aports_commit"]))
        if cached is None:
            return None
        packages, files = cached
        if sorted(packages, key=lambda item: item["package"]) != origin["packages"]:
            raise ValueError("Reuse package identity differs from current SDK ownership")
        record = files.get(name)
        if record is None:
            return None
        if (record.get("declared_sha512") != checksum or record.get("sha512") != checksum
                or not re.fullmatch(r"[0-9a-f]{64}", record.get("sha256", ""))
                or type(record.get("bytes")) is not int or record["bytes"] < 0
                or record.get("url") not in urls or record.get("resolved_url") != record["url"]):
            raise ValueError("Reuse metadata differs from fresh pinned APKBUILD: " + name)
        fetcher.check_size(record["bytes"], fetcher.max_file_bytes)
        directory = self.directory / (origin["origin"] + "-" + origin["aports_commit"])
        self._directories(directory)
        source_path = directory / name
        descriptor = os.open(source_path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        temporary = None
        try:
            with os.fdopen(descriptor, "rb") as source:
                info = os.fstat(source.fileno())
                if not stat.S_ISREG(info.st_mode) or info.st_size != record["bytes"]:
                    raise ValueError("Reuse source is not the expected regular file: " + name)
                fd, temporary = tempfile.mkstemp(prefix=".reuse-part-", dir=target.parent)
                h256, h512, size = hashlib.sha256(), hashlib.sha512(), 0
                with os.fdopen(fd, "wb") as destination:
                    while True:
                        if time.monotonic() >= fetcher.deadline:
                            raise TimeoutError("Source collection deadline exceeded during reuse")
                        block = source.read(min(128 * 1024, record["bytes"] + 1 - size))
                        if not block:
                            break
                        fetcher.check_size(len(block), fetcher.max_file_bytes)
                        size += len(block)
                        fetcher.accounted += len(block)
                        fetcher.reused_bytes += len(block)
                        if size > record["bytes"]:
                            raise ValueError("Reuse source grew during verification: " + name)
                        h256.update(block)
                        h512.update(block)
                        destination.write(block)
                if size != record["bytes"] or h256.hexdigest() != record["sha256"] or h512.hexdigest() != checksum:
                    raise ValueError("Reuse source hash mismatch: " + name)
            if target.exists() or target.is_symlink():
                raise ValueError("Reuse destination already exists")
            os.link(temporary, target)
            return dict(record, reused=True)
        finally:
            if temporary is not None:
                Path(temporary).unlink()


def collect_origin(origin, output, release, fetcher, reuse=None):
    destination = output / (origin["origin"] + "-" + origin["aports_commit"])
    destination.mkdir()
    result = dict(origin, files=[], pending=[], source_inputs_collected=False)
    try:
        base = None
        for repository in ("main", "community", "testing"):
            candidate = APORTS + "/" + origin["aports_commit"] + "/" + repository + "/" + origin["origin"]
            recipe = fetcher.fetch(candidate + "/APKBUILD", destination / "APKBUILD", maximum=MAX_RECIPE)
            if recipe is not None:
                base = candidate
                result["aports_directory"] = repository + "/" + origin["origin"]
                result["files"].append(recipe)
                break
        if base is None:
            raise ValueError("Exact origin/commit APKBUILD was not found")
        sources = parse_recipe(read_bounded(destination / "APKBUILD", MAX_RECIPE).decode("utf-8"),
                               [package["version"] for package in origin["packages"]])
        for name, checksum in sources.items():
            try:
                urls = (base + "/" + name, DISTFILES + "/v" + release + "/" + name)
                record = reuse.copy(origin, name, checksum, urls, destination / name, fetcher) if reuse else None
                if record is None:
                    record = fetcher.fetch(urls[0], destination / name, checksum)
                if record is None:
                    record = fetcher.fetch(urls[1], destination / name, checksum)
                if record is None:
                    raise ValueError("Missing from exact aports commit and release distfiles mirror")
                result["files"].append(record)
            except (OSError, ValueError, urllib.error.URLError) as error:
                result["pending"].append({"file": name, "sha512": checksum, "reason": str(error)})
        result["source_inputs_collected"] = not result["pending"]
        result["license_material"] = "Original license files remain in checksum-verified source archives; no legal completeness claim."
    except (OSError, ValueError, UnicodeError, urllib.error.URLError) as error:
        result["pending"].append({"file": "APKBUILD", "reason": str(error)})
    print(origin["origin"] + ": " + str(len(result["files"])) + " collected, "
          + str(len(result["pending"])) + " pending", file=sys.stderr, flush=True)
    return result


def collect_sdk_sources(relink, output):
    source = relink / "sdk/sources"
    destination = output / "non-apk-sdk-sources"
    destination.mkdir()
    files = []
    checksums = read_bounded(source / "SHA256SUMS", MAX_RECIPE).decode("ascii")
    for line in checksums.splitlines():
        checksum, name = line.split("  ", 1)
        safe_name(name)
        if not re.fullmatch(r"[0-9a-f]{64}", checksum):
            raise ValueError("Invalid SDK source checksum")
        content = read_bounded(source / name)
        if digest(content) != checksum:
            raise ValueError("Existing SDK source checksum mismatch: " + name)
        with (destination / name).open("xb") as target:
            target.write(content)
        files.append({"file": name, "bytes": len(content), "sha256": checksum})
    for name in ("SHA256SUMS", "SOURCES.txt"):
        content = read_bounded(source / name, MAX_RECIPE)
        with (destination / name).open("xb") as target:
            target.write(content)
    return files


def collect(relink, output, fetcher=None, reuse_kit=None, max_file_bytes=MAX_FILE):
    downloader = fetcher or Fetcher(max_file_bytes=max_file_bytes)
    relink = relink.resolve(strict=True)
    names = {"apk_installed": "sdk/metadata/apk-installed", "owners": "package-owners.txt",
             "link_inputs": "link-input-paths.txt", "alpine_release": "sdk/metadata/alpine-release"}
    inputs = {key: read_bounded(relink / relative) for key, relative in names.items()}
    plan = source_plan(inputs["apk_installed"].decode(), inputs["owners"].decode(),
                       inputs["link_inputs"].decode())
    match = re.fullmatch(rb"(\d+\.\d+)\.\d+\s*", inputs["alpine_release"])
    if not match:
        raise ValueError("Unknown exact SDK Alpine release")
    reuse = ReuseKit(reuse_kit) if reuse_kit is not None else None
    output = output.absolute()
    for parent in (*reversed(output.parents), output):
        if parent.is_symlink():
            raise ValueError("Source output ancestry must not contain symlinks")
    output.mkdir(mode=0o700, exist_ok=False)
    (output / "inputs").mkdir()
    for key, content in inputs.items():
        (output / "inputs" / key).write_bytes(content)
    sdk = collect_sdk_sources(relink, output)
    sdk_bytes = sum(record["bytes"] for record in sdk)
    downloader.check_size(sdk_bytes, MAX_TOTAL)
    downloader.accounted += sdk_bytes
    origins = [collect_origin(origin, output, match[1].decode(), downloader, reuse) for origin in plan["origins"]]
    headers = [collect_origin(origin, output, match[1].decode(), downloader, reuse) for origin in plan["header_origins"]]
    linked_complete = all(item["source_inputs_collected"] for item in origins)
    headers_complete = all(item["source_inputs_collected"] for item in headers)
    manifest = {"format_version": 1, "redistribution_ready": False,
                "offline_rebuild_verified": False,
                "linked_apk_source_inputs_collected": linked_complete,
                "header_apk_source_inputs_collected": headers_complete,
                "all_recorded_apk_source_inputs_collected": linked_complete and headers_complete,
                "input_sha256": {key: digest(content) for key, content in inputs.items()},
                "origins": origins, "header_origins": headers, "non_apk_sdk_sources": sdk,
                "non_apk_link_inputs": plan["non_apk_link_inputs"],
                "header_only_packages_pending": [package for item in headers if not item["source_inputs_collected"] for package in item["packages"]],
                "pending_scope": ["Only the actual linked-library and header origins are collected, not all SDK packages.",
                                  "Original sources/patches are retained; a complete offline APK rebuild and relink has not been verified.",
                                  "Standalone license extraction, full build-tool dependency sources and final redistribution review remain separate."],
                "network_bytes": downloader.received, "reused_source_bytes": downloader.reused_bytes,
                "accounted_source_bytes": downloader.accounted,
                "limits": {"max_file_bytes": downloader.max_file_bytes, "max_total_bytes": MAX_TOTAL, "timeout_seconds": 600},
                "reuse_inventory_sha256": reuse.inventory_sha256 if reuse else None}
    (output / "SOURCE-INVENTORY.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(output), "origins": len(origins), "header_origins": len(headers),
                      "linked_apk_source_inputs_collected": manifest["linked_apk_source_inputs_collected"],
                      "header_apk_source_inputs_collected": headers_complete,
                      "redistribution_ready": False, "network_bytes": downloader.received,
                      "reused_source_bytes": downloader.reused_bytes, "accounted_source_bytes": downloader.accounted}))
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--relink-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, default=Path("target/runtime-source-kit"))
    parser.add_argument("--reuse-kit", type=Path, help="existing kit; source files are rehashed against fresh pinned APKBUILD checksums")
    parser.add_argument("--max-file-bytes", type=int, default=MAX_FILE,
                        help="explicit single-file byte limit; default 128 MiB, at most the fixed 256 MiB total budget")
    arguments = parser.parse_args()
    try:
        result = collect(arguments.relink_dir, arguments.output_dir, reuse_kit=arguments.reuse_kit,
                         max_file_bytes=arguments.max_file_bytes)
        return 0 if result["all_recorded_apk_source_inputs_collected"] else 1
    except (OSError, ValueError, UnicodeError, urllib.error.URLError) as error:
        print("Source collection failed: " + str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
