# Localization implementation and acceptance plan

Frozen for **v0.1.0** on 2026-09-09: System + 29 saved choices, 848 English /
Simplified Chinese messages, and explicit English fallback for the other 27
targets. Remaining translation work is deferred, not completed. See the
[release work summary](WORK-STATUS.md) for the current scope; the sections below
retain implementation details and future acceptance requirements.

Status: advanced editor language slice added on main, 2026-09-09. The
`gif-from-screen-localization` crate provides the registry, validated preference
wire format, injected-system-locale negotiation and an initial English/Simplified
Chinese message catalog. The desktop now offers System plus 29 saved choices,
asynchronous private preference storage, immediate launcher/navigation switching
and an embedded licensed CJK fallback. Other catalogs explicitly fall back to
English. The recording form, source-local Wayland controls, compact X11 controls,
window snapping, shortcut settings, and editor navigation/Frames/Timing/Transform
now use explicit localizers. [Preview/crop and GIF export controls](PREVIEW-EXPORT-LOCALIZATION.md)
also include typed parameter-validation, preset and completion notices. Typed
notices re-render at presentation time. [Advanced editor controls](ADVANCED-EDITOR-LOCALIZATION.md)
extend this to effects, shapes/drawing/layers, project storage and statistics.
The [image-watermark slice](WATERMARK-LOCALIZATION.md) now covers its form,
typed validation and retained asynchronous outcomes. The [text/title slice](TEXT-TITLE-LOCALIZATION.md)
adds caption creation, saved-text editing, title insertion and typed text/font
errors, bringing the en/zh catalog to 848 messages. Shared toolkit color-picker
tooltips, import forms and other tool bodies still require migration; see
[editor/notice scope](EDITOR-LOCALIZATION.md).
The target includes the entire application-owned UI, not merely language selection.
See [the first desktop acceptance record](LOCALIZATION-QA-2026-09-08.md).
The [recorder integration contract](RECORDER-LOCALIZATION.md) separates automated
geometry/control tests from native acceptance and records the remaining text scope.

## Target languages and reference

Working assumption for “all language options”: **System plus the 29 actual
ScreenToGif 2.43.2 language catalogs**, at commit
`a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd`:

```text
ar, zh, zh-Hant, cs, da, nl, en, en-GB, fi, fr,
de, el, he, hu, it, ja, ko, pl, pt, pt-PT,
ta, tr, es-AR, es, sw, sv, ru, uk, vi
```

