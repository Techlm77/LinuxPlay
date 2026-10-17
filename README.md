# LinuxPlay

Low‑latency remote desktop for Linux, built from scratch in Rust.

- **Transport:** WebRTC (str0m) over mutual‑TLS signalling — encrypted, peer‑to‑peer.
- **Media:** GStreamer. Hardware H.264 / H.265 / AV1 (VA‑API, NVENC) with a software fallback; Opus audio.
- **UI:** GTK4 / libadwaita client.
- **Security:** self‑signed identities pinned by fingerprint; first pairing authenticated by a PIN (SPAKE2 + key confirmation binding both certificates).
- **Adaptive:** picks codec, bitrate and framerate from the link (LAN vs internet) and both ends' hardware, then follows the measured bandwidth live.

## Workspace

| crate | what it is |
|-------|-----------|
| `proto`  | wire protocol (signalling + data‑channel messages) |
| `net`    | WebRTC session, TLS identities, PIN pairing |
| `host`   | `linuxplay-host` — shares a desktop (X11 capture + XTest today) |
| `client` | `linuxplay` — the GTK4 app |

## Build

```
cargo build --release
```

Run `linuxplay-host` on the machine to share, then `linuxplay <address>` on the other.

Licensed GPL‑3.0‑or‑later.
