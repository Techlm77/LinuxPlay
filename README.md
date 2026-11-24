# LinuxPlay

Low-latency remote desktop and game streaming for Linux, written in Rust.

- **Fast:** about **5 ms** from capture to screen from an Nvidia host to an AMD
  client on a LAN at 1080p 60 fps, about 9 ms AMD to AMD (see
  [Performance](#performance)). Frames go from the decoder to the screen
  without leaving the GPU, and the newest frame always replaces an older one.
- **Any GPU:** VA-API (AMD, Intel), NVENC/NVDEC (Nvidia), Vulkan Video and
  software, for H.264, H.265 and AV1. The host and client agree on the best
  pair automatically; mixed vendors work.
- **Made for play:** game controllers are forwarded as virtual gamepads, the
  mouse can be locked to the window for games, and system shortcuts go to the
  host in fullscreen.
- **Find your computers:** over Tailscale, with an account server that runs on
  your own computer: connect to your devices by name from anywhere, or send a
  guest an invite link. No ports to open, no third-party server.
- **Secure by construction:** WebRTC (DTLS-SRTP) media, mutually authenticated
  TLS signalling with pinned device certificates, PIN pairing, approval at the
  host, view-only sessions. The account server can never see or control a
  session, and LinuxPlay warns if anything is left open to the internet. See [Security](#security).
- **Multi-monitor:** each display is offered separately and several can be
  streamed at once.

> **Status:** the engine below is working and tested. The graphical front end
> (sign-in, device list, invites) is being built separately; until it lands,
> everything is driven from the command line.

## Install

### Flatpak

```sh
packaging/flatpak/build.sh            # builds and installs for this user, no root
flatpak run io.github.techlm77.LinuxPlay ADDRESS
flatpak run --command=linuxplay-host io.github.techlm77.LinuxPlay
```

`build.sh --bundle` also writes `LinuxPlay.flatpak`, a single file that
installs on another machine with `flatpak install --user LinuxPlay.flatpak`.
Now and then a bundle install leaves out the runtime's graphics drivers or
codecs (seen on Linux Mint and Bazzite); the stream window then says so.
Install them with:

```sh
flatpak install flathub org.freedesktop.Platform.GL.default//26.08 org.freedesktop.Platform.codecs-extra//26.08-extra
```
The Flatpak brings its own GStreamer, SDL and codecs; on Nvidia, Flatpak adds
the matching driver extension by itself.

### AppImage

One file that runs without installing anything:

```sh
chmod +x LinuxPlay-*-x86_64.AppImage
./LinuxPlay-*-x86_64.AppImage TARGET          # the stream window
./LinuxPlay-*-x86_64.AppImage host            # share this computer
./LinuxPlay-*-x86_64.AppImage service         # the engine, for a front end
```

It bundles SDL and GStreamer with the plugins LinuxPlay uses, and takes the
graphics drivers (VA-API, NVIDIA's CUDA, Mesa) from the system. It needs glibc
2.39 or newer: Ubuntu 24.04, Debian 13, Fedora 40, Linux Mint 22 and later,
and the Arch family; on older systems use the Flatpak. Build it with
`packaging/appimage/build.sh` on Ubuntu 24.04 (for example in
`docker run --rm -v "$PWD:/src" -w /src ubuntu:24.04 packaging/appimage/build.sh`).

### From source

Rust 1.85+ (from [rustup](https://rustup.rs)), GStreamer with its base, good,
bad, VA-API and PipeWire plugins, and SDL 3. On Arch:

```sh
sudo pacman -S --needed rust base-devel cmake sdl3 pipewire \
    gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad \
    gst-plugin-va gst-plugin-pipewire gst-libav
cargo build --release
```

This builds `linuxplay` (stream window and engine), `linuxplay-host` and
`linuxplay-rendezvous` (the account server) into `target/release/`.

## Usage

### On your network

On the computer to share:

```sh
linuxplay-host
```

On the other one:

```sh
linuxplay 192.168.1.20        # or a hostname; add :PORT if you changed it
```

The first time, the host shows a PIN; type it into the client's window. After
that the two recognise each other. Unless the host runs `--unattended`,
someone at the host approves each connection (with a view-only choice).

`linuxplay hosts` lists the hosts this computer has paired with. If one is
reinstalled, its identity changes and the client refuses it (as it would an
impostor); `linuxplay forget ADDRESS` (or its name) drops the old pairing, and
the next connection pairs from scratch.

### From anywhere, over Tailscale

LinuxPlay reaches your computers away from home over
[Tailscale](https://tailscale.com), your own private network: **never open
ports on your router.** If a host or account server is reached from the open
internet anyway, LinuxPlay warns in its log, with a desktop notification, in
the stream window and in the front end, and tells you to close the ports and
use Tailscale.

Install Tailscale on your computers and sign them in to the same tailnet. A
host listens on its Tailscale address as well as the local network, so this
already works:

```sh
linuxplay office-pc                  # the host's Tailscale (MagicDNS) name
```

### With an account (your own, on your own computer)

One of your computers can also be the **account server**: your devices sign in
to it over Tailscale, list each other by name, show which are online, and can
hand out invite links. There is no central LinuxPlay server; your account
lives on your machine.

```sh
# on the computer that keeps the accounts (any of yours, usually the host)
linuxplay-rendezvous                 # listens on this computer's Tailscale addresses only
linuxplay-host account create --server office-pc --username you
linuxplay-host                       # now reachable through the account

# on another of your computers
linuxplay-host account login --server office-pc --username you
linuxplay "Office PC"                # a device's name, from `account status`
```

`--server` takes the account server's Tailscale name or address. LinuxPlay
refuses to sign in over plain HTTP to anything that is not Tailscale, which
encrypts the connection itself. `linuxplay-host account status` lists the
account's computers and which are online. A new pair of computers still pairs
once, so the account server alone can never connect anyone to your desktop.

### Inviting a guest

```sh
linuxplay-host invite --minutes 30            # add --view-only to only show
# ok: http://office-pc:47900/join#3fKq….8xQw…
```

Send the link; the guest runs `linuxplay "LINK"`, with no account needed.
The guest must be able to reach your account server, so share that computer
with them on Tailscale (Tailscale's *Share* feature) rather than opening ports.
`linuxplay-host uninvite ID` withdraws it and disconnects whoever came by it.

### In the stream window

| Keys | |
|---|---|
| Ctrl+Alt+Shift+**Q** | disconnect |
| Ctrl+Alt+Shift+**F** | fullscreen (system shortcuts then go to the host) |
| Ctrl+Alt+Shift+**M** | game mouse: lock the pointer and send raw movement |
| Ctrl+Alt+Shift+**S** | statistics: resolution, bitrate, and where each millisecond goes |

Game controllers plugged into the client appear on the host as virtual
gamepads, and rumble from games on the host plays on the client's controller.
Text copied on either computer can be pasted on the other (only in sessions
allowed to control the host). That needs the host's user to be allowed to create input devices.
In our tests Bazzite, SteamOS, CachyOS, EndeavourOS, Manjaro, Fedora KDE and
Pop!_OS already allow it; elsewhere (Arch, Debian, Ubuntu, Mint, Zorin, Fedora GNOME,
openSUSE, NixOS) install LinuxPlay's udev rule once:

```sh
sudo cp packaging/70-linuxplay-uinput.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger --name-match=uinput
```
`linuxplay --help` lists the options (`--display`, `--fullscreen`,
`--game-mouse`, `--stats`).

### Headless hosts: virtual displays

On Wayland, `linuxplay-host --virtual-display` (or `"virtual_display": true`
through the API) adds "a new virtual screen" to the share dialog: the
compositor makes a display that exists only for the stream, so a computer with
no monitor attached can still be used. It is 1920×1080. GNOME offers it as
"Virtual Monitor" (tested on GNOME 51); KDE Plasma's dialog doesn't offer one
yet and shares the real monitors instead.

### Managing a running host

| | |
|---|---|
| `linuxplay-host sessions` | pairing PINs, connections waiting for approval, who is connected |
| `linuxplay-host approve ID [--view-only]` / `deny ID` | answer a waiting connection |
| `linuxplay-host disconnect ID` / `disconnect all` | end a session / stop sharing |
| `linuxplay-host revoke FINGERPRINT` / `revoke --all` | forget a device; its live sessions end at once |
| `linuxplay-host invite [--minutes N] [--view-only]` / `uninvite ID` | make or withdraw an invite link |

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

### For front-end developers: the engine's API

`linuxplay service` runs the engine: it keeps the computer signed in, shares
it on request, opens stream windows, and reports everything a front end shows.
A front end talks to it over a Unix socket only the same user can open,
`$XDG_RUNTIME_DIR/linuxplay/api.sock`, in JSON-RPC 2.0, one message per line:

```text
→ {"jsonrpc":"2.0","id":1,"method":"host.start","params":{"account_server":true}}
← {"jsonrpc":"2.0","id":1,"result":{"device":{…},"account":null,"hosting":{…},"connections":[]}}
```

After `subscribe` the engine pushes the whole new state whenever anything
changes (a PIN to show, a connection waiting for approval, a guest leaving),
so a front end only has to draw the latest one. Methods: `state`, `subscribe`,
`account.create`/`login`/`logout`/`devices`/`remove_device`,
`host.start`/`stop`/`decide`/`disconnect`/`invite`/`uninvite`/`trusted`/`revoke`,
`connect`, `disconnect`. Every type is documented in
[`crates/proto/src/api.rs`](crates/proto/src/api.rs). Try it from a shell:

```sh
linuxplay service &
linuxplay api state
linuxplay api host.start '{"unattended": false}'
linuxplay api subscribe               # prints every change
```

`state.hosting.warning` is set while the computer can be reached from the
internet; show it prominently.

## Performance

Capture to screen at 1080p 60 fps, measured per frame by the client (the
statistics panel shows the same breakdown live):

| Host → client | Codec | Total | of which encoding |
|---|---|---|---|
| Nvidia RTX 5060 Ti → AMD RX 7800 XT, wired LAN | H.265 | 5.3 ms | 4.5 ms |
| Nvidia RTX 5060 Ti → AMD RX 7800 XT, wired LAN | H.264 | 5.5 ms | 4.9 ms |
| AMD Radeon 610M (integrated) → AMD RX 7800 XT, wired LAN | H.265 | 7.1 ms | 6.5 ms |
| AMD RX 7800 XT → same machine | H.265 / AV1 | 9.1 ms | 8.7 ms |
| AMD RX 7800 XT → same machine | H.264 | 12.1 ms | 11.7 ms |

Decoding takes about 0.3 ms and presenting 0.1 to 0.2 ms; the rest is the
network. These use a generated test picture (`linuxplay-host --capture test
--test-size 1920x1080`), so they exclude the compositor's own capture delay.

## Tested on

Every row is a fresh, fully updated install in a virtual machine (no GPU, so
software encoding and decoding), with the Flatpak installed the way the
Install section says, a host and a client run in the desktop session, and the
picture checked to arrive at full frame rate. On X11 the real screen was
captured too. The AppImage went through the same test on the same machines.
Tested 2026-11-23 and 2026-11-24 with the current build.

| System | Desktop | Session | Flatpak | Flatpak latency* | AppImage, latency* | Screen capture (X11) | Extra steps (Flatpak; the AppImage needed none) |
|---|---|---|---|---|---|---|---|
| Arch Linux | GNOME 51.1 | Wayland | passed | 7.5 ms | passed, 7.2 ms | – | none |
| Arch Linux | Hyprland 0.57.2 | Wayland | passed | 7.3 ms | passed, 7.3 ms | – | install Flatpak first |
| Arch Linux | sway 1.12 | Wayland | passed | 7.6 ms | passed, 7.4 ms | – | install Flatpak first |
| Arch Linux | KDE Plasma 6.8.3 | Wayland | passed | 7.8 ms | passed, 7.4 ms | – | none |
| Bazzite | KDE Plasma 6.8.2 | Wayland | passed | 8.4 ms | passed, 7.7 ms | – | none |
| CachyOS | KDE Plasma 6.8.3 | Wayland | passed | 7.5 ms | passed, 6.8 ms | – | install Flatpak first |
| Debian GNU/Linux 12 (bookworm) | GNOME 43.9 | Wayland | passed | 8.5 ms | use the Flatpak (glibc older than 2.39) | – | install Flatpak first |
| Debian GNU/Linux 13 (trixie) | GNOME 48.7 | Wayland | passed | 8.1 ms | passed, 7.4 ms | – | install Flatpak first |
| EndeavourOS | KDE Plasma 6.8.3 | Wayland | passed | 7.4 ms | passed, 7.3 ms | – | install Flatpak first |
| Fedora Linux 45 (Forty Five) | KDE Plasma 6.8.3 | Wayland | passed | 7.8 ms | passed, 8.2 ms | – | none |
| Fedora Linux 45 (Workstation Edition) | GNOME 51.1 | Wayland | passed | 7.5 ms | passed, 7.9 ms | – | none |
| Linux Mint 22.3 | Cinnamon 6.6.9 | X11 | passed | 12.6 ms | passed, 10.2 ms | yes | runtime parts had to be installed (see Install) |
| Manjaro Linux | KDE Plasma 6.7.5 | Wayland | passed | 7.7 ms | passed, 7.4 ms | – | none |
| NixOS 26.05 (Yarara) | GNOME 50.6 | Wayland | passed | 8.0 ms | passed, 8.1 ms | – | none |
| Pop!_OS 24.04 LTS | COSMIC 1.11.0 | Wayland | passed | 11.4 ms | passed, 12.2 ms | – | none |
| SteamOS | KDE Plasma 6.7.3 | X11 | passed | 6.9 ms | passed, 7.4 ms | yes | none |
| Ubuntu 22.04.5 LTS | GNOME 42.9 | X11 | passed | 7.4 ms | use the Flatpak (glibc older than 2.39) | yes | install Flatpak first |
| Ubuntu 24.04.5 LTS | GNOME 46.0 | Wayland | passed | 8.0 ms | passed, 8.0 ms | – | install Flatpak first |
| Ubuntu 26.04.1 LTS | GNOME 50.1 | Wayland | passed | 7.5 ms | passed, 7.5 ms | – | install Flatpak first |
| Zorin OS 18.1 | GNOME 46.0 | Wayland | passed | 8.9 ms | passed, 7.9 ms | – | none |
| openSUSE Tumbleweed | KDE Plasma 6.8.2 | Wayland | passed | 7.8 ms | passed, 7.7 ms | – | none |

\* Capture to screen inside the VM, software H.264 on both ends; on real
hardware it is lower (see Performance).

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
3. On the client: `linuxplay ADDRESS` with the host's Tailscale address or
   MagicDNS name. Pair with the PIN as usual.

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

### Accounts and the account server

An account makes your computers findable by name and lets you invite guests.
It is deliberately not a source of trust:

- **Yours, on your tailnet.** The account server runs on one of your own
  computers and listens only on its Tailscale addresses. Nothing goes through
  a LinuxPlay-run server or relay.
- The server is a **directory and a pipe**. It knows who is signed in and
  passes bytes between two devices that asked to be connected. Inside that
  pipe the devices run the same mutually authenticated TLS session they use
  directly, so the server cannot read or change what they say.
- Being introduced by the server **earns nothing**: a new device still pairs
  with the host once (PIN or pairing password), and attended hosts still ask
  before every connection. A compromised server can refuse service; it cannot
  watch or control a computer.
- An **invite** is a link holding the invite's id and a secret. The secret
  stays in the link's fragment, is never sent to the server, and is proven to
  the host with the same exchange that checks a pairing PIN. Guests are let
  in for the visit, limited to what the invite allows, and never become
  trusted devices. Withdrawing the invite disconnects them.
- Passwords are hashed with Argon2id; sign-in tokens are stored only as
  hashes; sign-ins, new accounts and calls are rate-limited.
- **Open ports are flagged.** A host or account server that listens on a
  public address, or hears from one (a forwarded router port), warns at once
  and keeps warning: close the port and use Tailscale.

## Compatibility

| | Supported |
|---|---|
| **Display server** | X11 (any window manager); Wayland via `xdg-desktop-portal` |
| **GPU** | Intel / AMD (VA‑API), Nvidia (NVENC / NVDEC), Vulkan Video, or software — chosen automatically, with a fallback if a decoder fails |
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

| Crate | |
|---|---|
| `crates/proto` | wire formats only: signalling, data-channel messages, the engine API, the account API |
| `crates/net` | device identities and TLS, pairing, the WebRTC session, candidate gathering, tunnels, the account client |
| `crates/media` | what host and client share about GStreamer: codecs' framing, the latency clock, helpers |
| `crates/host` | `linuxplay-host`: capture, encoding, input injection and virtual gamepads, approval, invites |
| `crates/client` | the stream window: decoding, its render thread, input capture |
| `crates/rendezvous` | `linuxplay-rendezvous`: your own account server: accounts, device lists, invites, tunnels |
| `crates/app` | `linuxplay`: the stream window, the engine (`service`) and its API client (`api`) |

`cargo test --workspace` runs the tests, including a host and client talking
over real sockets and the account server end to end.

## Not there yet

- The graphical front end (the engine and its API are ready for it).
- A client-side cursor (the host draws the pointer into the picture).

## License

GPL-3.0-or-later.
