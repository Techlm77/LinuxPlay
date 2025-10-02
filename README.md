# LinuxPlay

Ultra-low-latency desktop streaming over UDP using FFmpeg, with a simple Qt GUI for both **Host** and **Client** and optional **WireGuard** helper for WAN use.

- **Cross-platform:** Windows & Linux host/client.
- **Codecs:** H.264, H.265 (HEVC), AV1 — with NVENC/QSV/AMF/VAAPI/CPU backends.
- **Transport:** MPEG-TS over UDP for video/audio; TCP for handshake; UDP for input control & clipboard; simple TCP for file drop.
- **Multi-monitor:** Stream a single monitor or spawn windows for **all** monitors.
- **Clipboard sync & drag-drop:** Bi-directional clipboard and drag-and-drop file upload to the host.
- **GUI launcher:** `start.py` gives you tabs for Host, Client, and Help.
- **WAN-ready:** Built-in WireGuard helpers (Linux hosts) to easily tunnel traffic for use over the internet.

> **What “WAN-ready” means here**  
> By default, Host and Client talk directly over your network (LAN). For use across the internet (WAN), the GUI can bring up a WireGuard tunnel on Linux hosts and show a QR for peers. There are **no third-party relay servers or accounts** involved — just direct UDP/TCP or your own WG tunnel.

---

## Table of Contents

