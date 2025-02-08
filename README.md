# LinuxPlay – A Fast, Fully Open-Source Remote Desktop for Linux

LinuxPlay is a lightweight, low-latency, fully open-source remote desktop solution designed specifically for Linux. It provides seamless video streaming and full keyboard/mouse control with ultra-low latency, making it a superior alternative to VNC and other traditional remote desktop solutions. LinuxPlay is 100% local and private – no accounts, no cloud dependency, just pure performance.

## Features

- **Ultra-low latency** using UDP multicast for real-time streaming
- **Full keyboard and mouse support**, including function keys and modifiers
- **Adaptive bitrate streaming** to optimize quality based on network conditions
- **Completely local and private** – no third-party servers, no cloud accounts
- **Hardware-accelerated encoding and decoding** for maximum performance
- **Fully open-source** and customizable to fit your needs
- **Clipboard sharing** between the client and host

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
python3 client.py --decoder nvdec --host_ip 192.168.1.123 --remote_resolution 1600x900 --audio enable --password password123
```

#### Available Options:
| Option                | Description                                            | Default     |
|-----------------------|--------------------------------------------------------|-------------|
| `--decoder`           | Video decoder: `nvdec`, `vaapi`, or `none` (CPU)       | `none`      |
| `--host_ip`           | The IP address of the host machine                     | Required    |
| `--remote_resolution` | Remote screen resolution (e.g., `1600x900`)            | `1920x1080` |
| `--audio`             | Enable or disable audio playback (`enable`, `disable`) | `disable`   |
| `--password`          | Optional password for control events and handshake     | None        |

---

## Why Choose LinuxPlay?

LinuxPlay is designed specifically for Linux, unlike many other remote desktop solutions that have limited or outdated Linux support. Here’s how LinuxPlay compares:

| Feature             | LinuxPlay | VNC | X2Go | NoMachine |
|--------------------|-----------|-----|------|-----------|
| **Latency**        | Ultra-low | High | Medium | Medium |
| **Video Streaming** | Hardware-accelerated | Software-based | Software-based | Limited hardware acceleration |
| **Audio Support**  | Yes | No | Yes | Yes |
| **Clipboard Sharing** | Yes | Yes | Yes | Yes |
| **Adaptive Bitrate** | Yes | No | No | No |
| **Encryption** | Planned (Future) | Yes | Yes | Yes |
| **Open-Source** | **Yes** | Yes | Yes | No |
| **Cloud-Dependency** | None | None | None | Required for full features |

---

## Contribute

LinuxPlay is **fully open-source** and welcomes contributions. Whether you want to improve performance, add features, or report bugs, you are welcome to participate.

---

## License

LinuxPlay is licensed under the MIT License.

---

## Future Plans

Development is ongoing, and here are some planned features for future updates:

- **(Added) Clipboard Sharing** – Copy and paste text between client and host.
- **H.265 and AV1 Support** – Improve compression and reduce bandwidth usage for better performance.
- **Full Encryption** – Implement TLS encryption for control messages and video streaming to enhance security.
- **Internet-Ready Security** – Enable secure connections over the internet with end-to-end encryption.

### Security Warning

LinuxPlay can be used over the internet, but **it currently does not have encryption**. It is recommended to use a **VPN, SSH tunnel, or manually restrict access via firewall rules** if you plan to use it remotely. Future versions will include built-in encryption to enhance security.

---
