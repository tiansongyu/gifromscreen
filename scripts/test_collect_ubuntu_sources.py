"""Source-collection contracts with deterministic responses; never use a network."""

import copy
from contextlib import redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import Mock, patch
from urllib.parse import parse_qs, urlsplit
from urllib.request import Request

import collect_ubuntu_sources as collector


API_URL = "https://api.launchpad.net/devel/ubuntu/+archive/primary"
SOURCE_URL = "https://launchpad.net/ubuntu/+archive/primary/+sourcefiles/pipewire/0.3.48-1ubuntu3.2/pipewire_0.3.48.orig.tar.gz"


def package(name, source, version):
    return {"package": name, "version": version, "source_package": source,
            "source_version": version, "license_files": [],
            "source_reference": "https://launchpad.net/ubuntu/+source/" + source + "/" + version,
            "corresponding_source_collected": False}


def payload(target, role, owner=None):
    record = {"target": target, "role": role, "package": owner["package"] if owner else None,
              "source": "/usr/lib/synthetic-fixture", "source_realpath": "/usr/lib/synthetic-fixture",
              "source_byte_len": 4, "byte_len": 4, "source_sha256": "a" * 64,
              "patched_sha256": "b" * 64}
    if owner:
        record.update({key: owner[key] for key in ("version", "source_package", "source_version")})
    return record


def metadata():
    library = package("libpipewire-0.3-0:amd64", "pipewire", "0.3.48-1ubuntu3.2")
    config = package("pipewire-bin", "pipewire", "0.3.48-1ubuntu3.2")
    xkb = package("xkb-data", "xkeyboard-config", "2.33-1")
    common = package("base-files", "base-files", "12ubuntu4.7")
    return {"format_version": 1, "development_only": True, "redistribution_ready": False,
            "native": {"schema_version": 1, "packages": [library, config, xkb, common],
                       "files": [payload("usr/lib/libpipewire-0.3.so.0", "system-elf", library),
                                 payload("usr/share/gifromscreen/pipewire/client.conf", "pipewire-config", config),
                                 payload("usr/share/X11/xkb/symbols/us", "xkb-data", xkb),
                                 payload("usr/share/licenses/native/pipewire/copyright", "copyright", library),
                                 payload("usr/share/licenses/native/common/GPL-3", "common-license", common),
                                 payload("usr/bin/gif-from-screen", "project-elf")]}}


class Response(io.BytesIO):
    def __init__(self, data, url=SOURCE_URL, headers=None):
        super().__init__(data)
        self.headers = headers or {}
        self.url = url
        self.read_sizes = []

    def geturl(self):
        return self.url

    def read(self, size=-1):
        if size < 0:
            raise AssertionError("network body must be read in bounded chunks")
        self.read_sizes.append(size)
        return super().read(size)


class IsolatedTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix="gfs-source-collector-test-")
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        guard = patch.object(collector, "open_source", side_effect=AssertionError("no network in unit tests"))
        guard.start()
        self.addCleanup(guard.stop)


