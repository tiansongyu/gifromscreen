#!/usr/bin/env python3
"""Collect exact Ubuntu source inputs recorded in an AppImage BUILD-INFO.

Only native code/resource roles request source archives. Public licence text
provenance is retained separately. No source, recipe, archive or signature is
executed/extracted. Verified downloads are not a redistribution approval.
"""

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import signal
import stat
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

MAX_METADATA = 4 * 1024 * 1024
MAX_DSC = 1024 * 1024
MAX_FILE = 192 * 1024 * 1024
MAX_TOTAL = 512 * 1024 * 1024
MAX_SECONDS = 600
MAX_ORIGINS = 64
MAX_SOURCE_FILES = 128
API = "https://api.launchpad.net/devel/ubuntu/+archive/primary"
SERIES = "https://api.launchpad.net/devel/ubuntu/jammy"
PAYLOAD_ROLES = {"system-elf", "pipewire-config", "xkb-data"}
NOTICE_ROLES = {"common-license", "copyright"}
NAME = re.compile(r"[a-z0-9][a-z0-9+.-]{0,127}")
VERSION = re.compile(r"[0-9A-Za-z][0-9A-Za-z.+:~\-]{0,127}")
FILENAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9+._~\-]{0,239}")
SHA256 = re.compile(r"[0-9a-fA-F]{64}")


def _pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate JSON key: " + key)
        result[key] = value
    return result


def _json(content):
    return json.loads(content.decode("utf-8"), object_pairs_hook=_pairs)


def read_bounded(path, maximum=MAX_METADATA):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
            raise ValueError("Expected a regular metadata file")
        data = source.read(maximum + 1)
    if len(data) > maximum:
        raise ValueError("Metadata exceeds its byte limit")
    return data


def _checked_text(value, pattern, description):
    if not isinstance(value, str) or not pattern.fullmatch(value):
        raise ValueError("Invalid " + description)
    return value


def _relative(value):
    if not isinstance(value, str) or not value or "\\" in value or any(ord(c) < 32 for c in value):
        raise ValueError("Invalid native target path")
    path = PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts or str(path) != value:
        raise ValueError("Native target must be a normalized relative path")
    return value


def source_plan(metadata):
    if not isinstance(metadata, dict):
        raise ValueError("BUILD-INFO must be a JSON object")
    native = metadata.get("native", {})
    if not isinstance(native, dict):
        raise ValueError("Native inventory must be an object")
    if type(metadata.get("format_version")) is not int or metadata["format_version"] != 1:
        raise ValueError("Unsupported BUILD-INFO format version")
    if type(native.get("schema_version")) is not int or native["schema_version"] != 1:
        raise ValueError("Unsupported native schema version")
    package_list, files = native.get("packages"), native.get("files")
    if not isinstance(package_list, list) or not 1 <= len(package_list) <= 128:
        raise ValueError("Invalid native package inventory")
    if not isinstance(files, list) or not 1 <= len(files) <= 4096:
        raise ValueError("Invalid native file inventory")
    packages = {}
    for package in package_list:
        if not isinstance(package, dict):
            raise ValueError("Invalid native package record")
        name = _checked_text(package.get("package"), re.compile(r"[a-z0-9][a-z0-9+.-]*(?::[a-z0-9]+)?"), "binary package")
        if name in packages:
            raise ValueError("Duplicate native package")
        for key, pattern in (("version", VERSION), ("source_package", NAME), ("source_version", VERSION)):
            _checked_text(package.get(key), pattern, key)
        packages[name] = package
    origins, used, targets, notices = {}, set(), set(), {}
    for item in files:
        if not isinstance(item, dict):
            raise ValueError("Invalid native file record")
        role = item.get("role")
        if role not in PAYLOAD_ROLES | NOTICE_ROLES | {"project-elf"}:
            raise ValueError("Unrecognized native file role")
        target = _relative(item.get("target"))
        if target in targets:
            raise ValueError("Duplicate native target")
        targets.add(target)
        if role == "project-elf":
            if item.get("package") is not None:
                raise ValueError("Project ELF cannot be attributed to an Ubuntu package")
            continue
        owner = item.get("package")
        if owner not in packages:
            raise ValueError("Native file owner is absent from packages")
        package = packages[owner]
        for key in ("version", "source_package", "source_version"):
            if item.get(key) != package[key]:
                raise ValueError("Native file source/version disagrees with its package")
        if role in NOTICE_ROLES:
            notices.setdefault(owner, []).append(item)
            continue
        used.add(owner)
        key = (package["source_package"], package["source_version"])
        origin = origins.setdefault(key, {"source_package": key[0], "source_version": key[1], "packages": [], "payload_files": []})
        if package not in origin["packages"]:
            origin["packages"].append(package)
        origin["payload_files"].append(item)
    if not origins or len(origins) > MAX_ORIGINS:
        raise ValueError("Empty or oversized Ubuntu source plan")
    for origin in origins.values():
        origin["packages"].sort(key=lambda package: package["package"])
        origin["payload_files"].sort(key=lambda item: item["target"])
    excluded = []
    for name in sorted(set(packages) - used):
        if name not in notices:
            raise ValueError("Unused package has no licence-material provenance")
        excluded.append(dict(packages[name], classification="license-text-only", source_collection_required=False,
                             notice_files=notices[name]))
    return {"sources": [origins[key] for key in sorted(origins)], "license_only_packages": excluded}


