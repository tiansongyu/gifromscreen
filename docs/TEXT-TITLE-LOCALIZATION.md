# Text and title tool: localized application UI

The application-owned `text_overlay_ui.rs` tool now presents English and
Simplified Chinese for all three existing workflows: add a caption, load/edit a
saved text group, and insert a title frame. This is a bounded UI migration, not
a claim that the entire application or every toolkit-supplied popup is translated.

## Included

- Caption prompt; editable font-family field, font size and alignment; text and
  background colors; position and text-box bounds; automatic-size explanation.
- Saved-text selector, its absolute layer numbers and frame/timed-item counts,
  New text and Save text changes. Equal names still select the exact `TrackId`.
- Title mode, At start / After current frame, duration, canvas background and
  Insert title frame. Title and caption authoring remain distinct operations.
- Preparing, cancelling, cancelled, worker-exited, stale-target and completion
  notices, including results received while the tool is hidden.
- Exhaustive typed `TextError` presentation: empty/overlong text, invalid or
  unavailable font, font size, invisible color, box dimensions, missing glyphs,
  insufficient box space, glyph-memory budget and allocation failures.

The catalog has **848 en/zh messages**, adding 52 to the previous 796. A parsed
comparison verifies every previous variant, ID, parameter contract and en/zh
template is unchanged. All other 27 catalog targets still explicitly fall back
to English; coverage does not claim the complete interface is translated.

## Preserved contracts

Font family names (including `sans-serif`), entered text, saved group names,
technical units and raw backend diagnostics remain data, not translation keys.
Whitespace handling and UTF-8 byte limits are unchanged. Message arguments use
fixed named parameters and are never recursively interpreted. Known text errors
retain their enum identity through the worker channel; workspace and OS errors
remain literal arguments in localized application wrappers, without matching
English strings.

The worker still receives a frozen request, position, operation and strict
project/selection anchor. Later form edits cannot change queued work. Cancellation
retains the occupied worker slot until termination and prevents any commit; it
does not claim to interrupt an in-progress font scan. No project schema,
font-discovery service, renderer or language preference/storage policy changed.

Style, position and title settings use paired label/value grids and explicit
widget identities. Switching language retains values and text-field focus;
pending work disables the already-focused form as well as its action buttons.
An idle-color regression exposed the old direct egui sRGBA round-trip mutating
`[4,5,6,123]` to `[4,4,6,123]` and `[7,8,9,210]` to `[7,9,9,210]` without input.
The three color controls now use temporary values and write back only when
their actual response reports a change. Tests retain the original color bytes.

## Verification

Explicit Rust **1.98.0 and 1.88.0** each pass:

- `text_overlay_ui::tests`: **14**, including the original seven tests.
- `editor_workspace::text::tests`: **11**, unchanged backend persistence,
  frame-ownership, edit/reorder, title insertion, Undo/Redo and reopen checks.
- `gif-from-screen-localization`: **43**.

The new real egui inputs exercise all three localized action buttons, paired
fields at 480 physical pixels with 1.0/1.5 UI zoom, stable IDs, literal font/text
editing, language-switch focus, running-form lockout and cancellation. Title
completion tests cover both insertion positions, frozen parameters, selected
new frames, exact Undo/Redo and journal reopen. Failure tests retain distinct
enum identities and prove that cancelled/stale work does not change the project.

Strict 1.98 Clippy passes for desktop/localization with all targets and features.
The existing whole-catalog Chinese glyph check passes in both egui font families;
format and diff checks pass. These are automated checks. Separate
[native X11 acceptance](TEXT-TITLE-QA-2026-09-09.md) now records actual bilingual
validation, caption editing, both title insertion positions, Undo/Redo, saved
language and byte-identical GUI/CLI reopen exports. It covers Ubuntu/Latin text
and Chinese UI, not every system font or complex script.

The existing egui color picker remains shared toolkit UI, including its numeric
color-space/channel identifiers and built-in tooltip text. This slice does not
replace that picker or remove its functionality to claim complete translation.
Its shared localization, remaining application tools and broader native QA remain
separate work.
