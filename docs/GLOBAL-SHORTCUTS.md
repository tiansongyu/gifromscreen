# Global recorder shortcuts

The Linux recorder now has an opt-in shortcut service separate from captured
keystroke metadata. It registers only the configured actions, never all keyboard
input. There is deliberately no destructive Discard shortcut.

## Controls and lifetime

Open **Screen recorder → Global recorder shortcuts → Enable shortcuts**.
Bindings can use F1–F24 or a letter/digit with Control, Alt or Super; duplicate
actions/triggers and unmodified typing keys are rejected. Edit the draft and use
**Apply bindings**. Registration failures require explicit Retry, not repeated
permission prompts every UI frame.

| Default | Action |
|---|---|
| Ctrl+Shift+F7 | Prepare/open the recorder, then start when ready; pause/resume an active recording |
| Ctrl+Shift+F8 | Cancel preparation/countdown, or stop and save an active recording |
| Ctrl+Shift+F9 | Request a frame during running manual-snapshot capture |

Preparation, selection and source authorization are not skipped. Opening a new
controller ends the current batch of queued actions, so rapid presses cannot
start capture before its first presentation/geometry. An active countdown ignores
another start; a stop cancels the countdown without inventing recorded frames.

The enabled preference is saved, but the launcher/editor do not register shortcuts.
Each recorder scope owns at most one registration worker. Leaving that scope,
changing display/bindings, cancelling preparation, finishing a recording or closing
the application invalidates old callbacks and drains old actions before a new
worker may start. Failure keeps recorder buttons and timed stop available.

The settings file is `$XDG_CONFIG_HOME/gifromscreen/shortcuts.json`, or the user's
`.config/gifromscreen/shortcuts.json` fallback. It is bounded to 4 KiB, versioned,
validated and saved atomically in an owner-only file. Existing unsafe permissions,
corrupt settings or conflicting external edits are not silently overwritten.
Loading/saving runs in the background; a late load cannot overwrite new UI edits.
Closing waits for registration cleanup and already-requested settings I/O.

## Active recording does not depend on repaint

Wayland can withhold frame callbacks for an occluded window. Therefore an active
recording's actions go directly from the shortcut thread to its particular cloned
`RecordingController`. They do not wait for egui's next frame. The callback performs
only bounded command dispatch, never window operations or source authorization.
After its recording ends, it consumes stale actions rather than turning them into
a new UI Start. A shared terminal flag prevents actions after Stop.

`toggle_pause()` chooses pause/resume on the capture worker from the actual native
session state, with at most one toggle queued/executing. Explicit pause/resume keep
their ordered semantics. `pause_status()` provides a coherent pending count and
native acknowledgement, so UI state does not depend on seeing every intermediate
progress phase. Pending/failed transitions never supply a privacy-safe Paused
claim. Manual snapshots retain the existing 64-request bound and capture clock.

Before recording, preparation/start remains a UI operation; a compositor that
fully suspends an obscured **ready** window may require that window to be restored.
The background active-recording path does not certify hidden-ready-window activation.

## Backend behavior and limits

X11 resolves the current XKB group/base key and actual modifier map. It registers
explicit key/modifier/lock combinations on the server roots; Caps/Num Lock do not
accidentally disable the configured chord. It verifies detectable auto-repeat,
deduplicates press edges, checks conflicts and rolls back only its own grabs.
Layout/group changes release the session and request explicit re-registration.
It neither changes the global keyboard layout/repeat settings nor subscribes to
raw input. Local Unix, localhost and numeric IPv4/IPv6 displays are supported;
DNS hostnames are rejected rather than claiming that blocking resolution is bounded.
Protocol setup has a five-second budget, cancellation checks at most every 20 ms,
and explicit cleanup is limited to 100 ms before closing the owned connection.
Authentication files are read only, regular and bounded to 1 MiB; filesystem I/O
is cooperative, not a promise to interrupt a kernel-blocked filesystem call.
[X.Org passive-grab contract](https://xorg.freedesktop.org/archive/X11R6.7.0/doc/XGrabKey.3.html).

Wayland uses a dedicated GlobalShortcuts Portal connection/session. It verifies
the actual interface, binds once, and reports the returned subset and desktop
trigger descriptions; preferred triggers are not assumed to be accepted. Signals
are ordered and checked against the service owner, session and known actions.
Before its first portal operation, each host connection registers
`io.github.tiansongyu.gifromscreen` through the identity Registry. Modern frontends
require a nonempty application identity; the matching installed `.desktop` file
is required for host registration. Failures explain installation instead of
creating host files automatically. Only precise missing-Registry compatibility
errors fall back; permission denial and invalid identity are not swallowed.
The root window uses the same app ID, but that does not replace bus registration.
Empty desktop trigger strings are treated as unbound actions; valid remaining
bindings still work. Unknown/repeated IDs, control characters and oversized
descriptions are still rejected.
[Registry contract](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.host.portal.Registry.html).
Cancellation closes the actual pending Request and Session and disconnects the
owned bus connection, without waiting for a dialog response. Interface probing is
bounded to five seconds, each permission step to 120 seconds, object Close to two
seconds, followed by bounded disconnection. Unsupported/denied/cancelled sessions
do not abort a recording. Actual desktop support remains a separate gate.
[Portal contract](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html),
[base-key shortcut syntax](https://specifications.freedesktop.org/shortcuts/latest/).

The fallback UI queue holds at most 64 nonterminal presses. Stop has a separate
priority slot and discards older queued nonterminal actions. Active handlers
instead use the bounded capture-control path. Backend failure revokes actions
before potentially slow cleanup, not only when the UI later observes termination.

## Verification scope

Automated coverage includes private D-Bus permission/lifecycle/forged-signal cases,
private Xvfb focus/locks/conflicts/layout/held-key-release cases, shared queue and
callback revocation, paused-state acknowledgements, settings recovery, small-window
actions and direct active dispatch with no app update. Synthetic repeated KeyPress
events are tested; XTEST on this server did not produce hardware-style autorepeat,
so that physical-device gate is not marked complete.

The QA harness now supports `start --display-server x11` using its own Xvfb,
Mutter and private bus/configuration. Default nested Wayland behavior remains.
Harness readiness is not recorder acceptance: full native app/GIF verification is
recorded separately after execution. Hardware keyboards, additional layouts,
GNOME/KDE/wlroots Portal implementations, hidden-ready activation and the wider
Linux release matrix remain explicit open gates.
