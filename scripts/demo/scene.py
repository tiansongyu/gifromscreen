"""Original animated artwork and captions behind the recorded application."""
import json
import math
from pathlib import Path
import sys
import time
import tkinter as tk

from PIL import Image, ImageDraw, ImageFont, ImageTk

FONT_DIRECTORY = Path("/usr/share/fonts/truetype/dejavu")


def font(size, bold=False):
    name = "DejaVuSans-Bold.ttf" if bold else "DejaVuSans.ttf"
    return ImageFont.truetype(str(FONT_DIRECTORY / name), size)


def background():
    image = Image.new("RGB", (1024, 768), "#edf1f5")
    draw = ImageDraw.Draw(image)
    draw.text((32, 19), "GifFromScreen", font=font(21, True), fill="#182a3d")
    draw.text((992, 27), "SCREEN → GIF", font=font(12, True), fill="#526476", anchor="rt")
    for y in (58, 704):
        draw.line((32, y, 992, y), fill="#d5dde6")
    return image


def motion_canvas():
    image = Image.new("RGB", (640, 480), "#14283b")
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle((26, 24, 179, 53), radius=14, fill="#284256")
    draw.text((42, 32), "MOTION STUDY / 01", font=font(12, True), fill="#b9d9e3")
    draw.text((34, 78), "Make a little", font=font(36, True), fill="#f6f8fa")
    draw.text((34, 119), "motion.", font=font(36, True), fill="#f6f8fa")
    draw.text((35, 180), "A small loop. Endless possibilities.", font=font(15), fill="#a6c0cd")
    # This artwork is illustrative capture content, never reconstructed app UI.
    for x in range(35, 625, 24):
        for y in range(235, 430, 24):
            draw.ellipse((x, y, x + 1, y + 1), fill="#355165")
    draw.ellipse((394, 45, 581, 232), outline="#365c70", width=2)
    draw.ellipse((420, 71, 555, 206), outline="#284c61", width=1)
    draw.ellipse((472, 123, 503, 154), fill="#89c9cd")
    points = [(x, 326 + 48 * math.sin((x - 34) / 572 * math.tau)) for x in range(34, 608)]
    draw.line(points, fill="#5a8d9e", width=2)
    draw.line((34, 441, 606, 441), fill="#365065")
    draw.text((35, 455), "DESIGNED TO LOOP", font=font(11, True), fill="#a8c5d0")
    draw.text((605, 455), "GIFROMSCREEN", font=font(11, True), fill="#a8c5d0", anchor="rt")
    return image


class Scene:
    def __init__(self, state_path, version):
        self.state_path = state_path
        self.root = tk.Tk()
        self.root.overrideredirect(True)
        self.root.geometry("1024x768+0+0")
        self.canvas = tk.Canvas(self.root, width=1024, height=768, highlightthickness=0, bg="#edf1f5")
        self.canvas.pack()
        self.background = ImageTk.PhotoImage(background())
        self.canvas.create_image(0, 0, anchor="nw", image=self.background)
        self.canvas.create_text(512, 207, text="A moment worth keeping.", fill="#182a3d",
                                font=("DejaVu Sans", 27, "bold"), tags="intro")
        self.canvas.create_text(512, 244, text="Record it. Refine it. Share it.", fill="#586b7c",
                                font=("DejaVu Sans", 14), tags="intro")
        self.scene = self.canvas.create_image(192, 144, anchor="nw", state="hidden")
        self.caption = self.canvas.create_text(32, 735, anchor="w", fill="#182a3d",
                                               font=("DejaVu Sans", 15, "bold"))
        self.canvas.create_text(992, 735, anchor="e", text=f"Real app · Linux X11 · v{version}",
                                fill="#526476", font=("DejaVu Sans", 10))
        self.base = motion_canvas()
        self.started = time.monotonic()
        self.previous_state = None
        self.photo = None
        self.tick()

    def tick(self):
        try:
            state = json.loads(self.state_path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            state = {"scene": False, "caption": "01  /  Open the recorder"}
        if state != self.previous_state:
            self.canvas.itemconfigure(self.caption, text=state["caption"])
            self.canvas.itemconfigure("intro", state="hidden" if state["scene"] else "normal")
            self.canvas.itemconfigure(self.scene, state="normal" if state["scene"] else "hidden")
            self.previous_state = state
        if state["scene"]:
            phase = (time.monotonic() - self.started) % 3 / 3
            image = self.base.copy()
            draw = ImageDraw.Draw(image)
            for index in range(13, -1, -1):
                position = (phase - index * 0.009) % 1
                x = 34 + 572 * position
                y = 326 + 48 * math.sin(position * math.tau)
                radius = 3 + (13 - index) * 0.5
                color = tuple(round(a + (b - a) * (13 - index) / 13)
                              for a, b in zip((46, 69, 80), (255, 174, 118)))
                draw.ellipse((x - radius, y - radius, x + radius, y + radius), fill=color)
            angle = phase * math.tau
            x, y = 487 + 94 * math.cos(angle), 139 + 94 * math.sin(angle)
            draw.ellipse((x - 10, y - 10, x + 10, y + 10), fill="#ffae76")
            self.photo = ImageTk.PhotoImage(image)
            self.canvas.itemconfigure(self.scene, image=self.photo)
        self.root.after(33, self.tick)


if __name__ == "__main__":
    Scene(Path(sys.argv[1]), sys.argv[2]).root.mainloop()
