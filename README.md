# LinuxPlay

Low‑latency remote desktop for Linux, built from scratch in Rust.

- **Transport:** WebRTC (str0m) over mutual‑TLS signalling — encrypted, peer‑to‑peer.
- **Media:** GStreamer. Hardware H.264 / H.265 / AV1 (VA‑API, NVENC) with a software fallback; Opus audio.
- **UI:** GTK4 / libadwaita client.
- **Security:** self‑signed identities pinned by fingerprint; first pairing authenticated by a PIN (SPAKE2 + key confirmation binding both certificates).
- **Adaptive:** picks codec, bitrate and framerate from the link (LAN vs internet) and both ends' hardware, then follows the measured bandwidth live.

## Workspace

| crate | what it is |
|-------|-----------|
| `proto`  | wire protocol (signalling + data‑channel messages) |
| `net`    | WebRTC session, TLS identities, PIN pairing |
| `host`   | `linuxplay-host` — shares a desktop (X11 capture + XTest today) |
| `client` | `linuxplay` — the GTK4 app |

## Install

```
./packaging/install.sh
```

This builds LinuxPlay and installs it for the current user, including an icon
and two **application‑menu entries** so you never need the terminal:

- **LinuxPlay** — connect to another computer.
- **LinuxPlay Host** — share this computer's screen.

Install system‑wide with `sudo PREFIX=/usr/local ./packaging/install.sh`.

### Flatpak

```
./packaging/flatpak/build.sh
```

Builds and installs a sandboxed Flatpak (GNOME runtime, no root needed — it
uses `flatpak-builder` from Flathub). The same two menu entries appear, and it
runs as `flatpak run io.github.techlm77.LinuxPlay`.

## Build and run manually

```
cargo build --release
```

Run `linuxplay-host` on the machine to share, then `linuxplay <address>` on the
other (or just launch **LinuxPlay** from the menu and type the address).

### Multiple displays

The host offers each of its monitors separately. The client asks which one to
open, and the window's **displays** button opens the host's other monitors,
each in its own window. When more than one display streams at once, the host
automatically re‑balances bandwidth and framerate across them.

## Usage notes

- First connection pairs with a PIN the host shows (or a password set with
  `--password`). After that the client remembers the host.
- On a Wayland host the compositor asks which monitors to share the first time.
- Hotkeys in a stream window: **Ctrl+Alt+Shift+F** fullscreen, **+S** stats,
  **+Q** disconnect.

Licensed GPL‑3.0‑or‑later.
