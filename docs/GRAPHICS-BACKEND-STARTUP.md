# Automatic X11 graphics selection

The v0.1.0 desktop forces the GL UI backend under X11. On the reported Ubuntu
22.04 / X11 / NVIDIA RTX 4090 environment (driver 580.142), its exact official
binary fails with `incompatible_surface_backends: Backends(GL)`. The installation
checksum is correct. Vulkan is present and the same binary starts with
`WGPU_BACKEND=vulkan`; this is not evidence that the graphics driver is missing.

## Fix

Automatic X11 startup enables both GL and Vulkan in **one instance and event
loop**. The native adapter selector receives the real window surface. It skips
adapters without surface support, formats, presentation modes, render-attachment
usage, or the features/limits required by eframe's device descriptor. Among the
remaining candidates, GL stays preferred to preserve the previously tested
software-GL transparency path; otherwise Vulkan is selected. Hardware/power
preference ranks devices within each backend before software adapters.

Explicit `WGPU_BACKEND` is still a strict diagnostic restriction, not silently
overridden. `WGPU_POWER_PREF` is honored within each backend. Wayland retains
eframe's existing automatic selection. Window transparency, capture geometry,
saved frames, GIF rendering, project schema and preferences are unchanged.

There is no process-restart loop, post-launch backend switching or driver
installation. A machine with no compatible adapter still reports an actionable
startup error. This change cannot recover arbitrary driver/device failures after
selection or guarantee transparent presentation on every compositor.

## Verification

Eight targeted tests cover incompatible GL → Vulkan, preserving software GL,
incompatible higher-ranked devices, software fallback, power preference, no
compatible candidate, strict overrides and unchanged Wayland configuration.
The existing transparent-window/configuration regression is retained.

Actual development-binary startup was checked on both paths, with private
config/data/state/cache directories, no recording and no keyboard/mouse injection:

| Environment | Automatic result | Explicit diagnostics |
| --- | --- | --- |
| User's X11 / RTX 4090 | Visible, rendered window; `Vulkan (NVIDIA GeForce RTX 4090)` | Forced GL reproduces the old error; forced Vulkan starts |
| Owned Xvfb / Mesa software | Visible, rendered window; `Gl (llvmpipe (LLVM 15.0.7, 256 bits))` | No override needed |

Each successful window stayed alive for at least three seconds and its own
window pixels were inspected. Only the newly launched test processes were
terminated/reaped. No installed file, existing preference or user project was
changed. Evidence: `/tmp/gfs-graphics-fix-native.YBnx9MOP`, development executable
SHA-256 `a99d88dd4eff7c3459836370e651f5d3017dffd58c242f0b1f93710b9e99432f`.
This is startup acceptance, not a new full recording/hardware qualification.

The portable smoke defaults to `--backend auto` and requires a continuously
visible window for one second while the process remains alive. Explicit
`--backend gl` / `--backend vulkan` are diagnostic options. Its owned-Xvfb mode
does not test the user's hardware driver.
Four mocked smoke regressions cover automatic/explicit backend isolation,
an initial window followed by a startup failure, and no-window timeout cleanup;
the portable CI runs them before testing the real archive.

The patch build is versioned **0.1.1** to distinguish it from the immutable
v0.1.0 release. No dependency upgrades or project-format changes are included.

For the unchanged v0.1.0 package, the temporary workaround remains:

```sh
WGPU_BACKEND=vulkan ./bin/gif-from-screen
```
