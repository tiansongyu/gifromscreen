# Version 0.1.6 workflow recording

[Watch the demonstration](workflow-v016.gif).

Captured on 2026-10-23 from the **actual v0.1.6 Linux x86_64 release executable**,
inside a private 1024 × 768 Xvfb desktop. The application pixels are unmodified.
The background, step captions and animated “Motion Study” are original project
artwork rendered in a separate window behind the app. They are illustrative
capture content, not application features or performance measurements.

The sequence runs at normal speed, without cuts: open the compact launcher,
open the X11 recorder, set 20 FPS, record after the default countdown, stop into
the editor, replay and select a frame, export all frames, then replay again.
The editor is resized to 960 × 632 to keep the desktop captions visible.

| Asset | Dimensions | Frames | Duration | Bytes |
| --- | --- | --- | --- | --- |
| `workflow-v016.gif` | 1024 × 768 | 484 | 24.20 s | 1,262,305 |
| `website/assets/export-demo.gif` | 640 × 480 | 62 | 3.10 s | 647,528 |

Both GIFs loop indefinitely. Every frame was decoded after conversion. The
exported sample contains the 62 frames captured by the application. The website
recorder still is an uncropped desktop capture; its editor still is cropped to
the actual app window. The export poster is the first frame of the exported GIF.

The executable was verified against the binary inside the published v0.1.6
archive, built from commit `fd48baadc79fc9aa807bab524a3d59f9e2fedbfa`.

```text
6437a8edc168051306c9729424a61ca8f575a49fe1bcb61d96de1f2502179a26  bin/gif-from-screen
4b4ceecca480bd806062811514bbbab731e06247b54ed4cdb80c752f286fa303  workflow-v016.gif
2283e6b07e62f133ad72840d284e9b7a4af8350a143d7804f19cd56662d7e5b5  website/assets/export-demo.gif
```

## Reproduce

The optional capture tools require Python 3 with Tk and Pillow, Xvfb, xdotool,
xsetroot, ImageMagick (`import`), FFmpeg and DejaVu Sans fonts. They always start
their own X server and isolated app configuration; they do not use the host
screen. The script owns and stops the application, fixture and recorder.

From the repository root, supply the v0.1.6 portable binary and a new output directory:

```sh
python3 scripts/demo/record_readme.py \
  --binary /path/to/gifromscreen-0.1.6-linux-x86_64/bin/gif-from-screen \
  --version 0.1.6 \
  --output-dir /tmp/gifromscreen-readme-demo
```

The script writes the lossless `workflow.mkv`, screenshots, the application’s
`motion-study.gif`, the final `workflow.gif`, logs and a `provenance.json` with
binary/asset hashes and timestamps. FFmpeg captures at 20 FPS and creates a
shared GIF palette with Bayer dithering and changed-rectangle optimization.
The script targets this version’s English UI at 100% scale; coordinates must be
reviewed when the interface changes. Scheduling makes frame counts and hashes
vary between recordings.

The committed take was recorded in `/tmp/gfs-readme-take2`. The older demos and
their historical provenance remain available in [the asset notes](README.md).
