# Native editor and shortcut language acceptance

Source `89021bdab4cdb6cf85f6b9e0f06ad0c45c236ffb`, debug executable SHA-256
`ba67e8ffabf57c5618ebd1cc3d43c093264369be6ceab865af157bafab58a030`.
The private GNOME/Xvfb X11 profiles use `LC_ALL=zh_CN.UTF-8`, `LANGUAGE=zh`.
These are not physical GPU, mixed-DPI, Portal-shortcut, IME or all-language claims.

## Editor and existing notices

Lab `/tmp/gfs-wayland-qa.8q708eyo`, display `:100`, 1440 × 1000. A CLI-created
demo has 36 frames at 50,000 µs each. The GUI shows Chinese navigation, filmstrip,
Frames and Timing controls (`logs/02-editor-zh.png`, `03-timing-zh.png`).

1. Set the first selected frame to 200,000 µs in the Chinese Timing panel.
   Journal record 37 stores exactly that frame/delay change; the UI displays
   200.000 ms while the other frames remain 50.000 ms.
2. Undo stores record 38 restoring the same frame to 50,000 µs. The notice is
   `已撤销上一次编辑。` (`05-undo-notice-zh.png`). Switching to English updates
   this existing notice to `Undid the previous edit.` without another edit
   (`06-notice-switched-english.png`). Manifest and journal hashes do not change.
3. Create an unapplied crop with exact field strings `010`, `015`, `080`, `050`.
   Switch back to Chinese: the rectangle and spellings are retained
   (`08-ready-crop-english.png`, `09-ready-crop-after-zh.png`). Both project-file
   hashes still match. Cancel the crop without applying it.
4. Export through the GUI, close normally, and export after CLI reopening.
   Reopening reports revision 38. The original baseline, GUI output and reopened
   output are byte-identical: 36 GIF images, 160 × 96, 1,800 ms, 56,940 bytes.

The journal-first snapshot manifest stays at revision 36 during this session;
that is not a failed edit. Records 37/38 and the reopened project prove persistence.
Hashes checked across language/unapplied-draft changes:

```text
d51bb1930cbe5151bd61c34f210500c93cecfa3a3089492e030c7c898034f5f8  manifest.json
6921c18b1a93bf14e049877c9285f85208321e7231708e78b3d8b0e03076733a  journal.ndjson
5756aa86082026c4fa8f7a461a324188095aa766a0cf09331bf7e0d76559c9d7  seed.gif / editor-locale.gif / reopened.gif
```

`ffprobe -min_delay 0` separately confirms GIF dimensions, duration and count.
Scoped stop reports `cleanup_complete: true`, launcher exit 0. Unfinished
press/drag cancellation is covered by automated real-egui input tests, not inferred
from this native Ready-draft preservation check.

## X11 shortcut registration and persistence

Separate lab `/tmp/gfs-wayland-qa.tt49fdz2`, display `:101`, app PID 575859;
source/hash as above. Its local `SHORTCUTS-QA.md` contains the full observations.

- Chinese default-disabled, enable, actual 3/3 registration and Saved states work.
  Registered tokens remain `CTRL+SHIFT+F7/F8/F9`.
- Chinese → English → System/Chinese changes the existing summary/Saved notice.
  Observed native worker TID 578878 and the entire enabled-settings hash stay equal.
- With the fixture focused, Ctrl+Shift+F7 opens Ready without starting capture;
  another press starts, another pauses at eight frames, and Ctrl+Shift+F8 stops
  into the editor. Resume/F9 were not tested in this bounded run.
- Disable persists false and removes the worker, without changing bindings.
  A later Ctrl+Shift+F7 has no effect; screenshots 18 and 19 are byte-identical.

`shortcuts.json` remains version 1 and mode 0600, with canonical action/key tokens.
Enabled: 435 bytes, SHA-256
`bbc80c4188f5e42304591e5b256608887a8f092361357dffc45f308603e728fa`.
Disabled: 436 bytes, SHA-256
`9a9c2f2c426bed36acfeb4c41ed09013bbbfb7522b11db657052aa0c8db480c1`.
The recording retains eight 640 × 480 frames, revision 13, 800,055 µs at (0,0),
one clock `449cf2d1f3c342e0affc3c86fe688aad`; no GIF export is claimed for that run.
Normal titlebar close exits 0, the project lock disappears, and scoped stop
reports complete cleanup with the startup handle consumed.

## CI

[Linux CI 34244415923](https://github.com/tiansongyu/gifromscreen/actions/runs/34244415923)
and [portable CI 34244415812](https://github.com/tiansongyu/gifromscreen/actions/runs/34244415812)
pass for the source commit, including current-source packaging and native-window
smoke. They do not close the broader Linux delivery ledger.
