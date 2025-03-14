# LinuxPlay – A Fast, Fully Open-Source Remote Desktop for Linux

LinuxPlay is a lightweight, low-latency, fully open-source remote desktop solution designed specifically for Linux. It provides seamless video streaming and full keyboard/mouse control with low latency, making it a superior alternative to VNC and other traditional remote desktop solutions. LinuxPlay is 100% local and private – no accounts, no cloud dependency, just pure performance.

## Features

- **Low-latency** using UDP multicast streaming
- **Full keyboard and mouse support**, including function keys and modifiers
- **Multi-monitor support** – Choose individual displays or view all at once
- **Adaptive bitrate streaming** to optimize quality based on network conditions
- **Hardware-accelerated encoding and decoding** for superior performance
- **Drag-and-drop file transfer** for seamless file sharing
- **Clipboard sharing** between the client and host

## Installation

```bash
sudo apt-get update && sudo apt-get install -y python3-pip python3-pyqt5 ffmpeg xdotool xclip libgl1-mesa-dev mesa-utils && pip3 install av
```

## Usage

### Start the Host (Server)

Run the following command on the remote machine (the one you want to control):

```bash
python3 host.py --encoder vaapi --resolution 1600x900 --framerate 60 --audio enable --password password123
```

#### Available Options:
| Option          | Description                                             | Default     |
|-----------------|---------------------------------------------------------|-------------|
| `--encoder`    | Video encoder: `nvenc`, `vaapi`, or `none` (CPU)         | `none`      |
| `--resolution` | Capture resolution (e.g., `1920x1080`)                   | `1920x1080` |
| `--framerate`  | Capture framerate (e.g., `30`, `60`)                     | `30`        |
| `--bitrate`    | Initial video bitrate (e.g., `8M`)                       | `8M`        |
| `--audio`      | Enable or disable audio streaming (`enable`, `disable`)  | `disable`   |
| `--adaptive`   | Enable adaptive bitrate switching                        | Off         |
| `--password`   | Set an optional password for control messages            | None        |

---

### Start the Client (Viewer)

Run this command on the local machine (the one used to control the remote PC):

```bash
python3 client.py --decoder nvdec --host_ip 192.168.1.123 --remote_resolution 1600x900 --audio enable --password password123
```

#### Available Options:
| Option                | Description                                             | Default     |
|-----------------------|---------------------------------------------------------|-------------|
| `--decoder`           | Video decoder: `nvdec`, `vaapi`, or `none` (CPU)        | `none`      |
| `--host_ip`           | The IP address of the host machine                      | Required    |
| `--remote_resolution` | Remote screen resolution (e.g., `1600x900`)             | `1920x1080` |
| `--audio`             | Enable or disable audio playback (`enable`, `disable`)  | `disable`   |
| `--password`          | Optional password for control events and handshake      | None        |

## Why No Wayland Support?

LinuxPlay does **not** support Wayland due to the following reasons:

- **FFmpeg does not support Wayland for screen capture.** Even if I wanted to, there is no reliable FFmpeg-based solution.
- **GStreamer relies on PipeWire for screen capture, but PipeWire does not work on any of my systems, making it unusable.**
- **KMS/DRM does not work for screen capture, even with direct display output.**

I have extensively tested all these alternatives and none of them worked reliably, which is why LinuxPlay remains X11-only.

## Contribute

LinuxPlay is fully open-source and welcomes contributions. Whether you want to improve performance, add features, or report bugs, your input is appreciated.

## License

LinuxPlay is licensed under the MIT License.

## Future Plans

- **Xbox/PlayStation Controller**

## Testing

- **Full Encryption** – Implement TLS encryption for control messages and video streaming.
- **Internet-Ready Security** – Enable secure remote connections with end-to-end encryption.

## Security Warning

LinuxPlay can be used over the internet, but it currently does **not** include built-in encryption. It is recommended to use a **VPN, SSH tunnel, or firewall rules** to secure your connection if accessing remotely. Future versions will include built-in encryption for enhanced security.
