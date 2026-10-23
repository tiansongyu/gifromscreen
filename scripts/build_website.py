#!/usr/bin/env python3
"""Stage only public website files and verify local HTML asset/anchor references."""

from html.parser import HTMLParser
from pathlib import Path
import shutil
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]


class References(HTMLParser):
    def __init__(self):
        super().__init__()
        self.links = []
        self.ids = set()

    def handle_starttag(self, tag, attributes):
        attrs = dict(attributes)
        if "id" in attrs:
            if attrs["id"] in self.ids:
                raise ValueError("duplicate HTML ID: " + attrs["id"])
            self.ids.add(attrs["id"])
        for key in ("href", "src"):
            if key in attrs:
                self.links.append(attrs[key])


def main():
    destination = ROOT / "target/site"
    # Only explicitly public website inputs are staged; never upload the workspace.
    destination.mkdir(parents=True, exist_ok=True)
    for name in ("index.html", "styles.css", "app.js"):
        shutil.copyfile(ROOT / "website" / name, destination / name)
    shutil.copytree(ROOT / "website/assets", destination / "assets", dirs_exist_ok=True)
    shutil.copyfile(ROOT / "docs/assets/workflow-v016.gif", destination / "assets/workflow-v016.gif")
    shutil.copyfile(ROOT / "packaging/linux/io.github.tiansongyu.gifromscreen.svg", destination / "assets/icon.svg")
    (destination / ".nojekyll").touch()
    references = References()
    references.feed((destination / "index.html").read_text())
    for url in references.links:
        parsed = urlsplit(url)
        if parsed.scheme or parsed.netloc:
            continue
        if parsed.path:
            path = (destination / unquote(parsed.path)).resolve()
            if not path.is_relative_to(destination.resolve()) or not path.is_file():
                raise ValueError("missing/unsafe website asset: " + url)
        elif parsed.fragment and unquote(parsed.fragment) not in references.ids:
            raise ValueError("missing website anchor: " + url)
    print("Website staged at target/site; HTML assets and anchors verified.")


if __name__ == "__main__":
    main()
