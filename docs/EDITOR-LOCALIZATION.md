# Editor, shortcuts and live notice localization

The main branch now localizes editor navigation, filmstrip, primary tabs,
Frames/Timing/Transform controls and clipboard history into English and Simplified
Chinese. Global-shortcut settings, actual registration summaries, binding-validation
explanations and asynchronous settings notices use explicit localizers too.
This extends [recorder localization](RECORDER-LOCALIZATION.md), not all 29 languages.

The catalog has 459 en/zh messages: the preceding 285 plus 39 shortcut, 117
editor-control, 15 recorder-notice and three editor-result messages. Existing
IDs, argument contracts and the original 285 translations are retained. Other
catalogs explicitly fall back to English; this count is not full-UI coverage.

## Stable data and interactions

- Frame/project/clipboard identities, names and paths remain literal data.
  Localization does not rewrite delays, source timestamps, artwork, capture
  regions, selection expressions or persisted project/settings formats.
- Navigation and commands retain their enums/action paths. Tabs, filmstrip,
  clipboard history and transitions have locale-independent identities; changing
  labels does not reset their selection or open state.
- Shortcut key tokens and desktop-reported trigger descriptions are preserved.
  The original validator/store/registration stay authoritative. Presentation maps
  known invalid-binding predicates, not English error strings.
- Only the current applied save revision displays Saved. A newer pending write
  remains Saving; un-applied draft bindings are separately identified. Changing
  language does not itself re-register shortcut keys.

## Notices retain identity until presentation

`ui_notice::Notice` owns a typed `Message` plus named literal values, or an explicit
Raw string for unmigrated/external diagnostics. It is not serialized into projects
or preferences. Parameter names are machine identifiers, never translated labels;
formatting errors include the message ID.

The root refreshes cached notices after language polling and before painting.
Recorder/shortcut views also render with the current localizer, covering results
received later in a frame. Migrated completion/failure, retarget and snapshot
notices retranslate without losing source data. Editor Undo/Redo and clipboard
history-cleared results preserve their message identity through the root view.
Raw strings matching English UI wording are not guessed to be catalog keys.

The live countdown suppresses its initial-duration notice by identity, avoiding
an initial three-second value alongside a one-second remaining counter. An
unrelated Raw string is not hidden by English-text matching. Unmigrated notices
and detailed backend/parser errors remain coverage debt, not translated successes.

## Language changes cannot finish an old layout gesture

Editor text can move the painted image. Before previews are painted, an actual
catalog change calls `EditorUiState::cancel_layout_gestures`:

- Active crop gestures restore the previous rectangle and exact numeric-field
  spelling. A release guard also consumes a press below the drag threshold, so
  its old release cannot create an accidental 1×1 crop. Fresh clicks/drags work.
- Panning stops without resetting zoom or scroll offset.
- Capturing ordinary drawing is cancelled; a Ready draft, its points, style and
  target remain intact. Confirmed crop sessions and project data remain intact.
- Existing Cinemagraph mapping-change protection is retained and retested.

Unavailable languages sharing English fallback do not spuriously cancel gestures.
This closes the language-switch path; general ordinary-drawing mapping-epoch
protection for other programmatic layout changes remains separate work.

## Verification and remaining scope

The Rust 1.98 all-target/all-feature workspace passes 1,665 tests, with 51 explicit
environment/benchmark ignores; desktop has 726 tests and localization has 35.
Those two crates also pass all 726/35 tests on Rust 1.88. Strict Rust 1.98 Clippy
and formatting pass. Tests cover real egui Chinese input, clipboard
and frame metadata, timing/transform actions, Undo/Redo, small/large-font layout,
fold state, literal notice arguments, stale-save acknowledgements and crop pointer
sequences across a layout change. Original English and Cinemagraph cases remain.
Native acceptance is recorded separately; unit rendering is not physical desktop,
global-shortcut or IME certification.

Still open: Effects/Overlays/Project tool bodies, remaining preview/crop text,
import/camera/board/automation/export dialogs, detailed errors, additional-language
catalogs, RTL/shaping and IME. Published Preview 1 is still the earlier frozen build.
