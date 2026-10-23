# Project website

The static site is published at <https://tiansongyu.github.io/gifromscreen/>.
It uses plain HTML, CSS and JavaScript, with no package manager or external fonts.

From the repository root, using Python 3.9+ and Node.js:

```sh
node --check website/app.js
python3 scripts/build_website.py
python3 -m http.server 8000 --directory target/site
```

Open `http://localhost:8000`. The build checks local HTML assets and anchors.
Pushing website changes to `main` runs `.github/workflows/pages.yml`, which
publishes only `target/site` through GitHub Actions. GitHub Pages must use the
**GitHub Actions** build source in the repository settings.

`record-preview.webp` and `editor-preview.webp` are stills from frames 110 and 197
of the committed `docs/assets/record-and-retarget.gif`. They show the earlier
X11 interface, as labeled on the site. See [recording provenance](../docs/assets/README.md).
`export-demo.gif` is synthetic content generated with
`gif-from-screen-cli demo`; `export-poster.webp` is its first frame.
Animation starts only after an explicit Play action and stops when hidden.

The installation command downloads `scripts/gifromscreen-installer.sh` from the
latest stable Release. The installer needs Python 3.8+, Linux x86_64 and glibc
2.35+. It verifies the release archive, rejects unsafe archive entries, then
runs the package's existing per-user installer. Upgrades verify all managed files and retain a rollback copy; modified files
are not removed. Newer installed versions are kept unless `--allow-downgrade`
is explicitly supplied.

Run `python3 scripts/test_release_installer.py` and
`shellcheck scripts/gifromscreen-installer.sh` before publishing changes.
The release-installer workflow attaches it to future published releases and
can also be dispatched manually for an existing release tag.
