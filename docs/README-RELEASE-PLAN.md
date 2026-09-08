# README, demonstrations and downloadable releases

User request recorded on 2026-09-08: after the ongoing Linux work, add complete
selectable/persistent UI languages, default to the machine language, then improve
the whole project README with real animated demonstrations and convenient Release
downloads. This plan supplements, not replaces, the Linux feature/release ledger.

Latest downloadable preview: [Preview 2](LINUX-PREVIEW-2-QA.md), built from
`89021bd`, adds editor/shortcut/live-notice localization and language-layout guards.
Its public tarball and checksum were independently downloaded and verified.
The two existing demonstration GIFs retain their recorded-source provenance;
they are not relabeled as new captures or as all-language/full-parity evidence.

Progress on 2026-09-08: Chinese/English product READMEs now replace the audit-heavy
entry page; the previous detail is preserved in [development status](DEVELOPMENT-STATUS.md).
Two [actual operation GIFs](assets/README.md) show X11 pause/retarget/stop and saved
language switching. [Linux Preview 1](https://github.com/tiansongyu/gifromscreen/releases/tag/v0.1.0-preview.1)
provides a real tarball/checksum, verified again via unauthenticated public
download. This closes the initial README/demo/download slice, not full UI
translation, AppImage redistribution or a stable Linux release.

## Product-facing README

Use the [ScreenToGif README](https://github.com/NickeManarin/ScreenToGif/blob/master/README.md)
as a presentation reference: concise product identity, prominent real downloads,
clear capabilities, runtime requirements and visual demonstrations. Write original
copy and use this application's own icon and captures. Do not copy upstream brand
assets, screenshots, donation accounts, download counts or endorsements.

The root README should prioritize:

1. What GifFromScreen does: local recording, frame editing and GIF-only output.
2. A working Linux download link, architecture, preview/stable status and checksum
   instructions. Do not point a prominent button at a nonexistent asset.
3. Short real workflow GIFs and a compact recording/editing/recovery feature summary.
4. Three-step use instructions and a small, honest X11/Wayland comparison.
5. Language settings, with actual supported/translated coverage and a link to
   [localization status](LOCALIZATION-PLAN.md), not a misleading empty-language badge.
6. Runtime requirements, development commands, contribution/license information and
   links to detailed docs. Keep the long audit history in the documentation rather
   than making new users read it before finding a download.

Keep Chinese and English entry points consistent. Internal reference-generator
READMEs remain technical documents; do not replace their provenance instructions
with product marketing text.

## Actual animated demonstrations

Record the final tested UI after language/font behavior is stable. Use an isolated
desktop with synthetic content and no personal data. Capture these real paths:

- X11: select/move the independent recording frame, Start/Pause/Stop.
- Editor: choose frames, change timing/crop or add an annotation, then export GIF.
- Language: choose a language, see the interface change, close/reopen to show it
  was saved; show System as a policy rather than a one-time detected value.

Wayland visuals must show its real Portal/source-local workflow, not a fabricated
global transparent frame. A keyframe montage may be used only when labeled as
such; do not present selected stills as a continuous recording. Encode compact,
readable GIFs, verify frame dimensions/timing and inspect them before embedding.
Use repository-relative assets, meaningful alt text and bounded download sizes.

## Release and quick-download contract

The user has requested software Releases, not only temporary CI artifacts. Publish
a verified Linux preview first when the relevant package is ready; do not call
unfinished Linux parity a stable or zero-defect release. The supported tarball
and a separately gated AppImage may have different readiness dates. Do not upload
an AppImage while its metadata still says `redistribution_ready: false`.

Before creating a tag/Release:

- Select the exact commit and coherent package version. Build from that clean
  revision and verify the source/binary receipt, tests, native launch, project
  recovery and GIF export. Existing releases/assets must not be overwritten.
- Retain licenses/notices and required source materials for the actual format;
  verify checksums and the artifact's intended architecture/runtime requirements.
- Mark pre-release status and remaining platform/feature limits honestly. Include
  installation/removal instructions and release notes describing real changes.
- Upload the actual artifacts and SHA256 files to this repository's Release,
  inspect the API's resulting asset identities and test the public download links.
  Only then wire the README's quick-download links and badges to those assets.

No Release is claimed by this plan, and a green build alone is not publication.
After publication, downloading the documented asset and verifying/installing it
is the final quick-start check. macOS remains deferred until Linux is delivered.
