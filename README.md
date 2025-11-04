# LinuxPlay

> The open source, ultra low latency remote desktop and game streaming stack for Linux. Built with FFmpeg, UDP and Qt.

![License: GPLv2](https://img.shields.io/badge/License-GPLv2-blue.svg)
![Platform: Linux](https://img.shields.io/badge/Platform-Linux-green.svg)
![Python 3.9+](https://img.shields.io/badge/Python-3.9%2B-blue)
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
  - Copy these three files with a USB drive to the client folder next to `start.py` and `client.py`.
  - The client detects the files and skips the PIN. The GUI disables the PIN field automatically (no restart needed).
- **Controller Support**: Full gamepad forwarding over UDP using a virtual uinput device on the host. Works with Xbox, DualSense, 8BitDo and other HID controllers.
- **Multi Monitor**: Stream one or more displays. Resolution and offsets are detected per display.
- **Clipboard and File Transfer**: Bi directional clipboard sync and client to host file uploads on TCP.
- **Link Aware Streaming**: Buffers adapt for LAN and Wi Fi to reduce jitter and stalls.
- **Resilience**: Heartbeat with ping and pong. The host stops streams and returns to the waiting state on timeout or disconnect.
- **Stats Overlay (Client)**: Real time FPS, CPU, RAM and GPU metrics via OpenGL with triple buffered PBO uploads.
- **Cross Platform**: Host on Linux. Clients available for Linux and Windows.

---

## Why LinuxPlay

LinuxPlay is for people who want speed, control and transparency.  
No accounts. No hidden daemons. No black boxes.  
You pick the codec, bitrate, buffers and behavior. Every knob is exposed and does something you can measure.

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

### Ubuntu 24.04 packages

```bash
sudo apt update
sudo apt install -y ffmpeg xdotool xclip pulseaudio-utils libcap2-bin wireguard-tools qrencode python3 python3-venv python3-pip libgl1 libegl1 python3-evdev
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
python3 start.py
# Or with uv
uv run python start.py
```
- Host tab. Pick a preset and select Start Host.
- Client tab. Enter the host LAN IP or WireGuard tunnel IP and select Start Client.
- The client GUI detects the certificate bundle if present and disables the PIN field automatically.

### Command line

```bash
# Host
python3 host.py --gui --encoder h.264 --hwenc auto --framerate 60 --bitrate 8M --audio enable --gop 15 --pix_fmt yuv420p

# Client
python3 client.py --host_ip 192.168.1.20 --decoder h.264 --hwaccel auto --audio enable --monitor 0 --gamepad enable --debug
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

| Purpose                  | Protocol | Port            |
|--------------------------|----------|-----------------|
| Handshake                | TCP      | 7001            |
| Video per monitor        | UDP      | 5000 plus index |
| Audio                    | UDP      | 6001            |
| Control mouse keyboard   | UDP      | 7000            |
| Clipboard                | UDP      | 7002            |
| File upload              | TCP      | 7003            |
| Heartbeat ping pong      | UDP      | 7004            |
| Gamepad controller       | UDP      | 7005            |

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
  - Copy `client_cert.pem`, `client_key.pem`, `host_ca.pem` to the client folder next to `start.py` and `client.py`.
  - The client detects the files, disables the PIN field and authenticates with the certificate.
  - Private keys never leave the client. Only a fingerprint is sent during handshake.
- Revoke a client by removing or marking the entry in `trusted_clients.json` on the host.

---

## Changelog (recent)

- Added certificate based authentication with automatic PIN to certificate upgrade flow.
- Added session lock. New handshakes are rejected with BUSY while a client is active.
- Client GUI now auto detects certificate bundle and disables the PIN field live.
- Improved heartbeat handling and reconnect behavior.
- Expanded controller support and stability.

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