- [Quick Start](#quick-start)
- [How it Works](#how-it-works)
- [Host](#host)
- [Client](#client)
- [WireGuard (Linux hosts)](#wireguard-linux-hosts)
- [Capture & Encoder Backends](#capture--encoder-backends)
- [Recommended Settings](#recommended-settings)
- [Ports & Firewall](#ports--firewall)
- [Drag & Drop Upload](#drag--drop-upload)
- [Clipboard Sync](#clipboard-sync)
- [Environment Variables](#environment-variables)
- [Troubleshooting](#troubleshooting)
- [Development](#development)

---

## Quick Start

1. **Install FFmpeg** (must include `ffplay` for client audio):  
   - Windows: place `ffmpeg/bin` next to this project (or install FFmpeg and ensure it’s in `PATH`).  
   - Linux: `sudo apt install ffmpeg` (or distro equivalent).

2. **Install Python deps** (Python 3.9+ recommended):
   ```bash
   pip install PyQt5 PyOpenGL PyOpenGL_accelerate av numpy pynput pyperclip
   ```

3. **Run the GUI:**
   ```bash
   python start.py
   ```
   - Use the **Host** tab on the machine you want to capture.
   - Use the **Client** tab on the viewing machine; enter the Host’s LAN IP (or WG tunnel IP).

> You can also run `host.py` / `client.py` directly if you prefer CLI flags.

---

## How it Works

- **Handshake:** Client connects to Host over **TCP 7001** and receives codec & monitor geometry.
- **Video:** Host uses FFmpeg to capture & encode, then sends **MPEG-TS over UDP** to **5000 + monitor_index**.
- **Audio:** (Optional) Host encodes Opus in MPEG-TS over **UDP 6001**; Client plays with `ffplay`.
- **Input control:** Client sends mouse/keyboard to Host over **UDP 7000**.
- **Clipboard:** Bi-directional updates over **UDP 7002**.
- **File upload:** Client drag-drops files; uploads over **TCP 7003** to `~/LinuxPlayDrop` on Host.

All traffic is direct. For WAN you can enable the WireGuard helper on Linux hosts.

---

## Host

Launch via GUI (**recommended**):
- Pick a **Profile** (Lowest Latency / Balanced / High Quality) or tune manually:
  - **Encoder (codec)**: h.264 / h.265 / av1 / none
  - **Encoder Backend**: auto / cpu / nvenc / qsv / amf / vaapi
  - **Framerate, Bitrate, GOP, QP, Tune, Pixel Format**
  - **Audio** on/off
  - **Adaptive Bitrate** (periodically toggles between two bitrates)
  - **Linux capture**: auto / kmsgrab / x11grab
- Click **Start Host**.

CLI (advanced):
```bash
python host.py --gui   --encoder h.264 --hwenc auto --framerate 60 --bitrate 8M   --audio enable --gop 15 --pix_fmt yuv420p --adaptive
# Linux only capture/display flags:
#   --display :0   and env LINUXPLAY_CAPTURE=auto|kmsgrab|x11grab
```

### Capture on Linux
- **kmsgrab** (KMS/DRM) for minimal overhead (no cursor), typically requires:  
  ```bash
  sudo setcap cap_sys_admin+ep $(which ffmpeg)
  ```
- **x11grab** is the fallback when kmsgrab isn’t viable.

### Capture on Windows
- Prefers **ddagrab** (DXGI) if supported, else **gdigrab** (GDI).

---

## Client

GUI:
- Enter **Host IP** (LAN IP or WG tunnel IP, e.g. `10.13.13.1`).
- Choose **Decoder** (h.264/h.265/av1) and **HW accel** (auto/cpu/cuda/qsv/d3d11va/dxva2/vaapi).
- Choose **Monitor** index or `"all"` to open a window per monitor.
- Enable **Audio** if desired.
- Click **Start Client**.

CLI:
```bash
python client.py --host_ip 192.168.1.50   --decoder h.264 --hwaccel auto   --audio enable --monitor 0 --debug
```

---

## WireGuard (Linux hosts)

The GUI can run helper scripts to bring up a WireGuard interface for WAN use and display a **QR** for peers.

- Scripts expected at:
  - `/usr/local/bin/linuxplay-wg-setup-host.sh`
  - `/usr/local/bin/linuxplay-wg-teardown.sh`
- Status artifacts:
  - `/tmp/linuxplay_wg_info.json` (e.g., `{"host_tunnel_ip":"10.13.13.1"}`)
  - `/tmp/linuxplay_wg_peer.png` (QR code)
- Client can connect to the **tunnel IP** instead of LAN IP.

> These scripts are optional and not bundled; adapt them to your environment.

---

## Capture & Encoder Backends

- **Backends:** `auto`, `cpu` (`libx264`/`libx265`/`libaom-av1`), `nvenc`, `qsv`, `amf` (Windows), `vaapi` (Linux).
- The GUI filters backends to what your FFmpeg actually supports.
- On Linux `vaapi` requires `/dev/dri/renderD128` access.
- Auto-selection in `host.py` tries NVENC ? QSV ? AMF/VAAPI ? CPU depending on platform and availability.

---

## Recommended Settings

### “Feels fast” presets
- **Lowest Latency (esports / input-critical):**
  - `h.264`, **60–90 fps** (or **120 fps** if your GPU/decoder is stable)
  - Bitrate: **2–6 Mbit/s** (raise if you see artifacts), `GOP 10–15`
  - Preset: `llhq` (NVENC) or `ultrafast` (CPU), `tune=zerolatency`
  - Consider **Audio: off** if you need to squeeze a few ms.
- **Balanced (general desktop / gaming):**
  - `h.264`, **45–75 fps**, Bitrate **4–10 Mbit/s**, `GOP 15`
  - Audio on, preset `fast`–`medium`.
- **High Quality (media / text):**
  - `h.265` or `av1`, **30–60 fps**, Bitrate **12–20+ Mbit/s**
  - Pixel format `yuv444p` (if your decoder supports it).

### FPS “sweet spot”
- Most setups feel best at **90–120 fps** if your capture+encode+decode chain keeps up.
- If you see drops/tearing/jank, try **60 or 75 fps** before jumping to 144/240 — those higher modes often add encode/decode pressure that **increases** end-to-end latency on mid-range GPUs.

### Tips
- Keep **GOP** short for snappier recovery (`10–15` at 60–90 fps).
- Avoid excessive bitrate on Wi-Fi; it increases queueing delay.
- Hardware decode on the client (“**HW accel: auto**”) usually lowers latency/jitter.

---

## Ports & Firewall

Host listens/binds on these defaults:

| Purpose      | Proto | Port        |
|--------------|-------|-------------|
| Handshake    | TCP   | **7001**    |
| Video (per monitor) | UDP | **5000 + index** |
| Audio        | UDP   | **6001**    |
| Control (mouse/keys) | UDP | **7000** |
| Clipboard    | UDP   | **7002**    |
| File upload  | TCP   | **7003**    |

Open/allow these on Host and ensure the Client can reach them (LAN or via WG).

---

## Drag & Drop Upload

Drop files (or folders) onto the Client window to upload to the Host. Files are saved to:
```
~/LinuxPlayDrop
```

---

## Clipboard Sync

Clipboard is bi-directional. Updates are sent over UDP 7002. (Linux requires `pyperclip`; Windows uses the Qt clipboard.)

---

## Environment Variables

- `LINUXPLAY_MARKER` / `LINUXPLAY_SID` — Tag FFmpeg processes spawned by the app.
- `LINUXPLAY_CAPTURE` (Linux) — `auto` | `kmsgrab` | `x11grab`.
- `LINUXPLAY_KMS_DEVICE` (Linux) — e.g. `/dev/dri/card0`.
- `PULSE_MONITOR` (Linux) — Override PulseAudio monitor source for audio capture.
- `LINUXPLAY_DSHOW_AUDIO` (Win) — Preferred DirectShow audio device name.
- `LINUXPLAY_FORCE_AMF=1` (Win) — Force AMF availability check for AMD.
- `LINUXPLAY_MAX_EMIT_HZ` (legacy decoder thread) — Limit UI frame emits (Linux-only variant).

---

## Troubleshooting

### “Invalid data found when processing input: avcodec_send_packet()”
- Usually means the UDP stream isn’t arriving or is mangled.
  - Verify **Host logs** show FFmpeg started and the correct **client IP**.
  - Check **firewall** on Host and Client.
  - Confirm **ports** and that you’re not behind NAT without WG/VPN.
  - Try **lower bitrate** and **60 fps** first.
  - If using **HW accel** on Client, the app will auto-fallback to software; you can also set **HW accel: cpu** to test.

### Black window or OpenGL errors
- Ensure your GPU/driver supports modern OpenGL (texture upload & PBOs). Update drivers.  
- On VMs, enable 3D acceleration.

### No audio
- Client requires `ffplay` (comes with FFmpeg). Ensure it’s in `PATH`.  
- On Linux Host, verify PulseAudio monitor source exists; set `PULSE_MONITOR` if needed.

### Linux kmsgrab fails / no cursor
- kmsgrab typically needs `cap_sys_admin` on `ffmpeg` and won’t draw the cursor by design. Use `x11grab` if you need the cursor drawn by capture.

### AV1 too slow / choppy
- AV1 encoders are heavier; start with H.264, then try AV1 once everything is smooth.

### Multi-monitor coordinates weird
- The Host sends monitor `WxH+X+Y` geometries. If your layout changes, restart the Host so the Client re-handshakes and parses the new geometry list.

---

## Development

- GUI launcher: `start.py`
- Host core: `host.py`
- Client core: `client.py`

Useful logs:
- Start Host with **Debug** checked (or `--debug`) to see encoder selection, capture mode, and ABR events.
- Client **--debug** logs decoder opts and any fallback from HW ? SW.

Contributions welcome. If you add a new capture/encoder path, include a short note in **Help** tab text in `start.py`.
