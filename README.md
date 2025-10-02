# LinuxPlay — Low‑Latency Remote Desktop (Host + Client)

LinuxPlay is a lightweight, **local‑first** remote desktop that prioritizes latency.  
It uses FFmpeg for capture/encode on the host and a tiny OpenGL viewer on the client.  
No accounts, no relays — just your LAN.

> ✅ Host: Linux or Windows • ✅ Client: Linux or Windows • ❌ Wayland capture (X11/KMS only)

---

## What’s in this repo

- `host.py` – captures the desktop and streams video (and optional audio) to the client.
- `client.py` – decodes and displays the stream with an OpenGL blitter and sends input back.
- `start.py` – convenience launcher (optional).
- `ffmpeg/` (optional) – if you include a portable FFmpeg here, the apps will auto‑use it.

**Default ports**

| Purpose        | Proto | Port | Notes |
|----------------|:-----:|:----:|------|
| Control        | UDP   | 7000 | Mouse/keyboard events to host |
| Handshake      | TCP   | 7001 | Host reports monitors/encoder |
| Clipboard      | UDP   | 7002 | Bi‑directional text clipboard |
| File upload    | TCP   | 7003 | Client → host dropbox |
| Video          | UDP   | 5000 + N | One port per monitor (N = index) |
| Audio          | UDP   | 6001 | Optional Opus-in-MPEG‑TS |

Open these on your firewall if needed.

---

## Dependencies

### Common (host & client)
- Python 3.9+  
- `pip install av PyQt5 PyOpenGL numpy`

### FFmpeg
- **Required on host, optional on client** (client will use bundled `ffplay` only if you enable audio).
- Linux (Debian/Ubuntu): `sudo apt install ffmpeg`
- Windows: Install FFmpeg and add it to `PATH`, **or** drop a portable build at `./ffmpeg/bin/ffmpeg(.exe)`.

### Linux host utilities
- `x11grab` path: X11 session required (Wayland not supported).
- Optional for input injection/clipboard: `xdotool`, `xclip`
- For KMS capture (no X): FFmpeg built with `kmsgrab` + `/dev/dri/renderD128`.

---

## Quick start

### 1) Start the host (the machine you want to control)

Pick the encoder that matches your hardware. Examples:

**NVIDIA (NVENC, H.264 @ 60 fps):**
```bash
python3 host.py --encoder h.264 --hwenc nvenc --framerate 60 --audio enable --bitrate 8M
```

**Intel (QSV, H.264):**
```bash
python3 host.py --encoder h.264 --hwenc qsv --framerate 60 --audio enable
```

**AMD on Windows (AMF, H.264):**
```bash
python host.py --encoder h.264 --hwenc amf --framerate 60 --audio enable
```

**CPU (libx264, lowest compatibility):**
```bash
python3 host.py --encoder h.264 --hwenc cpu --framerate 60 --audio enable
```

> Linux capture defaults to X11 (`x11grab`). If FFmpeg has `kmsgrab` and VAAPI and your flags allow, the host may auto‑use KMS for very low overhead capture.

#### Host flags

| Flag | Values | Default | What it does |
|------|--------|---------|--------------|
| `--gui` | (flag) | off | Shows a minimal host GUI window |
| `--encoder` | `none`,`h.264`,`h.265`,`av1` | `none` | Video codec (when `none`, nothing is sent) |
| `--hwenc` | `auto`,`cpu`,`nvenc`,`qsv`,`amf`,`vaapi` | `auto` | Hardware encoder backend |
| `--capture` (Windows) | `auto`,`ddagrab`,`gdigrab` | `auto` | Desktop capture method |
| `--framerate` | integer | `30` | Capture FPS |
| `--bitrate` | e.g. `8M` | `8M` | Target video bitrate |
| `--audio` | `enable`,`disable` | `disable` | Stream system audio (Opus) |
| `--adaptive` | (flag) | off | Simple demo adaptive bitrate toggle |
| `--display` (Linux) | X11 display | `:0` | Which X server to capture |
| `--preset` | encoder-specific | `""` | Maps to NVENC/QSV/AMF/VAAPI presets when supported |
| `--gop` | integer | `30` | Keyframe interval |
| `--qp` | integer | `""` | Constant QP (if supported) |
| `--tune` | encoder string | `""` | e.g. NVENC `ll` |
| `--pix_fmt` | e.g. `yuv420p` | `yuv420p` | Pixel format |
| `--debug` | (flag) | off | Verbose logging |

---

### 2) Start the client (the machine you control from)

**Windows or Linux:**
```bash
python3 client.py --host_ip 192.168.1.101 --audio enable --display_mode native
```

#### Client flags

| Flag | Values | Default | What it does |
|------|--------|---------|--------------|
| `--host_ip` | IPv4 | (required) | Host’s LAN IP address |
| `--decoder` | `none`,`h.264`,`h.265`,`av1` | `none` | (Reserved) codec hint; stream format is auto‑detected |
| `--hwaccel` | `auto`,`cpu`,`cuda`,`qsv`,`d3d11va`,`dxva2`,`vaapi` | `auto` | HW decode preference (falls back to CPU) |
| `--monitor` | index or `all` | `0` | Which monitor to view (indices from handshake) |
| `--display_mode` | `native`,`fit` | `native` | `native` = 1:1 pixels + black bars; `fit` = scale letterboxed |
| `--audio` | `enable`,`disable` | `disable` | Play host audio via `ffplay` |
| `--debug` | (flag) | off | Verbose logging |

**Tips**
- If you see HW‑decode errors, try `--hwaccel cpu`.
- For the lowest latency, keep `--display_mode native` (no resizes), and run 60–120 FPS on the host if your GPU/network can handle it.

---

## Multi‑monitor behaviour

- The host advertises monitors as `WxH+X+Y;…` during the TCP handshake.
- The client’s `--monitor` can be `0`, `1`, … or `all`. Each monitor uses `UDP 5000 + index`.

---

## Clipboard, input, and file drop

- Clipboard: both ways on UDP/7002 (text only).
- Input: mouse/keyboard over UDP/7000 with throttled MOUSE_MOVE to reduce packet spam.
- File upload: client → host on TCP/7003 into `~/LinuxPlayDrop`.

---

## Performance notes

- **FPS “sweet spot”**: 60 or 75 is a safe default. If your GPU and LAN are strong, **120 FPS** often improves click‑to‑photon latency. Diminishing returns above ~144.
- **Minimize buffering**: both ends use small FFmpeg buffers and set `-fflags nobuffer -flags low_delay`.
- **Display sync**: the client disables vsync via Qt surface format to keep blits unblocked.

---

## Troubleshooting

- `Invalid data found when processing input: 'avcodec_send_packet()'`  
  Host stream isn’t reaching the client. Check firewall/NAT, confirm host is running, and that UDP 5000/6001 are open. Try `--hwaccel cpu` on the client.

- Black window / GL error `glViewport invalid operation`  
  Ensure GPU drivers and a desktop OpenGL context. On Linux, try `QT_OPENGL=desktop` before launching.

- Stutter at high FPS  
  Lower `--framerate`, reduce `--bitrate`, or try `--preset llhq` (NVENC) / a “balanced” preset (QSV/AMF).

---

## Security

Traffic is **not encrypted**. Use a VPN, SSH tunnel, or restrict to a trusted LAN.  
(Planned: optional DTLS/TLS for control and media.)

---

## License

MIT
