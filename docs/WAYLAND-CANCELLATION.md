# Cancelling Wayland source preparation

The setup job owns one shared cancellation flag from its creation through portal startup. Cancel and dropping the setup handle set it immediately; they do not wait for the command receiver to return from a blocking native startup call.

The normal `CaptureBackend::start_session` interface remains available. Native Wayland setup supplies the shared flag through `WaylandCaptureBackend::connect_cancellable`, and the portal checks it during connection/probing, `CreateSession`, `SelectSources`, `Start`, and remote opening. Connection/probing retains its 10-second deadline.

Ashpd waits for a portal Response before returning its high-level Request object. Cancellation therefore tracks the actual handle tokens serialized in the request options together with the owned D-Bus connection's unique name. Those determine the request/session paths specified by the [XDG Request protocol](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Request.html). The application sends `Request.Close` and `Session.Close` on that same connection; it does not forge, inject, or interpret cancellation as a successful permission Response. Close has a five-second combined deadline, after which the owned connection is relinquished rather than waiting indefinitely.

Cancellation is checked again after successful replies, before handing a granted portal to PipeWire, and while waiting for native format negotiation. A cancelled pending setup never returns a capture session to the recorder. If the PipeWire worker already owns the session, its normal shutdown command retains ownership through teardown; native calls are not synchronously joined after a cancellation/timeout.

Committing the prepared crop adopts the same paused session. Setup detaches its cancellation flag at this handoff, so dropping the old setup handle does not cancel the recording. Recording then uses its own controller/cancellation handle.

## Verification

- An isolated private D-Bus test receives actual Close method calls at the Create/Select/Start token paths, without returning a permission Response.
- Grant/cancel races must run cleanup and return cancellation; an already-cancelled request is never polled.
- Pending native-format negotiation wakes on cancellation, even if a Ready message is already queued.
- A pending chooser test needs no grant to finish cancellation and cannot commit/create a project or GIF output.
- Dropping a connecting job propagates cancellation; the same-session commit test verifies that dropping setup does not cancel its adopted recording.

```sh
cargo test -p gif-from-screen-capture-linux --features native-wayland wayland::cancel
cargo test -p gif-from-screen-capture-linux --features native-wayland startup_cancellation
cargo test -p gif-from-screen --bin gif-from-screen wayland_prepare_job
```

The transport test uses a privately spawned `dbus-daemon`; it never changes the user's session-bus environment. A real trusted-chooser cancellation test is ignored by default and must run only in an isolated desktop:

```sh
GFS_ISOLATED_WAYLAND_TEST=1 cargo test -p gif-from-screen-capture-linux \
  --features native-wayland cancelling_real_pending_chooser_does_not_require_a_permission_response \
  -- --ignored
```
