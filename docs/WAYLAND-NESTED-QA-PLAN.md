# Isolated nested GNOME / Portal / PipeWire acceptance plan

Status: **original planning snapshot**. The initial 2026-09-06 investigation was read-only. The harness has since been implemented and run; actual outcomes are recorded separately in [the native acceptance results](WAYLAND-NESTED-QA-RESULTS-2026-09-06.md). The proposal below is retained as planning evidence, not as a current statement that the scripts are absent or the tests are unexecuted.

This is the next stage after the current source, native X11 checks and Linux package are frozen. First capture one dedicated test window. Only after that succeeds, test monitor capture and controller occlusion. Nested acceptance cannot replace GNOME/KDE hardware, mixed-DPI, multi-monitor or DMA-BUF acceptance.

## 1. Read-only feasibility evidence

| Component | Installed evidence | Consequence |
| --- | --- | --- |
| GNOME Shell / Mutter library | 42.9 / `libmutter-10-0` 42.9 | `gnome-shell --help` advertises `--nested`, `--wayland`, `--no-x11`, `--wayland-display`, `--sm-disable`. |
| Portal frontend | `xdg-desktop-portal` 1.14.4 | Backend selection must follow this version, not assume newer `portals.conf` behavior. |
| GNOME portal backend | 42.1, `/usr/libexec/xdg-desktop-portal-gnome` | Installed `gnome.portal` advertises ScreenCast and `UseIn=gnome`. |
| PipeWire | 0.3.48 | Exercises the older supported native ABI. |
| Session manager | `pipewire-media-session` 0.4.1 | WirePlumber is absent, but a session manager is already available. No installation is required for the first attempt. |
| Harness prerequisites | Xvfb, xauth, dbus-run-session, gdbus, pw-cli, zenity | Separate X display, bus, readiness probes and a GUI fixture can be built. |
| Software rendering / fixture | Mesa software DRI, Lavapipe x86_64 ICD, Python GI, GTK 3 introspection | A software-rendered nested compositor and a numbered GTK test pattern are plausible, not yet verified. |

