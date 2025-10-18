# LinuxPlay

Ultra‑low‑latency desktop streaming over UDP using FFmpeg, with a Qt GUI for both **Host** and **Client**. Includes:
- **Codecs:** H.264 / H.265 (HEVC) / AV1 via NVENC, QSV, AMF, VAAPI, or CPU.
- **Transport:** MPEG‑TS over UDP for video/audio; TCP for handshake; UDP for control & clipboard; TCP for drag‑and‑drop upload.
- **Multi‑monitor:** Stream one or all monitors.
- **Clipboard & drag‑drop:** Bi‑directional clipboard, and client→host file upload.
- **WAN ready (optional):** WireGuard helpers for tunnelling over the internet.
- **Link aware:** Auto **Wi‑Fi / LAN** detection with network‑tuned buffers.
- **Resilience:** 5 s **PING** / 10 s **PONG** heartbeat; host auto‑stops streams if the client drops and returns to *Waiting for connection*.

---

## TL;DR install

### Python deps (pip, one‑liner)

```bash
python3 -m pip install -U pip wheel setuptools && \
python3 -m pip install PyQt5 PyOpenGL PyOpenGL_accelerate av numpy pynput pyperclip
```

> Tip: Use a venv (`python3 -m venv .venv && source .venv/bin/activate` on Linux/macOS; `.\.venv\Scripts\activate` on Windows).

### Ubuntu 24.04 Desktop (apt, one‑liner)

```bash
sudo apt update && sudo apt install -y \
  ffmpeg xdotool xclip pulseaudio-utils libcap2-bin \
  wireguard-tools qrencode python3 python3-venv python3-pip libgl1
```

- `ffmpeg` — capture/encode/ffplay (client audio)
- `xdotool` — inject mouse/keys on Linux hosts
- `xclip` — required by `pyperclip` for host clipboard
- `pulseaudio-utils` — provides `pactl` for audio monitor detection (works with PipeWire’s Pulse shim)
- `libcap2-bin` — gives `setcap` for **kmsgrab** (see below)
- `wireguard-tools`, `qrencode` — **optional**, only for the WG helper
- `libgl1` — OpenGL runtime for the client’s GL renderer

> If `pip install av` fails to find wheels on your distro mirror, install FFmpeg dev headers and build tools:  
> `sudo apt install -y pkg-config python3-dev libavdevice-dev libavfilter-dev libavformat-dev libavcodec-dev libswscale-dev libswresample-dev libavutil-dev`

### Windows

