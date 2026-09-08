# Embedded CJK user-interface fallback

The unmodified `NotoSansCJKsc-Regular.otf` is Noto Sans CJK SC Regular 2.004,
face index 0, from the official Noto CJK `Sans2.004` release. `sources.json`
records the immutable upstream commit, exact download URL, byte length and
SHA-256. The complete upstream SIL Open Font License 1.1 is in `OFL.txt`.

The font is appended after egui's existing proportional and monospace fonts;
Latin and emoji keep their original preference. The same static bytes serve
both fallback lists without a filesystem lookup or system-font dependency.

This is one Simplified Chinese regional face with broad Chinese, Traditional
Chinese, Japanese kana and Korean Hangul character coverage. It does not select
locale-specific Han variants, make egui a complex-script shaping engine, or
promise Arabic, Hebrew, Tamil, every historic ideograph, or emoji sequence
support. It affects UI labels only, not text assets rendered into GIF projects.
