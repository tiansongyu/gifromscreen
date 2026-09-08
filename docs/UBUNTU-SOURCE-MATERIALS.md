# AppImage payload: exact Ubuntu source inputs

All source-file sets for the recorded AppImage's 25 Ubuntu source versions have
been collected and verified. This is separate from the
[Alpine-linked runtime material](RUNTIME-SOURCE-MATERIALS.md), and is not final
redistribution approval. The collector never executes or extracts downloaded
sources, APK recipes, Debian descriptors or signatures.

## Scope comes from actual payload roles

Input: `target/appimage-repack-first/GifFromScreen.AppDir/BUILD-INFO.json`, SHA-256
`a62ec8ebf53aa67c7ed7d2b37f619c3b4d38394fce2154937884859d99636dce`.
Its 34 package records comprise:

- 31 binary packages supplying 39 ELF files.
- `pipewire-bin`, supplying the client configuration only.
- `xkb-data`, supplying 297 resources.
- `base-files`, supplying public license texts and its copyright only.

Thus 33 packages supply application code/resources, mapping to 25 distinct
source/version pairs. `base-files` text provenance is retained but is not
misrepresented as linked code. The plan is derived from `system-elf`,
`pipewire-config` and `xkb-data` roles; it is not a blanket source request for
every package mentioned by a copyright file.

## Acquisition and integrity

`scripts/collect_ubuntu_sources.py` resolves the exact source name/version in
Ubuntu's primary archive and Jammy series through the
[official Launchpad API](https://api.launchpad.net/devel/). It retains epoch,
`+dfsg`, revision and historical publication identity; it never substitutes a
newer release. `sourceFileUrls(include_meta=true)` supplies the file names,
declared sizes and SHA-256 values instead of guessing archive paths.

The original `.dsc` is checked first. Its Source/Version must agree, and its
SHA-256 inventory is cross-checked against Launchpad before large source files
are downloaded. Files are streamed to private temporary files, verified, then
published without overwriting an existing destination. The output directory
must be new. A complete initial source plan and per-origin checkpoints preserve
explicit pending work if collection fails.

API and file URL rules are distinct, restricted to credential-free HTTPS on
the official Ubuntu primary/Launchpad Librarian endpoints. Redirect targets
are checked **before** the next connection, with hop/repetition limits. The
current transport closes redirect responses without draining arbitrary response
bodies. Default limits are 192 MiB per file, 512 MiB total response bodies and
a 600-second CLI deadline, including a main-thread deadline alarm. These are
bounded automation controls, not a real-time or wire-bandwidth guarantee.

```sh
python3 scripts/test_collect_ubuntu_sources.py
python3 scripts/collect_ubuntu_sources.py \
  --metadata target/appimage-repack-first/GifFromScreen.AppDir/BUILD-INFO.json \
  --output-dir target/ubuntu-source-kit
```

Use another fresh output path if that directory exists. No package installation,
host service change, `.deb` execution or source build is performed.

## Actual collection and code-version boundary

The single actual collection completed all 25 source sets with no pending files:
84 source files plus 50 API metadata files. All 25 `.dsc` descriptors matched
the inventory, covering all 59 non-descriptor files; no unaccounted API-only
source file remained. All source-file redirects ended at the official
`launchpadlibrarian.net` host.

Source payload: 127,638,686 bytes. API bodies: 92,272 bytes. Recorded total:
127,730,958 bytes. The largest file is GCC's 91,555,468-byte original archive,
within the original limits. Source-file sizes and hashes and all API file
hashes were independently rechecked offline. No executable permissions or
partial-download files remained.

Kit: `target/ubuntu-source-kit`; `index.json` SHA-256:
`d4c6fbb4412167b186068400986c7ce37c42c082d2c1ab4d5989b03c606f4a8e`.
The input metadata copy still matches the original AppImage build information.

The download run used collector source SHA-256
`5aa23d1ab93327f0f88ff32e84957577f4147e11181227a5666ada271f93c087`.
After it completed, redirect/error-response handling was hardened; final source
SHA-256 is `8df69e0dd25d26fba0d38a72d2efbb71945e38fc1d2ba7889a58ac22b10cdb42`.
The final implementation passes 24 mock tests. The payload recheck remains valid,
but the earlier live run is not relabeled as a live test of the later transport
change. No second full download was performed to disguise that distinction.

## Remaining review

The metadata contains `redistribution_ready: false` and is not changed in place.
OpenPGP signatures on `.dsc`/source files are retained, not independently
authenticated by this collector. Official `.deb` contents have not yet been
compared against the original pre-RUNPATH file hashes. Build/relink and final
source/license distribution review remain work; downloading all named inputs
does not prove those outcomes. The application payload's Ubuntu inputs, the
runtime's Alpine inputs and the Rust/project source have separate provenance.
