# LinuxPlay

Low‑latency remote desktop for Linux, built from scratch in Rust.

- **Transport:** WebRTC (str0m) over mutual‑TLS signalling — encrypted, peer‑to‑peer.
- **Media:** GStreamer. Hardware H.264 / H.265 / AV1 (VA‑API on Intel/AMD, NVENC on
  Nvidia) with a software fallback; Opus audio.
- **UI:** GTK4 / libadwaita client.
- **Security:** self‑signed identities pinned by fingerprint; first pairing
  authenticated by a PIN (SPAKE2 + key confirmation binding both certificates);
  every connection approved at the host, with a view‑only option; listens only
  on the network you choose. See [Security](#security).
- **Adaptive:** picks the codec, bitrate and framerate from the link (LAN vs
  internet) and both ends' hardware, then follows the measured bandwidth live.
- **Multi‑monitor:** each display is offered separately and can be streamed at
  once, with the load re‑balanced across them.

It runs on **any desktop** — it does not depend on a particular desktop
environment or window manager. See [Compatibility](#compatibility).

## Requirements

A current **Rust** (1.85+) and **GStreamer** to build. The **host** needs only
GStreamer; the **client** also needs GTK4 and libadwaita.

> **Rust:** install it with [rustup](https://rustup.rs) —
> `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh` — not your
> distro's `cargo`, which is often too old to build this (Ubuntu 24.04 ships
> 1.75). Arch's rolling `rust` package is fine.

### Arch / Manjaro

```sh
sudo pacman -S --needed rust base-devel cmake \
    gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad \
    gst-plugin-va gst-libav gst-plugin-pipewire \
    gtk4 libadwaita
```

### Debian / Ubuntu

```sh
# Rust from rustup (above); the rest from apt:
sudo apt install build-essential cmake pkg-config \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    libgtk-4-dev libadwaita-1-dev \
    gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
    gstreamer1.0-vaapi gstreamer1.0-libav \
    gstreamer1.0-pipewire gstreamer1.0-pulseaudio
```

### Fedora

```sh
# Rust from rustup (above); the rest from dnf:
sudo dnf install gcc cmake pkgconf-pkg-config \
    gstreamer1-devel gstreamer1-plugins-base-devel \
    gtk4-devel libadwaita-devel \
    gstreamer1-plugins-good gstreamer1-plugins-bad-free \
    gstreamer1-plugin-pipewire
# gstreamer1-vaapi and gstreamer1-libav come from RPM Fusion.
```

### NixOS

A `flake.nix` is included: `nix build` produces the binaries, and `nix develop`
gives a shell with every build dependency.

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
3. The first time, enter the **PIN** the host shows. After that the client
   recognises the host by its certificate.
4. Each time someone connects, the host asks **View only**, **Allow control** or
   **Deny** — in a notification, in the tray menu, or from a terminal. No answer
   within a minute means no. While one window of a device is approved, its other
   display windows join without asking again.

Check what a machine can encode with `linuxplay-host encoders`, and its identity
and paired clients with `linuxplay-host info`.

- **Multiple displays:** the client asks which display to open; the window's
  *displays* button opens the host's others, each in its own window.
  `--displays 0,1` limits which displays the host offers at all.
- **Hotkeys** in a stream window: **Ctrl+Alt+Shift+F** fullscreen, **+S** stats
  overlay, **+Q** disconnect.

### Managing a running host

| | |
|---|---|
| `linuxplay-host sessions` | pairing PINs, connections waiting for approval, who is connected |
| `linuxplay-host approve ID [--view-only]` / `deny ID` | answer a waiting connection |
| `linuxplay-host disconnect ID` / `disconnect all` | end a session / stop sharing |
| `linuxplay-host revoke FINGERPRINT` / `revoke --all` | forget a device; its live sessions end at once |

These talk to the running host over a private socket, so they work over SSH
too, and they report what the host actually did. The tray menu offers the same.

### Unattended access

For a computer nobody sits at, `linuxplay-host --unattended` lets **paired**
devices connect without the approval prompt (each connection still shows a
notification), and lets the Wayland screen‑share grant be remembered. Since no
one is there to read a PIN, you can set a pairing password instead:

```sh
linuxplay-host set-password     # asks twice, without echo; stored 0600
linuxplay-host --unattended
```

The password is only used with `--unattended`, must be at least 12 characters,
and is never taken from the command line or the environment.

## Network exposure

The host listens only where you say:

| `--listen` | listens on |
|---|---|
| `lan` (default) | private addresses of this machine's network interfaces (not VPNs, containers or public addresses) |
| `tailscale` | this machine's Tailscale addresses only; refuses to start if Tailscale is down |
| `loopback` | this machine only |
| `all` | every interface — not recommended |

`--bind ADDRESS` (repeatable) picks exact addresses instead. Signalling uses TCP
**47800** (`--port`); media uses one UDP port per display from **47801–47816**
(`--media-ports`), so a firewall can allow exactly those.

### Tailscale

Tailscale gives you a private network between your machines without opening
ports. It carries the traffic; it doesn't replace LinuxPlay's own pairing and
approval — being on the tailnet doesn't make a device trusted.

1. Install Tailscale on both machines and log in to the same tailnet.
2. On the host: `linuxplay-host --listen tailscale`.
3. On the client, pick the host under **On your tailnet**, or type its Tailscale
   address or MagicDNS name. Pair with the PIN as usual.

To let only your own devices reach it, tag the host (e.g. `tag:linuxplay-host`)
and allow just LinuxPlay's ports in your tailnet policy:

```json
{
  "grants": [
    { "src": ["you@example.com"], "dst": ["tag:linuxplay-host"], "ip": ["tcp:47800", "udp:47801-47816"] }
  ]
}
```

Grants add access, so remove any broader allow‑all rule if you want this to be
the only way in. Don't use Tailscale Funnel for this (it publishes a service to
the internet). `tailscale ping HOST` shows whether the path is direct or relayed;
direct is what you want for low latency.

## Security

- **Identity:** each side has a self‑signed certificate; the client pins the
  host's. A host the client hasn't pinned must complete PIN pairing — the
  client refuses one that claims to know it without pairing. A changed host
  certificate is refused, never silently re‑pinned.
- **Consent:** nothing is shared until the person at the host approves the
  connection (unless the host runs `--unattended`). A view‑only session's input
  is dropped by the host, not just hidden by the client. On Wayland the
  compositor's share prompt appears again for each new visit.
- **Revocation:** `revoke`, *Disconnect* and *Stop sharing* end live sessions
  immediately; the host also notices edits to its trust file within seconds.
- **Limits:** every handshake stage has a deadline; unauthenticated
  connections, pairing attempts and wrong PINs are limited per source address
  (with back‑off), plus a global rate; network, media and input queues are
  bounded, and input is validated and rate‑limited.
- **Secrets:** the PIN is shown only on the host's screen (a transient
  notification, the tray, `linuxplay-host sessions`, or its terminal) — never in
  the log. Keys, trust lists and the portal grant are `0600` files in `0700`
  directories. If the client's list of paired hosts is damaged, it won't
  connect until you repair it or choose to start over.

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
