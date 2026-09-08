# First desktop language slice — 2026-09-08

This is partial UI localization, not completion of the 29-language target.
The launcher/navigation, recent-project controls and language panel use typed
English/Simplified Chinese messages. The language panel reports the initial
catalog denominator separately from full-interface coverage. Other registered
choices remain selectable and persist, but explicitly use English fallback.
Unknown valid saved tags are retained rather than overwritten with `en`.

## Behavior and storage

- First launch defaults to System. Linux environment negotiation is injected,
  bounded and independent of process-wide `setlocale`; C/POSIX forces English.
- Manual selection takes effect in memory immediately and saves asynchronously.
  Changing back to System saves the policy, not today's detected language.
- `$XDG_CONFIG_HOME/gifromscreen/preferences.json`, otherwise absolute
  `$HOME/.config/gifromscreen/preferences.json`, is independent of projects,
  shortcut settings and recent-project state. The application directory must be
  private and owned by the user. Symlinked ancestors are intentionally rejected;
  errors are shown, never silently repaired with chmod or file replacement.
- Files are bounded to 4 KiB, version checked and mode 0600. Pinned directory
  handles, no-follow/nonblocking opens, owner/link checks, a try-only lock,
  raw-byte stale-writer detection, atomic rename and fsync protect writes.
  A post-rename directory-sync error reports that replacement already occurred;
  it does not pretend the previous bytes survived.
- A late load cannot replace a newer UI choice. Edits during a save get a
  following save. Failed/conflicting saves are not automatically rebased over
  another writer: explicit reload discards unsaved choices.
- Polling precedes the recorder early returns. Closing waits for queued/running
  preference I/O; a closing/deferred-shutdown frame cannot accept a final new
  preference edit after its close gate has run.

## Font and automated coverage

The unchanged official Noto Sans CJK SC Regular 2.004 face (16,437,364 bytes)
is embedded after all original proportional/monospace fallbacks. It supplies CJK
character coverage using **SC regional glyph forms**, not language-dependent
Japanese/Korean/Hant substitutions. It adds neither BiDi nor complex shaping.
Provenance, original OFL and Adobe copyright are in
[`apps/desktop/assets/fonts`](../apps/desktop/assets/fonts/README.md); portable
packages carry readable notices as a separately identified embedded-font entry.

Tests cover registry and locale negotiation, strict named placeholders and
literal user data, en/zh key parity, explicit fallback, private storage and
stale writes, async load/save ordering, application shutdown, and stable language
window identity. Font tests inspect the actual cmap, render through egui, compare
unchanged Latin/emoji galley and atlas pixels, and preserve PPP/user zoom.
Both Rust 1.88 and 1.98 are acceptance targets; complex-script editing/IME and
all remaining screens require separate acceptance.

## Native X11 observation

Owned private Xvfb/GNOME lab: `/tmp/gfs-wayland-qa.6k6d32ga`. This used an
intermediate debug binary while the slice was being completed, not a published
release or the later recent-project translation. Its SHA-256 was
`43352b4e1909280e02364b369d30f44d872b5d5896c9bc6f145196b8ce4c8703`.
The recorded Git revision was the pre-edit `d928a39`; the binary included the
working tree's initial 72-message UI integration.

1. With an empty private config and `LC_ALL=zh_CN.UTF-8`, `LANGUAGE=zh`, the
   application opened its launcher in readable Chinese (`02-system-home.png`).
2. The language window displayed System/Chinese and the incomplete-interface
   notice; its drop-down showed actual options (`03`/`04` screenshots).
3. Clicking English changed visible launcher and settings text immediately and
   displayed Saved (`05-english-saved.png`). The resulting JSON had
   `format_version: 1`, `language: {mode: explicit, tag: en}`, mode 0600.
4. The initial application was closed through its normal titlebar control.
   Reopening the same frozen executable with the same Chinese environment kept
   the English choice (`06-restarted-english.png`, `07-restarted-choice.png`).
   Reading did not rewrite the preference bytes. The repeated restart check
   exited normally with status 0 through the titlebar control.

The first restart harness's Alt+F4 did not close the window within its 5-second
observation period; its owned process was cleaned up, and the rerun used the
normal titlebar close. This is not counted as a successful Alt+F4 test.
The lab was explicitly stopped after these checks. Evidence is local under its
`logs/` directory, not a release asset or proof of physical desktop acceptance.

Still open: complete translation of every application-owned screen/error,
the other 27 catalogs, RTL/complex-script layout and text input, every supported
locale's native layout/IME checks, physical desktop coverage, and release QA.

Final slice checks: 76 typed en/zh messages; 24 localization tests; 682 desktop
tests (including 29 preferences/font tests and 3 new recent-project UI tests).
Both crates pass on Rust 1.88.0 and 1.98.0. The full Rust 1.98 workspace with all
targets/features passes 1,610 tests, with 51 explicitly ignored environment or
benchmark gates. Strict Clippy uses Rust 1.98.0. A separate Rust 1.88 strict
Clippy run stops at the pre-existing `needless_range_loop` in
`crates/render/src/renderer.rs`'s source-over blending loop; this is not a failed
MSRV compile/test and is not reported as a successful strict 1.88 run.
The portable builder's 12 tests include verified font-notice staging and
rejection of missing or modified source/font/license material.
