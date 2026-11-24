//! Which decoders exist for each codec, in the order they are tried.
//!
//! Hardware first: VA-API covers AMD and Intel (and hands frames to OpenGL
//! without a copy), NVDEC covers Nvidia, Vulkan Video covers drivers that
//! offer neither. Software decoders are the floor that works everywhere.
//! GStreamer only registers a hardware decoder when a device on this machine
//! supports the codec, so being installed is a good first test; a decoder
//! that still fails once frames arrive is replaced by the next one.

use linuxplay_media::{codec, element};
use linuxplay_proto::signal::VideoCodec;

/// One way to decode a codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// The decoder element.
    pub element: &'static str,
    /// Elements needed after it before the frame can go to OpenGL, as a
    /// pipeline fragment starting with ` ! `, or empty.
    pub post: &'static str,
    pub hardware: bool,
}

const fn hw(element: &'static str) -> Candidate {
    Candidate { element, post: "", hardware: true }
}

const fn sw(element: &'static str) -> Candidate {
    Candidate { element, post: "", hardware: false }
}

/// Vulkan decoders leave frames in Vulkan images, which have to come back to
/// system memory before OpenGL can take them.
const fn vulkan(element: &'static str) -> Candidate {
    Candidate { element, post: " ! vulkandownload", hardware: true }
}

const H264: &[Candidate] =
    &[hw("vah264dec"), hw("nvh264dec"), vulkan("vulkanh264dec"), sw("avdec_h264"), sw("openh264dec")];
const H265: &[Candidate] = &[hw("vah265dec"), hw("nvh265dec"), vulkan("vulkanh265dec"), sw("avdec_h265")];
const AV1: &[Candidate] = &[hw("vaav1dec"), hw("nvav1dec"), sw("dav1ddec")];

fn known(codec: VideoCodec) -> &'static [Candidate] {
    match codec {
        VideoCodec::H264 => H264,
        VideoCodec::H265 => H265,
        VideoCodec::Av1 => AV1,
    }
}

/// Names a decoder to try before all others, for diagnosing a machine.
const OVERRIDE_VARIABLE: &str = "LINUXPLAY_DECODER";

/// The decoders installed here for `codec`, best first.
pub fn candidates(codec: VideoCodec) -> Vec<Candidate> {
    let mut list: Vec<Candidate> = known(codec).iter().copied().filter(|c| element::exists(c.element)).collect();
    if let Ok(wanted) = std::env::var(OVERRIDE_VARIABLE)
        && let Some(index) = list.iter().position(|c| c.element == wanted) {
            let chosen = list.remove(index);
            list.insert(0, chosen);
        }
    list
}

/// Codecs this machine can decode, in the order we'd like the host to pick.
pub fn decodable() -> Vec<VideoCodec> {
    codec::ALL.into_iter().filter(|c| !candidates(*c).is_empty()).collect()
}

/// The subset of [`decodable`] decoded in hardware. The host weighs these
/// heavily: decoding AV1 or HEVC in software costs latency and battery.
pub fn hw_decodable() -> Vec<VideoCodec> {
    codec::ALL.into_iter().filter(|c| candidates(*c).iter().any(|d| d.hardware)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_codec_has_a_software_floor_listed_after_hardware() {
        for c in codec::ALL {
            let list = known(c);
            assert!(list.iter().any(|d| !d.hardware), "{c:?} has no software decoder");
            let first_sw = list.iter().position(|d| !d.hardware).unwrap();
            assert!(list[first_sw..].iter().all(|d| !d.hardware), "{c:?}: hardware listed after software");
        }
    }

    #[test]
    fn what_is_hardware_decodable_is_decodable() {
        let _ = gstreamer::init();
        let all = decodable();
        assert!(hw_decodable().iter().all(|c| all.contains(c)));
    }
}
