# LinuxPlay

> The open-source, ultra-low-latency remote desktop and game streaming stack for Linux; built with FFmpeg, UDP, and Qt.

![License: GPLv2](https://img.shields.io/badge/License-GPLv2-blue.svg)
![Platform: Linux](https://img.shields.io/badge/Platform-Linux-green.svg)
![Python 3.9+](https://img.shields.io/badge/Python-3.9%2B-blue)
![FFmpeg](https://img.shields.io/badge/FFmpeg-Required-critical)

---

## Features

- **Codecs:** H.264 / H.265 (HEVC) with hardware acceleration via **NVENC**, **QSV**, **VAAPI**, **AMF**, or CPU fallback.
- **Transport:** Ultra-low-latency architecture - video over **MPEG-TS/UDP**, audio over UDP, input (mouse, keyboard, gamepad), and clipboard over UDP; handshake and file upload over TCP.
- **Secure Handshake:**
  - Rotating 6-digit **PIN authentication** (changes every 30 s).
  - PIN rotation automatically **pauses during active sessions**.
  - Rejects new clients while another is connected (**BUSY** protection).
  - **Certificate-based login:** once authenticated via PIN, the host automatically issues your own **client certificate** - allowing future logins without a PIN.
- **Controller Support:** Full **gamepad forwarding** over UDP using a virtual `uinput` device on the host; compatible with Xbox, DualSense, 8BitDo, and other HID controllers.
- **Multi-Monitor:** Stream one or multiple displays, with per-monitor resolution and offset auto-detection.
- **Clipboard & File Transfer:** Bi-directional clipboard sync and client > host file uploads via TCP.
- **Link-Aware Streaming:** Automatically adjusts buffers for **LAN vs Wi-Fi** to minimize jitter and latency spikes.
- **Resilience:** Heartbeat (PING/PONG) system — host auto-stops and returns to *Waiting for connection* if the client disconnects or times out.
- **Stats Overlay (Client):** Real-time **FPS, CPU, RAM, GPU** metrics rendered via OpenGL with triple-buffered PBO uploads.
- **Cross-Platform:** Host on Linux; clients available for Linux and Windows.

---

## One-Time PIN > Permanent Cert Authentication

When you first connect using a **PIN**, the host will issue your own client-side certificate bundle for trusted authentication.  
This eliminates the need to type a PIN again — ideal for personal or multi-device setups.

### Certificate Files Issued
After a successful first connection (via PIN), the host automatically generates:

```
client_cert.pem
client_key.pem
host_ca.pem
```

These files are stored in the host’s `issued_clients/...` directory.

To enable **PIN-free login**:
1. Copy those three files (`client_cert.pem`, `client_key.pem`, `host_ca.pem`) to a USB stick or secure transfer medium.
2. On any trusted client device, place them **in the same folder** as your `start.py` and `client.py` files.
3. LinuxPlay will automatically detect them and skip the PIN entry on startup.

> Tip: The GUI refreshes dynamically, you’ll see “Client certificate detected; PIN not required” once the certs are present.  
> No restart or re-entry needed; just connect instantly.

---

## Why LinuxPlay?

LinuxPlay exists for people who like their tools fast, open, and understandable.  
No accounts, no mystery daemons, no “trust us” black boxes, just a lean pipeline built on FFmpeg and UDP that runs entirely on your machines.

It’s tuned for low latency and high control: you choose the codec, the bitrate, the buffers, and how aggressive you want to be.  
Stream your desktop, your game, or your workflow with knobs that actually do something, because you can see (and tweak) every step.

Ideal for developers, tinkerers, and power users who enjoy shaping their stack rather than being shaped by it.  
Light warning with a smile: LinuxPlay gives you real horsepower. If you floor it and the network skids... that’s on purpose.

---

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

---

## Installation

### Ubuntu 24.04 packages
```bash
sudo apt update && sudo apt install -y   ffmpeg xdotool xclip pulseaudio-utils libcap2-bin wireguard-tools qrencode   python3 python3-venv python3-pip libgl1 python3-evdev
```
> If `pip install av` fails, install FFmpeg dev headers:  
> `sudo apt install -y pkg-config python3-dev libavdevice-dev libavfilter-dev libavformat-dev libavcodec-dev libswscale-dev libswresample-dev libavutil-dev`

### Create and activate a virtual environment
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

---

## Controller Support

LinuxPlay can forward a physical controller connected to the **client** to a virtual controller on the **host**.

**Requirements**
- Client: Linux, a gamepad exposed via `/dev/input/event*` (e.g. Xbox, DualSense, 8BitDo), and `python-evdev`.
- Host: Linux with `/dev/uinput` available (most distros provide this).

**Quick start**
```bash
python3 client.py --host_ip 192.168.1.20 --decoder h.264 --hwaccel auto --gamepad enable
```

---

## Usage

### GUI launcher (recommended)
```bash
python3 start.py
```
- On **Host** tab: pick a preset (Lowest Latency / Balanced / High Quality), then **Start Host**.
- On **Client** tab: enter the host's LAN IP (or WireGuard tunnel IP).

### CLI (advanced)
```bash
# Host
python3 host.py --gui --encoder h.264 --hwenc auto --framerate 60 --bitrate 8M --audio enable --gop 15 --pix_fmt yuv420p

# Client
python3 client.py --host_ip 192.168.1.20 --decoder h.264 --hwaccel auto --audio enable --monitor 0 --gamepad enable --debug
```

---

## Network Modes (Wi-Fi vs LAN)
- Client auto-detects the route to host (Wi-Fi or Ethernet) and announces `NET WIFI` / `NET LAN`.
- Host retunes sender buffers accordingly.
- Manual override: `client.py --net wifi|lan` (default `auto`).

---

## Heartbeat and reconnects
- Host sends **PING** every 1 s; expects **PONG** within 10 s.
- On timeout, host **stops streams** and waits for reconnection.
- Reconnecting the client restarts video/audio automatically.

---

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

---

## Linux capture notes
- **kmsgrab** (lowest overhead, no cursor). Grant capability:
  ```bash
  sudo setcap cap_sys_admin+ep "$(command -v ffmpeg)"
  ```
- **x11grab** is the fallback if kmsgrab isn't viable or you require cursor capture.
- **VAAPI** encode requires access to `/dev/dri/renderD128` (add your user to the `video` group).

---

## Recommended presets
- **Lowest Latency:** H.264 @ 60–120 fps, GOP 8–15, low-latency tune.
- **Balanced:** H.264 @ 45–75 fps, 4–10 Mbit/s, GOP 15.
- **High Quality:** H.265 @ 30–60 fps, 12–20 Mbit/s, `yuv444p` if supported.

---

## Security & WAN
- For WAN use, tunnel over **WireGuard** and point the client to the tunnel IP.

---

## License
- **LinuxPlay** is licensed under **GNU GPL v2.0 only**. See [LICENSE](./LICENSE).
- External tools (FFmpeg, xdotool, xclip, ffplay, etc.) are executed as **separate processes** and retain their own licenses.

---

Developed and maintained by [Techlm77](https://github.com/Techlm77) :)
