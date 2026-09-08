# Real application demonstrations

These GIFs are recordings of the running application, not UI mock-ups or borrowed
ScreenToGif imagery. Only the project-owned QA fixture/private desktop is shown.
They demonstrate a Linux preview, not a finished 29-language or full-parity UI.

| File | Content | Encoded output |
| --- | --- | --- |
| `record-and-retarget.gif` | Open the X11 recording frame, start, pause, drag its border, resume, stop into the editor | 864 × 600, 200 images, 20 s, 490,880 bytes |
| `language-switch.gif` | System Chinese launcher → English → Chinese; the panel displays saved status and incomplete translation coverage | 832 × 640, 160 images, 16 s, 330,765 bytes |

SHA-256:

```text
20cedefd33a9340a88d8d81ad1d3ae74b9f187e152f7e27580c03f79d785b9d1  record-and-retarget.gif
627dc74d602c327d34ecb4bd11e5ef67cde03e23450cc98dcfc824db10f7c802  language-switch.gif
```

Captured 2026-09-08 inside owned GNOME/Xvfb labs using the repository's
`scripts/qa/wayland_nested.py --display-server x11` environment. FFmpeg x11grab
recorded actual interactions at 15 fps to lossless FFV1; GIF conversion used
10 fps, Lanczos downscaling, palettegen and Bayer paletteuse. Both GIFs were
decoded/inspected after conversion. No explanatory screen was fabricated and
no interaction order was changed. Their original local recordings/scripts:

- `/tmp/gfs-wayland-qa.6m7mgo54/logs/record-capture-demo.py` and
  `record-and-retarget.mkv`: actual portable release executable, SHA-256
  `861e786def10c84d321d8b480cc19b86e7489b8faf99d66a15d7b7a04685ae82`.
- `/tmp/gfs-wayland-qa.yerby2j2/logs/record-language-demo.py` and
  `language-switch.mkv`: debug build of the same UI source slice, before the
  documentation-only release commit. The 76-message coverage notice refers
  only to the initial catalog, not all application strings.

The “Wayland QA” name in the colored fixture identifies the reusable fixture,
not the display server in these videos; these recordings use **X11**.
The small red/green/blue/gold fixture is project-authored test content.
Both labs were explicitly stopped, leaving recordings for inspection.

See [preview package acceptance](../LINUX-PREVIEW-1-QA.md) for actual capture
origin, timing, project and GIF checks beyond the demonstration video.
