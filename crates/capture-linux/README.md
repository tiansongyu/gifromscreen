# gif-from-screen-capture-linux

Linux capture adapters for GifFromScreen.

- `native-x11` provides the complete X11 source enumeration and frame capture
  backend.
- `wayland-portal` provides live XDG `ScreenCast` capability discovery and the
  full `CreateSession → SelectSources → Start → OpenPipeWireRemote` lifecycle.
- `native-wayland` adds the complete portal + `PipeWire` capture backend. It
  requires PipeWire development headers on the build host. The Rust bindings
  are pinned to the 0.6 series so a clean dependency resolution remains
  compatible with PipeWire 0.3.48.

The Wayland portal exposes synthetic “choose a screen/window” sources because
the compositor owns source identity and the trusted selection dialog. Source
and cursor requests are checked against live portal bit flags and never
silently fall back. User cancellation is reported as a permission-class error.
`WaylandPortalSession` keeps the D-Bus session alive while its `OwnedFd` is
owned by the native video worker. The portable session closes the PipeWire
stream first and the portal session afterward on stop, discard, error, or
drop.

The native consumer negotiates raw `BGRA`, `RGBA`, `BGRx`, or `RGBx` video and
converts mapped `MemPtr`/`MemFd` planes to tightly packed RGBA8
`CapturedFrame`s. DMA-BUF-only or otherwise unmapped buffers are rejected with
an explicit `UnsupportedCapability` error; there is no silent X11 fallback.
Frames use active-session timestamps (paused time is excluded), cadence is
deadline anchored, and a three-frame bounded channel drops late frames without
rewriting sequence numbers or timestamps. Sequence gaps therefore expose
backpressure to callers. Resuming first drains frames queued before the pause
acknowledgement, so frozen-preview/setup pixels cannot leak into a later
recording interval. A manual snapshot switches the PipeWire worker to an
explicit one-permit gate, drains every pre-trigger setup frame, and releases
only the next post-trigger buffer; idle manual mode therefore does not keep
feeding stale frames into the bounded channel.

Monitor/window choice remains in the trusted compositor dialog. Region capture
is a source-local crop of the selected stream, and `update_target` can move that
crop while recording provided its fixed output dimensions and selected portal
source do not change. Pause/resume toggles the native stream; stop and discard
wait for worker acknowledgement and close the portal lifecycle deterministically.

Build both native Linux paths with:

```sh
cargo check -p gif-from-screen-capture-linux --no-default-features \
  --features native-wayland,native-x11
```
