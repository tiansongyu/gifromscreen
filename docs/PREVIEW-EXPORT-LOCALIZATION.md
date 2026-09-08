# Preview, crop and GIF export localization

This slice continues the [editor localization work](EDITOR-LOCALIZATION.md) and
is downloadable in [Preview 3](LINUX-PREVIEW-3-QA.md). The frozen
[Preview 2 download](LINUX-PREVIEW-2-QA.md) remains unchanged. This is not complete
application/29-language coverage.

## Translated controls and outcomes

English and Simplified Chinese now cover preview titles and dimensions, Fit/1:1
zoom, direct-crop controls and application-owned validation, GIF configuration,
export progress and results, and project-local export preset controls/results.
The catalog contains 587 en/zh entries; the preceding 459 IDs, arguments and
translations remain unchanged. Other target languages still explicitly fall
back to English.

Export validation returns a typed presentation notice, including empty frame
selections, invalid output extensions, palette limits and finite loop counts.
Preset results retain both their success/failure state and inner message identity
until presentation, so an already-visible error also changes language. External
filesystem/encoder diagnostics and detailed parser errors remain literal data
inside translated explanations; they are not classified by matching English.

Only `ProjectGifExportError::Cancelled` produces the normal cancellation receipt.
Requesting cancellation does not erase an actual export failure or successful
commit. An error remains an error even if its path or diagnostic contains `cancel`;
a successful result remains a success.

## Data and command boundaries

- Locale changes do not rewrite output paths, preset names, custom palette text,
  numeric input spelling, coordinates, frame delays or project data.
- Widget IDs and conditional-control identities remain independent of labels.
  The export configuration stays disabled while an export or conflicting editor
  mutation is active; cancellation still uses the actual job state.
- Presets use the existing validation, journal and Undo/Redo commands. Loading
  preserves the output path and clears overwrite permission. Translation cannot
  grant permission to replace a file.
- Crop parsing and whole-animation application are unchanged. Numeric input is
  not silently normalized merely because its label changes language.

## Automated verification

On 2026-09-09, the explicit Rust 1.98.0 all-target/all-feature workspace run
passes 1,708 tests, with 51 environment/benchmark cases explicitly ignored.
Desktop has 766 tests and localization has 38; both crates also pass in full
on Rust 1.88.0. Strict workspace Clippy on Rust 1.98.0, formatting and diff checks
pass. No Rust 1.88 whole-workspace strict-Clippy result is claimed here.

The new export tests exercise 40 option combinations, conditional-widget IDs,
real Chinese Export/Cancel input, disabled configuration, typed validation and
success/cancel/failure results. Preset tests perform actual save/load/delete and
language round trips, including 4096/4097-byte boundaries and unchanged failed
loads. Crop tests use real numeric text input and Apply/Undo/reopen operations.
The [ordinary drawing input tests](DRAWING-PREVIEW-INPUT.md) are part of the same
desktop run, not a replacement for native acceptance.
The [native acceptance record](PREVIEW-EXPORT-QA-2026-09-09.md) covers final-build
drawing, Undo/Redo, system-language negotiation and reopened GIF output.

## Remaining scope

Effects/Overlays/Project tool bodies, import/camera/board/automation dialogs,
remaining detailed errors and all additional catalogs still require migration.
Complex-script shaping, bidirectional editing, IME and physical desktop acceptance
remain separate gates in the [localization plan](LOCALIZATION-PLAN.md).
Translated message counts do not measure complete screen coverage, and headless
egui tests do not establish native desktop/font/IME behavior.
