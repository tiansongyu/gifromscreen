#!/usr/bin/python3
"""A native-Wayland visual fixture; no global input, device or file access."""
import gi
gi.require_version("Gtk", "3.0")
from gi.repository import GLib, Gtk  # noqa: E402


class Fixture(Gtk.Window):
    def __init__(self):
        super().__init__(title="GifFromScreen Wayland QA")
        self.set_default_size(640, 420)
        self.connect("destroy", Gtk.main_quit)
        self.frame = 0
        self.canvas = Gtk.DrawingArea()
        self.canvas.connect("draw", self.draw)
        self.add(self.canvas)
        GLib.timeout_add(100, self.tick)

    def tick(self):
        self.frame += 1
        self.canvas.queue_draw()
        return True

    def draw(self, widget, context):
        width, height = widget.get_allocated_width(), widget.get_allocated_height()
        colors = [(0.82, 0.22, 0.24), (0.12, 0.56, 0.31),
                  (0.15, 0.34, 0.82), (0.91, 0.69, 0.16)]
        for index, color in enumerate(colors):
            context.set_source_rgb(*color)
            context.rectangle((index % 2) * width / 2, (index // 2) * height / 2,
                              width / 2, height / 2)
            context.fill()
        context.set_source_rgb(1, 1, 1)
        context.select_font_face("Sans")
        context.set_font_size(22)
        for label, x, y in [("A — RED", 20, 38), ("B — GREEN", width / 2 + 20, 38),
                            ("C — BLUE", 20, height / 2 + 38),
                            ("D — GOLD", width / 2 + 20, height / 2 + 38)]:
            context.move_to(x, y)
            context.show_text(label)
        context.set_source_rgb(0.05, 0.07, 0.1)
        context.rectangle(12, height - 66, width - 24, 52)
        context.fill()
        context.set_source_rgb(1, 1, 1)
        context.move_to(24, height - 32)
        context.show_text(f"WAYLAND FIXTURE   frame {self.frame:06d}")
        context.set_source_rgb(1, 1, 1)
        context.rectangle(20 + (self.frame * 13) % max(1, width - 64), 70, 28, 28)
        context.fill()


if __name__ == "__main__":
    fixture = Fixture()
    fixture.show_all()
    Gtk.main()