XML parsing confirms 30 active options including `auto`, and 29 resource files.
Commented-out Romanian and duplicate Greek entries are not extra supported languages.
The reference persists `LanguageCode`, defaults to `auto`, selects the machine UI
culture, and falls back through parent cultures to English:
[language settings](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Views/Settings/LanguageSettings.xaml),
[culture selection](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif.Util/LocalizationHelper.cs#L27).
This is a coverage target, not permission to label unreadable/incomplete locales ready.
Upstream resources are Ms-PL, not this project's MIT/Apache-2.0; importing whole
translations requires separate attribution/license handling. Do not copy branding,
flags or execute/import WPF XAML as a translation format.

## Current code and reusable pieces

The baseline had no application locale implementation or global UI-language preference.
The foundation now exists independently of the desktop; it deliberately marks
all registry entries `TranslationReadiness::NotProvided`, preserving the
difference between a target identity and a translated/readable interface.
A rough `rg` audit found 656 common UI method-call candidates across 26 desktop
files; this includes repeated/test calls and is **not** a complete message count.
`main.rs` and `editor_ui.rs` dominate, followed by automation, annotations, source
recorders and secondary tool panels. Constructors, enum labels, formatted notices,
backend errors, file-dialog titles/filters and native window titles add more text.

- `main.rs::main`/`GifFromScreenApp::update` own initialization and all view routes.
  Preference polling must precede recorder early returns and join the shutdown gate.
- `appearance.rs::configure` changes style only; `preferences::fonts::install`
  now appends the embedded Noto Sans CJK SC 2.004 face to the original egui fonts.
- `shortcut_ui/store.rs` provides owner-only bounded files, atomic replacement,
  fsync, locking and stale-writer detection; `persistence.rs` supplies asynchronous
  load/save and “newer in-memory edit beats late load” behavior.
- Use a separate `$XDG_CONFIG_HOME/gifromscreen/preferences.json` (otherwise
  absolute `$HOME/.config/...`), not project manifests, `shortcuts.json`, or the
  state-directory files for recent projects/automatic tasks. Extract a small shared
  atomic-byte-store primitive if needed; do not duplicate another large settings stack.
- `sys-locale` 0.3.2 is already locked transitively through cosmic-text; making it
  a direct desktop dependency reuses its cross-platform detection, not a shell command.

## Preference, negotiation and switching contract

Persist a versioned `LanguagePreference::System | Explicit(tag)`; default to System.
Keep the requested tag separate from the resolved supported catalog. A valid but
unavailable saved tag falls back visibly without silently overwriting that choice.
Reject malformed/oversized preferences while preserving their original bytes.

For Linux System mode, apply an explicit, tested policy: `LC_ALL`, then
`LC_MESSAGES`, then `LANG` determine the effective message locale. C/POSIX,
including their codeset forms such as C.UTF-8, resolve to English. Otherwise use
the nonempty colon-separated `LANGUAGE` preferences before that effective locale.
This C/POSIX guard belongs to the application: sys-locale's Unix implementation
returns LANGUAGE first without that guard. Normalize `_`/codeset/modifier notation,
bound candidate count/tag size, and negotiate only supported tags. Map Chinese
script/region aliases deliberately (`zh-Hans`/CN/SG → `zh`; Hant/TW/HK/MO → `zh-Hant`);
retain `pt-PT`, `en-GB` and `es-AR` rather than collapsing every region.
An explicit application choice wins over environment detection. Missing or invalid
machine preferences end at English. Never mutate LANG/LC_* or call global setlocale.
See [sys-locale's API](https://docs.rs/sys-locale/0.3.2/sys_locale/) and its pinned
local `src/unix.rs`; unit tests inject environment values rather than altering them.

Switch immediately in memory, schedule an atomic save, and show Saving/Saved/error.
A failed save must not falsely promise restart persistence. Late loads cannot undo
a newer selection; retry and conflict handling must be explicit. Poll/save continues
when recorder pages replace the root UI. Closing waits for pending preference I/O.
System remains a live policy on the next launch, not a one-time detected tag saved
as an explicit override. No project/capture operation restarts just to change language.

## Messages, catalogs and genuine coverage

The initial slice uses a small embedded typed `Message` table with stable IDs,
mandatory en/zh entries and strict named parameters. No new parser dependency is
needed for these static sentences; bounded formatting scans the template once
and never interprets braces inside user data. Per-message fallback and coverage
are explicit. This is not a substitute for plural rules in the full catalog.

For the full plural/selectable-message migration, evaluate embedded Fluent
behind the existing `Localizer` interface. `fluent-bundle` 0.16.0's declared
MSRV is 1.67; still verify its resolved graph on this project's Rust 1.88 baseline.
Use named typed parameters, plural selectors and full sentences, not concatenated
translated fragments or positional English `format!` templates:
[Fluent API](https://docs.rs/fluent-bundle/0.16.0/fluent_bundle/),
[variables and plural selection](https://projectfluent.org/fluent/guide/variables.html).
Parse embedded catalogs once, bound formatting output/errors, and fall back per
message to complete English. Missing keys/parameters are test failures and visible
coverage debt, not a successful translation. A blank catalog is never “supported”.

Migrate application notices to message ID + arguments where they remain on screen;
otherwise switching locale leaves old `Option<String>` messages in the old language.
Workers should return typed outcomes/errors where practical, with localization at
presentation time. Localize the application's explanation of external failures;
preserve raw OS/Portal/FFmpeg diagnostic details separately, never string-match them
as translation keys. External portal chrome and other applications' window titles
remain controlled by those applications, not this catalog.

Cover launcher/navigation, recorder states and controls, import/create dialogs,
editor/timeline/layers, all authoring tools, automation/presets, export/progress,
shortcuts, recovery/errors, tooltips, file filters and application window titles.
Keep widget IDs/id salts independent of localized labels. Never translate existing
user subtitles, paths, project/layer/preset names, protocol keys, IDs or saved pixels.
Numbers used for editing/serialization retain their documented parsing contract;
localized display formatting must not change durations, coordinates or file bytes.
Track extracted IDs, translated IDs, parameter parity and reachable-screen coverage
separately; English fallback cannot inflate a locale's translated percentage.

## Text rendering is a separate release gate

Read-only `fc-query` on the four shipped epaint 0.32.3 fonts found coverage for
`Start Stop GIF`, but **none** of the characters in `中文日本語한국어`, `العربية`,
`עברית`, or `தமிழ்` across their union. This is an actual cmap probe, not native GUI
rendering/IME acceptance. Default fonts are Ubuntu-Light, Hack and two emoji fonts;
system-installed CJK fonts do not automatically become egui fallback fonts.
Bundle or explicitly load licensed, versioned UI fonts with bounded memory, then
test every catalog's glyph coverage and native rendered output. Record font sources,
hashes and notices in packaging. GIF authoring's cosmic-text is not the UI renderer.

Current epaint 0.32.3 lays out characters with pair kerning, not full script shaping;
its font code explicitly leaves BiDi handling as TODO and ignores directional
controls. RTL widget placement is not Unicode BiDi text, and Grid has an RTL limit:
[pinned text layout](https://github.com/emilk/egui/blob/af96e0373c18477b77236e2bfc89735af007b1c2/crates/epaint/src/text/text_layout.rs#L148).

Official latest **0.36.1** adds the harfrust shaping introduced in 0.35 and improved
IME handling, but still documents missing script-aware/BiDi segmentation and RTL
limitations. Its MSRV is **1.95**, with wgpu 30 rather than our 25; upgrading would
also change input APIs and invalidate the Rust 1.88/package baseline. It is not an
automatic all-language fix:
[release](https://github.com/emilk/egui/releases/tag/0.36.1),
[remaining BiDi boundary](https://github.com/emilk/egui/blob/0.36.1/crates/epaint/src/text/text_layout.rs#L1363),
[toolchain requirements](https://github.com/emilk/egui/blob/0.36.1/Cargo.toml#L27).

Keep the existing GUI architecture while establishing a measured text-integration
decision. Arabic/Hebrew/Tamil require correct shaping, BiDi where applicable,
wrapping, logical/visual cursor mapping, selection, clipboard and IME verification.
Existing cosmic-text/rustybuzz/unicode-bidi are reusable building blocks, not a
drop-in egui TextEdit fix. Do not reverse strings, replace text with unselectable
pictures, blindly fork the framework, or enable unreadable catalogs as complete.

## Phases and completion criteria

Foundation verification: 15 tests pass on Rust 1.88.0 and 1.98.0, with strict
Clippy and formatting checks. They cover registry identities, bounded BCP 47
syntax, preserved unavailable choices, strict versioned serde, injected Linux
precedence/C/POSIX, Chinese aliases and regional/parent fallbacks. The crate
never accesses or modifies process environment, files, capture sessions or
projects. These tests do **not** prove restart persistence or UI translation.

1. Establish System/override negotiation, persistent preferences, English message
   inventory and all 29 catalog identities; report actual readiness, not empty keys.
2. Complete English/Chinese UI migration and licensed font loading, then remaining
   Latin/Cyrillic/Greek/CJK catalogs with placeholder and rendered-layout checks.
3. Close the shaping/BiDi/TextEdit gate before claiming ar/he/ta ready. Preserve
   logical user text and test mixed paths/numbers/scripts; select a proven integration
   based on this evidence before authorizing a framework/MSRV change.
4. Require complete application-owned catalog coverage for all target languages,
   fallback/invalid-config/save-conflict tests, restart persistence, language switching
   during work, native fonts/IME and clipping tests at supported sizes/zoom levels.
   Language/font changes must not alter the capture canvas or revive a stale native
   picker: retain stable widget IDs and cancel a claimed gesture if its layout changes.
5. After language behavior is stable, rewrite README using the ScreenToGif-style
   feature/download structure, record **real product-operation GIFs**, and provide
   verified Release quick-download links. Do not record untranslated placeholder UI
   or advertise unavailable artifacts. This follows, rather than replaces, the
   remaining packaging/source-material work and native release checks.