class PlanTests(IsolatedTests):
    def test_nonregular_metadata_is_rejected_without_waiting_for_a_fifo_writer(self):
        fifo = self.root / "fifo"
        os.mkfifo(fifo)
        with self.assertRaises(ValueError):
            collector.read_bounded(fifo)
        target = self.root / "original.json"
        target.write_bytes(b"{}")
        link = self.root / "link"
        link.symlink_to(target)
        with self.assertRaises(OSError):
            collector.read_bounded(link)

    def test_existing_output_preserves_all_existing_files_and_never_fetches(self):
        input_path = self.root / "input.json"
        input_path.write_text(json.dumps(metadata()))
        output = self.root / "existing"
        output.mkdir()
        sentinel = output / "keep"
        sentinel.write_bytes(b"existing user data")
        with self.assertRaises(FileExistsError):
            collector.collect(input_path, output)
        self.assertEqual(sentinel.read_bytes(), b"existing user data")
        self.assertEqual(list(output.iterdir()), [sentinel])

    def test_payload_sources_group_exact_versions_and_exclude_license_only_owner(self):
        document = metadata()
        before = copy.deepcopy(document)
        plan = collector.source_plan(document)
        sources = {(source["source_package"], source["source_version"]): source for source in plan["sources"]}
        self.assertEqual(set(sources), {("pipewire", "0.3.48-1ubuntu3.2"), ("xkeyboard-config", "2.33-1")})
        self.assertEqual(len(sources[("pipewire", "0.3.48-1ubuntu3.2")]["packages"]), 2)
        self.assertEqual(len(sources[("pipewire", "0.3.48-1ubuntu3.2")]["payload_files"]), 2)
        self.assertEqual(len(plan["license_only_packages"]), 1)
        self.assertIn("base-files", str(plan["license_only_packages"]))
        self.assertEqual(document, before)

    def test_source_epoch_is_preserved_not_replaced_with_latest(self):
        document = metadata()
        for owner in document["native"]["packages"]:
            if owner["source_package"] == "pipewire":
                owner["source_version"] = "1:0.3.48-1ubuntu3.2"
        for item in document["native"]["files"]:
            if item.get("source_package") == "pipewire":
                item["source_version"] = "1:0.3.48-1ubuntu3.2"
        plan = collector.source_plan(document)
        source = next(source for source in plan["sources"] if source["source_package"] == "pipewire")
        self.assertEqual(source["source_version"], "1:0.3.48-1ubuntu3.2")

    def test_file_source_and_binary_identity_must_match_package_row(self):
        for key, value in (("source_package", "foreign"), ("source_version", "latest"),
                           ("version", "different"), ("package", "missing-owner")):
            with self.subTest(key=key):
                document = metadata()
                document["native"]["files"][0][key] = value
                with self.assertRaises(ValueError):
                    collector.source_plan(document)

    def test_duplicate_package_or_unknown_payload_role_is_rejected(self):
        document = metadata()
        document["native"]["packages"].append(copy.deepcopy(document["native"]["packages"][0]))
        with self.assertRaises(ValueError):
            collector.source_plan(document)
        document = metadata()
        document["native"]["files"][0]["role"] = "unknown-native-payload"
        with self.assertRaises(ValueError):
            collector.source_plan(document)

    def test_manifest_version_types_are_not_coerced(self):
        for outer in (False, True):
            for value in (True, "1", 0, 2, None):
                with self.subTest(outer=outer, value=value):
                    document = metadata()
                    if outer:
                        document["format_version"] = value
                    else:
                        document["native"]["schema_version"] = value
                    with self.assertRaises(ValueError):
                        collector.source_plan(document)


class UrlTests(IsolatedTests):
    def test_allowed_redirect_closes_body_without_an_unbounded_drain(self):
        handler = collector.SafeRedirect("source")
        handler.parent = Mock()
        request = Request(SOURCE_URL)
        request.timeout = 2
        response = Mock()
        response.read.side_effect = AssertionError("redirect body must never be drained")
        url = "https://launchpadlibrarian.net/12345/pipewire_0.3.48.orig.tar.gz"
        handler.http_error_302(request, response, 302, "Found", {"Location": url})
        response.read.assert_not_called()
        response.close.assert_called_once()
        self.assertEqual(handler.parent.open.call_args.args[0].full_url, url)

    def test_redirect_loops_are_bounded_and_rejected_hops_are_closed(self):
        handler = collector.SafeRedirect("source")
        handler.parent = Mock()
        handler.parent.open.side_effect = lambda request, timeout: request
        request = Request(SOURCE_URL)
        request.timeout = 2
        url = "https://launchpadlibrarian.net/12345/pipewire_0.3.48.orig.tar.gz"
        for _ in range(2):
            request = handler.http_error_302(request, Mock(), 302, "Found", {"Location": url})
            request.timeout = 2
        response = Mock()
        with self.assertRaisesRegex(ValueError, "redirect limit"):
            handler.http_error_302(request, response, 302, "Found", {"Location": url})
        response.close.assert_called_once()
        self.assertEqual(handler.parent.open.call_count, 2)
        response = Mock()
        with self.assertRaises(ValueError):
            handler.http_error_302(request, response, 302, "Found", {"Location": "http://127.0.0.1/private"})
        response.close.assert_called_once()
        self.assertEqual(handler.parent.open.call_count, 2)

    def test_initial_urls_require_https_and_correct_service(self):
        self.assertEqual(collector.validate_url(API_URL, "api"), API_URL)
        self.assertEqual(collector.validate_url(SOURCE_URL, "source"), SOURCE_URL)
        invalid = ("http://launchpad.net/source", "https://user:secret@launchpad.net/source",
                   "https://launchpad.net.evil.example/source", "https://evil.example/source",
                   "file:///etc/passwd", "https://launchpad.net:444/source")
        for url in invalid:
            with self.subTest(url=url), self.assertRaises(ValueError):
                collector.validate_url(url, "source")
        with self.assertRaises(ValueError):
            collector.validate_url(SOURCE_URL, "api")

    def test_redirect_rejects_downgrade_credentials_and_foreign_hosts_before_parent(self):
        request = Request(SOURCE_URL)
        for target in ("http://launchpad.net/source", "https://user@launchpad.net/source",
                       "https://evil.example/source", "https://launchpad.net.evil.example/source"):
            with self.subTest(target=target), \
                    patch("urllib.request.HTTPRedirectHandler.redirect_request", side_effect=AssertionError("unsafe redirect delegated")), \
                    self.assertRaises(ValueError):
                collector.SafeRedirect("source").redirect_request(request, None, 302, "Found", {}, target)


