//! Where the video sits inside the window, and how a point on the window
//! maps back onto the host's screen.

use linuxplay_proto::input::POINTER_MAX;

/// A rectangle in window coordinates, origin at the top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The largest rectangle with the video's aspect ratio that fits the window,
/// centred, leaving black bars on two sides where the ratios differ. `None`
/// while either size is still unknown.
pub fn letterbox(window: (u32, u32), video: (u32, u32)) -> Option<Rect> {
    let (ww, wh) = (window.0 as f32, window.1 as f32);
    let (vw, vh) = (video.0 as f32, video.1 as f32);
    if ww <= 0.0 || wh <= 0.0 || vw <= 0.0 || vh <= 0.0 {
        return None;
    }
    let scale = (ww / vw).min(wh / vh);
    let (w, h) = (vw * scale, vh * scale);
    Some(Rect { x: (ww - w) / 2.0, y: (wh - h) / 2.0, w, h })
}

impl Rect {
    /// Map a point to normalised host coordinates, or `None` when it falls
    /// outside the picture (in the bars).
    pub fn to_remote(self, x: f32, y: f32) -> Option<(u16, u16)> {
        let (nx, ny) = ((x - self.x) / self.w, (y - self.y) / self.h);
        if !(0.0..=1.0).contains(&nx) || !(0.0..=1.0).contains(&ny) {
            return None;
        }
        let max = POINTER_MAX as f32;
        Some(((nx * max).round() as u16, (ny * max).round() as u16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matching_window_is_filled() {
        let r = letterbox((1920, 1080), (3840, 2160)).unwrap();
        assert_eq!(r, Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 });
    }

    #[test]
    fn a_wide_window_gets_side_bars() {
        let r = letterbox((2000, 1000), (1000, 1000)).unwrap();
        assert_eq!(r, Rect { x: 500.0, y: 0.0, w: 1000.0, h: 1000.0 });
        assert_eq!(r.to_remote(499.0, 10.0), None);
        assert_eq!(r.to_remote(500.0, 0.0), Some((0, 0)));
        assert_eq!(r.to_remote(1500.0, 1000.0), Some((POINTER_MAX, POINTER_MAX)));
        assert_eq!(r.to_remote(1000.0, 500.0), Some((32768, 32768)));
    }

    #[test]
    fn unknown_sizes_give_no_rectangle() {
        assert_eq!(letterbox((0, 100), (100, 100)), None);
        assert_eq!(letterbox((100, 100), (100, 0)), None);
    }
}
