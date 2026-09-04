# gif-from-screen-capture-linux

Linux capture adapters for GifFromScreen.

- `native-x11` provides the complete X11 source enumeration and frame capture
  backend.
- `wayland-portal` provides live XDG `ScreenCast` capability discovery and the
  full `CreateSession → SelectSources → Start → OpenPipeWireRemote` lifecycle.
- `native-wayland` additionally links `pipewire-rs`; it requires PipeWire
  development headers on the build host.

The Wayland portal exposes synthetic “choose a screen/window” sources because
the compositor owns source identity and the trusted selection dialog. Source
and cursor requests are checked against live portal bit flags and never
silently fall back. User cancellation is reported as a permission-class error.
`WaylandPortalSession` keeps the D-Bus session alive while a caller transfers
its `OwnedFd` to a PipeWire consumer, and closes the session on explicit close
or drop.

This vertical slice ends at the real PipeWire node/remote handoff. It does not
yet claim to implement the portable `CaptureBackend`: format negotiation,
buffer conversion, cadence delivery, and pause/stop integration still need to
connect the PipeWire stream callback to `CapturedFrame`. Consequently
`LinuxCaptureBackend::initialize_native` reports this remaining boundary as an
actionable uninitialized error on Wayland instead of returning a fake backend
or falling back to X11.
