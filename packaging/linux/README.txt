GifFromScreen — Linux portable edition

Run directly (no installation):
  ./bin/gif-from-screen
  ./bin/gif-from-screen-cli --help

Optional per-user installation (Python 3.8+ required only by the installer):
  ./install.sh
  ./install.sh --prefix /absolute/path/to/user-prefix

The default prefix is ~/.local. No sudo is needed. Installation refuses to
overwrite existing files, including files belonging to other applications.
Installing the exact same package again is a verified no-op. To upgrade,
uninstall the existing package first, then install the new package.

Uninstall (use the same prefix that was used for installation):
  ~/.local/lib/gifromscreen/uninstall.sh
  /absolute/path/to/user-prefix/lib/gifromscreen/uninstall.sh --prefix /absolute/path/to/user-prefix

Uninstall removes only unchanged, individually recorded application files.
It never removes projects, recordings, configuration, or arbitrary directories.
Changed installed files are preserved; review them before retrying uninstall.

Requirements:
  Linux x86_64; glibc baseline and linked libraries are recorded in BUILD-INFO.json.
  Wayland or X11 desktop; an OpenGL ES or Vulkan capable driver.
  For native Wayland capture: PipeWire and an appropriate xdg-desktop-portal
  backend (GNOME, KDE, or a compatible wlroots portal). Access is granted by
  the desktop's sharing dialog. Wayland does not grant global window placement.
  Typical Ubuntu 22.04+ packages: libpipewire-0.3-0, libwayland-client0,
  libxkbcommon0, libxkbcommon-x11-0, libxcb-shape0, libxcb-xfixes0, libegl1,
  libgl1, libvulkan1, and the portal backend for your desktop.
  Text authoring uses installed fonts; install fonts for the languages you use.

FFmpeg and ffprobe are OPTIONAL system dependencies for video import. They
are not bundled. GIF recording, editing, and GIF export do not need FFmpeg.

Verify the extracted bundle:
  sha256sum --check SHA256SUMS

The adjacent .tar.gz.sha256 verifies the download archive. Checksums detect
corruption; they do not replace a trusted download source or a signature.
See share/licenses/gifromscreen for licenses and dependency notices.

Project: https://github.com/tiansongyu/gifromscreen
