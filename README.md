# LinuxPlay

Ultra‑low‑latency desktop streaming over UDP using FFmpeg, with a Qt GUI for **Host** and **Client**.

## Features

- **Codecs:** H.264 / H.265 (HEVC) via NVENC, QSV, VAAPI, AMF, or CPU.
- **Transport:** Video over **MPEG‑TS/UDP**, audio over UDP, control/clipboard over UDP, handshake & file upload over TCP.
- **Multi‑monitor:** Stream one or multiple monitors.
- **Clipboard & Drag‑and‑Drop:** Bi‑directional clipboard; client→host file upload (TCP).
- **WAN‑friendly:** Optional WireGuard helpers for tunnelling over the internet.
- **Link‑aware:** Client auto‑detects **Wi‑Fi vs LAN** and the host adapts buffers accordingly.
- **Resilience:** Heartbeat (PING/PONG); host auto‑stops and returns to *Waiting for connection* if the client drops.
- **Stats Overlay (Client):** FPS/CPU/RAM overlay via OpenGL (triple‑buffered PBO uploads).
## How it works

```
Client                        Network          Host
------                        -------          ----
TCP handshake (7001)   <-------------------->  Handshake
UDP control (7000)      -------------------->  xdotool/pynput
UDP clipboard (7002)   <-------------------->  pyperclip
UDP heartbeat (7004)   <-------------------->  PING/PONG
UDP video (5000+idx)   <--------------------   FFmpeg capture+encode
UDP audio (6001)       <--------------------   FFmpeg Opus (optional)
TCP upload (7003)      --------------------->  ~/LinuxPlayDrop
```
## Installation

### Python packages
```bash
python3 -m pip install -U pip wheel setuptools
python3 -m pip install PyQt5 PyOpenGL PyOpenGL_accelerate av numpy pynput pyperclip psutil
```

### Ubuntu 24.04 packages
```bash
sudo apt update && sudo apt install -y   ffmpeg xdotool xclip pulseaudio-utils libcap2-bin   wireguard-tools qrencode python3 python3-venv python3-pip libgl1
```

> If `pip install av` fails, install FFmpeg dev headers:
> `sudo apt install -y pkg-config python3-dev libavdevice-dev libavfilter-dev libavformat-dev libavcodec-dev libswscale-dev libswresample-dev libavutil-dev`

### Windows
- Install **FFmpeg** and ensure `ffmpeg`/`ffplay` are on `PATH`, or place `ffmpeg/bin` alongside the scripts.
- Install Python 3.9+ and the same pip packages as above.
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
python3 client.py --host_ip 192.168.1.20 --decoder h.264 --hwaccel auto --audio enable --monitor 0 --debug
```
## Wi‑Fi vs LAN

- Client auto‑detects the route to host (Wi‑Fi or Ethernet) and announces `NET WIFI`/`NET LAN`.
- Host retunes sender buffers accordingly (larger jitter buffers for Wi‑Fi, minimal for LAN).
- Manual override: `client.py --net wifi|lan` (default `auto`).

**Tip:** On spiky Wi‑Fi, try slightly **lower FPS** and **lower bitrate** before increasing buffers.
## Heartbeat and reconnects

- Host sends **PING** on UDP 7004 every 5 s; expects **PONG** within 10 s.
- On timeout, host **stops streams** and waits for reconnection.
- Reconnecting the client restarts video/audio automatically.
## Ports (host)

| Purpose                | Proto | Port |
|------------------------|-------|------|
| Handshake              | TCP   | 7001 |
| Video (per monitor)    | UDP   | 5000 + index |
| Audio                  | UDP   | 6001 |
| Control (mouse/keys)   | UDP   | 7000 |
| Clipboard              | UDP   | 7002 |
| File upload            | TCP   | 7003 |
| Heartbeat (ping/pong)  | UDP   | 7004 |
## Linux capture notes

- **kmsgrab** (lowest overhead, no cursor). Grant capability:
  ```bash
  sudo setcap cap_sys_admin+ep "$(command -v ffmpeg)"
  ```
- **x11grab** is the fallback if kmsgrab isn't viable or you require cursor capture.
- **VAAPI** encode requires access to `/dev/dri/renderD128` (add your user to the `video` group).
## Recommended presets

- **Lowest Latency:** H.264 @ 60–90 fps, GOP 10–15, `tune=zerolatency`, audio off if every ms matters.
- **Balanced:** H.264 @ 45–75 fps, 4–10 Mbit/s, GOP 15, audio on.
- **High Quality:** H.265 @ 30–60 fps, 12–20+ Mbit/s, `yuv444p` if your decoder supports it.
## Command‑line reference

### `client.py` options
| Flag(s) | Default | Description |
| --- | --- | --- |
| `--decoder` | `none` |  _(choices: none, h.264, h.265)_ |
| `--host_ip` | — |  |
| `--audio` | `disable` |  _(choices: enable, disable)_ |
| `--monitor` | `0` | Index or 'all' |
| `--hwaccel` | `auto` |  _(choices: auto, cpu, cuda, qsv, d3d11va, dxva2, vaapi)_ |
| `--debug` | — |  _(action: store_true)_ |
| `--net` | `auto` |  _(choices: auto, lan, wifi)_ |
| `--ultra` | — |  _(action: store_true)_ |

---

### `host.py` options
| Flag(s) | Default | Description |
| --- | --- | --- |
| `--gui` | — | Show host GUI window. _(action: store_true)_ |
| `--encoder` | `none` |  _(choices: none, h.264, h.265)_ |
| `--hwenc` | `auto` |  _(choices: auto, cpu, nvenc, qsv, vaapi)_ |
| `--framerate` | `DEFAULT_FPS` |  |
| `--bitrate` | `LEGACY_BITRATE` |  |
| `--audio` | `disable` |  _(choices: enable, disable)_ |
| `--adaptive` | — |  _(action: store_true)_ |
| `--display` | `:0` |  |
| `--preset` | — |  |
| `--gop` | `30` |  |
| `--qp` | — |  |
| `--tune` | — |  |
| `--pix_fmt` | `yuv420p` |  |
| `--debug` | — |  _(action: store_true)_ |

---

### `start.py` options
| Flag(s) | Default | Description |
| --- | --- | --- |
| `--debug` | — |  _(action: store_true)_ |
## Troubleshooting

- **Black client window / “Invalid data found when processing input”:**
  - The UDP stream isn’t arriving or is mangled. Check firewall/ports, confirm the host resolved your client IP, and try lowering bitrate/FPS.
  - On the client, try `--hwaccel cpu` to force software decode.

- **No audio:**
  - Ensure `ffplay` is installed and on PATH.
  - On Linux hosts, verify a Pulse monitor exists; override with `PULSE_MONITOR=<sink>.monitor`.

- **kmsgrab errors:**
  - Apply `setcap` to `ffmpeg` (see above) or switch to `x11grab`.

- **Input on Linux not working:**
  - Install `xdotool`. Some Wayland sessions may require Xwayland or different permissions.
## Security & WAN

- For WAN use, tunnel over **WireGuard** and point the client to the tunnel IP.
- Helper scripts (if installed) can set up the host and generate a peer QR.
## License

- **LinuxPlay** is licensed under **GNU GPL v2.0 only**. See [LICENSE](./LICENSE).
- External tools (FFmpeg, xdotool, xclip, ffplay, etc.) are executed as **separate processes** and retain their own licenses.
