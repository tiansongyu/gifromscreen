# Runtime source-input collection

The exact Alpine source inputs for the recorded runtime's linked libraries and
used header packages have been collected and verified. This is progress toward
distribution, not a statement that all licensing, offline rebuild or relink
requirements are complete. `redistribution_ready` stays false.

## Resolve from actual build evidence

`scripts/collect_runtime_sources.py` reads the retained link-input paths,
per-file package ownership, installed APK database and Alpine release from
`target/appimage-runtime-first/artifacts/relink`. It records hashes of these
inputs instead of guessing dependencies from a generic runtime README.

Five linked origins are selected: musl, gcc, mimalloc2, zlib and zstd. Two
additional origins supply headers actually used by the compiler:
fortify-headers and linux-headers. Each origin is bound to the recorded package
version/revision and aports commit. The fixed-commit APKBUILD is downloaded as
**data**, never executed or sourced. Its literal SHA512 inventory determines
the required files; missing or unsupported metadata becomes explicit pending
work. Files come from the fixed aports commit or Alpine's versioned official
distfiles mirror, with the same expected SHA512 in either case.

The existing libfuse/squashfuse archives and libfuse patch are separately copied
from the SDK's already-verified source inventory. They are not mislabeled as
APK-owned libraries. The runtime's own upstream archive, cleanup patch and
build recipe remain retained by the [runtime build](APPIMAGE-RUNTIME-QA-2026-09-08.md).

## Bounds, caching and failure behavior

Default single-file limit: 128 MiB. Shared source budget: 256 MiB, including
reused files and the preserved SDK archives. Collection has a 600-second
deadline checked between bounded network reads, with per-socket waits limited
to at most 20 seconds. A final blocking read may finish after the deadline;
this is not hard real-time scheduling. Redirects are rejected before contacting
another endpoint. Downloads must pass hashes before their files are atomically
published in a new output directory. Old kits are never overwritten.

`--reuse-kit` supplies only a cache index. The collector still obtains the
APKBUILD from the recorded commit, checks package/origin identity and fresh
declared checksums, and rehashes every cached byte with SHA512 and SHA256 while
copying to the new kit. A matching filename or old manifest alone is insufficient.
Bad cache data is reported, not silently counted as collected.

The Linux 6.6 archive is 140,064,536 bytes, so the default correctly rejected it
before reading its body. The completed run used an **explicit**
`--max-file-bytes 150994944` (144 MiB); the shared 256 MiB budget stayed unchanged.
No retry automatically enlarged a limit. The option is positive and cannot
exceed the total budget.

```sh
python3 scripts/collect_runtime_sources.py \
  --relink-dir target/appimage-runtime-first/artifacts/relink \
  --reuse-kit target/runtime-source-kit-complete \
  --output-dir target/runtime-source-kit-all-inputs \
  --max-file-bytes 150994944
```

Omit `--reuse-kit` for a fresh collection. Choose a new output path if the one
above already exists.

## Measured collection

The final kit is `target/runtime-source-kit-all-inputs`:

- Seven APKBUILDs and 68 checksum-declared source files: **75 files** across
  five linked and two header-only origins. All were independently rehashed.
- 140,110,745 network bytes, 104,968,854 reused bytes and 4,667,588 preserved SDK
  source bytes: **249,747,187 accounted bytes**, below 268,435,456.
- Linux 6.6 SHA-256:
  `d926a06c63dd8ac7df3f86ee1ffc2ce2a3b81a2d168484e76b5b389aba8e56d0`;
  its APKBUILD SHA512 also matches.
- `SOURCE-INVENTORY.json` SHA-256:
  `3cf348af6daa252f7386917b91f32794288d0e897cdbb2ff1398d169cf2b205d`.
  Linked/header source-input completion is true; no header package remains
  pending in this recorded set.

The earlier five-origin kit and default-limit partial kit are retained with
unchanged manifests. The collector's 18 parser, transport, cache and budget
tests pass without network access and run in the portable CI workflow.

## Remaining scope

The source archives retain their original license files, but standalone notice
extraction and final distribution review remain work. An offline rebuild of
the relevant libraries and an independently exercised runtime relink have not
been established by downloading files. The collector does not claim to recreate
every SDK build-tool package, authenticate arbitrary user-supplied build records,
or prove reproducible compilation across machines.

Ubuntu libraries bundled inside the application payload have their own source
origins, distinct from this Alpine-linked runtime. Their collection and release
review are separate. Do not set AppImage distribution ready from this kit alone.
