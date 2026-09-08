# Source-built AppImage runtime correction

The runtime cleanup failure is fixed and verified through actual AppImages in
both extract-and-run and normal FUSE modes. This closes that specific defect,
not the full AppImage distribution, Wayland/hardware or Linux feature gates.
`redistribution_ready` remains false and no AppImage is uploaded as a release.

## Root cause and bounded correction

The [original failure](APPIMAGE-DEVELOPMENT-QA-2026-09-08.md) retained about
49 MiB after a reported successful exit. The pinned
[upstream runtime](https://github.com/AppImage/type2-runtime/blob/dd6cebedcbddde9c82f89b011e8e1d40b6e43868/src/runtime/runtime.c)
passes zero as nftw's descriptor limit; [musl 1.2.5](https://git.musl-libc.org/cgit/musl/tree/src/misc/nftw.c?h=v1.2.5)
returns success without calling the callback in that case. The deletion
callback additionally returned zero on failures, which nftw treats as success.

An isolated Alpine 3.21/musl 1.2.5-r11 probe verified zero-limit no-op behavior,
successful shallow traversal with a positive limit, and the old callback's
silent deep-tree failure. It ran as UID 65534 with no network/capabilities and
preserved an external symlink sentinel. Evidence is retained at
`/tmp/gfs-musl-nftw.XYaQwr/EVIDENCE.md`. Its exact owned container was removed
after confirmed exit; host services and the original failing runtime were not
changed.

The local patch sets a positive bounded limit of 64, returns nonzero on callback
failures, and checks that the root is actually absent before reporting success.
It retains depth-first, mount-boundary and no-symlink-following flags. More than
64 nested levels may fail; the patch reports failure instead of claiming an
uncleaned tree is gone. This is not an arbitrary-depth traversal claim.

## Actual build, not only a copied test algorithm

`scripts/build_appimage_runtime.py` verifies the original runtime, libfuse and
squashfuse source archives, copies and verifies them again, applies the checked-in
patch, and builds the SDK from an immutable Alpine base digest. The local
SDK image is
`sha256:ef24cbe6a29984e2366cb9a1cfbfe4532695c0abaa9e305e363bbff27a852c9f`.

Final runtime compilation ran in container
`40e71dc8c5ab05a1bd3d2198626441c61d63563d55f06264fff318973a67470c`:
UID/GID 1000, no network, all capabilities dropped, no-new-privileges, read-only
root/source, private tmpfs, two CPUs and 2 GiB memory. It exited 0, was not OOM
killed, and was explicitly removed after identity/label checks. The SDK image
remains as build cache; no existing container or host service was stopped.

The build extracts the cleanup functions from the **actual patched runtime.c**
and compiles/runs four tests in that SDK: shallow cleanup, deep failure,
injected no-op detection and permission failure. All pass, all external symlink
targets survive, and each fixture is removed. The full runtime is then compiled
with Alpine Clang 19.1.4 and linked as static PIE with no DT_NEEDED libraries.

Build directory: `target/appimage-runtime-first`. Runtime SHA-256:
`8793391deb07d95c1761658363ec87d377400d49a78d41a5fc5d5b9f5e6b4c44`
(944,632 bytes). The receipt binds the upstream commit, patch SHA, recipe,
SDK image, source materials and all 150 output files (28,649,033 bytes).
It includes the real relocatable `runtime.o`, map, compiler command, actual
static archives/CRT/header inputs, version/linker script, debug file and SDK
package/build records. A `.debug` file is not substituted for a relink object.
Changing/removing a retained source archive or any output invalidates reuse.

## Actual AppImage and deterministic repackaging

The first corrected image, before provenance normalization, has SHA-256
`f61fd444c9459ea735e0258fae8889a3cb3f1cd263f91b8f9958036884cec78a`.
Both launch modes passed independently. A further packaging fix removes the
ephemeral AppDir path from exported **project-binary** provenance: its source
is now a verified portable-payload relative path. Original/patched hashes remain
recorded; actual host-library source paths are not normalized or concealed.

With a frozen recipe and the same verified tarball/runtime/host libraries,
`target/appimage-repack-first` and `target/appimage-repack-second` produce
byte-identical AppImages in different directories (`cmp` exits 0). Each is
16,845,304 bytes, SHA-256:
`b48e2e578fb8efff9e226d00e644431c1ce46e340a998df0657c88030e6a43ff`.
This proves same-input repackaging, not independent-machine recompilation.
The builder also rejects packager changes to the inventoried AppDir and ignores
an ambient VERSION variable that could otherwise rewrite its desktop entry.

That final image passes `scripts/test_appimage.py` in both modes. Each uses its
own Xvfb, private XDG directories and owned process sessions:

- AppImage → AppRun → CLI version 0.1.0.
- A 160×96, 36-image GIF, inspected and fully decoded with ffprobe/FFmpeg.
- A visible native window, verified PID/group ownership and standard
  WM_DELETE_WINDOW close, followed by runtime exit 0.
- Empty runtime temporary directory after normal close; no cleanup is performed
  by the test to manufacture that assertion.

Extract evidence is at `target/appimage-repack-check-extract/result.json`;
FUSE evidence at `target/appimage-repack-check-fuse/result.json`. These tests use
software GL on the current Linux host, not a physical-display or Wayland sharing
session. New process tests retain the unreaped leader until group cleanup,
preventing signals to a numerically reused process group.

## Tests and still-open distribution work

82 AppImage unit/orchestration tests, eight real owned-process tests and nine
portable build-receipt tests pass. The container cleanup tests and two actual
image-mode checks above are additional, separately scoped evidence. CI now runs
the runtime build-wrapper tests; it does not pretend mocked Docker proves a
fresh hosted runtime compilation.

Full corresponding-source material is **not** complete. The static link also
includes mimalloc and compiler support libraries; those must not be omitted
merely because the upstream top-level notice lists fewer components. Exact
Alpine APK sources/patches and complete relink/offline-rebuild validation remain
work, alongside the bundled Ubuntu libraries' corresponding-source materials.
Retained archives and package identity records are not a legal-compliance
guarantee. AppImage Wayland capture, desktop registration and broader hardware
acceptance also remain open; the [full Linux ledger](LINUX-STATUS.md) remains
authoritative.
