# LinuxPlay

Low‑latency remote desktop for Linux, built from scratch in Rust.

- **Transport:** WebRTC (str0m) over mutual‑TLS signalling — encrypted, peer‑to‑peer.
- **Media:** GStreamer. Hardware H.264 / H.265 / AV1 (VA‑API on Intel/AMD, NVENC on
  Nvidia) with a software fallback; Opus audio.
- **UI:** GTK4 / libadwaita client.
- **Security:** self‑signed identities pinned by fingerprint; first pairing
  authenticated by a PIN (SPAKE2 + key confirmation binding both certificates).
- **Adaptive:** picks the codec, bitrate and framerate from the link (LAN vs
  internet) and both ends' hardware, then follows the measured bandwidth live.
- **Multi‑monitor:** each display is offered separately and can be streamed at
  once, with the load re‑balanced across them.

It runs on **any desktop** — it does not depend on a particular desktop
environment or window manager. See [Compatibility](#compatibility).

## Requirements

Rust (1.85+) and GStreamer to build. The **host** needs only GStreamer; the
**client** also needs GTK4 and libadwaita.

### Arch / Manjaro

```sh
# Build + runtime
sudo pacman -S --needed rust base-devel cmake \
    gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad \
    gst-plugin-va gst-libav gst-plugin-pipewire \
    gtk4 libadwaita
```

### Debian / Ubuntu

```sh
sudo apt install cargo build-essential cmake pkg-config \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    libgtk-4-dev libadwaita-1-dev \
    gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
    gstreamer1.0-vaapi gstreamer1.0-libav \
    gstreamer1.0-pipewire gstreamer1.0-pulseaudio
```

### Fedora

```sh
sudo dnf install cargo gcc cmake pkgconf-pkg-config \
    gstreamer1-devel gstreamer1-plugins-base-devel \
    gtk4-devel libadwaita-devel \
    gstreamer1-plugins-good gstreamer1-plugins-bad-free \
    gstreamer1-plugin-pipewire
# gstreamer1-vaapi and gstreamer1-libav come from RPM Fusion.
```

**Nvidia (NVENC/NVDEC):** also install the `nvcodec` GStreamer plugin — Arch
`gst-plugins-bad` includes it; Debian/Ubuntu ship it in `gstreamer1.0-plugins-bad`;
Fedora needs RPM Fusion. Without it, Nvidia machines fall back to software.

## Install

```sh
./packaging/install.sh
```

This builds LinuxPlay and installs it for the current user under `~/.local`,
with an icon and **application‑menu entries** so you never need the terminal:

- **LinuxPlay** — connect to another computer.
- **LinuxPlay Host** — share this computer's screen.

On a machine you only want to *share* (a host with no GTK4), the installer builds
and installs just **LinuxPlay Host**. Install system‑wide with
`sudo PREFIX=/usr/local ./packaging/install.sh`, and remove with
`./packaging/install.sh uninstall`.

## Usage

1. On the computer to share, launch **LinuxPlay Host** (or run `linuxplay-host`).
2. On the other computer, launch **LinuxPlay**, type the host's address, Connect.
3. The first time, enter the **PIN** the host shows (or a password set with
   `linuxplay-host --password …`). After that it reconnects automatically.

Check what a machine can encode with `linuxplay-host encoders`, and its identity
and paired clients with `linuxplay-host info`.

- **Multiple displays:** the client asks which display to open; the window's
  *displays* button opens the host's others, each in its own window.
- **Hotkeys** in a stream window: **Ctrl+Alt+Shift+F** fullscreen, **+S** stats
  overlay, **+Q** disconnect.

## Compatibility

| | Supported |
|---|---|
| **Display server** | X11 (any window manager); Wayland via `xdg-desktop-portal` |
| **GPU** | Intel / AMD (VA‑API), Nvidia (NVENC), or software — chosen automatically |
| **Desktops** | any — GNOME, KDE, XFCE, Cinnamon, sway/wlroots, … |

Notes:
- **X11 hosts** capture with `ximagesrc` and inject input with XTEST — works on
  every X11 window manager, no prompts.
- **Wayland hosts** use the ScreenCast + RemoteDesktop portals, so they need
  `xdg-desktop-portal` plus a backend for the compositor
  (`xdg-desktop-portal-gnome`, `-kde`, or `-wlr`). The compositor shows a
  one‑time share prompt. Input injection needs a backend that implements the
  RemoteDesktop portal (GNOME and KDE do; wlroots added it more recently).
- **Host tray icon** uses the StatusNotifierItem standard — visible on KDE,
  XFCE, and most panels. GNOME hides it unless the *AppIndicator* extension is
  installed; the host still runs and posts a desktop notification on start.

## Workspace

| crate | what it is |
|-------|-----------|
| `proto`  | wire protocol (signalling + data‑channel messages) |
| `net`    | WebRTC session, TLS identities, PIN pairing |
| `host`   | `linuxplay-host` — shares a desktop (X11 or Wayland portal) |
| `client` | `linuxplay` — the GTK4 app |

Licensed GPL‑3.0‑or‑later.