class FetcherTests(IsolatedTests):
    def test_expired_deadline_prevents_network_and_cli_alarm_interrupts_blocking_work(self):
        with self.assertRaises(TimeoutError):
            collector.Fetcher(timeout=-1).fetch(SOURCE_URL, self.root / "late")
        previous = collector.signal.getsignal(collector.signal.SIGALRM)
        with self.assertRaises(TimeoutError), collector.deadline_alarm(0.02):
            time.sleep(0.2)
        self.assertEqual(collector.signal.getsignal(collector.signal.SIGALRM), previous)
        self.assertEqual(collector.signal.getitimer(collector.signal.ITIMER_REAL)[0], 0)

    def test_streamed_download_preserves_bytes_hash_and_resolved_url(self):
        body = b"source-archive-bytes\x00\xff"
        response = Response(body, headers={"Content-Length": str(len(body))})
        target = self.root / "source.tar.gz"
        digest = hashlib.sha256(body).hexdigest()
        with patch.object(collector, "open_source", return_value=response) as opened:
            record = collector.Fetcher().fetch(SOURCE_URL, target, expected_sha256=digest, expected_size=len(body))
        self.assertEqual(target.read_bytes(), body)
        self.assertEqual(record["bytes"], len(body))
        self.assertEqual(record["sha256"], digest)
        self.assertEqual(record["url"], SOURCE_URL)
        self.assertEqual(record["resolved_url"], SOURCE_URL)
        self.assertTrue(response.read_sizes)
        self.assertEqual(opened.call_count, 1)

    def test_hash_and_size_mismatch_cannot_produce_final_file(self):
        body = b"source bytes"
        for expected in ({"expected_sha256": "0" * 64}, {"expected_size": len(body) + 1}):
            with self.subTest(expected=expected):
                target = self.root / ("hash" if "expected_sha256" in expected else "size")
                with patch.object(collector, "open_source", return_value=Response(body)), self.assertRaises(ValueError):
                    collector.Fetcher().fetch(SOURCE_URL, target, **expected)
                self.assertFalse(target.exists())

    def test_response_body_and_declared_size_cannot_exceed_per_file_budget(self):
        for headers in ({}, {"Content-Length": "64"}):
            with self.subTest(headers=headers):
                target = self.root / ("declared" if headers else "streamed")
                with patch.object(collector, "open_source", return_value=Response(b"x" * 64, headers=headers)), \
                        self.assertRaises(ValueError):
                    collector.Fetcher().fetch(SOURCE_URL, target, maximum=8)
                self.assertFalse(target.exists())

    def test_global_budget_cannot_be_bypassed_by_multiple_small_files(self):
        fetcher = collector.Fetcher(maximum=10)
        first, second = self.root / "first", self.root / "second"
        with patch.object(collector, "open_source", side_effect=[Response(b"123456"), Response(b"123456")]):
            fetcher.fetch(SOURCE_URL, first, maximum=8)
            with self.assertRaises(ValueError):
                fetcher.fetch(SOURCE_URL, second, maximum=8)
        self.assertEqual(first.read_bytes(), b"123456")
        self.assertFalse(second.exists())

    def test_existing_target_is_never_overwritten(self):
        target = self.root / "existing"
        target.write_bytes(b"user data")
        with self.assertRaises((ValueError, FileExistsError)):
            collector.Fetcher().fetch(SOURCE_URL, target)
        self.assertEqual(target.read_bytes(), b"user data")

    def test_final_response_url_is_validated_even_if_custom_transport_skips_redirect_handler(self):
        target = self.root / "unsafe"
        with patch.object(collector, "open_source", return_value=Response(b"bytes", url="https://evil.example/source")), \
                self.assertRaises(ValueError):
            collector.Fetcher().fetch(SOURCE_URL, target)
        self.assertFalse(target.exists())


