# AppImage development packaging — incomplete acceptance

Historical first-image record. The subsequent
[source-built runtime correction](APPIMAGE-RUNTIME-QA-2026-09-08.md) now passes
the unchanged cleanup requirement. The failed image/hash below is not relabeled
as fixed, and overall distribution acceptance is still incomplete.

The first real image builds and runs its desktop and CLI, but its runtime
cleanup test **fails**. No AppImage is uploaded as a distributable artifact.
This record does not close the [AppImage plan](APPIMAGE-PLAN.md), Flatpak gate
or full Linux completion criteria.

## Portable build correction

Commit `a143c06` fixes the Cargo target/receipt mismatch and adds nine bounded
regressions. Its Linux CI run `34198967050` and portable-package run
`34198967108` both completed successfully. The preceding drag-feature commit
`ffa9040` also passed both workflows (`34198071721`, `34198071777`).

The local fresh target-qualified release build took approximately 77 seconds
and produced `target/receipt-fix-first/gifromscreen-0.1.0-linux-x86_64.tar.gz`:
13,007,091 bytes; SHA-256
`d7cb42e0db83ec01be292b8d790f0f8912096030e090bf9c70a8468d59393e49`.
The package contains 317 conservative Cargo normal/build dependency records.
All 12 real portable installation/integrity/CLI tests passed, followed by a
successful packaged native-window launch on an owned Xvfb.

A later `receipt-fix-second` archive differs because AppImage packaging sources
and their recorded digest had changed in the working tree between builds.
That cross-source comparison is not reproducible-repack evidence. The committed
workflow's same-source deterministic comparison passed; no metadata was removed
to conceal the local difference.

## Actual first AppImage

Both downloaded official tool files passed the fixed size, SHA-256 and ELF64
x86_64 checks. The packager was invoked with the supplied pinned runtime, not
an implicit latest-runtime download. The first image is
`target/appimage-development-first/gifromscreen-0.1.0-development-x86_64.AppImage`;
SHA-256 `71e2d9fd973e9e4660ce0017375a190d6f5ff1ebd4e51b400b2230dc842ae5aa`.
Its runtime reports `dd6cebe`. The AppDir has relative root links, private
libraries with per-file RUNPATH, XKB resources and native package/license
inventory. The image explicitly records `redistribution_ready: false`.

This first image predates the additional default-conditional SPA journal
plugin. A later rebuilt image must be separately identified; source changes are
not retroactively attributed to this hash.

`scripts/test_appimage.py` uses a newly owned Xvfb, private XDG directories,
extract-and-run, software GL and a fresh child session for each invocation.
Before the final failing assertion it completed:

- AppImage → AppRun → CLI version `0.1.0`.
- CLI animation generation, ffprobe inspection and full FFmpeg decode.
- An actual visible GifFromScreen window. Its `_NET_WM_PID` was verified as a
  member of the owned session before sending standard WM_DELETE_WINDOW.
- AppImage exit status 0 after normal close; no XDestroyWindow was used.

The exit check nevertheless found the complete approximately **49 MiB**
`appimage_extracted_eeabc6db07072d4042c5f6fef3f1f826` directory remaining.
`target/appimage-check-first` retains the second run's GIF, log and leftovers.
No assertion was weakened to treat the directory as acceptable cleanup.

## Runtime investigation

A separate invocation without the Python ownership wrapper also leaves the
directory: `target/runtime-cleanup-direct/trace.log` records runtime fork,
payload exit, runtime wait and exit 0, but no cleanup unlink/rmdir calls.
`NO_CLEANUP` was explicitly unset. This independent trace rules out blaming
the Python supervisor merely because it wraps the process.

The pinned upstream
[`rm_recursive`](https://github.com/AppImage/type2-runtime/blob/dd6cebedcbddde9c82f89b011e8e1d40b6e43868/src/runtime/runtime.c#L968)
passes zero as nftw's descriptor limit. Its interaction with the runtime's musl
implementation is under investigation; this observation is not yet a validated
source-level fix. Build a corrected runtime from pinned, auditable sources or
select a separately verified upstream fix, then rerun the unchanged cleanup
requirement. Do not patch arbitrary binary offsets or silently delete leftovers
in the smoke test and label that runtime success.

## Automated implementation coverage

The development components currently pass 10 offline tool-validation tests,
16 payload/launcher/builder tests, 16 mock native-bundler tests, and eight real
owned-process lifecycle tests. The latter retain the leader with waitid/WNOWAIT
until all live members of its private process group have been handled, so an
exited runtime does not make its numeric PGID safe to reuse for later signals.
They reject PIPE capture to avoid a lingering child retaining an output pipe.

These tests are added to the portable workflow, but do not download tools or
publish an AppImage there. Runtime cleanup, corresponding-source materials,
FUSE, AppImage Wayland capture, desktop integration and broader hardware
acceptance remain separate unfinished gates.
