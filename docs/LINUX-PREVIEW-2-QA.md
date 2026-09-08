# Linux Preview 2 acceptance

[v0.1.0-preview.2](https://github.com/tiansongyu/gifromscreen/releases/tag/v0.1.0-preview.2)
is an unsigned Linux x86_64 pre-release from clean commit
`89021bdab4cdb6cf85f6b9e0f06ad0c45c236ffb`. Its internal package version is still
`0.1.0`; use the preview tag, source revision and checksum to distinguish it.

## Artifact and provenance

CI run [34244415812](https://github.com/tiansongyu/gifromscreen/actions/runs/34244415812)
produced artifact `10063513884`. The tarball is 26,726,038 bytes:

```text
4719493ff8e28bc10e3c9a7b80e38f52d903f8b53c5e289ce4b9f55ba9c8f676  gifromscreen-0.1.0-linux-x86_64.tar.gz
```

`BUILD-INFO.json` identifies Rust/Cargo 1.88.0, the explicit
`x86_64-unknown-linux-gnu` release target, clean source/package trees and a
glibc ceiling of 2.35. Independent recomputation from that Git commit matches:

```text
db9d385c207db7ce7456ffe81f6e3012cba6af028c71ba9c457b8bd93fbf9f60  Rust source tree
bad4b0af448f9011035bf57d524b6a160ba6c927479a5fded0f02eb3c20c267e  packaging tree
504f6ce0e3b7e115d2c9aef259be7adceca3823d688768c632506481c67f4112  Cargo.lock
```

Both actual executables are ELF64 x86_64. Desktop requires at most GLIBC 2.35;
CLI requires at most 2.34. Their hashes match the receipt. All 566 internal
checksum entries match, covering every file except `SHA256SUMS` itself (567 files,
894 total members, 49,943,402 unpacked bytes). No duplicate, escaping, symlink or
special member was found. The 318-entry third-party inventory references 549
nonempty license files. The original CJK font bytes were located in the desktop
ELF, and font/source/notice hashes were checked independently.

## Executed checks

- Source workspace: Rust 1.98 all-target/all-feature tests 1,665 passed, 51
  explicit environment/benchmark ignores; strict Clippy and formatting pass.
- Rust 1.88 desktop/localization: 726/35 tests pass.
- Portable CI passes actual build, byte-identical repackaging of the same binaries,
  archive/install tests, owned-display lifecycle, packaged-window smoke and upload.
- After downloading the CI artifact locally, all 12 real-archive installation,
  checksum, CLI-GIF and license tests passed again; the actual packaged desktop
  opened a native window in its owned Xvfb.
- The [native editor/shortcut checks](EDITOR-LOCALIZATION-QA-2026-09-08.md) use the
  same source's debug executable, not the release ELF. They cover real edit/Undo,
  existing-notice translation, preserved crop drafts/project bytes, byte-identical
  GUI/CLI GIF output and real X11 shortcut registration/disable.

The release was created with `prerelease: true`, `draft: false`, and only the
tarball plus its 105-byte checksum sidecar. Its remote tag resolves to the exact
source commit. GitHub's asset hash/size match the local artifact. Both public
HTTPS URLs were downloaded again **without authentication** into
`target/preview2-download-check`; the downloaded checksum passed and the tarball
matched the CI download byte for byte.

## Boundaries retained

Not a stable/full-parity/all-language release. Advanced UI translation, other
language catalogs, shaping/RTL/IME, physical desktops/cameras/GPUs, mixed-DPI and
multi-monitor matrices remain open. Ordinary drawing still needs general mapping
protection beyond the language-switch guard. No AppImage, Flatpak, macOS, detached
signature, complete SPDX SBOM or independent reproducible-compilation claim is
included. System graphics/PipeWire/portal dependencies remain external; optional
FFmpeg/ffprobe are not bundled. Checksums are not independent authentication.