class DescriptorAndCollectionTests(IsolatedTests):
    def setUp(self):
        super().setUp()
        self.origin = {"source_package": "pipewire", "source_version": "0.3.48-1ubuntu3.2"}
        self.body = b"synthetic archive, not extracted or executed"
        self.name = "pipewire_0.3.48.orig.tar.gz"
        self.digest = hashlib.sha256(self.body).hexdigest()
        self.descriptor = ("Format: 3.0 (quilt)\nSource: pipewire\nVersion: 0.3.48-1ubuntu3.2\n"
                           "Checksums-Sha256:\n " + self.digest + " " + str(len(self.body)) + " " + self.name + "\n").encode()
        self.inventory = {self.name: {"sha256": self.digest, "size": len(self.body)}}

    def test_original_dsc_crosschecks_identity_size_hash_without_claiming_signature_verification(self):
        check = collector.check_dsc(self.descriptor, self.origin, self.inventory)
        self.assertEqual(check["sha256_crosscheck"], "matched")
        self.assertEqual(check["entries"], 1)
        self.assertFalse(check["openpgp_signature_verified"])
        unsigned = b"Source: pipewire\nVersion: 0.3.48-1ubuntu3.2\n"
        self.assertEqual(collector.check_dsc(unsigned, self.origin, self.inventory)["sha256_crosscheck"], "not-present")
        signed = (b"-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\n" + self.descriptor
                  + b"-----BEGIN PGP SIGNATURE-----\nsynthetic-signature-not-verified\n-----END PGP SIGNATURE-----\n")
        self.assertFalse(collector.check_dsc(signed, self.origin, self.inventory)["openpgp_signature_verified"])

    def test_dsc_different_source_version_hash_size_duplicate_or_unsafe_name_is_rejected(self):
        changed = (self.descriptor.replace(b"Source: pipewire", b"Source: foreign"),
                   self.descriptor.replace(b"Version: 0.3.48-1ubuntu3.2", b"Version: latest"),
                   self.descriptor.replace(self.digest.encode(), b"0" * 64),
                   self.descriptor.replace((" " + str(len(self.body)) + " ").encode(), b" 999 "),
                   self.descriptor + self.descriptor.splitlines(keepends=True)[-1],
                   self.descriptor.replace(self.name.encode(), b"../escape.tar.gz"))
        for descriptor in changed:
            with self.subTest(descriptor=descriptor), self.assertRaises(ValueError):
                collector.check_dsc(descriptor, self.origin, self.inventory)

    def test_partial_download_retains_original_descriptor_and_false_completion_checkpoint(self):
        document = metadata()
        document["native"]["packages"] = [p for p in document["native"]["packages"] if p["source_package"] != "xkeyboard-config"]
        document["native"]["files"] = [p for p in document["native"]["files"] if p.get("source_package") != "xkeyboard-config"]
        metadata_path = self.root / "BUILD-INFO.json"
        raw = (json.dumps(document) + "\n").encode()
        metadata_path.write_bytes(raw)
        output = self.root / "kit"
        prefix = SOURCE_URL.rsplit("/", 1)[0] + "/"
        descriptor_name = "pipewire_0.3.48-1ubuntu3.2.dsc"
        publication = API_URL + "/+sourcepub/12345"
        requested = []

        def open_fixture(request, _timeout, _kind):
            url = request.full_url
            requested.append(url)
            query = parse_qs(urlsplit(url).query)
            if query.get("ws.op") == ["getPublishedSources"]:
                self.assertEqual(query["version"], [self.origin["source_version"]])
                value = {"entries": [{"source_package_name": "pipewire", "source_package_version": self.origin["source_version"],
                                      "archive_link": API_URL, "distro_series_link": collector.SERIES,
                                      "self_link": publication, "status": "Published", "pocket": "Updates"}]}
                return Response(json.dumps(value).encode(), url=url)
            if query.get("ws.op") == ["sourceFileUrls"]:
                value = [{"url": prefix + descriptor_name, "sha256": hashlib.sha256(self.descriptor).hexdigest(), "size": len(self.descriptor)},
                         {"url": prefix + self.name, "sha256": self.digest, "size": len(self.body)}]
                return Response(json.dumps(value).encode(), url=url)
            if url == prefix + descriptor_name:
                return Response(self.descriptor, url=url)
            if url == prefix + self.name:
                raise OSError("synthetic network failure")
            raise AssertionError("Unplanned mock URL: " + url)

        with patch.object(collector, "open_source", side_effect=open_fixture), redirect_stdout(io.StringIO()):
            index = collector.collect(metadata_path, output)
        self.assertFalse(index["source_inputs_collected"])
        self.assertFalse(index["redistribution_ready"])
        self.assertEqual((output / "BUILD-INFO.json").read_bytes(), raw)
        self.assertEqual(json.loads((output / "index.json").read_text()), index)
        source = index["sources"][0]
        self.assertEqual((output / source["directory"] / "files" / descriptor_name).read_bytes(), self.descriptor)
        self.assertFalse((output / source["directory"] / "files" / self.name).exists())
        self.assertIn("synthetic network failure", str(index["pending"]))
        self.assertEqual(len(requested), 4)
        self.assertFalse(list(output.rglob(".source-part-*")))

    def test_complete_source_set_is_verified_but_never_approves_redistribution(self):
        document = metadata()
        document["native"]["packages"] = [p for p in document["native"]["packages"] if p["source_package"] != "xkeyboard-config"]
        document["native"]["files"] = [p for p in document["native"]["files"] if p.get("source_package") != "xkeyboard-config"]
        input_path = self.root / "input.json"
        input_path.write_text(json.dumps(document))
        output = self.root / "success"
        publication = API_URL + "/+sourcepub/12345"
        prefix = SOURCE_URL.rsplit("/", 1)[0] + "/"
        descriptor_name = "pipewire_0.3.48-1ubuntu3.2.dsc"

        def opened(request, _timeout, _kind):
            url = request.full_url
            operation = parse_qs(urlsplit(url).query).get("ws.op")
            if operation == ["getPublishedSources"]:
                body = json.dumps({"entries": [{"source_package_name": "pipewire", "source_package_version": self.origin["source_version"],
                    "archive_link": API_URL, "distro_series_link": collector.SERIES, "self_link": publication,
                    "status": "Published", "pocket": "Updates"}]}).encode()
            elif operation == ["sourceFileUrls"]:
                body = json.dumps([{"url": prefix + name, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
                    for name, data in [(descriptor_name, self.descriptor), (self.name, self.body)]]).encode()
            elif url == prefix + descriptor_name:
                body = self.descriptor
            elif url == prefix + self.name:
                body = self.body
            else:
                raise AssertionError(url)
            return Response(body, url)

        with patch.object(collector, "open_source", side_effect=opened), redirect_stdout(io.StringIO()):
            index = collector.collect(input_path, output)
        self.assertTrue(index["source_inputs_collected"])
        self.assertFalse(index["redistribution_ready"])
        self.assertFalse(index["native_metadata_updated"])
        self.assertEqual(index["pending"], [])
        self.assertTrue(index["pending_review"])
        for path in output.rglob("*"):
            self.assertEqual(path.stat().st_mode & 0o077, 0, str(path))
        source = index["sources"][0]
        self.assertEqual((output / source["directory"] / "files" / self.name).read_bytes(), self.body)

    def test_wrong_publication_version_is_not_replaced_by_latest(self):
        record = {"entries": [{"source_package_name": "pipewire", "source_package_version": "99.0-latest",
            "archive_link": API_URL, "distro_series_link": collector.SERIES,
            "self_link": API_URL + "/+sourcepub/12345", "status": "Published", "pocket": "Updates"}]}
        with patch.object(collector, "open_source", return_value=Response(json.dumps(record).encode(), API_URL)):
            with self.assertRaisesRegex(ValueError, "exact Ubuntu"):
                collector.publications(self.origin, self.root, collector.Fetcher(), [])

    def test_dsc_subset_is_explicit_about_files_verified_only_by_api(self):
        inventory = dict(self.inventory, signature={"size": 0, "sha256": hashlib.sha256(b"").hexdigest()})
        result = collector.check_dsc(self.descriptor, self.origin, inventory)
        self.assertEqual(result["api_only_files"], ["signature"])


if __name__ == "__main__":
    unittest.main()
