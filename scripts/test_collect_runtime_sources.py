"""Bounded source-collector parser/transport fixtures; never run APKBUILD."""

import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import collect_runtime_sources as collector


COMMIT = "a" * 40


def package(name="musl-dev", origin="musl", version="1.2.5-r11"):
    return "\n".join(("P:" + name, "V:" + version, "A:x86_64", "L:MIT", "o:" + origin, "c:" + COMMIT))


class Response(io.BytesIO):
    def __init__(self, data, url):
        super().__init__(data)
        self.url = url
        self.headers = {"Content-Length": str(len(data))}

    def geturl(self):
        return self.url


class SourceCollectorTests(unittest.TestCase):
    def test_apk_identity_includes_origin_commit_and_rejects_missing_or_duplicate_fields(self):
        record = collector.parse_apk_installed(package())["musl-dev-1.2.5-r11"]
        self.assertEqual(record["origin"], "musl")
        self.assertEqual(record["aports_commit"], COMMIT)
        for invalid in (package().replace("c:" + COMMIT, "c:master"),
                        package() + "\nP:other", package().replace("o:musl", "o:../escape")):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                collector.parse_apk_installed(invalid)

    def test_linked_origin_resolution_retains_ssp_crt_and_separates_headers(self):
        apk = package() + "\n\n" + package("linux-headers", "linux-headers", "6.6-r1")
        owners = "/usr/lib/libssp_nonshared.a | /usr/lib/libssp_nonshared.a | /usr/lib/libssp_nonshared.a is owned by musl-dev-1.2.5-r11\n"
        owners += "/usr/lib/rcrt1.o | /usr/lib/rcrt1.o | /usr/lib/rcrt1.o is owned by musl-dev-1.2.5-r11\n"
        owners += "/usr/include/linux/types.h | /usr/include/linux/types.h | /usr/include/linux/types.h is owned by linux-headers-6.6-r1\n"
        plan = collector.source_plan(apk, owners, "/usr/lib/libssp_nonshared.a\n/usr/lib/rcrt1.o\n")
        self.assertEqual(len(plan["origins"]), 1)
        self.assertEqual(len(plan["origins"][0]["linked_inputs"]), 2)
        self.assertEqual(plan["header_only_packages"][0]["origin"], "linux-headers")
        self.assertEqual(plan["origins"][0]["role"], "linked-library")
        self.assertEqual(plan["header_origins"][0]["role"], "header-only")
        self.assertEqual(plan["header_origins"][0]["header_inputs"], [
            {"path": "/usr/include/linux/types.h", "resolved_path": "/usr/include/linux/types.h"}])
        with self.assertRaisesRegex(ValueError, "no recorded ownership"):
            collector.source_plan(apk, owners, "/usr/lib/missing.a\n")

    def test_exact_sha512_recipe_is_parsed_without_shell_evaluation(self):
        data = "pkgver=1.2.5\npkgrel=11\nsource=\"$(touch NEVER_EXECUTE)\"\nsha512sums=\"\n" + "b" * 128 + "  musl-1.2.5.tar.gz\n\"\n"
        self.assertEqual(collector.parse_recipe(data, ["1.2.5-r11"]), {"musl-1.2.5.tar.gz": "b" * 128})
        with self.assertRaisesRegex(ValueError, "release does not match"):
            collector.parse_recipe(data, ["1.2.5-r10"])
        for checksum in ("SKIP", "0" * 127):
            with self.assertRaises(ValueError):
                collector.parse_recipe(data.replace("b" * 128, checksum), ["1.2.5-r11"])
        with self.assertRaises(ValueError):
            collector.parse_recipe(data.replace("musl-1.2.5.tar.gz", "../outside"), ["1.2.5-r11"])

    def test_kernel_literal_version_with_comment_is_supported_without_expansion(self):
        recipe = "pkgver=6.6 # Follow the latest Linux stable\npkgrel=1\nsha512sums=\"\n" + "b" * 128 + "  linux-6.6.tar.xz\n\"\n"
        self.assertEqual(collector.parse_recipe(recipe, ["6.6-r1"]), {"linux-6.6.tar.xz": "b" * 128})
        with self.assertRaises(ValueError):
            collector.parse_recipe(recipe.replace("6.6 #", "$(touch NEVER_EXECUTE) #"), ["6.6-r1"])

    def test_verified_download_is_atomic_and_records_both_hashes(self):
        payload = b"checked fixture"
        url = collector.DISTFILES + "/v3.21/fixture.tar.gz"
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "fixture.tar.gz"
            with patch.object(collector, "open_source", return_value=Response(payload, url)):
                record = collector.Fetcher().fetch(url, target, hashlib.sha512(payload).hexdigest())
            self.assertEqual(target.read_bytes(), payload)
            self.assertEqual(record["sha256"], hashlib.sha256(payload).hexdigest())
            self.assertEqual(list(Path(directory).iterdir()), [target])

    def test_bad_hash_or_budget_does_not_leave_a_verified_or_partial_file(self):
        payload = b"not matching"
        url = collector.DISTFILES + "/v3.21/fixture"
        for maximum, checksum in ((100, "0" * 128), (2, hashlib.sha512(payload).hexdigest())):
            with self.subTest(maximum=maximum), tempfile.TemporaryDirectory() as directory:
                with patch.object(collector, "open_source", return_value=Response(payload, url)):
                    with self.assertRaises(ValueError):
                        collector.Fetcher().fetch(url, Path(directory) / "fixture", checksum, maximum)
                self.assertEqual(list(Path(directory).iterdir()), [])

    def test_explicit_file_limit_is_positive_bounded_and_default_remains_128_mib(self):
        self.assertEqual(collector.Fetcher().max_file_bytes, 128 * 1024 * 1024)
        for limit in (0, -1, True, "144", collector.MAX_TOTAL + 1):
            with self.subTest(limit=limit), self.assertRaisesRegex(ValueError, "max-file-bytes"):
                collector.Fetcher(max_file_bytes=limit)
        self.assertEqual(collector.Fetcher(max_file_bytes=1).max_file_bytes, 1)
        self.assertEqual(collector.Fetcher(max_file_bytes=collector.MAX_TOTAL).max_file_bytes,
                         collector.MAX_TOTAL)

    def test_explicit_larger_file_limit_changes_only_per_file_gate_not_hash_or_total(self):
        payload = b"12345"
        url = collector.DISTFILES + "/v3.21/source"
        checksum = hashlib.sha512(payload).hexdigest()
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "source"
            for file_limit, total_limit, accepted in ((4, 20, False), (5, 4, False), (5, 20, True)):
                with self.subTest(file_limit=file_limit, total=total_limit):
                    fetcher = collector.Fetcher(maximum=total_limit, max_file_bytes=file_limit)
                    with patch.object(collector, "open_source", return_value=Response(payload, url)):
                        if accepted:
                            self.assertEqual(fetcher.fetch(url, target, checksum)["bytes"], 5)
                        else:
                            with self.assertRaises(ValueError):
                                fetcher.fetch(url, target, checksum)
                            self.assertFalse(target.exists())

    def test_foreign_hosts_and_http_redirects_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "fixture"
            for url in ("http://distfiles.alpinelinux.org/fixture", "https://example.com/fixture"):
                with patch.object(collector, "open_source") as network:
                    with self.assertRaises(ValueError):
                        collector.Fetcher().fetch(url, target)
                    network.assert_not_called()
            url = collector.DISTFILES + "/v3.21/fixture"
            with patch.object(collector, "open_source", return_value=Response(b"x", "http://example.com/fixture")):
                with self.assertRaisesRegex(ValueError, "redirect"):
                    collector.Fetcher().fetch(url, target)

    def test_redirect_is_rejected_before_another_network_request(self):
        request = collector.urllib.request.Request(collector.DISTFILES + "/v3.21/fixture")
        with patch.object(collector.urllib.request.OpenerDirector, "open") as network:
            with self.assertRaisesRegex(ValueError, "redirects are not permitted"):
                collector.NoRedirect().redirect_request(request, None, 302, "redirect", {}, "http://127.0.0.1/private")
            network.assert_not_called()

    def test_collection_failure_is_explicit_pending_not_a_fake_complete_source_set(self):
        class Missing:
            received = 0

            def fetch(self, *args, **kwargs):
                return None

        origin = {"origin": "musl", "aports_commit": COMMIT, "packages": [{"version": "1.2.5-r11"}], "linked_inputs": []}
        with tempfile.TemporaryDirectory() as directory:
            result = collector.collect_origin(origin, Path(directory), "3.21", Missing())
        self.assertFalse(result["source_inputs_collected"])
        self.assertEqual(result["files"], [])
        self.assertTrue(result["pending"])


class ReuseTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix="gfs-source-reuse-test-")
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.kit = self.root / "old-kit"
        self.output = self.root / "new-output"
        self.kit.mkdir()
        self.output.mkdir()
        self.name = "musl-1.2.5.tar.gz"
        self.payload = b"small checksum-bound source fixture"
        self.checksum = hashlib.sha512(self.payload).hexdigest()
        self.package = collector.parse_apk_installed(package())["musl-dev-1.2.5-r11"]
        self.origin = {"origin": "musl", "aports_commit": COMMIT, "role": "linked-library",
                       "packages": [self.package], "linked_inputs": []}
        self.url = collector.DISTFILES + "/v3.21/" + self.name
        self.record = {"file": self.name, "url": self.url, "resolved_url": self.url,
                       "bytes": len(self.payload), "sha256": hashlib.sha256(self.payload).hexdigest(),
                       "sha512": self.checksum, "declared_sha512": self.checksum}
        self.source = self.kit / ("musl-" + COMMIT) / self.name
        self.source.parent.mkdir()
        self.source.write_bytes(self.payload)
        self.write_manifest()

    def write_manifest(self):
        (self.kit / "SOURCE-INVENTORY.json").write_text(json.dumps({"format_version": 1,
            "origins": [dict(self.origin, files=[self.record])]}), encoding="utf-8")

    def copy(self, fetcher=None, checksum=None, origin=None):
        return collector.ReuseKit(self.kit).copy(origin or self.origin, self.name,
            checksum or self.checksum, (self.url,), self.output / self.name, fetcher or collector.Fetcher())

    def test_reuse_rehashes_both_algorithms_copies_independently_and_counts_total_budget(self):
        fetcher = collector.Fetcher()
        with patch.object(collector, "open_source", side_effect=AssertionError("cache copy needs no network")):
            record = self.copy(fetcher)
        self.assertTrue(record["reused"])
        self.assertEqual((self.output / self.name).read_bytes(), self.payload)
        self.assertNotEqual(self.source.stat().st_ino, (self.output / self.name).stat().st_ino)
        self.assertEqual(self.source.read_bytes(), self.payload)
        self.assertEqual(fetcher.received, 0)
        self.assertEqual(fetcher.reused_bytes, len(self.payload))
        self.assertEqual(fetcher.accounted, len(self.payload))

    def test_tampered_cache_cannot_be_blessed_by_its_old_manifest(self):
        tampered = self.payload[:-1] + bytes([self.payload[-1] ^ 1])
        self.source.write_bytes(tampered)
        # Even updating the cache's SHA256 cannot override the freshly fetched
        # APKBUILD SHA512, which still binds the original source input.
        self.record["sha256"] = hashlib.sha256(tampered).hexdigest()
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, "Reuse source hash mismatch"):
            self.copy()
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertEqual(self.source.read_bytes(), tampered)

    def test_fresh_checksum_package_or_commit_identity_cannot_be_overridden_by_cache(self):
        with self.assertRaisesRegex(ValueError, "fresh pinned APKBUILD"):
            self.copy(checksum="d" * 128)
        changed = dict(self.origin, packages=[dict(self.package, version="1.2.5-r99")])
        with self.assertRaisesRegex(ValueError, "package identity"):
            self.copy(origin=changed)
        changed = dict(self.origin, aports_commit="e" * 40)
        self.assertIsNone(self.copy(origin=changed))
        self.assertEqual(list(self.output.iterdir()), [])

    def test_symlinked_reuse_source_is_not_followed(self):
        actual = self.root / "external"
        self.source.rename(actual)
        self.source.symlink_to(actual)
        with self.assertRaises(OSError):
            self.copy()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_reuse_and_download_share_the_same_finite_byte_budget(self):
        fetcher = collector.Fetcher(maximum=len(self.payload) + 1)
        self.copy(fetcher)
        url = collector.DISTFILES + "/v3.21/another-source"
        with patch.object(collector, "open_source", return_value=Response(b"xx", url)):
            with self.assertRaisesRegex(ValueError, "remaining total byte budget"):
                fetcher.fetch(url, self.output / "another-source")
        self.assertEqual(fetcher.accounted, len(self.payload))
        self.assertFalse((self.output / "another-source").exists())
        (self.output / self.name).unlink()
        with self.assertRaisesRegex(ValueError, "per-file limit"):
            self.copy(collector.Fetcher(max_file_bytes=len(self.payload) - 1))

    def test_origin_always_refreshes_recipe_before_reusing_source_inputs(self):
        recipe = ("pkgver=1.2.5\npkgrel=11\nsha512sums=\"\n" + self.checksum + "  " + self.name + "\n\"\n").encode()
        requests = []

        def open_recipe(request, timeout):
            requests.append(request.full_url)
            self.assertEqual(request.full_url, collector.APORTS + "/" + COMMIT + "/main/musl/APKBUILD")
            return Response(recipe, request.full_url)

        fetcher = collector.Fetcher()
        with patch.object(collector, "open_source", side_effect=open_recipe):
            result = collector.collect_origin(self.origin, self.output, "3.21", fetcher, collector.ReuseKit(self.kit))
        self.assertTrue(result["source_inputs_collected"])
        self.assertEqual(len(requests), 1)
        self.assertEqual(fetcher.received, len(recipe))
        self.assertEqual(fetcher.reused_bytes, len(self.payload))
        self.assertTrue(next(record for record in result["files"] if record["file"] == self.name)["reused"])

    def test_oversized_header_archive_is_explicit_pending_with_exact_limit(self):
        recipe = ("pkgver=6.6 # literal comment\npkgrel=1\nsha512sums=\"\n" + "f" * 128 + "  linux-6.6.tar.xz\n\"\n").encode()
        origin = {"origin": "linux-headers", "aports_commit": COMMIT, "role": "header-only",
                  "packages": [{"version": "6.6-r1"}], "header_inputs": []}

        def respond(request, timeout):
            if request.full_url.endswith("/APKBUILD"):
                return Response(recipe, request.full_url)
            response = Response(b"", request.full_url)
            response.headers["Content-Length"] = str(collector.MAX_FILE + 1)
            return response

        with patch.object(collector, "open_source", side_effect=respond):
            result = collector.collect_origin(origin, self.output, "3.21", collector.Fetcher())
        self.assertFalse(result["source_inputs_collected"])
        self.assertEqual(result["role"], "header-only")
        self.assertIn(str(collector.MAX_FILE), result["pending"][0]["reason"])
        self.assertEqual(result["pending"][0]["file"], "linux-6.6.tar.xz")


if __name__ == "__main__":
    unittest.main()
