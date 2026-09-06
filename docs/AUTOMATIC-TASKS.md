# Automatic tasks and reusable editing presets

The reference is ScreenToGif 2.43.2, commit `a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd`.
Its [new-project loading path](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L3544) executes six task classes: mouse events, key strokes, delay, progress, border and shadow. Recorded input tasks apply only to screen recording projects; the other tasks have broader creation-source filters. Enum values without an execution branch are not counted as implemented upstream tasks.

## Workflow

Create a named editing preset in **Automatic tasks & editing presets**, add tasks in execution order, edit their parameters, enable or disable individual tasks, and use Move up / Move down to reorder them. Select the active preset, choose the eligible creation sources and save. The master switch controls automatic application. **Run saved preset now** applies the active preset to the whole current timeline regardless of its automatic source filters. Unsaved settings cannot accidentally change a run.

These are editing presets, separate from project-local GIF export presets. They are application settings and survive app restarts. Opening an existing project does not rerun tasks. New screen/camera/board recordings and new imports are the trigger points; loading settings is awaited if a new project becomes ready during startup.

Available tasks reuse the same editor and annotation implementations as manual editing:

- Delay: set, signed adjustment, or percentage scaling.
- Border: independent physical-pixel edge widths and RGBA color.
- Shadow: signed offsets, blur radius, and RGBA color.
- Progress: bar direction, frame/time measure, elapsed/remaining mode, tokenized label, and text style.
- Recorded mouse clicks and key strokes, with source-data validation. The shared annotation form also supports manually authored input labels and cursor overlays.

Recorded inputs are never fabricated. Non-screen automatic sources skip recorded-input tasks and report them. A recorded-input task with no matching events is also skipped, allowing later delay, border and other tasks to continue. Invalid parameters or damaged source assets still produce task-numbered errors. Wayland capture has no general permission to observe other applications' global keyboard/mouse input; the manual annotation alternatives remain available.

When creating an annotation task with a project open, its initial box and font size fit that canvas. Saved presets retain their explicit physical-pixel parameters; they are never silently clipped or resized when applied to another project. Without an open project, the initial box is 240 × 40 pixels and can be edited before saving.

## Persistence and failures

Settings live at `$XDG_STATE_HOME/gifromscreen/editing-tasks.json`, or `$HOME/.local/state/gifromscreen/editing-tasks.json`. Both roots must be absolute. Missing/invalid environment values disable storage rather than writing into the current directory. Settings schema version 1 is data-only and rejects unknown executable task variants.

Saving validates the complete document, takes an advisory lock, checks the expected prior bytes, writes and syncs a temporary file, atomically replaces the destination, and syncs its parent. Closing the application waits for an in-flight settings save to finish. Corrupt, future-version, oversized and symlink settings files are preserved. A conflicting writer must reload; it cannot overwrite a newer configuration silently. Tests inject a settings path, and default app tests own an isolated temporary directory.

Task preparation, text rasterization, asset writes and journal synchronization run in a cancellable background worker holding an exclusive recoverable workspace loan. Each task is simulated on the output of the previous task. Only after the entire chain validates are immutable assets written and a single compound command committed. A preparation failure or cancellation leaves the project and existing undo history unchanged; verified unreferenced immutable assets can remain if cancellation occurs during asset saving. The final durable commit is the cancellation boundary.

The same compound edit records the preset name, original revision, trigger and ordered completed/skipped task names in `task_runs`. One undo restores both the prior edit results and the prior completion history. Journal recovery/reopen retains the results; the app does not rely on in-memory success flags.

## Resource limits and verification

Configuration is limited to 32 presets, 32 tasks per preset and 256 KiB. The last 32 completed run records are retained. Each run limits generated pixels to 256 MiB, and input/command metadata has a 64 MiB serialization budget. Annotation-specific per-frame/event/text limits also apply. These are explicit errors with an unchanged project, not truncated successful runs.

Regression coverage is in `auto_task_tests.rs`, `auto_task_store.rs`, domain `editing_task.rs` and `task_run.rs`: task ordering; disabled/source filters; late failure rollback; cancellation; background workspace recovery; worker panic; startup configuration readiness; damaged settings preservation; restart persistence; conflicting writers; schema/name/numeric limits; one-step undo/redo and journal reopen.
