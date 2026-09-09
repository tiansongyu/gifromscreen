# Narrow editor: header priority and one page scroll

This is the layout follow-up to the shared-header and nested-inspector behavior
observed during [native text/title QA](TEXT-TITLE-QA-2026-09-09.md). It does not
claim a new native acceptance run: a frozen committed build still needs that
separate verification.

## Cause

The editor already has an outer vertical page `ScrollArea`. In egui 0.32.3 its
content UI initially has the finite viewport-sized maximum rectangle, not an
infinite vertical layout budget. In the stacked branch, navigation, filmstrip,
preview controls and image consume that height before the inspector is created.

The inspector's old `max_height(380)` is a ceiling, not a reserved 380-point
height. `ScrollArea::begin` takes the lesser of available height and that ceiling;
when almost no height remains, the default minimum scrolling height is only
**64 logical points**. At 150% zoom this is approximately 96 physical pixels,
explaining the one/two visible rows and the need to scroll both page and inspector.
The regression reproduces this after the initial egui sizing/zoom passes settle.

Separately, the old header allocated Back, brand and subtitle before asking a
right-aligned Language control to fit the remainder. The subtitle could consume
space needed by the action, producing the crowded native header.

## Change

- The Language action is allocated first at the right. Back and the brand use
  a wrapping layout inside the remaining area; the subtitle is shown only when
  its measured text width and spacing fit. Critical actions are not removed.
  The row starts at the normal interaction height and grows for its contents;
  it does not feed the panel's previously cached height back into text wrapping.
  Existing large-font Wayland preparation tests caught that intermediate
  height-drift regression; their original position/click assertions were kept.
- The existing 900-logical-point split threshold is unchanged. Wide pages still
  use two columns and the independent inspector scroll with its existing ID and
  380-point maximum height.
- Stacked pages use one column and a normal child UI for the inspector. Its full
  height belongs to the outer page: there is no nested vertical inspector viewport
  and no nested consumer of wheel or scroll-to-focus requests.
- Both inspector modes retain a matching child-UI hierarchy. Explicit field IDs
  and text focus survive language changes and transitions across the split
  threshold. The wide inspector's stored scroll state is not reset by stacked use.

The preview's own exact-pixel zoom/pan viewport is intentionally retained. The
change does not promise that every interaction anywhere in the editor uses one
scrollbar: only the unnecessary nested **inspector** scroll is removed in stacked
mode. It does not shrink image content or remove settings to fit the form.

## Safety and verification

`main.rs` retains the existing Back enable conditions, playback pause, Wayland
preparation cancellation, worker guards and preference policy. Recorder-specific
early returns still occur before `show_main_view`, so this header is not painted
over the dedicated recorder. Main-window restore code is unchanged. Ordinary
canvas input still uses the actual painted image rectangle and existing mapping
checks; no new input capture or modifier interception was introduced.

Seven new real egui tests pass on explicit Rust **1.98.0 and 1.88.0**:

- Visible, non-overlapping brand/Back/Language and actual pointer clicks in en/zh,
  including 680 native pixels at 150% and an additional narrower headless case.
- The old 64-point inspector compression versus full inline content allocation.
- A real project editor's wide columns, font-field focus and literal input
  retained across wide/stacked resize, 150% zoom and en/zh changes.
- Actual page wheel input reaching Add text and dispatching typed empty-text
  validation without modifying the project.
- Back remains disabled during an actual pending decoder job, while Language
  opens; enabled Back leaves the editor and pauses preview playback.
- A held drawing is rolled back on layout change, the stale release cannot
  complete it, and a fresh completed draft survives a later layout change.

Strict desktop Clippy with all targets/features, formatting and diff checks pass.
The unchanged 21 Wayland controller/preparation tests also pass on both Rust
versions, including the original large-font hit targets and fixed top actions.
The existing 22 ordinary-drawing/input-boundary tests pass on Rust 1.98.0.
The tests use real egui events and rendered geometry, not fabricated Response
flags. Header checks use painted glyph bounds, because wrapped galleys can include
leading empty space for preceding widgets. Exact offscreen Tab-navigation scroll
destinations and native integration remain additional checks, not implied passes.

No localization keys, project schema, WPF renderer, board tool or saved user data
are changed by this layout slice.
