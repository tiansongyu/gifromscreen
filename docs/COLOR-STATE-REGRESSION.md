# Preserve authored colors when a picker is idle

Real egui tests exposed an existing lossy conversion path:
`color_edit_button_srgba_unmultiplied` may round-trip the supplied array even
when no color is edited. Passing persistent draft state directly caused, for
example, `[4,5,6,123]` to become `[4,4,6,123]` on an input-free frame. Disabled
UI still executes the widget closure, so disabling a form did not prevent it.

The [text/title slice](TEXT-TITLE-LOCALIZATION.md) fixes its three color fields.
A follow-up audit found two remaining direct-write calls in the drawing-board
recorder: pen/highlighter color and background. Both now edit a temporary array
and copy it back only when the actual widget response reports a change.
The full existing color picker remains available; no control was removed.

The Board regression was reproduced as two failing real egui tests before the
fix, including the disabled background picker. Its nine tests (original five
plus four new ones) now pass on explicit Rust 1.98.0 and 1.88.0. They cover:

- Multiple idle passes, existing text focus, disabled UI and transparent RGB.
- Actual popup RGB and Alpha edits, including keeping the popup open afterwards.
- Initial canvas background bytes and subsequent pen/highlighter colors.
- Two-frame `LiveRecorder` projects for pen and highlighter, with persisted
  assets compared to an independently constructed BoardCanvas reference.

Strict 1.98 desktop Clippy, formatting and diff checks pass. This is real egui
input plus recording/persistence test evidence, not a new native-desktop capture
or complete Board/Wayland acceptance record.

The audit found existing safe local-copy/changed guards in annotation controls,
automatic-task colors, vector-shape colors and image-effect colors. Shared egui
picker tooltips are still a separate localization task; avoiding data drift does
not claim that every toolkit string is translated.
