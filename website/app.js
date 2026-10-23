"use strict";

const views = {
  record: {
    image: "assets/record-preview.webp",
    alt: "X11 recording region and separate recorder controls",
    title: "Capture just what you need.",
    description: "Resize before recording. Pause, move your region and pick up where you left off. Keep the controls outside your capture."
  },
  edit: {
    image: "assets/editor-preview.webp",
    alt: "Native GifFromScreen editor with a frame timeline and selected frame preview",
    title: "Find the frame. Get it right.",
    description: "Remove the extras, set individual frame delays and add text or shapes. Undo and redo edits, then keep your project for later."
  },
  export: {
    image: "assets/export-demo.gif",
    alt: "Motion study recorded and exported as a GIF in GifFromScreen v0.1.6",
    title: "Ready for one more loop.",
    description: "Choose your frames, colors, looping, transparency and dithering. This original motion study was captured and exported with the app."
  }
};
const tabs = [...document.querySelectorAll('[role="tab"]')];
const panel = document.getElementById("workflow-panel");
const image = document.getElementById("workflow-image");
const play = document.getElementById("play-demo");
const note = document.getElementById("demo-note");
const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
let currentView = "record";
let playing = false;

function stopDemo() {
  playing = false;
  image.src = views[currentView].image;
  image.alt = views[currentView].alt;
  play.textContent = currentView === "export" ? "Play exported GIF" : "Play real recording";
  play.setAttribute("aria-pressed", "false");
  if (currentView === "export") image.src = "assets/export-poster.webp";
}

function selectView(tab, moveFocus = false) {
  currentView = tab.dataset.view;
  tabs.forEach((item) => {
    const selected = item === tab;
    item.setAttribute("aria-selected", String(selected));
    item.tabIndex = selected ? 0 : -1;
  });
  panel.setAttribute("aria-labelledby", tab.id);
  image.classList.toggle("export-view", currentView === "export");
  document.getElementById("view-title").textContent = views[currentView].title;
  document.getElementById("view-description").textContent = views[currentView].description;
  note.textContent = currentView === "export"
    ? "Original demo artwork, captured and exported in v0.1.6. Playback starts when you press Play."
    : "Recorded in v0.1.6 on Linux/X11: launch, record, preview and export.";
  stopDemo();
  if (moveFocus) tab.focus();
}

tabs.forEach((tab, index) => {
  tab.addEventListener("click", () => selectView(tab));
  tab.addEventListener("keydown", (event) => {
    let next;
    if (event.key === "ArrowRight") next = (index + 1) % tabs.length;
    if (event.key === "ArrowLeft") next = (index - 1 + tabs.length) % tabs.length;
    if (event.key === "Home") next = 0;
    if (event.key === "End") next = tabs.length - 1;
    if (next !== undefined) { event.preventDefault(); selectView(tabs[next], true); }
  });
});
play.addEventListener("click", () => {
  if (playing) return stopDemo();
  playing = true;
  image.src = currentView === "export" ? views.export.image : "assets/workflow-v016.gif";
  image.alt = currentView === "export" ? views.export.alt : "GifFromScreen v0.1.6 on X11: open the recorder, capture motion, preview frames and export a GIF";
  play.textContent = "Stop animation";
  play.setAttribute("aria-pressed", "true");
});
document.addEventListener("visibilitychange", () => { if (document.hidden && playing) stopDemo(); });
new IntersectionObserver(([entry]) => { if (!entry.isIntersecting && playing) stopDemo(); }, { threshold: 0.05 }).observe(panel);
reducedMotion.addEventListener("change", () => { if (reducedMotion.matches && playing) stopDemo(); });

document.getElementById("copy-command").addEventListener("click", async () => {
  const status = document.getElementById("copy-status");
  try {
    await navigator.clipboard.writeText(document.getElementById("install-command").textContent.trim());
    status.textContent = "Copied. Paste it into your terminal to install.";
  } catch {
    status.textContent = "Clipboard unavailable. Select and copy the command above.";
  }
});