- Install [FFmpeg](https://ffmpeg.org/) and ensure `ffmpeg`/`ffplay` are on `PATH` **or** drop an `ffmpeg/bin` folder next to these scripts.
- Python 3.9+; run the same pip one‑liner as above in an **elevated** terminal if needed.

---

## Quick start

```bash
python3 start.py
```

- Use the **Host** tab on the capture machine, pick a preset (Lowest Latency / Balanced / High Quality), then **Start Host**.
- Use the **Client** tab on the viewing machine; enter the Host’s **LAN IP** (or your **WireGuard** tunnel IP).

CLI examples:

```bash
# Host (GUI)
python host.py --gui --encoder h.264 --hwenc auto --framerate 60 --bitrate 8M --audio enable --gop 15 --pix_fmt yuv420p

# Client (auto‑detect link type; see Wi‑Fi/LAN below)
python client.py --host_ip 192.168.1.20 --decoder h.264 --hwaccel auto --audio enable --monitor 0 --debug
```

---

## Wi‑Fi vs LAN (what changed)

- **Auto detect:** Client figures out if the route to the host is Wi‑Fi or Ethernet (Linux: `ip route get`; Windows: `Get‑NetRoute`/`Get‑NetAdapter`).  
- **Announce:** Client tells the host via control channel: `NET WIFI` or `NET LAN`.
- **Retune:** Host restarts sender pipelines with link‑appropriate **UDP buffer sizes** and **max_delay**.
- **Manual override:** `client.py --net wifi` or `--net lan` (default `--net auto`).

**Why:** Wi‑Fi needs bigger jitter buffers to prevent corrupt frames/dropouts; LAN can run tiny buffers for minimal latency.

---

## Heartbeat (disconnect detection)

- Host sends **`PING`** to the client on UDP **7004** every 5 s.  
- If no **`PONG`** is received within **10 s**, the host **stops streams** and switches to *Waiting for connection…*.  
- Reconnecting the client triggers an automatic restart of video/audio.

---

## How it works (pipeline)

- **Handshake:** TCP **7001** → codec & monitor geometry.
- **Video:** FFmpeg capture → encode → **UDP** MPEG‑TS to **5000 + monitor_index**.
- **Audio:** (optional) FFmpeg Opus → **UDP** **6001**; client plays via `ffplay`.
- **Input:** Client → Host mouse/keys over **UDP 7000** (Linux uses `xdotool`, Windows uses `pynput`).
- **Clipboard:** Bi‑directional text over **UDP 7002** (`pyperclip` on host sets system clipboard).
- **Drag & Drop:** Client uploads to Host via **TCP 7003** into `~/LinuxPlayDrop`.
- **Link announce:** Client → Host **UDP 7000**: `NET WIFI|LAN`.
- **Heartbeat:** **UDP 7004** (`PING`/`PONG`).

---

## Ports (open these on the **Host**)

| Purpose                  | Proto | Port / Notes                          |
|-------------------------|-------|---------------------------------------|
| Handshake               | TCP   | **7001**                              |
| Video (per monitor)     | UDP   | **5000 + index**                      |
| Audio                   | UDP   | **6001**                              |
| Control (mouse/keys)    | UDP   | **7000**                              |
| Clipboard               | UDP   | **7002**                              |
| File upload             | TCP   | **7003**                              |
| Heartbeat (ping/pong)   | UDP   | **7004**                              |

For WAN use, tunnel over WireGuard and point the client at the **tunnel IP**.

---

## Linux capture specifics

- **kmsgrab** (lowest overhead, no on‑screen cursor): you likely need to grant FFmpeg `cap_sys_admin`:
  ```bash
  sudo setcap cap_sys_admin+ep "$(command -v ffmpeg)"
  ```
- **x11grab** is the fallback if kmsgrab isn’t viable or you need cursor capture.
- VAAPI hardware encode requires access to `/dev/dri/renderD128` (add user to `video` group if needed).

---

## Recommended settings

- **Lowest Latency (esports):** H.264 @ **60–90 fps**, GOP **10–15**, `tune=zerolatency`, **audio off** if you need every last ms.
- **Balanced:** H.264 @ **45–75 fps**, 4–10 Mbit/s, GOP 15, audio on.
- **High Quality:** H.265 / AV1 @ **30–60 fps**, 12–20+ Mbit/s, `yuv444p` if your decoder supports it.

> If Wi‑Fi is still spiky, try **lower FPS** and **slightly lower bitrate** before increasing buffers further.

---

## Troubleshooting

- **Client window is black / “Invalid data found when processing input”:**  
  The UDP stream isn’t arriving or is mangled. Check firewall/ports, confirm host found your client IP, and reduce bitrate/FPS. On the client, try `--hwaccel cpu` to test SW decode.

- **No audio:**  
  Ensure `ffplay` is installed and on `PATH`. On Linux hosts, verify a Pulse monitor exists; set `PULSE_MONITOR` to override.

- **kmsgrab errors:**  
  Apply `setcap` to `ffmpeg` (see above) or switch to `x11grab`.

- **Input doesn’t work on Linux:**  
  Install `xdotool`. Some Wayland sessions may need Xwayland or different permissions.

---

## Development

- `start.py` — GUI launcher (Host/Client/Help)
- `host.py` — Host core (capture/encode, control, clipboard, heartbeat, file server)
- `client.py` — Client core (decoder, GL renderer, control, clipboard, DnD, Wi‑Fi/LAN detection)

Run with `--debug` to see pipeline selection and link/heartbeat events.
