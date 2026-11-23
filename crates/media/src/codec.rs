//! How each codec is framed between our pipelines and the network: one
//! complete access unit per buffer, with parameter sets in-band so a decoder
//! can start at any keyframe.

use std::borrow::Cow;

use linuxplay_proto::signal::VideoCodec;

/// Caps of the encoded stream as it enters or leaves a pipeline.
pub fn caps(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "video/x-h264,stream-format=byte-stream,alignment=au",
        VideoCodec::H265 => "video/x-h265,stream-format=byte-stream,alignment=au",
        VideoCodec::Av1 => "video/x-av1,stream-format=obu-stream,alignment=tu",
    }
}

/// The parser element (with its settings) that produces or accepts [`caps`].
pub fn parser(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "h264parse config-interval=-1",
        VideoCodec::H265 => "h265parse config-interval=-1",
        VideoCodec::Av1 => "av1parse",
    }
}

/// Every codec, in a fixed order.
pub const ALL: [VideoCodec; 3] = [VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1];

/// An AV1 temporal delimiter: an OBU of type 2 with a size field saying it
/// is empty.
const AV1_TEMPORAL_DELIMITER: [u8; 2] = [0x12, 0x00];
/// Bits of an OBU's first byte that hold its type.
const AV1_OBU_TYPE_MASK: u8 = 0x78;

/// Turn a frame as it comes off the network into what [`caps`] promises the
/// decoder. RTP carries AV1 without the temporal delimiter that starts every
/// temporal unit, so it is put back; the other codecs arrive ready.
pub fn for_decoder(codec: VideoCodec, frame: &[u8]) -> Cow<'_, [u8]> {
    let starts_with_delimiter =
        frame.first().is_some_and(|b| b & AV1_OBU_TYPE_MASK == AV1_TEMPORAL_DELIMITER[0] & AV1_OBU_TYPE_MASK);
    if codec != VideoCodec::Av1 || starts_with_delimiter {
        return Cow::Borrowed(frame);
    }
    let mut unit = Vec::with_capacity(AV1_TEMPORAL_DELIMITER.len() + frame.len());
    unit.extend_from_slice(&AV1_TEMPORAL_DELIMITER);
    unit.extend_from_slice(frame);
    Cow::Owned(unit)
}

/// The part of a decoded picture to show. Some encoders can only code sizes
/// that are a multiple of their block size and pad the picture to it (AMD's
/// AV1 encoder turns 1080 rows into 1082); the padding is not part of the
/// screen. `announced` is the size the host says its screen has.
pub fn visible_size(decoded: (u32, u32), announced: (u32, u32)) -> (u32, u32) {
    /// The largest block any of our codecs pads to.
    const MAX_PADDING: u32 = 64;
    let fits = |shown: u32, coded: u32| shown > 0 && shown <= coded && coded - shown < MAX_PADDING;
    if fits(announced.0, decoded.0) && fits(announced.1, decoded.1) { announced } else { decoded }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn av1_frames_get_their_temporal_delimiter_back() {
        // A sequence header OBU (type 1) with a size field.
        let frame = [0x0a, 0x01, 0xff];
        assert_eq!(&*for_decoder(VideoCodec::Av1, &frame), &[0x12, 0x00, 0x0a, 0x01, 0xff]);
        // Already delimited, with or without a size field on the delimiter.
        assert_eq!(&*for_decoder(VideoCodec::Av1, &[0x12, 0x00, 0x0a]), &[0x12, 0x00, 0x0a]);
        assert_eq!(&*for_decoder(VideoCodec::Av1, &[0x10, 0x0a]), &[0x10, 0x0a]);
        assert_eq!(&*for_decoder(VideoCodec::Av1, &[]), &AV1_TEMPORAL_DELIMITER);
    }

    #[test]
    fn other_codecs_pass_through_untouched() {
        let frame = [0, 0, 0, 1, 0x67];
        assert!(matches!(for_decoder(VideoCodec::H264, &frame), Cow::Borrowed(_)));
        assert!(matches!(for_decoder(VideoCodec::H265, &frame), Cow::Borrowed(_)));
    }

    #[test]
    fn encoder_padding_is_cropped_but_a_real_resize_is_not() {
        assert_eq!(visible_size((1920, 1082), (1920, 1080)), (1920, 1080));
        assert_eq!(visible_size((1920, 1088), (1920, 1080)), (1920, 1080));
        assert_eq!(visible_size((1920, 1080), (1920, 1080)), (1920, 1080));
        // The host's screen changed size before the announcement caught up.
        assert_eq!(visible_size((2560, 1440), (1920, 1080)), (2560, 1440));
        assert_eq!(visible_size((1280, 720), (1920, 1080)), (1280, 720));
        assert_eq!(visible_size((1920, 1080), (0, 0)), (1920, 1080));
    }
}
