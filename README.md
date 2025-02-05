# LinuxPlay – A Fast Remote Desktop for Linux

LinuxPlay is a lightweight, low-latency, open-source remote desktop solution for Linux. It provides seamless video streaming and full keyboard/mouse control with ultra-low latency, making it a great alternative to VNC or Parsec (which lacks proper Linux support). No accounts, no cloud, 100% local and private.

## Features

- Ultra-low latency using UDP multicast for real-time streaming
- Full keyboard and mouse support, including function keys and modifiers
- Adaptive bitrate streaming to adjust quality based on network conditions
- Local only, no cloud dependency or account requirements
- Hardware-accelerated encoding and decoding for better performance
- Fully open-source and customizable

## Installation

LinuxPlay requires Python 3, FFmpeg, xdotool, and PyQt5. Install them using:

```bash
sudo apt install python3 ffmpeg xdotool python3-pyqt5
```

## Usage

### Start the Host (Server)

Run the following command on the remote machine (the one you want to control):

```bash
python3 host.py --encoder vaapi --resolution 1600x900 --framerate 60 --audio enable --password password123
```

#### Available Options:
| Option             | Description                                            | Default     |
|--------------------|--------------------------------------------------------|-------------|
| `--encoder`       | Video encoder: `nvenc`, `vaapi`, or `none` (CPU)        | `none`      |
| `--resolution`    | Capture resolution (e.g., `1920x1080`)                  | `1920x1080` |
| `--framerate`     | Capture framerate (e.g., `30`, `60`)                    | `30`        |
| `--bitrate`       | Initial video bitrate (e.g., `8M`)                      | `8M`        |
| `--audio`         | Enable or disable audio streaming (`enable`, `disable`) | `disable`   |
| `--adaptive`      | Enable adaptive bitrate switching                       | Off         |
| `--password`      | Set an optional password for control messages           | None        |

---

### Start the Client (Viewer)

Run this command on the local machine (the one used to control the remote PC):

```bash
python3 client.py --decoder nvdec --udp_port 5000 --host_ip 192.168.1.123 --remote_resolution 1600x900 --audio enable --password password123
```

#### Available Options:
| Option                | Description                                            | Default     |
|-----------------------|--------------------------------------------------------|-------------|
| `--decoder`           | Video decoder: `nvdec`, `vaapi`, or `none` (CPU)       | `none`      |
| `--udp_port`          | UDP port for video streaming                           | `5000`      |
| `--host_ip`           | The IP address of the host machine                     | Required    |
| `--remote_resolution` | Remote screen resolution (e.g., `1600x900`)            | `1920x1080` |
| `--audio`             | Enable or disable audio playback (`enable`, `disable`) | `disable`   |
| `--password`          | Optional password for control events and handshake     | None        |

---

## Why Choose LinuxPlay?

LinuxPlay is designed specifically for Linux, unlike Parsec, which lacks proper Linux support.

- Faster than VNC with low-latency streaming
- More flexible than RDP, works over LAN without cloud lock-in
- Supports full keyboard and mouse functionality, including all special keys

---

## Contribute

This project is open-source. Contributions, bug reports, and feature suggestions are welcome.

---

## License

This project is licensed under the MIT License.


---

## Future Plans

Development is ongoing, and here are some planned features for future updates:

- **Clipboard Sharing** – Allow users to copy and paste text between the client and host.
- **H.265 and AV1 Support** – Improve compression and reduce bandwidth usage for better performance.
- **Full Encryption** – Implement TLS encryption for control messages and video streaming to enhance security.
- **Internet-Ready Security** – Enable secure connections over the internet with end-to-end encryption.
- **Multi-Client Support** – Allow multiple clients to connect and view the same session.
- **File Transfer** – Enable seamless file transfers between the host and client.
- **Dynamic Encoding Adjustment** – Allow changing resolution, bitrate, and framerate on the fly.
- **Wayland Support** – Extend compatibility beyond X11 for modern Linux desktops.

### Security Warning
LinuxPlay can be used over the internet, but **it currently does not have encryption**. It is recommended to use a **VPN, SSH tunnel, or manually restrict access via firewall rules** if you plan to use it remotely. Future versions will include built-in encryption to enhance security.

---
