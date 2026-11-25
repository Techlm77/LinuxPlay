# LinuxPlay

Low-latency remote desktop and game streaming between Linux computers.

- **Fast:** about 3–5 ms from one screen to the other on a home network.
- **Any GPU:** AMD, Intel and Nvidia, with H.264, H.265 or AV1. Mixed brands work.
- **Made for games:** controllers (with rumble), a locked mouse, and keyboard shortcuts in fullscreen.
- **Private:** everything is encrypted, nothing is shared without your OK, and
  remote access goes through your own [Tailscale](https://tailscale.com) network — no open ports, no outside server.
- **Several screens:** share any monitor, or a few at once.

## Install

Download from the [latest release](https://github.com/Techlm77/LinuxPlay/releases/latest)
(each release lists SHA-256 checksums).

**Flatpak** — works on any distribution with Flatpak:

```sh
flatpak install --user LinuxPlay-*-x86_64.flatpak
```

If the stream window says graphics drivers or codecs are missing (seen on
Linux Mint and Bazzite), run:

```sh
flatpak install flathub org.freedesktop.Platform.GL.default//26.08 org.freedesktop.Platform.codecs-extra//26.08-extra
```

**AppImage** — one file, nothing to install. Needs glibc 2.39 or newer
(Ubuntu 24.04, Debian 13, Fedora 40, Mint 22 or later, Arch); otherwise use the Flatpak.

```sh
chmod +x LinuxPlay-*-x86_64.AppImage
./LinuxPlay-*-x86_64.AppImage
```

<details>
<summary>Build from source</summary>

You need Rust 1.85+, GStreamer (base, good, bad, VA-API and PipeWire plugins),
SDL 3, GTK 4.14+ and libadwaita 1.5+. On Arch:

```sh
sudo pacman -S --needed rust base-devel cmake sdl3 gtk4 libadwaita pipewire \
    gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad \
    gst-plugin-va gst-plugin-pipewire gst-libav
cargo build --release
```

To build the packages yourself: `packaging/flatpak/build.sh --bundle`, or
`packaging/appimage/build.sh` on Ubuntu 24.04 (for example in
`docker run --rm -v "$PWD:/src" -w /src ubuntu:24.04 packaging/appimage/build.sh`).

</details>

## Getting started

Open **LinuxPlay** from your app menu. It has three pages:

| Page | What it does |
|---|---|
| **Connect** | Connect to a computer by its address, its name, or an invite link. Recent computers are one click away. |
| **Share** | Let others connect to this computer. PINs and connection requests pop up here. |
| **Account** | Sign in, so your computers can find each other by name from anywhere. |

**The first time** you connect to a computer, it shows a 6-digit PIN; type it
on the computer you're connecting from. After that they remember each other.
Someone at the shared computer approves each connection, unless you turn on
*Let paired devices in without asking*.

If you close the window while sharing or streaming, LinuxPlay keeps running in
the background. Open it again to bring the window back.

### From the command line

Everything also works from a terminal (with the Flatpak, use
`flatpak run io.github.techlm77.LinuxPlay` for `linuxplay`, and add
`--command=linuxplay-host` for `linuxplay-host`):

```sh
linuxplay-host                 # share this computer
linuxplay 192.168.1.20         # connect to it (an address or a name)
linuxplay hosts                # computers you've paired with
linuxplay forget ADDRESS       # forget one (for example after a reinstall)
```

## Away from home

Use [Tailscale](https://tailscale.com): install it on your computers and sign
them in. Then connect by the Tailscale name, for example `linuxplay office-pc`.

**Never open ports on your router.** If LinuxPlay notices it can be reached
from the internet, it warns you and tells you how to fix it.

**Accounts (optional).** One of your computers can keep your account: turn on
*Be the account server for my devices* on its Share page, then sign in to it
from your other computers (Account page). They can then see each other by name
and send invite links. The account lives on your computer, not on ours.

**Invite a guest:** on the Share page, choose *New link…*, set how long it
works and whether the guest may control, and send them the link. They don't
need an account. The guest needs to reach your account server, so share that
computer with them through Tailscale.

## In the stream window

| Keys | |
|---|---|
| Ctrl+Alt+Shift+**Q** | disconnect |
| Ctrl+Alt+Shift+**F** | fullscreen (shortcuts then go to the other computer) |
| Ctrl+Alt+Shift+**M** | game mouse: lock the pointer to the window |
| Ctrl+Alt+Shift+**S** | statistics: frame rate, bitrate and latency |

Controllers plugged into your computer show up on the other one, rumble
included. Copied text can be pasted on the other computer, both ways (in sessions that may control).

Controllers need the shared computer to allow virtual input devices. Bazzite,
SteamOS, CachyOS, EndeavourOS, Manjaro, Fedora KDE and Pop!_OS already do. On
others (Arch, Debian, Ubuntu, Mint, Zorin, Fedora GNOME, openSUSE, NixOS), run
this once:

```sh
sudo cp packaging/70-linuxplay-uinput.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger --name-match=uinput
```

## More for the shared computer

- **No monitor attached?** Turn on *Offer a virtual screen* (or run
  `linuxplay-host --virtual-display`). GNOME then offers a 1920×1080
  "Virtual Monitor"; KDE Plasma doesn't support this yet.
- **Nobody sitting there?** Turn on *Let paired devices in without asking*
  (`linuxplay-host --unattended`). Each connection still shows a notification.
  To pair new devices without reading a PIN off the screen, set a password
  with `linuxplay-host set-password` (12 characters or more).
- **Managing it remotely**, for example over SSH:

| Command | |
|---|---|
| `linuxplay-host sessions` | PINs, waiting connections, who is connected |
| `linuxplay-host approve ID` / `deny ID` | answer a waiting connection (`--view-only` to only show) |
| `linuxplay-host disconnect ID` / `disconnect all` | end a session / stop sharing |
| `linuxplay-host revoke FINGERPRINT` | remove a paired device |
| `linuxplay-host invite --minutes 30` / `uninvite ID` | make or withdraw an invite link |

## Performance

1080p at 60 fps, measured by the client for every frame:

| Shared computer → your computer | Codec | Latency |
|---|---|---|
| Nvidia RTX 5060 Ti → AMD RX 7800 XT, wired | H.265 | 3.0 ms (real screen) |
| Nvidia RTX 5060 Ti → AMD RX 7800 XT, wired | H.264 | 5.5 ms (test picture) |
| AMD Radeon 610M (integrated) → AMD RX 7800 XT, wired | H.265 | 7.1 ms (test picture) |

On Wayland, the screen goes from the GPU straight to the encoder without
passing through the CPU ("zero copy"). Sharing a busy 1080p60 screen uses
about 6 % of one CPU core on an Nvidia card and 2 % on AMD.

## Tested on

Each was a fresh install in a virtual machine, set up the way this README
says, with a stream checked at full frame rate. Latency here is with software
encoding (the VMs have no GPU), so real computers are faster.

| System | Desktop | Flatpak | AppImage | Extra steps |
|---|---|---|---|---|
| Arch Linux | GNOME 51 | ✓ 7.5 ms | ✓ 7.2 ms | – |
| Arch Linux | Hyprland 0.57 | ✓ 7.3 ms | ✓ 7.3 ms | install Flatpak |
| Arch Linux | sway 1.12 | ✓ 7.6 ms | ✓ 7.4 ms | install Flatpak |
| Arch Linux | KDE Plasma 6.8 | ✓ 7.8 ms | ✓ 7.4 ms | – |
| Bazzite | KDE Plasma 6.8 | ✓ 8.4 ms | ✓ 7.7 ms | – |
| CachyOS | KDE Plasma 6.8 | ✓ 7.5 ms | ✓ 6.8 ms | install Flatpak |
| Debian 12 | GNOME 43 | ✓ 8.5 ms | use the Flatpak | install Flatpak |
| Debian 13 | GNOME 48 | ✓ 8.1 ms | ✓ 7.4 ms | install Flatpak |
| EndeavourOS | KDE Plasma 6.8 | ✓ 7.4 ms | ✓ 7.3 ms | install Flatpak |
| Fedora 45 | KDE Plasma 6.8 | ✓ 7.8 ms | ✓ 8.2 ms | – |
| Fedora 45 Workstation | GNOME 51 | ✓ 7.5 ms | ✓ 7.9 ms | – |
| Linux Mint 22.3 | Cinnamon 6.6 (X11) | ✓ 12.6 ms | ✓ 10.2 ms | drivers/codecs (see Install) |
| Manjaro | KDE Plasma 6.7 | ✓ 7.7 ms | ✓ 7.4 ms | – |
| NixOS 26.05 | GNOME 50 | ✓ 8.0 ms | ✓ 8.1 ms | – |
| Pop!_OS 24.04 | COSMIC 1.11 | ✓ 11.4 ms | ✓ 12.2 ms | – |
| SteamOS | KDE Plasma 6.7 (X11) | ✓ 6.9 ms | ✓ 7.4 ms | – |
| Ubuntu 22.04 | GNOME 42 (X11) | ✓ 7.4 ms | use the Flatpak | install Flatpak |
| Ubuntu 24.04 | GNOME 46 | ✓ 8.0 ms | ✓ 8.0 ms | install Flatpak |
| Ubuntu 26.04 | GNOME 50 | ✓ 7.5 ms | ✓ 7.5 ms | install Flatpak |
| Zorin OS 18.1 | GNOME 46 | ✓ 8.9 ms | ✓ 7.9 ms | – |
| openSUSE Tumbleweed | KDE Plasma 6.8 | ✓ 7.8 ms | ✓ 7.7 ms | – |

Wayland unless marked X11. On X11 the real screen was captured too.

## Security

- **Encrypted:** video, sound and input are always encrypted.
- **Paired:** computers prove who they are with certificates. A new computer
  must enter a PIN once; if a known computer's identity changes, LinuxPlay
  refuses to connect.
- **Your OK:** nothing is shared until someone at the computer approves it.
  A view-only session can't type or click, even if it tries.
- **Easy to undo:** removing a device or stopping sharing ends its sessions at once.
- **Account server can't spy:** it only introduces your computers to each
  other. It can't see or control a session, and introduced computers still
  have to pair.
- **Guests stay guests:** an invite works for one visit, with only the rights
  you gave it, and withdrawing it disconnects them.
- **Open ports are flagged:** LinuxPlay warns if it can be reached from the
  internet.

## Network and firewall

The shared computer listens on your local network by default.
`linuxplay-host --listen tailscale` limits it to Tailscale, `loopback` to this
computer only. Ports: TCP **47800**, and UDP **47801–47816** (one per screen).

To let only your own devices in through Tailscale, tag the computer (for
example `tag:linuxplay-host`) and allow just these ports in your tailnet
policy:

```json
{ "grants": [ { "src": ["you@example.com"], "dst": ["tag:linuxplay-host"], "ip": ["tcp:47800", "udp:47801-47816"] } ] }
```

Don't use Tailscale Funnel for LinuxPlay; it puts things on the public internet.

## Compatibility

- **X11:** any desktop or window manager.
- **Wayland:** GNOME, KDE Plasma and wlroots desktops (sway, Hyprland), through
  `xdg-desktop-portal`. The desktop asks once before sharing the screen.
- **GPUs:** AMD and Intel (VA-API), Nvidia (NVENC/NVDEC), Vulkan Video, or
  software, picked automatically.
- **Tray icon** (standalone host): shown on KDE and most panels; GNOME needs the
  AppIndicator extension.

## For developers

The window is a front end for an engine you can drive yourself.
`linuxplay service` runs it; it listens on `$XDG_RUNTIME_DIR/linuxplay/api.sock`
(JSON-RPC 2.0, one message per line, this user only). After `subscribe` it
sends the full state whenever something changes. Every method and type is
documented in [`crates/proto/src/api.rs`](crates/proto/src/api.rs).

```sh
linuxplay service &
linuxplay api state
linuxplay api subscribe        # prints every change
```

| Crate | |
|---|---|
| `crates/app` | `linuxplay`: stream window, engine and API client |
| `crates/gui` | `linuxplay-gui`: the window (GTK 4 + libadwaita) |
| `crates/host` | `linuxplay-host`: capture, encoding, input, approval, invites |
| `crates/client` | the stream window: decoding, drawing, input |
| `crates/net` | identities, pairing, encryption, WebRTC, the account client |
| `crates/media` | shared GStreamer helpers and the latency clock |
| `crates/proto` | message formats |
| `crates/rendezvous` | `linuxplay-rendezvous`: the account server |

`cargo test --workspace` runs the tests. `LINUXPLAY_ZERO_COPY=0` turns zero
copy off, for comparisons.

## Not there yet

- The mouse pointer is drawn into the video, so it follows your hand with a
  tiny delay.

## License

GPL-3.0-or-later.
