# Watermark language slice

The Linux main branch now localizes the image-watermark tool and its retained
application-owned notices in English and Simplified Chinese. This is newer than
Preview 3 and does not complete the remaining UI or the other 27 target catalogs.

The catalog adds 24 messages (796 total); every earlier message ID, argument
contract and translation is unchanged. The source-format/size hint, path label,
source-dimension behavior, opacity labels, blend modes, decode/apply buttons,
selection guidance and background status follow the active localizer.
Names and paths are literal user data, including braces or non-Latin characters.

Validation and deferred outcomes carry message identities, not cached English
sentences. Empty names, zero opacity, missing/non-file/unsupported-extension
paths, stale targets, closed projects and missing worker results have their own
messages. Success retains source width, height and path as named arguments.
Underlying IO/codec/workspace diagnostics remain literal parameters; no code
matches English error text to decide how to translate it.

This keeps the original asynchronous behavior: decode receives a settings
snapshot and a strict project/revision/selection anchor, the focused form is
locked while a decode is running, and stale results cannot attach themselves to
a different target. Frame ownership, source pixels, undo and project writes are
not changed by language switching.

Verification on explicit Rust 1.98.0 and 1.88.0: watermark tests 13/13, localization
tests 42/42. New tests exercise actual egui button input, disabled empty selection,
480×640 with enlarged UI, focused-field locking while running, en→zh→en stable
field values/widget identities/focus, retained typed notices and literal paths.
The follow-up layout fix gives all ten fields separate label/value rows with
stable IDs. A real 480-pixel viewport at 1.5 UI zoom checks every field's paired
rectangles, viewport boundaries, hover hit targets and cross-language identity.
No extra translations or image-rendering semantics changed in that fix.
The existing seven asynchronous snapshot, stale-anchor and durable-edit tests
still pass. Strict Clippy and the embedded-font glyph coverage checks pass.
Those tests are automated evidence. Separate [native X11 acceptance](WATERMARK-QA-2026-09-09.md)
now confirms retained bilingual notices, literal names/paths, narrow-window
150% interaction, gapped Apply, undo/redo, saved language and identical GUI/CLI
reopen exports. That native build has a 680-pixel minimum width; the 480-pixel
egui test is not mislabeled as a real 480-pixel OS window.

System-default locale detection, saved overrides and explicit English fallback
are unchanged. Text/title forms, imports, other tool bodies and full translated
catalogs remain in the [localization plan](LOCALIZATION-PLAN.md).
