# Advanced editor localization

This slice extends the [preview/crop/export work](PREVIEW-EXPORT-LOCALIZATION.md)
on main. The published [Preview 3](LINUX-PREVIEW-3-QA.md) remains a frozen earlier
build; its binaries are not relabeled as containing these later changes.

## Actual coverage

The catalog now has 743 English/Simplified Chinese messages, adding 156 while
preserving the previous 587 IDs, parameter contracts and translations. This is
not full-application coverage and does not claim 29 completed catalogs.

- Effects: all eight existing families, their controls, legacy options and
  application-owned numeric validation. Shared image-border and image-shadow
  controls also receive the same explicit localizer inside automatic task forms.
- Overlays: tool navigation, current shape authoring, ordinary drawing, layer
  descriptions, visibility/removal, legacy conversion explanations and pagination.
  Text/title and image/watermark tool bodies remain separate migration work.
- Project: checkpoint, checkpoint/compact, journal-repair notices and statistics.
  Project insertion, Save As, the project library and import dialogs are separate.

No effect algorithms, project schemas or authoring coordinate systems change.
Default authored names and user names are data, not translation keys.

## Validation keeps both the sentence and field identity

`Notice` can retain a parameterless application message as an argument alongside
literal values. For example, the required-input sentence and the "blur radius"
field label both translate when a displayed validation failure changes language.
The parser's actual diagnostic remains literal. This is a single level of typed
message labels, not recursive user-authored templates or English string matching.
Caching uses the requested locale, because a sentence and its labels may fall
back independently in future partially translated catalogs.

`EditorUiFailure` preserves the typed notice through the UI-to-shell boundary.
Application validation already names the affected setting and is shown directly;
unmigrated/backend failures retain the existing operation-context wrapper. The
shell refreshes newly received notices before displaying them in the same frame.
An English raw diagnostic that happens to match a translated sentence remains raw.

## Data, interaction and recovery invariants

- Numeric spelling, signed values, RGB/alpha bytes, presets, names, frame IDs and
  source metadata are not rewritten by opening a panel or changing language.
- Signed border widths keep their thousandths-of-pixel storage. Shadow values
  keep hundredths/basis points, angle units and their original input rounding.
  Painting an imported fractional value does not normalize it silently.
- Effect Add/Replace/Clear preserves existing selection versus whole-animation
  rules for canvas-changing effects, including authored-stage ordering.
- Shape/drawing commits retain explicit selected-frame gaps and stage ownership.
  A failed drawing validation keeps the Ready draft available for correction.
- Layer rows stay bounded to 64 per page; pagination, row and Statistics identities
  are independent of localized labels. Hide/Show is non-destructive. Unknown
  legacy annotation coverage does not acquire permission to convert merely
  because a tooltip changes language.
- Checkpoint, compaction and journal repair use their existing durable commands;
  notices preserve literal recovery paths. Statistics keep their exact durations.

## Automated verification

On 2026-09-09, explicit Rust 1.98.0 all-target/all-feature workspace verification
passes 1,737 tests, with 51 environment/benchmark cases explicitly ignored.
All 792 desktop tests and 41 localization tests also pass on Rust 1.88.0. Strict
workspace Clippy on Rust 1.98.0, formatting and diff checks pass; this is not a
claim of whole-workspace strict Clippy on the minimum toolchain.

The 26 added desktop tests include eight effect, four shared image-form, eleven
overlay/project, two notice-argument and one shell-boundary regression. They use
real egui actions and input where appropriate, not forged response flags. Coverage
includes explicit frame-selection gaps, stage ordering, Undo/Redo, reopen, invalid
fields, signed/fractional units, non-opaque stored colors, disabled controls,
129-layer pagination and stable panel identities. Headless UI checks are not
physical desktop, pressure-device, font-shaping or IME certification.

Native inspection found orphaned labels in wrapped rows: Radius and track opacity
could stay on one line while their input moved to the next. The follow-up layout
fix keeps effect scalar controls paired, uses field grids for shape/drawing
properties and groups RGBA channels. Six additional tests verify actual label/
widget geometry, literal numeric edits, locale-independent hit IDs and reachable
Add/Commit actions through the existing host scroll area at narrow sizes and large
fonts. This fixes field association without changing parsing or authored values.
The [native acceptance record](ADVANCED-EDITOR-QA-2026-09-09.md) documents the
actual defect, corrected controls, layer visibility, signed border, checkpoint/
compaction, stopped-owner recovery and byte-identical reopened GIFs.

The remaining language/tool and physical desktop gates stay in the
[localization plan](LOCALIZATION-PLAN.md) and [Linux ledger](LINUX-STATUS.md).
Current shape authoring is not complete ScreenToGif shape-interaction parity.
