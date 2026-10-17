//! Picking the best stream settings for a given client and link, the way
//! Steam Remote Play sizes a session: look at what both ends can encode and
//! decode in hardware, whether the link is a LAN or the internet, and the
//! screen's resolution, then choose a codec, a framerate and a bitrate.

use std::net::IpAddr;

use anyhow::{Result, anyhow};
use linuxplay_proto::signal::VideoCodec;
use tracing::info;

use crate::video::{Backend, Encoder};

/// What the client told us it can do.
pub struct ClientCaps {
    /// Codecs it can decode, in its own order of preference.
    pub codecs: Vec<VideoCodec>,
    /// The subset it decodes in hardware.
    pub hw_codecs: Vec<VideoCodec>,
    /// Highest framerate it wants (its monitor's refresh rate).
    pub max_fps: u32,
}

impl ClientCaps {
    fn can_decode(&self, c: VideoCodec) -> bool {
        self.codecs.contains(&c)
    }

    fn decodes_in_hw(&self, c: VideoCodec) -> bool {
        self.hw_codecs.contains(&c)
    }

    /// Lower is better: where the client ranked this codec.
    fn preference(&self, c: VideoCodec) -> usize {
        self.codecs.iter().position(|x| *x == c).unwrap_or(usize::MAX)
    }
}

/// The settled stream parameters.
#[derive(Debug, Clone, Copy)]
pub struct Plan {
    pub encoder: Encoder,
    pub fps: u32,
    pub start_kbps: u32,
    pub max_kbps: u32,
    pub lan: bool,
}

/// Overrides from the command line; `None`/`0` means "decide automatically".
#[derive(Default, Clone)]
pub struct Overrides {
    pub encoder: Option<String>,
    pub fps: u32,
    pub start_kbps: u32,
    pub max_kbps: u32,
}

/// Is the peer on the same local network (so we can be lavish with bitrate
/// and prefer the lowest-latency codec over the most efficient one)?
pub fn is_lan(host: IpAddr, peer: IpAddr) -> bool {
    match (host, peer) {
        _ if peer.is_loopback() => true,
        (IpAddr::V4(h), IpAddr::V4(p)) => {
            p.is_private() && h.octets()[..3] == p.octets()[..3]
        }
        (IpAddr::V6(h), IpAddr::V6(p)) => {
            // Link-local, or a shared /64.
            (p.segments()[0] & 0xffc0) == 0xfe80 || h.segments()[..4] == p.segments()[..4]
        }
        _ => false,
    }
}

/// The most total video bitrate a link should carry, kbit/s. The budget
/// controller divides this across the active displays.
pub fn link_ceiling(lan: bool) -> u32 {
    if lan { 80_000 } else { 25_000 }
}

/// Roughly how many bits each pixel of each frame costs at good quality for
/// desktop content, per codec. Newer codecs reach the same look for less.
fn bits_per_pixel(codec: VideoCodec) -> f64 {
    match codec {
        VideoCodec::H264 => 0.090,
        VideoCodec::H265 => 0.070,
        VideoCodec::Av1 => 0.060,
    }
}

/// Score a candidate encoder: hardware on both ends matters most, then the
/// LAN/internet-appropriate codec, then the client's stated preference.
fn score(enc: &Encoder, caps: &ClientCaps, lan: bool) -> i64 {
    let codec = enc.codec;
    let host_hw = enc.backend != Backend::Software;
    let client_hw = caps.decodes_in_hw(codec);

    let mut s: i64 = 0;
    // Client-side hardware decode weighs most: software AV1/HEVC at 1080p60
    // can melt a laptop and add latency.
    s += if client_hw { 1200 } else { 0 };
    s += if host_hw { 1000 } else { 0 };
    if !client_hw && codec != VideoCodec::H264 {
        s -= 800;
    }

    // Efficiency vs. latency, by codec. On a LAN there's bandwidth to burn,
    // so favour the simplest/lowest-latency codec; on the internet, favour
    // the one that looks best per megabit.
    let efficiency = match codec {
        VideoCodec::Av1 => 3,
        VideoCodec::H265 => 2,
        VideoCodec::H264 => 1,
    };
    let latency = 4 - efficiency; // H264 lowest latency, AV1 highest
    s += if lan { latency * 60 } else { efficiency * 60 };

    // Break ties toward what the client asked for first.
    s -= caps.preference(codec) as i64;
    s
}

