# LinuxPlay

> The open source, ultra low latency remote desktop and game streaming stack for Linux. Built with FFmpeg, UDP and Qt.

![License: GPLv2](https://img.shields.io/badge/License-GPLv2-blue.svg)
![Platform: Linux Host](https://img.shields.io/badge/Host-Linux-green.svg)
![Platform: Windows Client](https://img.shields.io/badge/Client-Windows%20%7C%20Linux-blue.svg)
![Python 3.10+](https://img.shields.io/badge/Python-3.10%2B-blue)
![FFmpeg](https://img.shields.io/badge/FFmpeg-Required-critical)

---

## Features

- **Codecs**: H.264 and H.265 (HEVC) with hardware acceleration via NVENC, QSV, VAAPI, AMF, or CPU fallback.
- **Transport**: Ultra low latency design. Video over MPEG-TS on UDP. Audio over UDP. Input for mouse, keyboard and gamepad over UDP. Clipboard on UDP. Handshake and file upload on TCP.
- **Secure Handshake**
  - Rotating 6 digit PIN authentication that refreshes every 30 seconds.
  - PIN rotation pauses while a session is active.
  - Session lock that rejects new clients while another is connected (BUSY protection).
  - Certificate based login after the first trusted PIN session.
- **PIN to Certificate Upgrade**
  - On the first successful PIN login the host issues a per device client certificate and key signed by a local host CA.
  - The bundle contains `client_cert.pem`, `client_key.pem`, `host_ca.pem` (exported under `issued_clients/...`).
  - Copy these three files to the client's `src/linuxplay/` folder (next to `start.py` and `client.py`).
  - The client detects the files and skips the PIN. The GUI disables the PIN field automatically (no restart needed).
- **Controller Support**: Full gamepad forwarding over UDP using a virtual uinput device on the host. Works with Xbox, DualSense, 8BitDo and other HID controllers.
- **Multi Monitor**: Stream one or more displays. Resolution and offsets are detected per display.
- **Clipboard and File Transfer**: Bi directional clipboard sync and client to host file uploads on TCP.
- **Link Aware Streaming**: Buffers adapt for LAN and Wi Fi to reduce jitter and stalls.
- **Resilience**: Heartbeat with ping and pong. The host stops streams and returns to the waiting state on timeout or disconnect.
- **Stats Overlay (Client)**: Real time FPS, CPU, RAM and GPU metrics via OpenGL with triple buffered PBO uploads.
- **Firewall Friendly**: Windows clients no longer require firewall configuration. Uses outbound-only connections with ephemeral ports that work through NAT and firewalls automatically.
- **Cross Platform**: Host runs on Linux. Clients support both Linux and Windows 10/11.

---

## Why LinuxPlay

LinuxPlay is for people who want speed, control and transparency.  
No accounts. No hidden daemons. No black boxes.  
You pick the codec, bitrate, buffers and behavior. Every knob is exposed and does something you can measure.

---

## Architecture

**Network Design:**  
LinuxPlay uses a firewall-friendly architecture where the client initiates all connections using ephemeral ports. This allows Windows clients to work without firewall configuration, as Windows automatically permits responses to outbound connections.

```
Client                        Network           Host
------                        -------           ----
TCP handshake (7001)   <-------------------->  Handshake
UDP control (7000)      -------------------->  Input (mouse/keyboard)
UDP clipboard (7002)   <-------------------->  Clipboard sync (client initiates)
UDP heartbeat (7004)   <-------------------->  Keepalive (client sends PONG first)
UDP gamepad (7005)      -------------------->  Virtual gamepad (uinput)
UDP video (5000+idx)   <--------------------   FFmpeg capture + encode
UDP audio (6001)       <--------------------   FFmpeg Opus audio
TCP upload (7003)      --------------------->  File upload handler
```

**Key Design:**
- Client uses ephemeral (random high) ports for heartbeat and clipboard
- Host learns and responds to client's actual source port
- No client-side firewall rules needed on Windows
- Works through NAT/routers automatically

---

## Installation

### System Requirements

**Host (Linux):**
- Ubuntu 22.04+ (or equivalent)
- Python 3.10 or higher
- FFmpeg with hardware encoding support
- X11 or Wayland display server

**Client (Linux or Windows):**
- Python 3.10 or higher
- FFmpeg for audio playback
- OpenGL 3.0+ support

### Ubuntu 24.04 packages

```bash
sudo apt update
sudo apt install -y ffmpeg xdotool xclip pulseaudio-utils libcap2-bin \
    wireguard-tools qrencode python3 python3-venv python3-pip libgl1 libegl1 \
    python3-evdev libxcb-xinerama0 libxcb-cursor0 libxcb-icccm4 libxcb-image0 \
    libxcb-keysyms1 libxcb-randr0 libxcb-render-util0
```

If `pip install av` fails, install FFmpeg development headers:

```bash
sudo apt install -y pkg-config python3-dev libavdevice-dev libavfilter-dev libavformat-dev libavcodec-dev libswscale-dev libswresample-dev libavutil-dev
```

### Modern Setup with uv (Recommended)

[uv](https://github.com/astral-sh/uv) is a fast Python package manager written in Rust. It's significantly faster than pip and provides better dependency resolution.

#### Install uv

```bash
# Linux/macOS
curl -LsSf https://astral.sh/uv/install.sh | sh

# Or with pip
pip install uv
```

#### Setup project with uv

```bash
# Clone the repository
git clone https://github.com/Techlm77/LinuxPlay.git
cd LinuxPlay

# Create virtual environment and install dependencies (uv does this automatically)
uv venv
uv pip install -e ".[dev]"

# Or use the Makefile
make install-dev
```

### Traditional Setup

```bash
python3 -m venv .venv
source .venv/bin/activate   # Linux or macOS
# .venv\Scripts\activate    # Windows PowerShell

python3 -m pip install -U pip wheel setuptools
python3 -m pip install PyQt5 PyOpenGL PyOpenGL_accelerate av numpy pynput pyperclip psutil evdev
```

`evdev` is required on Linux clients for controller capture.  
Hosting already requires Linux. Controller forwarding currently supports Linux to Linux.

---

## Usage

### Quick Start with Makefile

```bash
# Install dependencies
make install-dev

# Run the GUI launcher
make run-gui

# Or directly
make run-host    # Start host
make run-client  # Show client help
```

### GUI launcher

```bash
python3 src/linuxplay/start.py
# Or with uv
uv run python src/linuxplay/start.py
```
- Host tab. Pick a preset and select Start Host.
- Client tab. Enter the host LAN IP or WireGuard tunnel IP and select Start Client.
- The client GUI detects the certificate bundle if present and disables the PIN field automatically.

### Command line

```bash
# Host (with GUI - requires desktop session)
uv run python src/linuxplay/host.py --gui --encoder h.264 --hwenc auto --framerate 60 --bitrate 8M --audio enable --gop 15 --pix_fmt yuv420p

# Host (headless/SSH - no GUI)
uv run python src/linuxplay/host.py --encoder h.264 --hwenc auto --framerate 60 --bitrate 8M --audio enable --gop 15 --pix_fmt yuv420p

# Client (Linux or Windows)
uv run python src/linuxplay/client.py --host_ip SERVER_IP --decoder h.264 --hwaccel auto --audio enable --monitor 0 --gamepad enable --debug

# Or with traditional python3 (if not using uv)
python3 src/linuxplay/host.py --encoder h.264 --hwenc auto --framerate 60 --bitrate 8M --audio enable
python3 src/linuxplay/client.py --host_ip SERVER_IP --decoder h.264 --hwaccel auto --audio enable
```

---

## Network Modes

- The client auto detects whether the link is Wi Fi or Ethernet and announces NET WIFI or NET LAN.
- The host adjusts buffers for the detected link.
- Manual override is available with `client.py --net wifi` or `client.py --net lan`. Default is auto.

---

## Heartbeat and Reconnects

- The host sends PING every second and expects PONG within ten seconds.
- On timeout or client exit the host stops streams, clears state and returns to Waiting for connection.
- Reconnecting starts video and audio again without manual intervention.

---

## Ports on Host

The host listens on these ports. **The client does not need any firewall rules** as it uses outbound-only connections with ephemeral (randomly assigned) ports.

| Purpose                  | Protocol | Port            | Client Port        |
|--------------------------|----------|-----------------|-------------------|
| Handshake                | TCP      | 7001            | Ephemeral         |
| Video per monitor        | UDP      | 5000 plus index | Ephemeral         |
| Audio                    | UDP      | 6001            | Ephemeral         |
| Control mouse keyboard   | UDP      | 7000            | Ephemeral         |
| Clipboard                | UDP      | 7002            | Ephemeral         |
| File upload              | TCP      | 7003            | Ephemeral         |
| Heartbeat ping pong      | UDP      | 7004            | Ephemeral         |
| Gamepad controller       | UDP      | 7005            | Ephemeral         |

**Windows Client**: No firewall configuration needed. The client initiates all UDP connections, and Windows automatically allows responses on the same connection.

---

## Linux Capture Notes

- **kmsgrab** gives the lowest overhead and does not draw the cursor. Grant capability:
  ```bash
  sudo setcap cap_sys_admin+ep "$(command -v ffmpeg)"
  ```
- **x11grab** is the fallback when kmsgrab is not viable or you need cursor capture.
- **VAAPI** encode needs access to `/dev/dri/renderD128`. Add your user to the `video` group if needed.

---

## Recommended Presets

- **Lowest Latency**. H.264 at 60 to 120 fps. GOP 8 to 15. Low latency tune.
- **Balanced**. H.264 at 45 to 75 fps. 4 to 10 Mbit per second. GOP 15.
- **High Quality**. H.265 at 30 to 60 fps. 12 to 20 Mbit per second. `yuv444p` if supported by your pipeline.

---

## Security

- Use WireGuard for WAN use. Point the client to the tunnel IP.
- One active client at a time. Additional clients receive BUSY until the session ends.
- Certificate based login after first PIN:
  - On first trusted connection the host creates a mini CA and issues a per device certificate.
  - Copy `client_cert.pem`, `client_key.pem`, `host_ca.pem` to the client's `src/linuxplay/` folder (next to `start.py` and `client.py`).
  - The client detects the files, disables the PIN field and authenticates with the certificate.
  - Private keys never leave the client. Only a fingerprint is sent during handshake.
- Revoke a client by removing or marking the entry in `trusted_clients.json` on the host.

---

## Changelog (recent)

- **Firewall-free Windows clients**: Redesigned network architecture to use outbound-only connections with ephemeral ports. Windows clients no longer require firewall configuration.
- Added certificate based authentication with automatic PIN to certificate upgrade flow.
- Added session lock. New handshakes are rejected with BUSY while a client is active.
- Client GUI now auto detects certificate bundle and disables the PIN field live.
- Improved heartbeat handling and reconnect behavior.
- Expanded controller support and stability.

---

## Troubleshooting

### Host Issues

#### PyQt5 module not found

```bash
# Install all dependencies
uv pip install -e ".[dev]"
# Or with traditional pip
pip3 install PyQt5
```

#### Qt platform plugin "xcb" error

Install required X11 libraries:

```bash
sudo apt install -y libxcb-xinerama0 libxcb-cursor0 libxcb-icccm4 libxcb-image0 \
    libxcb-keysyms1 libxcb-randr0 libxcb-render-util0 libxcb-shape0 \
    libxcb-xfixes0 libxcb-xkb1 libxkbcommon-x11-0
```

If running over SSH, either:
- Run on the actual desktop session (recommended)
- Use SSH with X11 forwarding: `ssh -X user@host`
- Set display: `export DISPLAY=:0`

#### VAAPI error: "No VA display found for device /dev/dri/renderD128"

Add your user to the video and render groups:

```bash
sudo usermod -a -G video,render $USER
# Log out and log back in for changes to take effect
```

Create persistent udev rule:

```bash
echo 'KERNEL=="renderD*", GROUP="render", MODE="0660"' | sudo tee /etc/udev/rules.d/70-render.rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Verify access:

```bash
ls -l /dev/dri/renderD128
# Should show: crw-rw---- 1 root render ...
groups
# Should include: video render
```

Alternatively, use CPU encoding:

```bash
python src/linuxplay/host.py --hwenc cpu --encoder h.264
```

#### Gamepad error: "/dev/uinput" cannot be opened

```bash
sudo chmod 666 /dev/uinput
# Or add to input group:
sudo usermod -a -G input $USER
```

### Windows Client Issues

#### Windows Firewall blocking UDP packets (Legacy Issue - Fixed)

**As of the latest version, Windows clients no longer need firewall configuration!**

The client now uses outbound-only connections with ephemeral ports. Windows Firewall automatically allows responses to outbound connections, so no manual firewall rules are needed.

If you previously added firewall rules for LinuxPlay, you can safely remove them - they're no longer necessary.

**How it works:**
- Client initiates connections using random high ports (ephemeral)
- Host learns the client's actual port and responds there
- Windows sees these as responses to outbound connections and allows them
- Works through NAT and home routers automatically

#### ffplay not found (Audio error)

Download and install FFmpeg for Windows:
1. Download from [FFmpeg Builds](https://github.com/BtbN/FFmpeg-Builds/releases)
2. Extract and add to PATH
3. Or disable audio: `--audio disable`

#### Windows Firewall blocking UDP packets

Add firewall rule for Python:

```powershell
# Run PowerShell as Administrator
New-NetFirewallRule -DisplayName "LinuxPlay Client" -Direction Inbound `
    -Program "C:\Path\To\python.exe" -Action Allow -Profile Private
```

Or through GUI:
1. Windows Security → Firewall & network protection
2. Advanced settings → Inbound Rules → New Rule
3. Program → Browse to your Python executable
4. Allow the connection → Private networks
5. Name: "LinuxPlay Client"

#### OpenGL GLError on Windows

Use ANGLE backend for better Windows compatibility:

```powershell
$env:QT_OPENGL="angle"
$env:QT_ANGLE_PLATFORM="d3d11"
python src/linuxplay/client.py --host_ip YOUR_HOST_IP --decoder h.264 --hwaccel auto
```

### General Issues

#### PIN rotation or connection timeouts

- Ensure UDP ports 5000-7005 are accessible on the **host** (no client firewall config needed)
- Check firewall rules on the **host** machine
- Verify network connectivity: `ping YOUR_HOST_IP`
- For WAN connections, use WireGuard VPN

#### Certificate authentication not working

Ensure all three certificate files are in the client's `src/linuxplay/` folder:
- `client_cert.pem`
- `client_key.pem`
- `host_ca.pem`

They should be next to `client.py` and `start.py` in `src/linuxplay/`.

#### Poor streaming quality or stuttering

- Check network bandwidth: `iperf3` between client and host
- Lower bitrate: `--bitrate 4M` or `--bitrate 6M`
- Reduce framerate: `--framerate 30`
- On WiFi, ensure good signal strength (> -60 dBm)
- Try `--net wifi` mode for WiFi connections

---

## Development

### Code Quality Tools

LinuxPlay uses modern Python tooling for development:

- **[uv](https://github.com/astral-sh/uv)**: Fast Python package manager (pip replacement)
- **[ruff](https://github.com/astral-sh/ruff)**: Extremely fast Python linter and formatter
- **[pytest](https://pytest.org)**: Comprehensive testing framework

### Running Tests

LinuxPlay includes a comprehensive test suite with 150+ tests covering utilities, authentication, and network protocols.

```bash
# Install test dependencies
make install-test

# Run all tests
make test

# Run unit tests only
make test-unit

# Run integration tests only
make test-integration

# Run tests with coverage
make test-cov

# Quick interactive test runner
./run_tests.sh
```

See [tests/README.md](tests/README.md) for detailed testing documentation.

### Using ruff

```bash
# Format code
make format
# Or directly
uv run ruff format .

# Check for issues
make lint
# Or
uv run ruff check .

# Auto-fix issues
make fix
# Or
uv run ruff check . --fix
```

### Available Make Commands

```bash
make help          # Show all available commands
make install       # Install project dependencies
make install-dev   # Install with dev dependencies
make lint          # Run ruff linter
make format        # Format code with ruff
make check         # Check without making changes
make fix           # Auto-fix linting issues
make run-host      # Run the host application
make run-client    # Run the client application
make run-gui       # Run the GUI launcher
make clean         # Clean cache and build artifacts
```

---

## Support LinuxPlay

LinuxPlay is a fully open-source project built from scratch and originally maintained by a single developer in spare time.  
It has since grown into a collaborative, community-driven project thanks to contributors who share a passion for performance, networking, and open-source streaming technology.

If you enjoy LinuxPlay or use it in your workflow, you can help sustain and expand development through GitHub Sponsors:

[![Sponsor @Techlm77](https://img.shields.io/badge/Sponsor-Techlm77-pink.svg?logo=github-sponsors)](https://github.com/sponsors/Techlm77)

Your support helps cover hardware testing, development time, and ongoing improvements to performance, security, and cross-platform compatibility across many different Linux distros, while encouraging future contributors to join and help LinuxPlay continue to evolve.

Special thanks to **@bjornjorgensen** for helping bring LinuxPlay’s tooling, testing, and CI to a production-grade level.  
This collaboration marks a major step toward making LinuxPlay a robust, scalable, and community-driven open-source streaming platform.

---

## License

LinuxPlay is licensed under GNU GPL v2.0 only. See `LICENSE`.  
External tools such as FFmpeg, xdotool, xclip and ffplay are executed as separate processes and retain their own licenses.

---

Developed and maintained by [Techlm77](https://github.com/Techlm77) :)