GNOME documents `dbus-run-session -- gnome-shell --nested --wayland` as the separate-session nested route. [GNOME nested testing](https://wiki.gnome.org/Initiatives%282f%29Wayland%282f%29GnomeShell%282f%29Testing.html)

Media Session supports an independent configuration directory. Its installed default **enables `v4l2`**, so launching it with its defaults is unsuitable for this lab. [Media Session configuration and modules](https://pipewire.pages.freedesktop.org/media-session/)

## 2. Isolation requirements

The harness must implement all of these before launching a service:

1. Allocate a fresh `mktemp -d /tmp/gfs-wayland-qa.XXXXXX` directory with mode 0700 and `umask 077`. Create private runtime, config, cache, data, state, output and log subdirectories. Never assign, change, replace or delete `HOME`; all test output paths must be explicit children of the lab directory.
2. Use a new `xvfb-run --auto-servernum` display. Do not reuse the root agent's current `:0` or a real user's display for this phase. Keep that display's Xauthority scoped to the harness.
3. Start a new bus with `dbus-run-session --config-file=...`. The generated bus configuration must listen below the private runtime directory, use EXTERNAL authentication, and **omit standard service directories and systemd activation**. Launch the necessary portal processes explicitly. Do not run `systemctl --user`, `dbus-update-activation-environment --systemd`, a user-bus import command, or `gnome-session`.
4. Inside that new bus, point `DBUS_SYSTEM_BUS_ADDRESS` at the same private bus. Missing system interfaces must produce a lab limitation, never trigger fallback to the host system bus. This intentionally prevents GNOME/RTKit/logind/network/power requests from reaching real system services.
5. Set private `XDG_RUNTIME_DIR`, `XDG_CONFIG_HOME`, `XDG_CONFIG_DIRS`, `XDG_CACHE_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `PIPEWIRE_RUNTIME_DIR`, `PIPEWIRE_CONFIG_DIR` and `MEDIA_SESSION_CONFIG_DIR`. Restrict `XDG_DATA_DIRS` to system read-only resources in `/usr/share`; no existing user data/config directories participate. The daemon and every client must use the same private PipeWire socket. Clear inherited `PIPEWIRE_REMOTE`, PipeWire config name/prefix, `WAYLAND_SOCKET`, Wayland display, D-Bus starter/session/system addresses and desktop/session-manager identifiers before entering the lab.
6. Use `GSETTINGS_BACKEND=memory`; disable the accessibility bridge for this protocol-only lab. Set `PULSE_SERVER` to a nonexistent socket under the private runtime directory. Do not start pipewire-pulse, a full settings-daemon suite, audio/BlueZ/ALSA/V4L2/libcamera monitors, or hardware seat management.
7. Start GNOME with `--nested --wayland --no-x11 --sm-disable`, never `--replace` or `--display-server`. Software rendering must be explicit. Do not enable GNOME unsafe mode or use Shell Eval to skip UI interactions.
8. Record each process's actual child PID, command and start identity. Cleanup may signal and wait for **only those children**, in reverse dependency order. No `killall`, `pkill`, broad process matching, global service restarts or recursive cleanup of a variable that has not been validated. Preserve the owned lab directory and its evidence by default.

The private bus configuration should have a substituted, validated absolute socket path; D-Bus XML must not be assumed to expand shell variables:

```xml
<busconfig>
  <type>session</type>
  <listen>unix:path=__PRIVATE_RUNTIME__/bus</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow own="*"/>
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
  </policy>
</busconfig>
```

No service directories are listed deliberately. If GNOME cannot start without additional isolated services, retain the logs and stop this stage; do not weaken isolation to obtain a green result.

## 3. Proposed harness files

These are implementation tasks, not existing scripts:

- `scripts/qa/wayland-nested.sh`: allocate private directories; sanitize inherited connection variables; render validated configuration templates; enter Xvfb and private D-Bus; record environment names, versions, source commit and PIDs without dumping unrelated environment values.
- `scripts/qa/wayland-nested-session.sh`: start only the specified children; wait for each readiness condition with a deadline; launch the fixture and app; keep the session alive during GUI acceptance; clean up only its own PIDs.
- `scripts/qa/wayland-fixture.py`: GTK 3 native-Wayland window titled `GifFromScreen Wayland QA`, with a visible frame counter, four distinguishable colored quadrants and an independently movable marker. It must neither open a camera nor read keyboard events globally. The available Python GI package is sufficient for this test-only fixture; application code remains Rust.
- Private D-Bus, PipeWire and Media Session configuration templates, plus an acceptance report recording exact source, package hash, selected source kind, negotiated buffer format, crops, timings, screenshots and exported GIF checks.

For PipeWire 0.3.48, derive a private daemon configuration from the installed `/usr/share/pipewire/pipewire.conf`, then explicitly omit realtime/RTKit, hardware node factories/objects and child-execution entries that are not needed. Keep protocol-native, metadata, client-node/device, SPA node factory with the support dummy driver, access/portal support, adapter, link factory and session-manager facilities needed by screen casting. Verify the resulting graph has no ALSA, camera or Bluetooth devices before testing.

For Media Session 0.4.1, use an explicit private configuration instead of its installed default. Preserve its necessary core `context.modules` (protocol-native, client-node, client-device, adapter, metadata, session-manager), omit RTKit and hardware SPA mappings, and allow only:

```text
session.modules = {
    default = [ flatpak portal suspend-node policy-node ]
}
```

Do not copy the installed `v4l2` default or `with-audio`, `with-pulseaudio`, `with-jack`, ALSA, BlueZ or logind bundles. This fragment is not a complete stand-alone config; the harness must supply the core context modules as well. Portal and session-manager permissions must remain normal: no `pw-cli permissions` grants, allow-all permission-store edits or manufactured authorization responses. [PipeWire portal access model](https://pipewire.pages.freedesktop.org/pipewire/page_portal.html)

## 4. Launch sequence to implement

The outer script should construct the following command only after its directories and templates have been created and validated. `gfs_lab` and `gfs_app` are task-specific variables; the normal home remains untouched.

```bash
env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET \
    -u DBUS_SESSION_BUS_ADDRESS -u DBUS_STARTER_ADDRESS -u DBUS_STARTER_BUS_TYPE \
    -u DBUS_SYSTEM_BUS_ADDRESS \
    -u SESSION_MANAGER -u XDG_SESSION_ID -u GNOME_SETUP_DISPLAY \
    -u PIPEWIRE_REMOTE -u PIPEWIRE_CONFIG_NAME -u PIPEWIRE_CONFIG_PREFIX \
    -u GDK_BACKEND -u QT_QPA_PLATFORM -u G_DEBUG \
    XDG_RUNTIME_DIR="$gfs_lab/runtime" \
    XDG_CONFIG_HOME="$gfs_lab/config" XDG_CACHE_HOME="$gfs_lab/cache" \
    XDG_DATA_HOME="$gfs_lab/data" XDG_STATE_HOME="$gfs_lab/state" \
    XDG_CONFIG_DIRS="$gfs_lab/config-system" XDG_DATA_DIRS=/usr/share \
    PIPEWIRE_RUNTIME_DIR="$gfs_lab/runtime" \
    PIPEWIRE_CONFIG_DIR="$gfs_lab/pipewire" \
    MEDIA_SESSION_CONFIG_DIR="$gfs_lab/media-session" \
    XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_DESKTOP=gnome XDG_SESSION_TYPE=wayland \
    GSETTINGS_BACKEND=memory NO_AT_BRIDGE=1 \
    PULSE_SERVER="unix:$gfs_lab/runtime/no-pulse-server" \
    LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe \
    VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json \
    MUTTER_DEBUG_DUMMY_MODE_SPECS=1280x720 \
    xvfb-run --auto-servernum \
      --server-args='-screen 0 1440x1000x24 +extension GLX +extension RENDER -nolisten tcp' \
      dbus-run-session --config-file="$gfs_lab/private-bus.conf" -- \
      scripts/qa/wayland-nested-session.sh "$gfs_lab" "$gfs_app"
```

Inside the private bus, the proposed inner script must immediately set `DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"`. Its service commands, each tracked as a separate child with an owner-only log, are:

```text
pipewire -c <private absolute pipewire.conf>
pipewire-media-session -c <private absolute media-session.conf>
gnome-shell --nested --wayland --no-x11 --sm-disable --wayland-display=gfs-qa-wayland
/usr/libexec/xdg-permission-store
/usr/libexec/xdg-desktop-portal-gnome --verbose
/usr/libexec/xdg-desktop-portal --verbose
```

Wait for PipeWire first; then GNOME's private Wayland socket and `org.gnome.Mutter.ScreenCast`; then launch portal helpers with `WAYLAND_DISPLAY=gfs-qa-wayland` and `GDK_BACKEND=wayland`. Launch the GTK fixture and the frozen app binary with those Wayland variables, from the private output directory. Leave `DISPLAY` available only for the nested compositor and screenshots/normal pointer clicks on **its dedicated Xvfb**; verify the app actually selected its Wayland backend.

Portal 1.14.4 initializes document and permission-store proxies early. Permission Store is listed explicitly above. The first screen-cast-only pass must not start a FUSE document portal or exercise file-chooser portals implicitly; if an absent Documents service prevents ScreenCast from becoming ready, record it as a missing isolated dependency rather than mounting into the real runtime. This prerequisite needs execution evidence. [Portal startup](https://raw.githubusercontent.com/flatpak/xdg-desktop-portal/1.14.4/src/xdg-desktop-portal.c), [document proxy initialization](https://raw.githubusercontent.com/flatpak/xdg-desktop-portal/1.14.4/src/documents.c)

Readiness must use bounded checks, not a fixed sleep followed by assumptions:

- Check child liveness and private socket paths.
- On the private bus, query `org.freedesktop.DBus.NameHasOwner` for `org.gnome.Mutter.ScreenCast`, `org.freedesktop.impl.portal.desktop.gnome` and `org.freedesktop.portal.Desktop`.
- Query `org.freedesktop.DBus.Properties.Get` at `/org/freedesktop/portal/desktop` for `org.freedesktop.portal.ScreenCast.AvailableSourceTypes` and `AvailableCursorModes`; save the actual values, not an expected constant.
- Run `pw-cli -r pipewire-0 info 0` / `list-objects` with the private runtime environment. Assert the inspected graph belongs to the launched daemon and contains no real hardware device nodes.
- Record renderer identity and negotiated buffers. SHM/MemFd acceptance under software rendering is not DMA-BUF acceptance.

## 5. GUI authorization and first acceptance: one test window

1. Start the fixture and GifFromScreen as native Wayland clients inside the nested GNOME desktop. Set project/GIF destinations explicitly under the private output directory. Input-event recording stays off; Wayland global keys/clicks remain unavailable.
2. In the app, choose Wayland recording. In the **real GNOME portal chooser**, select the fixture window and click Share using normal visible UI interaction. A prior Cancel attempt must return an actionable cancellation without creating a recording or leaving a portal session. Do not patch a permission database, pre-grant access, substitute a portal or programmatically forge its response.
3. Verify the frozen preview shows the fixture's counter/quadrants. Select a bounded crop and start recording. The main editor should hide; the recording controller remains usable. Since the source is the dedicated fixture window, the controller cannot recursively appear inside that window's captured surface.
4. Move the fixed-size crop between distinct fixture quadrants during recording, and again while paused. Verify movement acknowledgements, matching recorded pixels and an unchanged native portal session. Reject unsupported canvas resizing explicitly.
5. Verify pause/resume, start delay, time limit, manual snapshots, stop, discard, source/window closure and cancellation. During pause, the fixture continues changing but no paused frames or paused wall time should enter the project. Base assertions on frame markers and active clock, not a fragile number of frames per wall-clock sleep.
6. Reopen the saved project, check journal recovery/assets, and decode its exported GIF. Confirm captured regions, marker order, durations, dimensions and cursor mode. Repeat hidden and embedded cursor variants through normal authorization; editable cursor and global input events must not silently fall back to unsupported Wayland behavior.

## 6. Second acceptance: monitor source and occlusion

Only after the window pass succeeds, authorize the nested monitor through the normal portal UI. Place the fixture and controller visibly apart, choose a crop containing just the fixture, and verify:

- the main editor hides before capture;
- toolbar/window borders remain outside the selected crop;
- moving the crop while recording and while paused selects the intended monitor pixels;
- intentionally moving the controller into the selected rectangle produces observed/documented compositor behavior, not a claim that Wayland can exclude it automatically;
- the application explains any controller-occlusion or global-position limitation and does not present its source-local preview rectangle as an unrestricted desktop overlay.

Capture screenshots of both the normal and deliberate-overlap cases, and inspect decoded GIF frames. Continue to list GNOME/KDE physical desktops, hardware rendering, mixed-DPI and multiple real monitors as separate pending gates.

## 7. Completion and cleanup evidence

Do not mark this plan accepted until a report includes the actual source commit and binary hash, installed versions, renderer, readiness properties, normal consent/cancel screenshots, window-source and monitor-source outcomes, frame/timing checks, project recovery, exported GIF inspection and teardown results.

On success or failure, stop only the registered fixture/app/portal/GNOME/session-manager/PipeWire children; let `dbus-run-session` and `xvfb-run` release their own bus and X server. Verify no registered child is still alive and no private portal or PipeWire socket remains active. Preserve bounded logs and synthetic artifacts in the owner-only lab directory. Verify the original display/session processes and configuration files were not restarted, replaced or modified.
