//! Input on an X11 session through the XTEST extension.

use anyhow::{Context, Result, anyhow};
use linuxplay_proto::input::MouseButton;
use tracing::warn;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT, Window,
};
use x11rb::protocol::xtest::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

use super::{Injector, scale};

/// X11 keycodes are evdev codes offset by 8.
const EVDEV_OFFSET: u16 = 8;
const NOTCH: i32 = 120;

pub struct X11Injector {
    conn: RustConnection,
    root: Window,
    /// The captured area within the root window.
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    scroll: (i32, i32),
}

impl X11Injector {
    /// Inject into the area `(x, y, width, height)` of the root window.
    pub fn connect(display: Option<&str>, area: (i32, i32, u32, u32)) -> Result<Self> {
        let (conn, screen) = x11rb::connect(display).context("connecting to the X server")?;
        conn.extension_information(xtest::X11_EXTENSION_NAME)?
            .ok_or_else(|| anyhow!("the X server has no XTEST extension"))?;
        let root = conn.setup().roots[screen].root;
        let (x, y, width, height) = area;
        Ok(Self { conn, root, x, y, width, height, scroll: (0, 0) })
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) {
        if let Err(e) = self.conn.xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, self.root, x, y, 0) {
            warn!("XTEST: {e}");
        }
    }

    fn click(&self, button: u8) {
        self.fake(BUTTON_PRESS_EVENT, button, 0, 0);
        self.fake(BUTTON_RELEASE_EVENT, button, 0, 0);
    }
}

/// Take whole notches out of an accumulator, returning how many (signed).
fn notches(acc: &mut i32, delta: i32) -> i32 {
    *acc += delta;
    let n = *acc / NOTCH;
    *acc -= n * NOTCH;
    n
}

impl Injector for X11Injector {
    fn pointer_abs(&mut self, x: u16, y: u16) {
        let px = self.x + scale(x, self.width) as i32;
        let py = self.y + scale(y, self.height) as i32;
        self.fake(MOTION_NOTIFY_EVENT, 0, px as i16, py as i16);
    }

    fn pointer_rel(&mut self, dx: f32, dy: f32) {
        self.fake(MOTION_NOTIFY_EVENT, 1, dx.round() as i16, dy.round() as i16);
    }

    fn button(&mut self, button: MouseButton, pressed: bool) {
        let kind = if pressed { BUTTON_PRESS_EVENT } else { BUTTON_RELEASE_EVENT };
        self.fake(kind, button.x11(), 0, 0);
    }

    fn scroll(&mut self, dx: i32, dy: i32) {
        // Buttons 4/5 scroll up/down, 6/7 left/right.
        let v = notches(&mut self.scroll.1, dy);
        for _ in 0..v.abs() {
            self.click(if v > 0 { 5 } else { 4 });
        }
        let h = notches(&mut self.scroll.0, dx);
        for _ in 0..h.abs() {
            self.click(if h > 0 { 7 } else { 6 });
        }
    }

    fn key(&mut self, code: u16, pressed: bool) {
        let Ok(keycode) = u8::try_from(code + EVDEV_OFFSET) else { return };
        let kind = if pressed { KEY_PRESS_EVENT } else { KEY_RELEASE_EVENT };
        self.fake(kind, keycode, 0, 0);
    }

    fn flush(&mut self) {
        if let Err(e) = self.conn.flush() {
            warn!("X11 flush: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::notches;

    #[test]
    fn scroll_accumulates_partial_notches() {
        let mut acc = 0;
        assert_eq!(notches(&mut acc, 60), 0);
        assert_eq!(notches(&mut acc, 60), 1);
        assert_eq!(notches(&mut acc, -240), -2);
        assert_eq!(acc, 0);
        assert_eq!(notches(&mut acc, 250), 2);
        assert_eq!(acc, 10);
    }
}
