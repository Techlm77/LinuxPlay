# LinuxPlay

> The open‑source, ultra‑low‑latency remote desktop and game streaming stack for Linux; built with FFmpeg, UDP, and Qt.

![License: GPLv2](https://img.shields.io/badge/License-GPLv2-blue.svg)
![Platform: Linux](https://img.shields.io/badge/Platform-Linux-green.svg)
![Python 3.9+](https://img.shields.io/badge/Python-3.9%2B-blue)
![FFmpeg](https://img.shields.io/badge/FFmpeg-Required-critical)

---

Ultra‑low‑latency desktop streaming over UDP using FFmpeg, with a Qt GUI for **Host** and **Client**.

## Features

- **Codecs:** H.264 / H.265 (HEVC) via NVENC, QSV, VAAPI, AMF, or CPU.
- **Transport:** Video over MPEG‑TS/UDP, audio over UDP, control (mouse, keyboard, gamepad), and clipboard over UDP; handshake & file upload over TCP.
- **Controller Support:** Forwards gamepad input from client→host using UDP and a `uinput` virtual device on the host; compatible with Xbox, DualSense, 8BitDo, and others.
- **Multi‑monitor:** Stream one or multiple monitors.
- **Clipboard & Drag‑and‑Drop:** Bi‑directional clipboard; client→host file upload (TCP).
- **Link‑aware:** Client auto‑detects **Wi‑Fi vs LAN** and the host adapts buffers accordingly.
- **Resilience:** Heartbeat (PING/PONG); host auto‑stops and returns to *Waiting for connection* if the client drops.
- **Stats Overlay (Client):** FPS / CPU / RAM overlay via OpenGL (triple‑buffered PBO uploads).

## Why LinuxPlay?

LinuxPlay exists for people who like their tools fast, open, and understandable.  
No accounts, no mystery daemons, no “trust us” black boxes, just a lean pipeline built on FFmpeg and UDP that runs entirely on your machines.

It’s tuned for low latency and high control: you choose the codec, the bitrate, the buffers, and how aggressive you want to be.  
Stream your desktop, your game, or your workflow with knobs that actually do something, because you can see (and tweak) every step.

Ideal for developers, tinkerers, and power users who enjoy shaping their stack rather than being shaped by it.  
Light warning with a smile: LinuxPlay gives you real horsepower. If you floor it and the network skids... that’s on purpose.

## How it works

```
Client                        Network          Host
------                        -------          ----
TCP handshake (7001)   <-------------------->  Handshake
UDP control (7000)      -------------------->  xdotool/pynput
UDP clipboard (7002)   <-------------------->  pyperclip
UDP heartbeat (7004)   <-------------------->  PING/PONG
UDP gamepad (7005)      -------------------->  uinput virtual controller
UDP video (5000+idx)   <--------------------   FFmpeg capture+encode
UDP audio (6001)       <--------------------   FFmpeg Opus (optional)
TCP upload (7003)      --------------------->  ~/LinuxPlayDrop
```

## Installation

### Ubuntu 24.04 packages
```bash
sudo apt update && sudo apt install -y \
  ffmpeg xdotool xclip pulseaudio-utils libcap2-bin wireguard-tools qrencode \
  python3 python3-venv python3-pip libgl1 python3-evdev
```
> If `pip install av` fails, install FFmpeg dev headers:  
> `sudo apt install -y pkg-config python3-dev libavdevice-dev libavfilter-dev libavformat-dev libavcodec-dev libswscale-dev libswresample-dev libavutil-dev`

# Create and activate a virtual environment
```bash
python3 -m venv .venv
source .venv/bin/activate   # (on Linux/macOS)
# .venv\Scripts\activate    # (on Windows PowerShell)
```

### Python packages
```bash
python3 -m pip install -U pip wheel setuptools
python3 -m pip install PyQt5 PyOpenGL PyOpenGL_accelerate av numpy pynput pyperclip psutil evdev
```
> `evdev` is required on **Linux clients** for controller capture. Hosting already requires Linux; controller forwarding currently supports Linux→Linux.

### Windows
- Install **FFmpeg** and ensure `ffmpeg`/`ffplay` are on `PATH`, or place `ffmpeg/bin` alongside the scripts.
- Install Python 3.9+ and the same pip packages as above.  
  *(Gamepad forwarding from Windows clients is not available yet.)*

## Controller Support

LinuxPlay can forward a physical controller connected to the **client** to a virtual controller on the **host**.

**Requirements**
- Client: Linux, a gamepad exposed via `/dev/input/event*` (e.g. Xbox, DualSense, 8BitDo), and `python-evdev`.
- Host: Linux with `/dev/uinput` available (most distros provide this).

**Quick start**
```bash
# Client side (example)
python3 client.py --host_ip 192.168.1.20 --decoder h.264 --hwaccel auto --gamepad enable

# Optionally pick a specific device (e.g. /dev/input/event12)
python3 client.py --host_ip 192.168.1.20 --gamepad enable --gamepad_dev /dev/input/event12
```

**Troubleshooting**
- **No controller on host:** Check that `/dev/uinput` exists and you have permission (often via the `input` group).  
  Try: `sudo modprobe uinput` and re‑run.
- **Client can’t read device:** Ensure your user can read `/dev/input/event*` for your pad (udev rules or group membership).
- **Lag spikes:** Prefer wired LAN; Wi‑Fi adds jitter. Controller packets are tiny but still subject to queueing.
- **Wayland sessions:** Input injection for mouse/keyboard uses xdotool/pynput; some Wayland compositors restrict this.

## Usage

### GUI launcher (recommended)
```bash
python3 start.py
```
- On **Host** tab: pick a preset (Lowest Latency / Balanced / High Quality), then **Start Host**.
- On **Client** tab: enter the host's LAN IP (or WireGuard tunnel IP).

### CLI (advanced)

**Host**
```bash
python3 host.py --gui --encoder h.264 --hwenc auto --framerate 60 --bitrate 8M --audio enable --gop 15 --pix_fmt yuv420p
```

**Client**
```bash
python3 client.py --host_ip 192.168.1.20 --decoder h.264 --hwaccel auto --audio enable --monitor 0 --gamepad enable --debug
```

## Network Modes (Wi‑Fi vs LAN)
- Client auto‑detects the route to host (Wi‑Fi or Ethernet) and announces `NET WIFI` / `NET LAN`.
- Host retunes sender buffers accordingly (larger jitter buffers for Wi‑Fi, minimal for LAN).
- Manual override: `client.py --net wifi|lan` (default `auto`).

**Tip:** On spiky Wi‑Fi, try slightly **lower FPS** and **lower bitrate** before increasing buffers.

## Heartbeat and reconnects
- Host sends **PING** on UDP 7004 every 1 s; expects **PONG** within 10 s.
- On timeout, host **stops streams** and waits for reconnection.
- Reconnecting the client restarts video/audio automatically.

## Ports (host)
| Purpose                    | Proto | Port           |
|---------------------------|-------|----------------|
| Handshake                 | TCP   | 7001           |
| Video (per monitor)       | UDP   | 5000 + index   |
| Audio                     | UDP   | 6001           |
| Control (mouse/keyboard)  | UDP   | 7000           |
| Clipboard                 | UDP   | 7002           |
| File upload               | TCP   | 7003           |
| Heartbeat (ping/pong)     | UDP   | 7004           |
| Gamepad (controller)      | UDP   | 7005           |

## Linux capture notes
- **kmsgrab** (lowest overhead, no cursor). Grant capability:
  ```bash
  sudo setcap cap_sys_admin+ep "$(command -v ffmpeg)"
  ```
- **x11grab** is the fallback if kmsgrab isn't viable or you require cursor capture.
- **VAAPI** encode requires access to `/dev/dri/renderD128` (add your user to the `video` group).

## Recommended presets
- **Lowest Latency:** H.264 @ 60–120 fps, GOP 8–15, low‑latency tune, audio off if every ms matters.
- **Balanced:** H.264 @ 45–75 fps, 4–10 Mbit/s, GOP 15, audio on.
- **High Quality:** H.265 @ 30–60 fps, 12–20+ Mbit/s, `yuv444p` if your decoder supports it.

## Security & WAN
- For WAN use, tunnel over **WireGuard** and point the client to the tunnel IP.

## License
- **LinuxPlay** is licensed under **GNU GPL v2.0 only**. See [LICENSE](./LICENSE).
- External tools (FFmpeg, xdotool, xclip, ffplay, etc.) are executed as **separate processes** and retain their own licenses.

---

*Developed and maintained by [Techlm77](https://github.com/Techlm77) — proudly built for Linux enthusiasts and creators.*
