# Linux Preview 1 package acceptance

Source: clean `e9f43b267b55d6639b06d244f807e47fdcbccfc0`, Rust 1.88.0,
`x86_64-unknown-linux-gnu`, locked release build, glibc ceiling 2.35.
This is a limited unsigned preview; physical-desktop, complete-language and
full ScreenToGif parity gates remain open.

## Exact artifact

`gifromscreen-0.1.0-linux-x86_64.tar.gz`, 26,670,477 bytes:

```text
7a5975bf2e556ac601f5b9f3b2fe45dd9a6c438011c64bb18b43d84bed6a14f4
```

Local builds are retained in `target/language-preview-first` and
`target/language-preview-second`; `cmp` passed. This proves repackaging of the
same compiled binaries, not independent reproducible compilation. The extracted
`BUILD-INFO.json` has `source_dirty: false`, `package_git_dirty: false`, desktop
GLIBC maximum 2.35 and CLI 2.34. It inventories 317 Cargo normal/build dependencies
plus the new embedded Noto CJK font and includes readable licenses/provenance.

Checks run:

- Rust 1.98 all-target/all-feature workspace: 1,610 passed, 51 explicitly ignored;
  strict Clippy and formatting passed.
- Rust 1.88 desktop: 682 passed; localization: 24 passed. Strict 1.88 Clippy has
  unrelated existing lints described in [localization QA](LOCALIZATION-QA-2026-09-08.md).
- Portable builder tests: 12 passed. Actual archive tests: 12 passed, including
  internal checksums, CLI GIF output, safe install/reinstall/uninstall and notices.
- Owned-Xvfb tests: 3 passed; the actual packaged desktop displayed a native
  X11 window. All helper Xvfb processes were checked gone afterward.
- GitHub [Linux CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34218979462)
  and [portable workflow](https://github.com/tiansongyu/gifromscreen/actions/runs/34218979515)
  passed for the source commit.

## Packaged X11 recording and export

Owned private GNOME/Xvfb lab `/tmp/gfs-wayland-qa.6m7mgo54` froze the **extracted
portable desktop**, SHA-256
`861e786def10c84d321d8b480cc19b86e7489b8faf99d66a15d7b7a04685ae82`.
The recorder hid the normal pages and displayed a 320 × 220 physical-pixel guide
with independent controls. It recorded 21 frames at (100,230), acknowledged
pause, accepted a border drag to (280,230), resumed for 22 more frames and stopped
into the editor. Both regions remained the same size and used one capture clock
`fe91743c461b4310b00c9a94649558d5`.

The saved project has 43 frames, 4,253,246 µs total duration, revision 78. The GUI
exported the entire project to a 320 × 220 GIF with two coalesced images, 4,250 ms
encoded duration, 3,090 bytes. The application was closed normally; the package's
CLI reopened the project and exported a **byte-identical** GIF:

```text
42be4f92d81f083a76c00fa0836bf44b3f5798dee08e35da6224b2cbda496d7f
```

`ffprobe -min_delay 0` independently decoded/count-checked the output.
Screenshots `05-paused`, `06-retargeted`, `07-editor` and `10-exported` in the lab's
`logs/` directory show the actual transitions. The [README demonstration](assets/README.md)
is a separate screen recording of this run, not the exported two-image GIF.
An initial xdotool command accidentally typed later command tokens into a numeric
field; the fixture setup was corrected before the recorded acceptance run.
This was a harness input error, not counted as successful region selection.

The lab was explicitly stopped after normal application close and CLI export.
These software-rendered X11 checks do not certify physical GPU/multi-monitor
behavior or re-run the separate Wayland/portal and camera acceptance matrices.

## Published asset verification

[v0.1.0-preview.1](https://github.com/tiansongyu/gifromscreen/releases/tag/v0.1.0-preview.1)
was published with `prerelease: true`, `draft: false`, target `e9f43b2`, and only
the tarball plus its 105-byte checksum file. GitHub's reported asset size and
SHA-256 agree with the local artifact. Both assets were downloaded again through
their public HTTPS URLs **without authentication**, into
`target/preview-download-check`; `sha256sum --check` and comparison with the
original tarball passed. The remote tag resolves to the exact build commit.
No AppImage, Flatpak, macOS package, detached signature or stable-release claim
was attached.
