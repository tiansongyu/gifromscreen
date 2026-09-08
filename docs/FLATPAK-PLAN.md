# Flatpak delivery plan — not yet delivered

Status: design and source review only, 2026-09-08. No Flatpak manifest, runtime
installation, sandbox build, signed bundle, or published Flatpak is claimed by
this document. AppImage is the current distribution priority. Flatpak remains
a subsequent Linux delivery target; its sandbox is not a reason to abandon
the existing Linux feature roadmap or silently remove functions.

## Selected baseline

Use `io.github.tiansongyu.gifromscreen`, `org.freedesktop.Platform//26.08`, and
the matching `org.freedesktop.Sdk//26.08`, initially on x86_64. The official
[support table](https://freedesktop-sdk.gitlab.io/documentation/wiki/Releases.html)
lists 26.08 through September 2028; 25.08 through September 2027; 24.08 reaches
end of support in September 2026. The
[26.08.0 release](https://gitlab.com/freedesktop-sdk/freedesktop-sdk/-/tags/freedesktop-sdk-26.08.0)
is published, not merely a future version recommendation.

Build-only SDK extensions:

- `org.freedesktop.Sdk.Extension.rust-stable//26.08`: its
  [reviewed manifest](https://github.com/flathub/org.freedesktop.Sdk.Extension.rust-stable/blob/c9fd7fde3fa6181016a6fbae3e26958670389445/org.freedesktop.Sdk.Extension.rust-stable.json)
  supplies Rust 1.98.0 under `/usr/lib/sdk/rust-stable`; the project MSRV remains 1.88.
- `org.freedesktop.Sdk.Extension.llvm22//26.08`: the
  [matching manifest](https://github.com/flathub/org.freedesktop.Sdk.Extension.llvm22/blob/fdc562ff878779b239b20ef2a28f9719d2381804/org.freedesktop.Sdk.Extension.llvm22.json)
  builds Clang/LLVM under `/usr/lib/sdk/llvm22`. Use build-only PATH/library-path
  settings and `LIBCLANG_PATH=/usr/lib/sdk/llvm22/lib` for bindgen.

Do not copy host Ubuntu libraries or toolchains into this package. Record the
resolved runtime/SDK/extension OSTree commits and compiler versions in CI
provenance. The source manifests above establish a plausible dependency set;
the actual published files and old `libspa`/bindgen compatibility still need
the first real sandbox build, not an assertion based on a successful host build.

## Runtime dependencies and permissions

| Existing path | Flatpak treatment |
| --- | --- |
| eframe/wgpu Wayland and X11 UI | `--socket=wayland`, `--socket=fallback-x11`, `--device=dri`; retain `--share=ipc` for X11 graphics compatibility. |
| X11 GetImage, XI2, SHAPE/XFixes, shortcuts and picker | Use the granted X11 connection on an X11 desktop. No `/dev/input`, keyboard-device permission, or extra keyboard daemon is needed. |
| Wayland screen capture | Keep the existing ScreenCast Portal session and the authorized PipeWire FD. Do not expose the host PipeWire socket. |
| Global shortcuts | Keep the existing portal registration and user binding dialog on Wayland; preserve capability errors and recorder-button fallback. |
| Video import | Run FFmpeg/ffprobe inside the runtime/sandbox, never `flatpak-spawn --host`. Existing input restrictions remain in force. |
| Camera | The current direct V4L2 implementation needs a Camera Portal adapter before full sandbox delivery; see below. |

The four proposed finish arguments follow the
[official sandbox guidance](https://docs.flatpak.org/en/latest/sandbox-permissions.html).
Portal D-Bus names are already allowed by the standard filtered bus policy.
Do not add unrestricted session/system buses, `--filesystem=host`/`home`,
`--device=all`, `--device=input`, or network access to make tests pass.
The application imports local video and excludes audio, so neither network nor
PulseAudio access is required for its current functionality. X11 permission is
still inherently broader than Wayland Portal access; document that honestly.

[wayland.rs](../crates/capture-linux/src/wayland.rs) already performs
CreateSession/SelectSources/Start/OpenPipeWireRemote, and
[wayland_pipewire.rs](../crates/capture-linux/src/wayland_pipewire.rs) uses `connect_fd`.
This matches the [ScreenCast FD contract](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html).
[Portal identity registration](../crates/capture-linux/src/shortcuts/portal/identity.rs)
already lets the portal identify Flatpak sandboxes instead of impersonating an
unsandboxed application. Keep the app ID aligned with the viewport, launcher,
icon and metadata. Never force X11 access on a Wayland session to bypass consent
or manufacture unsupported global input metadata.

## FFmpeg and native build dependencies

The [26.08 platform stack](https://gitlab.com/freedesktop-sdk/freedesktop-sdk/-/blob/freedesktop-sdk-26.08.0/elements/platform.bst)
contains FFmpeg, PipeWire, Wayland/X11, libxkbcommon, libdbus, font and graphics
libraries. The SDK inherits that stack and retains development files. The
[FFmpeg configuration](https://gitlab.com/freedesktop-sdk/freedesktop-sdk/-/blob/freedesktop-sdk-26.08.0/elements/include/ffmpeg.yml)
does not disable programs; its base
[codec selection](https://gitlab.com/freedesktop-sdk/freedesktop-sdk/-/blob/freedesktop-sdk-26.08.0/elements/components/ffmpeg.bst)
includes rawvideo encoding, but excludes software h264/hevc/vc1/vvc decoding.
Prefer these maintained runtime tools over bundling a duplicate FFmpeg.

Before accepting that choice, test `ffmpeg -version`, `ffprobe -version`, required
demuxers, `fps/scale/setsar/crop/format` filters and actual raw-RGBA output in the
runtime. Our [video importer](../crates/application/src/video_import.rs) invokes
both executables and restricts inputs to local files; tool presence alone is
insufficient. Test common MP4/H.264 and WebM/VP9 fixtures, including missing-codec
diagnostics. Since 25.08, the runtime-managed
[codecs-extra extension](https://docs.flatpak.org/en/latest/extension.html#freedesktop-sdk-extensions)
replaces the old ffmpeg-full model. Do not add an obsolete ffmpeg-full stanza or
assume optional codecs are present on every installation.

Build preflight must resolve `libpipewire-0.3`, `libspa-0.2`, Wayland,
libxkbcommon/X11 and native graphics development dependencies through SDK
pkg-config. Both `libspa-sys` and `pipewire-sys` 0.6 use bindgen, so verify that
the supplied libclang loads and the current SDK headers compile with the locked
Rust crates. Clang is a build dependency, not a reason to enlarge runtime access.

## Required application changes

1. **Separate durable projects from exported files.**
   [RecordingSettings::default and project_path_for_output](../apps/desktop/src/main.rs)
   currently use the working directory and derive a sibling `.gfsproj` from the
   GIF path. Use a unique owner-only project directory below
   `$XDG_DATA_HOME/gifromscreen/projects` instead. Do not rely on sandbox HOME or
   `/tmp` for durability. Existing XDG-aware settings stores can remain separate
   from non-Flatpak settings; do not expose host configuration directories.
2. **Grant the operations actually needed.**
   [PathPicker](../apps/desktop/src/path_picker.rs) already uses portal-enabled
   rfd for input files/project folders, but output/Save As is not yet a complete
   portal workflow. A [FileChooser grant](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html)
   to one file must not be treated as permission for unrelated siblings.
   [Project export](../crates/application/src/project_export.rs) creates a sibling
   temporary file, fsyncs and atomically commits; project Save As also needs a
   writable directory. Add explicit output-directory authorization and verify
   write/rename/lock behavior on Document Portal mounts. Preserve atomicity,
   no-clobber policy and cancellation; do not replace failures with blanket home access.
3. **Move camera access behind its portal.**
   [camera_capture.rs](../crates/application/src/camera_capture.rs) enumerates
   `/dev/videoN`, reads sysfs names and launches FFmpeg V4L2. Implement
   [Camera.AccessCamera/OpenPipeWireRemote](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Camera.html)
   and consume only the authorized camera nodes, reusing bounded frame/writer
   control where appropriate. The USB portal is not a V4L2 substitute. Until
   this adapter and its tests exist, a test build must clearly report camera
   unavailable; that is a remaining feature gate, not completed Linux parity.
4. **Audit X11 ownership across PID namespaces.**
   Flatpak normally [unshares the PID namespace](https://github.com/flatpak/flatpak/blob/main/common/flatpak-run.c).
   Locked winit 0.30.13 writes its namespace-local `getpid()` to `_NET_WM_PID`.
   [x11.rs](../crates/capture-linux/src/x11.rs) and
   [window_snap.rs](../crates/capture-linux/src/window_snap.rs) compare that
   property with `process::id()` to exclude own windows. Different sandboxes can
   reuse the same PID: this creates a concrete compatibility risk, not proof of
   shared ownership. Use trusted app-window identities/owned-window registry and
   nonce checks where needed; retain PID as supplementary validation. Test a
   second sandbox with a colliding PID. Do not weaken native write authorization
   or remove PID isolation as a shortcut.

## Build and acceptance order

1. Implement the application gates above and retain native Linux regressions.
   Add the actual release manifest only with working sources/build commands.
2. Pin the app source commit and generate `cargo-sources.json` from the checked-in
   lockfile using the [official Cargo generator](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo).
   Use module-local `CARGO_HOME`, SDK-extension paths and
   `cargo build --locked --offline --release -p gif-from-screen -p gif-from-screen-cli`.
   Fetch declared sources separately; do not enable network for the build phase.
3. Install both binaries, existing desktop/icon files and license notices under
   `/app`. Add matching AppStream metadata; it is not present in the packaging
   baseline reviewed here. Validate using `desktop-file-validate` and
   `appstreamcli validate --explain` per the
   [integration conventions](https://docs.flatpak.org/en/latest/conventions.html).
4. On an isolated builder, run `flatpak-builder --repo=target/flatpak-repo
   target/flatpak-app packaging/flatpak/io.github.tiansongyu.gifromscreen.yml`
   only after that real manifest exists. Bound CI time/disk/parallelism and save
   source, SDK and compiler provenance. Test without host FFmpeg or Cargo caches.
5. Validate first run, private project durability, restart/reopen, input grants,
   folder-scoped export/Save As, cancelled dialogs and denied/revoked access.
   Confirm no ungranted sibling is written and failed exports preserve originals.
6. Run the installed Flatpak in isolated real GNOME and KDE sessions: normal
   ScreenCast Cancel/Share, capture/pause/retarget/stop/GIF, shortcut approval and
   fallback, and Camera grant/cancel/loss. Check X11 picking/guides/input with
   another sandbox present. Never replace these with forged portal responses.
7. Test GPU/CPU rendering and codec availability, then produce an inspectable
   `.flatpak` artifact and checksums. Public repository/Flathub submission comes
   after these gates; manifest existence alone is not delivery evidence.
