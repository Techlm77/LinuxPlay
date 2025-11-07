# LinuxPlay

> An experimental, ultra low latency remote desktop and game streaming stack for Linux.
> Built with FFmpeg, UDP, Qt, and zero corporate junk.

[![License: GPLv2](https://img.shields.io/badge/License-GPLv2-blue.svg)](LICENSE)
![Platform: Linux](https://img.shields.io/badge/Platform-Linux-green.svg)
![Python 3.9+](https://img.shields.io/badge/Python-3.9%2B-blue)
![FFmpeg](https://img.shields.io/badge/FFmpeg-Required-critical)
[![GitHub stars](https://img.shields.io/github/stars/Techlm77/LinuxPlay?style=flat)](https://github.com/Techlm77/LinuxPlay/stargazers)

---

## Project Status: Experimental & Power User Focused

LinuxPlay is not a polished commercial product.
It’s an **experimental, community-driven toy for power users** who:

- are comfortable with FFmpeg, networking, and Linux internals,
- want **full control** over codec, bitrate, capture, buffers and behavior,
- are happy to run it on **trusted LANs or inside a secure VPN/WireGuard tunnel**,
- understand that **traffic is not end-to-end encrypted by LinuxPlay itself**.

If you expose these ports raw to the internet or run it on an untrusted network without a VPN:
**you are doing something this project does not recommend.**

For fast, safe usage:

- Use it on a wired home LAN **or**
- Run it strictly through **WireGuard/OpenVPN/SSH tunnels** and point the client at the tunnel IP.

---

## A Message from the Developer

Hey everyone,

LinuxPlay has been a **one-person journey for over a year**. Every feature, every bug fix, every idea has been hand-built in my spare time. There’s no team, no funding, no auto-generated codebase behind it, just me learning, experimenting, and building because I love Linux and wanted to see how far open tools like FFmpeg, UDP, and Qt could go.

What started as a fun experiment has grown into something people actually use and enjoy. That genuinely means a lot. ❤️

I know not everything is perfect yet, and that’s fine. That’s what open source is about:
**progress, not perfection.** If you’ve got ideas, improvements, or features you’d love to see, please share them in
[GitHub Discussions](https://github.com/Techlm77/LinuxPlay/discussions). Constructive feedback and collaboration are what keep LinuxPlay moving in the right direction.

## Community Shoutout

If you make a YouTube video, blog post, benchmark, setup tour, or wild experiment with LinuxPlay, use the hashtag **#LinuxPlay**.

I’d love to see what you’re doing with it and highlight cool community setups in future updates. That kind of stuff keeps the motivation alive more than anything else.

For now, I’m taking a short break to recharge, but **LinuxPlay isn’t going anywhere.** It’ll keep improving, step by step, just like it has since the first line of code.

Thanks for believing in something made by one person, from scratch, with actual curiosity and passion.

---

## Features

- **Codecs**
  - H.264 and H.265 (HEVC)
  - Hardware acceleration via **NVENC, QSV, VAAPI, AMF**, or CPU fallback.
- **Transport**
  - Ultra low latency design.
  - Video over MPEG-TS on UDP.
  - Audio over UDP.
  - Input (mouse/keyboard/gamepad) over UDP.
  - Clipboard sync over UDP.
  - Handshake and file upload over TCP.
- **Audio Features**
  - Surround Sound Support (5.1 / 7.1): Host detects and captures up to 8 audio channels.
  - Client performs intelligent downmixing (FFplay filters) to stereo for local speakers when necessary.
- **Granular Encoder Control**
  - Direct control over FFmpeg parameters: **GOP, QP/CRF, Preset, Tune, and Pixel Format (`yuv420p`, `yuv444p`)** are fully exposed via command line arguments.
- **Advanced Capture Methods**
  - Host supports both **kmsgrab** (lowest latency/overhead, requires setcap) and **x11grab** (general fallback, supports cursor capture).
- **Secure Handshake Layer**
  - Rotating 6-digit PIN, refreshes every 30 seconds.
  - PIN rotation pauses while a session is active.
  - Single-session lock: new clients get `BUSY` while a session is live.
  - Certificate-based login after the first trusted PIN session.
- **PIN → Certificate Upgrade**
  - On first successful PIN auth, the host:
    - Acts as a mini CA.
    - Issues a per-device client certificate + key.
    - Exports: `client_cert.pem`, `client_key.pem`, `host_ca.pem` under `issued_clients/...`.
  - Copy these to the client folder next to `start.py` and `client.py` (via USB, SCP, etc.).
  - The client:
    - Detects the bundle.
    - Skips PIN.
    - Uses certificate-based authentication automatically.
- **Controller Support**
  - Full gamepad forwarding over UDP using a virtual uinput device.
  - Works with Xbox, DualSense, 8BitDo and other HID controllers (Linux client → Linux host).
- **Multi-Monitor**
  - Stream one or more displays.
  - Resolutions and offsets auto-detected per monitor.
- **Clipboard & File Transfer**
  - Bi-directional clipboard sync.
  - Client → host file uploads over TCP with validation and safe paths.
- **Link-Aware Streaming**
  - Adapts buffer strategies for LAN vs Wi-Fi to reduce stalls and jitter.
- **Resilience**
  - Heartbeat (PING/PONG).
  - Host stops streams and returns to waiting state on timeout/disconnect.
- **Stats Overlay (Client)**
  - Real-time **FPS, CPU, RAM, GPU** metrics via OpenGL with triple-buffered PBO uploads.
- **Cross-Platform**
  - Host: Linux.
  - Clients: Linux and Windows.

---

## Why LinuxPlay?

Because sometimes you want:

- **No accounts.**
- **No vendor lock-in.**
- **No mystery processes.**
- A streaming stack you can **read**, **modify**, and **tune**.

LinuxPlay is intentionally **hands-on**:

- You choose the encoder, bitrate, GOP, pix_fmt, buffer sizes.
- You can see every FFmpeg command.
- You can measure every change.

If that sounds fun instead of scary, you’re the target audience.

---

## Architecture

```
Client                        Network           Host
------                        -------           ----
TCP handshake (7001)   <-------------------->  Handshake
UDP control (7000)      -------------------->  Input (mouse/keyboard)
UDP clipboard (7002)   <-------------------->  Clipboard sync
UDP heartbeat (7004)   <-------------------->  Keepalive (PING/PONG)
UDP gamepad (7005)      -------------------->  Virtual gamepad (uinput)
UDP video (5000+idx)   <--------------------   FFmpeg capture + encode
UDP audio (6001)       <--------------------   FFmpeg Opus audio
TCP upload (7003)      --------------------->  File upload handler
```

---

## Installation

### Option 1: Using run.sh (recommended)

```bash
chmod +x run.sh

# Check & install required tools and Python deps (on supported distros)
./run.sh check

# Launch the GUI
./run.sh start

# Or directly:
./run.sh host --gui --encoder h.264 --hwenc auto --bitrate 8M --audio enable
./run.sh client --host_ip 192.168.1.20 --decoder h.264 --hwaccel auto
```

- Uses a local `.venv` inside the repo.
- Installs only missing dependencies inside that venv.
- Never installs Python packages globally.

---

### Option 2: Manual setup (Ubuntu 24.04 example)

#### System packages

```bash
sudo apt update
sudo apt install -y ffmpeg xdotool xclip pulseaudio-utils libcap2-bin wireguard-tools qrencode python3 python3-venv python3-pip libgl1 python3-evdev
```

If `pip install av` or `pip install cryptography` fails, install FFmpeg/Python dev headers:

```bash
sudo apt install -y pkg-config python3-dev libavdevice-dev libavfilter-dev libavformat-dev libavcodec-dev libswscale-dev libswresample-dev libavutil-dev
```

#### Create and activate a virtual environment

```bash
python3 -m venv .venv
source .venv/bin/activate
```

#### Python packages (inside `.venv`)

```bash
python3 -m pip install -U pip wheel setuptools
python3 -m pip install PyQt5 PyOpenGL PyOpenGL_accelerate av numpy pynput pyperclip psutil evdev cryptography
```

`evdev` is required on Linux clients for controller capture.

---

## Usage

### GUI launcher

```bash
python3 start.py
```

- **Host** tab: pick a preset → **Start Host**.
- **Client** tab: enter host LAN IP or WireGuard tunnel IP → **Start Client**.
- If `client_cert.pem`, `client_key.pem`, `host_ca.pem` are present:
  - PIN field is disabled.
  - Client uses certificate auth automatically.

### Command line

```bash
# Host
python3 host.py --gui --encoder h.264 --hwenc auto --framerate 60 --bitrate 8M --audio enable --gop 15 --pix_fmt yuv420p

# Client
python3 client.py --host_ip 192.168.1.20 --decoder h.264 --hwaccel auto --audio enable --monitor 0 --gamepad enable --debug
```

---

## Network Modes

- Client auto-detects Wi-Fi vs Ethernet and sends `NET WIFI` / `NET LAN`.
- Host adjusts buffers accordingly.
- Manual override:
  - `client.py --net wifi`
  - `client.py --net lan`
- Default: `auto`.

---

## Heartbeat & Reconnects

- Host sends `PING` every second.
- Expects `PONG` within 10 seconds.
- On timeout or client exit:
  - Stops streams.
  - Clears session state.
  - Returns to “Waiting for connection…”.
- Reconnects start video/audio again without manual restart.

---

## Ports on Host

| Purpose                 | Protocol | Port            |
|-------------------------|----------|-----------------|
| Handshake               | TCP      | 7001            |
| Video per monitor       | UDP      | 5000 + index    |
| Audio                   | UDP      | 6001            |
| Control (mouse/keyboard)| UDP      | 7000            |
| Clipboard               | UDP      | 7002            |
| File upload             | TCP      | 7003            |
| Heartbeat (PING/PONG)   | UDP      | 7004            |
| Gamepad controller      | UDP      | 7005            |

---

## Linux Capture Notes

- **kmsgrab** (lowest overhead, no cursor):
  ```bash
  sudo setcap cap_sys_admin+ep "$(command -v ffmpeg)"
  ```
- **x11grab** fallback when kmsgrab is not available or cursor capture needed.
- **VAAPI**:
  - Needs `/dev/dri/renderD128`.
  - Add your user to `video` group if needed.

---

## Recommended Presets

- **Lowest Latency**
  - H.264, 60–120 fps, GOP 8–15, low-latency tune.
- **Balanced**
  - H.264, 45–75 fps, 4–10 Mbit/s, GOP 15.
- **High Quality**
  - H.265, 30–60 fps, 12–20 Mbit/s, `yuv444p` if supported.

---

## Security (Please Read!!)

- **LinuxPlay does not encrypt media/control traffic itself.**
- For **WAN / public / untrusted Wi-Fi**:
  - **Always run through WireGuard/OpenVPN/SSH tunnels.**
  - Point the client to the VPN/tunnel IP (e.g. `10.x.x.x`).
- For **trusted wired LAN at home**:
  - May be acceptable without VPN **if you fully trust all devices**.
- Authentication:
  - One active client at a time (`BUSY` for others).
  - First login via rotating PIN.
  - Subsequent logins via per-device certificate.
  - Private keys stay on the client; host tracks fingerprints.
- To revoke:
  - Edit or remove entries in `trusted_clients.json` on the host.

---

## Support LinuxPlay

LinuxPlay is:

- fully open-source,
- built from scratch,
- maintained in spare time by a solo developer.

If you like it and want to see it grow:

[Sponsor @Techlm77](https://github.com/sponsors/Techlm77)

Support helps cover hardware, testing, and makes it easier for others to join in and help turn this into a stronger ecosystem project.

---

## License

LinuxPlay is licensed under **GNU GPL v2.0 only**. See [`LICENSE`](LICENSE).
External tools like FFmpeg, xdotool, xclip, ffplay retain their own licenses.

---
