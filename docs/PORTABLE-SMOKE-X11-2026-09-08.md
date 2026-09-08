# Owned X11 display for portable smoke tests

Portable run `34184700324` for `a2b40da` successfully built and inspected the
archive, then failed before showing the desktop with
`WinitEventLoop ... XNotSupported(XOpenDisplayFailed)`. The separate Linux CI
run `34184700323` passed. The failed smoke step ran under `xvfb-run`; its output
did not retain server state/logs, so the exact cause of the display-open failure
is **not established**. It is not described as a proven application regression
or a proven server-reset race.

The smoke harness now has an explicit `--owned-xvfb` mode used by portable CI.
It starts one supervised `Xvfb -displayfd` child, disables TCP, uses `-noreset`,
and validates a real connection before launching the archive's desktop binary.
The server chooses a free display; inherited DISPLAY/XAUTHORITY do not determine
the test target. No host desktop, window manager or service is replaced. This
temporary local test display uses `-ac` and contains only the smoke application.

Startup parsing is bounded by five seconds and 32 bytes, connection checks have
timeouts, and both the desktop and display children are reaped on normal exit or
failure. Desktop window probes use the same explicit environment and are timed
out rather than hanging indefinitely. Failures include server exit status and a
bounded server-log excerpt. Existing-display mode remains available to callers
that intentionally provide their own display.

`scripts/test_owned_xvfb.py` covers malformed/oversized/missing display-number
output, failed-test cleanup, ignoring an invalid inherited display/auth path and
two overlapping owned displays receiving different server numbers. All three
tests pass locally and run before the packaged-desktop smoke in CI.

Two local runs of the new smoke command successfully showed a visible packaged
window and cleaned up their owned displays. Those runs used the existing
`target/package-current/gifromscreen-0.1.0-linux-x86_64.tar.gz`, whose BUILD-INFO
identifies source `c8fb5a00e3d1d885ba71254f80e88b7fecaef14e`, Rust 1.88.0 and glibc
maximum 2.35. They establish harness behavior, **not** acceptance of a newly
built current-source archive. The new portable CI run must establish that
separately; green local smoke results do not retroactively change the failed run.