def validate_url(url, kind):
    if not isinstance(url, str) or len(url) > 4096 or "\\" in url or any(ord(c) <= 32 for c in url):
        raise ValueError("Invalid source URL")
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme != "https" or parsed.username or parsed.password or parsed.fragment or parsed.port not in (None, 443):
        raise ValueError("Only credential-free, standard-port HTTPS source URLs are allowed")
    path = urllib.parse.unquote(parsed.path)
    if any(part in (".", "..") for part in path.split("/")) or "\\" in path or "%" in path or any(ord(c) <= 32 for c in path):
        raise ValueError("Source URL contains unsafe path segments")
    if kind == "api":
        if parsed.hostname != "api.launchpad.net" or not re.fullmatch(r"/devel/ubuntu/\+archive/primary(?:/\+sourcepub/[0-9]+)?", path):
            raise ValueError("API URL is outside the official Ubuntu primary archive")
    elif kind == "source":
        valid = (parsed.hostname == "launchpad.net" and path.startswith("/ubuntu/+archive/primary/+sourcefiles/"))
        valid |= parsed.hostname == "launchpadlibrarian.net" and bool(re.fullmatch(r"/[0-9]+/[^/]+", path))
        if not valid or parsed.query:
            raise ValueError("Source URL is outside the official Launchpad source hosts")
    else:
        raise ValueError("Unknown source request kind")
    return url


class SafeRedirect(urllib.request.HTTPRedirectHandler):
    max_repeats = 2
    max_redirections = 4

    def __init__(self, kind):
        self.kind = kind

    def redirect_request(self, request, response, code, message, headers, new_url):
        # Validate BEFORE urllib contacts the next host, not only after reading it.
        validate_url(new_url, self.kind)
        return super().redirect_request(request, response, code, message, headers, new_url)

    def http_error_302(self, request, response, code, message, headers):
        # urllib's default handler drains the redirect body using an unbounded
        # read(). Close it instead: no redirect body is a source input, and a
        # hostile body must not escape the streaming byte/memory budget.
        try:
            location = headers.get("Location") or headers.get("location") or headers.get("URI") or headers.get("uri")
            if not location:
                raise ValueError("Source redirect has no Location")
            new_url = urllib.parse.urljoin(request.full_url, location)
            redirected = self.redirect_request(request, response, code, message, headers, new_url)
            if redirected is None:
                raise ValueError("Source redirect did not produce a safe request")
            visited = dict(getattr(request, "source_redirects", {}))
            if sum(visited.values()) >= self.max_redirections or visited.get(new_url, 0) >= self.max_repeats:
                raise ValueError("Source redirect limit exceeded")
            visited[new_url] = visited.get(new_url, 0) + 1
            redirected.source_redirects = visited
        finally:
            response.close()
        return self.parent.open(redirected, timeout=request.timeout)

    http_error_301 = http_error_303 = http_error_307 = http_error_308 = http_error_302