/// Choose everything for this session.
pub fn plan(
    available: &[Encoder],
    caps: &ClientCaps,
    host_ip: IpAddr,
    peer_ip: IpAddr,
    width: u32,
    height: u32,
    ov: &Overrides,
) -> Result<Plan> {
    let lan = is_lan(host_ip, peer_ip);

    let encoder = if let Some(name) = &ov.encoder {
        let enc = available
            .iter()
            .find(|e| e.element == *name)
            .ok_or_else(|| anyhow!("encoder {name} is not available here"))?;
        if !caps.can_decode(enc.codec) {
            return Err(anyhow!("the client can't decode {} from {name}", enc.codec.name()));
        }
        *enc
    } else {
        available
            .iter()
            .filter(|e| caps.can_decode(e.codec))
            .max_by_key(|e| score(e, caps, lan))
            .copied()
            .ok_or_else(|| anyhow!("no encoder here for any codec the client can decode"))?
    };

    // Framerate: the lower of the client's refresh rate and our cap.
    let cap = if ov.fps > 0 { ov.fps } else { 60 };
    let fps = caps.max_fps.clamp(24, cap.max(24)).min(cap);

    // Bitrate ceiling: scale from resolution, framerate and codec, then clamp
    // to a LAN or internet ceiling. The CLI can override either end.
    let pixels = (width as f64) * (height as f64);
    let base = pixels * fps as f64 * bits_per_pixel(encoder.codec) / 1000.0;
    // On a LAN, latency comes from the encoder, not the link, so spend freely
    // for near-lossless frames; on the internet, hold to an efficient target.
    let factor = if lan { 2.5 } else { 1.0 };
    let ceiling = link_ceiling(lan);
    let auto_max = (base * factor) as u32;
    let max_kbps = if ov.max_kbps > 0 {
        ov.max_kbps
    } else {
        auto_max.clamp(2_000, ceiling)
    };
    // Start high on a LAN (BWE confirms headroom in under a second); start
    // nearer the middle on the internet to avoid an early overshoot.
    let start_kbps = if ov.start_kbps > 0 {
        ov.start_kbps.min(max_kbps)
    } else if lan {
        max_kbps
    } else {
        (max_kbps / 2).max(2_000)
    };

    let plan = Plan { encoder, fps, start_kbps, max_kbps, lan };
    info!(
        "negotiated: {} via {} ({:?}), {}x{} @ {} fps, {}–{} kbit/s, link {}",
        encoder.codec.name(),
        encoder.element,
        encoder.backend,
        width,
        height,
        plan.fps,
        plan.start_kbps,
        plan.max_kbps,
        if lan { "LAN" } else { "internet" },
    );
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::{Backend, Encoder};

    fn enc(element: &'static str, codec: VideoCodec, backend: Backend) -> Encoder {
        Encoder { element, codec, backend }
    }

    fn all() -> Vec<Encoder> {
        vec![
            enc("vah264enc", VideoCodec::H264, Backend::Va),
            enc("vah265enc", VideoCodec::H265, Backend::Va),
            enc("vaav1enc", VideoCodec::Av1, Backend::Va),
            enc("x264enc", VideoCodec::H264, Backend::Software),
        ]
    }

    fn caps(codecs: &[VideoCodec], hw: &[VideoCodec]) -> ClientCaps {
        ClientCaps { codecs: codecs.to_vec(), hw_codecs: hw.to_vec(), max_fps: 60 }
    }

    #[test]
    fn lan_detection() {
        assert!(is_lan("192.168.8.184".parse().unwrap(), "192.168.8.120".parse().unwrap()));
        assert!(!is_lan("192.168.8.184".parse().unwrap(), "9.9.9.9".parse().unwrap()));
        assert!(is_lan("1.2.3.4".parse().unwrap(), "127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn lan_prefers_hardware_h264_for_latency() {
        // Client can hw-decode everything; on a LAN we want H264 (lowest latency).
        let c = caps(&[VideoCodec::Av1, VideoCodec::H265, VideoCodec::H264], &[VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1]);
        let p = plan(&all(), &c, "192.168.8.1".parse().unwrap(), "192.168.8.2".parse().unwrap(), 1920, 1080, &Overrides::default()).unwrap();
        assert_eq!(p.encoder.codec, VideoCodec::H264);
        assert_eq!(p.encoder.backend, Backend::Va);
        assert!(p.lan);
    }

    #[test]
    fn internet_prefers_efficient_codec() {
        let c = caps(&[VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1], &[VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1]);
        let p = plan(&all(), &c, "10.0.0.1".parse().unwrap(), "203.0.113.5".parse().unwrap(), 1920, 1080, &Overrides::default()).unwrap();
        assert_eq!(p.encoder.codec, VideoCodec::Av1);
        assert!(!p.lan);
        assert!(p.max_kbps <= 25_000);
    }

    #[test]
    fn avoids_software_decode_on_client() {
        // Client lists HEVC but can only decode H264 in hardware.
        let c = caps(&[VideoCodec::H265, VideoCodec::H264], &[VideoCodec::H264]);
        let p = plan(&all(), &c, "203.0.113.1".parse().unwrap(), "198.51.100.2".parse().unwrap(), 1920, 1080, &Overrides::default()).unwrap();
        assert_eq!(p.encoder.codec, VideoCodec::H264);
    }
}
