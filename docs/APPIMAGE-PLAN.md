# AppImage implementation and acceptance plan

Status: Linux x86_64 development packaging, 2026-09-08. This is not yet a
redistributable AppImage release. The tar.gz package remains the supported
download. AppImage publication stays disabled until the source/license and
native-runtime acceptance gates below are complete. Flatpak has a separate
[sandbox/application migration plan](FLATPAK-PLAN.md).

## Chosen structure

Reuse the verified Ubuntu 22.04 / Rust 1.88.0 portable binaries and Cargo
license inventory. Cargo now receives an explicit target and records only the
matching target-qualified output; [packaging provenance](PACKAGING.md) explains
the schema and old-receipt migration boundary.

The AppDir follows the [official layout](https://docs.appimage.org/reference/appdir.html):
`AppRun`, one root desktop link, the project icon and `.DirIcon`, plus
`usr/bin`, `usr/lib` and `usr/share`. `AppRun --cli …` executes the packaged CLI;
normal invocation starts the native desktop. Argument boundaries, caller
working directory and exit status are preserved, including spaces in paths.

The original tar payload is read-only input. Its full SHA256SUMS inventory,
receipt binary hashes and current Rust source-tree identity are verified before
copying. A fresh output directory is mandatory. Existing package outputs and
user data are never replaced. This is integrity/provenance checking, not a
signature that authenticates an arbitrary third-party download.

## Toolchain boundary

[tools.json](../packaging/appimage/tools.json) pins appimagetool 1.9.1 and
type2-runtime 20251108, their upstream commits, sizes and SHA-256 values.
`scripts/appimage_tools.py` only validates existing local files; it does not
download, chmod, execute or update them. The builder executes the packager only
after both tools verify, and passes the explicit `--runtime-file` so appimagetool
cannot silently download a different runtime. See the
[official packager interface](https://github.com/AppImage/appimagetool).

The selected [type-2 runtime](https://github.com/AppImage/type2-runtime/tree/dd6cebedcbddde9c82f89b011e8e1d40b6e43868)
is statically linked and does not require the user's libfuse2. That does not
guarantee FUSE capability on every kernel/container. The local builder and
initial smoke test use the runtime's extract-and-run path. Normal FUSE mounting
needs separate validation; no root mount, host service replacement or system
package installation is performed by AppRun.

Keep the verified tool directory private/stable through execution. Hashes do
not lock a pathname against an arbitrary concurrent writer or prove that the
publisher's compilation was reproducible.

## Native dependency boundary

The main ELF's DT_NEEDED list alone is not sufficient: winit/wgpu also load
X11, Wayland, keyboard and graphics APIs dynamically. Bundle the explicitly
reviewed GUI client libraries and their system-package dependency closure.
Do not bundle glibc/the ELF loader, host GPU drivers/ICDs or the desktop's
display, Portal and PipeWire **servers**. Graphics loader/driver availability
remains an explicit host requirement. FFmpeg/ffprobe remain optional host tools
for local video import; system fonts and cursor themes remain host resources.

PipeWire must be a coherent private **client** installation, not just one `.so`:

- Same-package-version libpipewire and stock `client.conf`.
- Client protocol-native, client-node, client-device, adapter, metadata and
  session-manager modules named by that configuration.
- SPA support, audioconvert, D-Bus support and host-conditional journal logging, with their
  transitive libraries. No PipeWire daemon, session manager process, ALSA or
  Bluetooth device service is started by this package.
- Private module/config/SPA paths, plus bundled XKB configuration resources.

Use per-ELF relative RUNPATH: executable `$ORIGIN/../lib`, ordinary library
`$ORIGIN`, PipeWire module `$ORIGIN:$ORIGIN/..`, and nested SPA plugin
`$ORIGIN/../..`. Never export a private `LD_LIBRARY_PATH` or replace PATH;
otherwise the app's external host FFmpeg can accidentally load incompatible
bundled libraries. PipeWire and XKB variables affect the launched application
environment, not the already-running host services.

Do not install the tarball's payload-copying installer into a FUSE mount. A
desktop entry inside an AppDir is also not automatically installed into the
already-running host Portal's application registry. Stable-path, explicit
per-user AppImage desktop integration and Wayland GlobalShortcuts identity
acceptance remain work; never claim those from the internal root desktop link.

## Source and redistribution gate

The Cargo dependency graph does not describe native shared objects or the
AppImage runtime's statically linked libraries. Record each original/native
file's hash, destination, post-RUNPATH hash, binary/source package versions,
copyright notice and license texts. Complete the matching corresponding-source
materials before public artifact upload. Merely pointing at the runtime's MIT
license is insufficient: its own notice lists musl, libfuse, squashfuse, zstd
and zlib too. PipeWire modules also pull in other system packages.

The initial builder deliberately requires `--development-only`, sets
`redistribution_ready: false`, and emits a development notice. This temporary
mode exists to exercise real packaging and runtime behavior without presenting
incomplete distribution materials as a finished download. It is not the final
delivery target and does not close the AppImage release gate.

## Local validation

On the Ubuntu 22.04 builder, provide the two official tool files at the exact
paths in `tools.json`; validate them before any execution. Python 3.10+, patchelf,
readelf/ldd, dpkg-query and desktop-file-validate are build requirements. The
integration test also needs Xvfb, xdotool, xprop, libX11, a software GL driver,
FFmpeg and ffprobe. It never uses the user's DISPLAY.

```sh
python3 scripts/appimage_tools.py target/appimage-tools
python3 scripts/test_appimage_tools.py
python3 scripts/test_build_appimage.py
python3 scripts/test_appimage_native.py
python3 scripts/build_appimage.py target/package-first/gifromscreen-0.1.0-linux-x86_64.tar.gz \
  --output-dir target/appimage-first --development-only
python3 scripts/test_appimage.py target/appimage-first/gifromscreen-0.1.0-development-x86_64.AppImage
```

Acceptance order:

1. Actual target-qualified portable build and real CLI/install/native-window tests.
2. Pinned tool, full payload, native closure, relocated path and argument tests.
3. Actual AppImage runtime → AppRun → CLI GIF generation/decoding and native
   X11 window → normal WM close → runtime/extraction cleanup.
4. Normal FUSE execution and relocation/read-only payload checks; equivalent
   inputs repackaged twice with fixed timestamps/tool identities. Do not infer
   independent-machine reproducible compilation from this packaging check.
5. Native GNOME Wayland Portal Share/Cancel, private PipeWire capture, pause,
   region movement, stop, project reopening and GIF export using the AppImage.
   Repeat on another supported desktop and test explicit desktop registration.
6. Complete source/license material and metadata validation, then enable a
   dedicated artifact workflow. Signing/stable release distribution remain
   separate gates. Do not upload a development-only image as a release.