def open_source(request, timeout, kind):
    try:
        return urllib.request.build_opener(SafeRedirect(kind)).open(request, timeout=timeout)
    except urllib.error.HTTPError as error:
        error.close()
        raise


class Fetcher:
    def __init__(self, timeout=MAX_SECONDS, maximum=MAX_TOTAL):
        self.deadline = time.monotonic() + timeout
        self.maximum = maximum
        self.received = 0

    def remaining_time(self):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("Ubuntu source collection deadline exceeded")
        return remaining

    def fetch(self, url, target, expected_sha256=None, expected_size=None, maximum=MAX_FILE, kind="source"):
        validate_url(url, kind)
        if expected_sha256 is not None and (not isinstance(expected_sha256, str) or not SHA256.fullmatch(expected_sha256)):
            raise ValueError("Invalid declared SHA256")
        if expected_size is not None and (type(expected_size) is not int or not 0 <= expected_size <= maximum):
            raise ValueError("Declared source size exceeds its limit")
        if expected_size is not None and expected_size > self.maximum - self.received:
            raise ValueError("Source collection total byte limit exceeded")
        if target.exists() or target.is_symlink():
            raise ValueError("Source destination already exists")
        request = urllib.request.Request(url, headers={"User-Agent": "gifromscreen-ubuntu-source-collector/1", "Accept-Encoding": "identity"})
        temporary = None
        try:
            with open_source(request, min(20, self.remaining_time()), kind) as response:
                validate_url(response.geturl(), kind)
                length = response.headers.get("Content-Length")
                if length is not None:
                    if not re.fullmatch(r"[0-9]+", length):
                        raise ValueError("Invalid Content-Length")
                    length = int(length)
                    if length > maximum or length > self.maximum - self.received:
                        raise ValueError("Source download exceeds its byte budget")
                    if expected_size is not None and length != expected_size:
                        raise ValueError("Content-Length disagrees with declared source size")
                fd, temporary = tempfile.mkstemp(prefix=".source-part-", dir=target.parent)
                size, checksum = 0, hashlib.sha256()
                with os.fdopen(fd, "wb") as output:
                    while True:
                        self.remaining_time()
                        allowed = min(maximum - size, self.maximum - self.received)
                        if expected_size is not None:
                            allowed = min(allowed, expected_size - size)
                        block = response.read(min(128 * 1024, allowed + 1))
                        if not block:
                            break
                        size += len(block)
                        self.received += len(block)
                        if size > maximum or self.received > self.maximum or (expected_size is not None and size > expected_size):
                            raise ValueError("Source download exceeds its byte budget")
                        checksum.update(block)
                        output.write(block)
                    output.flush()
                    os.fsync(output.fileno())
                if expected_size is not None and size != expected_size:
                    raise ValueError("Source size mismatch")
                if expected_sha256 is not None and checksum.hexdigest() != expected_sha256.lower():
                    raise ValueError("Source SHA256 mismatch")
                os.link(temporary, target)  # Atomic, fails if the target appeared; never overwrites.
                sync_directory(target.parent)
                return {"file": target.name, "url": url, "resolved_url": response.geturl(), "bytes": size,
                        "sha256": checksum.hexdigest(), "declared_size": expected_size, "declared_sha256": expected_sha256}
        finally:
            if temporary is not None:
                Path(temporary).unlink()


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def write_json(path, value):
    data = (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode("utf-8")
    fd, temporary = tempfile.mkstemp(prefix=".metadata-part-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)  # Only this collector's own checkpoint, never source payloads.
        sync_directory(path.parent)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def _api_json(fetcher, url, path, records):
    record = fetcher.fetch(url, path, maximum=MAX_METADATA, kind="api")
    records.append(record)
    return _json(read_bounded(path))


def publications(origin, folder, fetcher, records):
    parameters = {"ws.op": "getPublishedSources", "source_name": origin["source_package"],
                  "version": origin["source_version"], "exact_match": "true", "distro_series": SERIES, "ws.size": "32"}
    url = API + "?" + urllib.parse.urlencode(parameters)
    entries, seen = [], set()
    for page in range(8):
        result = _api_json(fetcher, url, folder / f"publications-{page + 1:02}.json", records)
        if not isinstance(result, dict) or not isinstance(result.get("entries"), list) or len(result["entries"]) > 32:
            raise ValueError("Invalid source publication response")
        for item in result["entries"]:
            if not isinstance(item, dict):
                raise ValueError("Invalid publication entry")
            if (item.get("source_package_name"), item.get("source_package_version"), item.get("archive_link"), item.get("distro_series_link")) != (
                    origin["source_package"], origin["source_version"], API, SERIES):
                raise ValueError("Publication does not match the exact Ubuntu source/version/series")
            validate_url(item.get("self_link"), "api")
            if not re.fullmatch(re.escape(API) + r"/\+sourcepub/[0-9]+", item["self_link"]):
                raise ValueError("Invalid publication identity")
            if item["self_link"] not in seen:
                seen.add(item["self_link"])
                entries.append(item)
        next_url = result.get("next_collection_link")
        if not next_url:
            break
        validate_url(next_url, "api")
        parsed = urllib.parse.urlsplit(next_url)
        query = urllib.parse.parse_qs(parsed.query, strict_parsing=True)
        if any(query.get(key) != [value] for key, value in parameters.items()) or set(query) - set(parameters) != {"ws.start"}:
            raise ValueError("Publication pagination changed the exact source query")
        if not re.fullmatch(r"[0-9]+", query["ws.start"][0]) or next_url == url or not parsed.path == urllib.parse.urlsplit(API).path:
            raise ValueError("Invalid publication pagination")
        url = next_url
    else:
        raise ValueError("Publication pagination exceeded its limit")
    statuses = {"Published": 0, "Superseded": 1, "Deleted": 2, "Obsolete": 3}
    pockets = {"Security": 0, "Updates": 1, "Release": 2, "Backports": 3, "Proposed": 4}
    eligible = [item for item in entries if item.get("status") in statuses and item.get("pocket") in pockets]
    if not eligible:
        raise ValueError("No exact Ubuntu publication exists; latest-version substitution is prohibited")
    return sorted(eligible, key=lambda item: (statuses[item["status"]], pockets[item["pocket"]], item["self_link"]))


def file_inventory(origin, data):
    if not isinstance(data, list) or not 1 <= len(data) <= MAX_SOURCE_FILES:
        raise ValueError("Invalid source file inventory")
    files = {}
    for item in data:
        if not isinstance(item, dict):
            raise ValueError("Invalid source file metadata")
        url = validate_url(item.get("url"), "source")
        parsed = urllib.parse.urlsplit(url)
        parts = urllib.parse.unquote(parsed.path).split("/")
        if parsed.hostname == "launchpad.net" and parts[:-1] != ["", "ubuntu", "+archive", "primary", "+sourcefiles", origin["source_package"], origin["source_version"]]:
            raise ValueError("Source URL names another source or version")
        name = _checked_text(parts[-1], FILENAME, "source filename")
        if name in files:
            raise ValueError("Duplicate source filename")
        sha = _checked_text(item.get("sha256"), SHA256, "source SHA256").lower()
        size = item.get("size")
        if type(size) is not int or not 0 <= size <= MAX_FILE:
            raise ValueError("Declared source file exceeds the 192 MiB limit")
        files[name] = {"url": url, "size": size, "sha256": sha}
    if len([name for name in files if name.endswith(".dsc")]) != 1:
        raise ValueError("Expected exactly one original .dsc descriptor")
    return files


def check_dsc(content, origin, inventory):
    lines = content.decode("utf-8").splitlines()
    if lines and lines[0] == "-----BEGIN PGP SIGNED MESSAGE-----":
        try:
            lines = lines[lines.index("") + 1:lines.index("-----BEGIN PGP SIGNATURE-----")]
        except ValueError as error:
            raise ValueError("Malformed clear-signed .dsc") from error
        lines = [line[2:] if line.startswith("- ") else line for line in lines]
    fields, current = {}, None
    for line in lines:
        if not line:
            continue
        if line[0].isspace():
            if current is None:
                raise ValueError("Unattached .dsc continuation")
            fields[current] += "\n" + line.strip()
            continue
        key, separator, value = line.partition(":")
        key = key.lower()
        if not separator or not re.fullmatch(r"[a-z0-9-]+", key) or key in fields:
            raise ValueError("Malformed or duplicate .dsc field")
        current = key
        fields[key] = value.strip()
    if fields.get("source") != origin["source_package"] or fields.get("version") != origin["source_version"]:
        raise ValueError(".dsc Source/Version does not match the recorded origin")
    declared = fields.get("checksums-sha256")
    if declared is None:
        return {"sha256_crosscheck": "not-present", "entries": 0, "api_only_files": sorted(inventory), "openpgp_signature_verified": False}
    seen = set()
    for line in declared.splitlines():
        if not line.strip():
            continue
        parts = line.split()
        if len(parts) != 3 or not SHA256.fullmatch(parts[0]) or not re.fullmatch(r"[0-9]+", parts[1]):
            raise ValueError("Malformed .dsc SHA256 entry")
        name = _checked_text(parts[2], FILENAME, ".dsc filename")
        if name in seen or name not in inventory or inventory[name]["sha256"] != parts[0].lower() or inventory[name]["size"] != int(parts[1]):
            raise ValueError(".dsc checksum/size inventory disagrees with Launchpad")
        seen.add(name)
    if not seen:
        raise ValueError("Empty .dsc SHA256 inventory")
    return {"sha256_crosscheck": "matched", "entries": len(seen),
            "api_only_files": sorted(set(inventory) - seen - {name for name in inventory if name.endswith(".dsc")}),
            "openpgp_signature_verified": False}


def collect_origin(origin, output, fetcher):
    directory = origin["source_package"] + "-" + hashlib.sha256(origin["source_version"].encode()).hexdigest()[:16]
    folder = output / directory
    folder.mkdir(mode=0o700)
    metadata = folder / "metadata"
    payload = folder / "files"
    metadata.mkdir(mode=0o700)
    payload.mkdir(mode=0o700)
    result = dict(origin, directory=directory, files=[], api_metadata=[], pending=[], source_inputs_collected=False)
    try:
        candidates = publications(origin, metadata, fetcher, result["api_metadata"])
        result["publications"] = candidates
        selected = candidates[0]
        result["selected_publication"] = selected["self_link"]
        url = selected["self_link"] + "?ws.op=sourceFileUrls&include_meta=true"
        data = _api_json(fetcher, url, metadata / "source-files.json", result["api_metadata"])
        inventory = file_inventory(origin, data)
        result["declared_files"] = inventory
        if sum(item["size"] for item in inventory.values()) > fetcher.maximum - fetcher.received:
            raise ValueError("Remaining total budget cannot contain this exact source set")
        descriptor = next(name for name in inventory if name.endswith(".dsc"))
        ordered = [descriptor] + sorted(set(inventory) - {descriptor})
        for name in ordered:
            item = inventory[name]
            try:
                record = fetcher.fetch(item["url"], payload / name, item["sha256"], item["size"], MAX_DSC if name == descriptor else MAX_FILE)
                result["files"].append(record)
                if name == descriptor:
                    result["dsc"] = check_dsc(read_bounded(payload / name, MAX_DSC), origin, inventory)
            except (OSError, ValueError) as error:
                result["pending"].append({"file": name, "error": str(error)})
                if name == descriptor:
                    break  # Never download large inputs for a mismatched descriptor.
        result["source_inputs_collected"] = not result["pending"] and len(result["files"]) == len(inventory)
    except (OSError, ValueError) as error:
        result["pending"].append({"phase": "metadata", "error": str(error)})
    write_json(folder / "index.json", result)
    return result


def collect(metadata_path, output_dir, fetcher=None):
    raw = read_bounded(metadata_path)
    plan = source_plan(_json(raw))
    output = Path(output_dir)
    output.mkdir(mode=0o700)  # Must be new; never reuse or remove an existing kit.
    fd = os.open(output / "BUILD-INFO.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as copy:
        copy.write(raw)
        copy.flush()
        os.fsync(copy.fileno())
    fetcher = fetcher or Fetcher()
    result = {"format_version": 1, "collector": "gifromscreen-ubuntu-sources/1", "metadata_sha256": hashlib.sha256(raw).hexdigest(),
              "archive": API, "series": SERIES, "limits": {"seconds": MAX_SECONDS, "per_file_bytes": MAX_FILE, "total_bytes": MAX_TOTAL},
              "license_only_packages": plan["license_only_packages"],
              "sources": [dict(origin, files=[], source_inputs_collected=False,
                               pending=[{"phase": "not-started", "error": "Source inputs have not been collected"}])
                          for origin in plan["sources"]], "source_inputs_collected": False,
              "native_metadata_updated": False,
              "redistribution_ready": False, "pending_review": ["Ubuntu .deb byte provenance has not been compared.",
                  ".dsc OpenPGP signatures and redistribution obligations require separate review.",
                  "Runtime APK inputs and Rust/project source are outside this Ubuntu collector."]}
    def checkpoint():
        result["network_received_bytes"] = fetcher.received
        result["pending"] = [{"source_package": item["source_package"], "source_version": item["source_version"], **issue}
                             for item in result["sources"] for issue in item["pending"]]
        write_json(output / "index.json", result)

    checkpoint()
    for index, origin in enumerate(plan["sources"], 1):
        print(f"SOURCE {index}/{len(plan['sources'])} {origin['source_package']} {origin['source_version']}", flush=True)
        try:
            result["sources"][index - 1] = collect_origin(origin, output, fetcher)
        except (OSError, ValueError) as error:
            result["sources"][index - 1]["pending"] = [{"phase": "collection", "error": str(error)}]
        checkpoint()
    result["source_inputs_collected"] = all(item["source_inputs_collected"] for item in result["sources"])
    checkpoint()
    return result


@contextmanager
def deadline_alarm(seconds):
    # CLI runs on the main Linux thread: interrupt a blocked socket read at the
    # overall deadline as well as checking the monotonic budget between chunks.
    def expired(_signal, _frame):
        raise TimeoutError("Ubuntu source collection hard deadline exceeded")
    previous = signal.signal(signal.SIGALRM, expired)
    timer = signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, *timer)
        signal.signal(signal.SIGALRM, previous)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--metadata", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    try:
        with deadline_alarm(MAX_SECONDS):
            result = collect(args.metadata, args.output_dir)
        print(json.dumps({"source_inputs_collected": result["source_inputs_collected"], "sources": len(result["sources"]),
                          "pending": len(result["pending"]), "redistribution_ready": False}), flush=True)
        return 0 if result["source_inputs_collected"] else 2
    except (OSError, ValueError) as error:
        print("Ubuntu source collection failed: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
