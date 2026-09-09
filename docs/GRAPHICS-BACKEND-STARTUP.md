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
changed. Evidence: `/tmp/gfs-graphics-fix-native.YBnx9MOP`, early development executable
SHA-256 `a99d88dd4eff7c3459836370e651f5d3017dffd58c242f0b1f93710b9e99432f`.
That probe precedes the patch-version bump; the actual 0.1.1 package is verified
separately below. This is startup acceptance, not full recording qualification.

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

## Verified 0.1.1 patch package and local upgrade

The actual package was built from
[`ce65429feef775bfd69b82218d143cd54d5b3fda`](https://github.com/tiansongyu/gifromscreen/commit/ce65429feef775bfd69b82218d143cd54d5b3fda).
[Linux CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34329731608)
and [portable CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34329731622)
passed. Local Rust 1.88.0 and 1.98.0 each pass strict workspace Clippy and
**1,876 tests**, with **56 ignored**; the four smoke-harness regressions pass too.
This does not turn the previously known WPF pixel differences into passes.

The Ubuntu 22.04 / Rust 1.88.0 artifact has clean source/package receipts and
GLIBC requirements no higher than 2.35. Source, packaging, lockfile and executable
fingerprints were checked. The actual tarball passes all **12 portable tests**
and the updated **automatic** owned-Xvfb desktop smoke locally as well as in CI.

- Archive: `gifromscreen-0.1.1-linux-x86_64.tar.gz`, **27,142,357 bytes**.
- Archive SHA-256:
  `7ac6fcd67bec2d2b8fd60422ad319c6042c751e78cef3a7d1a1d065e059ebb4b`.
- Desktop SHA-256:
  `b08816eff20a4f2f6b0b0fc379ab4d7d9ada853d3051b51e22484fd98d8a114b`.
- CLI SHA-256:
  `c6a8ed55185eae699c7a796cc1fa1cd29efb066a618bc73dd71a5f701b5392ad`.

The **packaged executable**, not only the development binary, repeats the actual
hardware/software startup probes above. Automatic mode selects NVIDIA Vulkan on
the reported workstation and Mesa GL on the owned Xvfb. The expected forced-GL
failure remains reproducible, proving the explicit restriction is still honored.

A verified copy and archive/checksum were placed beside the old version under
`/home/ubuntu/Videos/gifromscreen-0.1.1-linux-x86_64`. The existing user installation
at `/home/ubuntu/.local` was verified against the old package's ownership manifest
before replacing its **575 registered application files** through the checked
installer. The desktop menu and executable links now resolve to 0.1.1. Existing
preferences have the same hash, and the original Videos/0.1.0 package is intact
for rollback. No user projects or unlisted files were removed. The installed
desktop executable also starts automatically on NVIDIA Vulkan without an override;
its own rendered window was inspected and its test process cleaned up.

Evidence is retained in `/tmp/gfs-graphics-011-package.rW0gXVQi`, including
`verification.json`, actual package probes, `local-upgrade.json` and
`installed/results.json`. This is a local/CI patch build; the v0.1.0 GitHub Release
assets and tag have not been replaced, and no separate v0.1.1 Release is claimed.
