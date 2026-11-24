//! Text drawn over the picture: notices in the middle of the window, the
//! statistics panel in a corner, and short hints along the bottom.

use anyhow::Result;

use crate::gl::{Font, FontFile, Renderer, Rgba, TextStyle};
use crate::layout::Rect;

const TITLE_PX: f32 = 20.0;
const BODY_PX: f32 = 14.5;
/// Space between a box's edge and its text, and between a box and the
/// window's edge, in unscaled pixels.
const PADDING: f32 = 14.0;
const MARGIN: f32 = 16.0;

const BACKDROP: Rgba = [0.06, 0.07, 0.09, 0.84];
const BRIGHT: Rgba = [1.0, 1.0, 1.0, 1.0];
const NORMAL: Rgba = [0.82, 0.85, 0.89, 1.0];
const DIM: Rgba = [0.58, 0.62, 0.68, 1.0];

/// How prominent a line of text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// The heading of a box, in the larger font.
    Title,
    Normal,
    /// Secondary information and key hints.
    Dim,
}

/// One line of a box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub tone: Tone,
    pub text: String,
}

impl Line {
    pub fn title(text: impl Into<String>) -> Self {
        Self { tone: Tone::Title, text: text.into() }
    }

    pub fn normal(text: impl Into<String>) -> Self {
        Self { tone: Tone::Normal, text: text.into() }
    }

    pub fn dim(text: impl Into<String>) -> Self {
        Self { tone: Tone::Dim, text: text.into() }
    }
}

/// Where a box sits in the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Centre,
    TopRight,
    BottomCentre,
}

/// A font and its atlas on the GPU.
struct Face {
    font: Font,
    atlas: glow::Texture,
}

impl Face {
    fn new(renderer: &Renderer, file: &FontFile, px: f32) -> Result<Self> {
        let font = Font::new(file, px)?;
        let a = font.atlas();
        let atlas = renderer.create_atlas(a.width, a.height, &a.pixels)?;
        Ok(Self { font, atlas })
    }
}

pub struct Hud {
    file: FontFile,
    title: Face,
    body: Face,
    /// Framebuffer pixels per unscaled pixel.
    scale: f32,
}

impl Hud {
    /// Find the system font and prepare it for `scale` framebuffer pixels per
    /// unscaled pixel.
    pub fn new(renderer: &Renderer, scale: f32) -> Result<Self> {
        let file = FontFile::system()?;
        let scale = scale.max(1.0);
        let (title, body) = faces(renderer, &file, scale)?;
        Ok(Self { file, title, body, scale })
    }

    /// Re-rasterise the text for another scale, when the window has moved to
    /// a display that has one. Keeps the old faces if that fails.
    pub fn rescale(&mut self, renderer: &Renderer, scale: f32) {
        let scale = scale.max(1.0);
        if scale == self.scale {
            return;
        }
        if let Ok((title, body)) = faces(renderer, &self.file, scale) {
            for old in [std::mem::replace(&mut self.title, title), std::mem::replace(&mut self.body, body)] {
                renderer.delete_atlas(old.atlas);
            }
            self.scale = scale;
        }
    }

    /// Draw a box of text lines. `frame` is the framebuffer size in pixels.
    pub fn draw(&self, renderer: &Renderer, frame: (u32, u32), anchor: Anchor, lines: &[Line]) {
        if lines.is_empty() {
            return;
        }
        let (padding, margin) = (PADDING * self.scale, MARGIN * self.scale);
        let width = lines.iter().map(|l| self.face(l.tone).font.measure(&l.text)).fold(0.0, f32::max);
        let height: f32 = lines.iter().map(|l| self.face(l.tone).font.line_height()).sum();
        let (w, h) = (width + 2.0 * padding, height + 2.0 * padding);
        let (fw, fh) = (frame.0 as f32, frame.1 as f32);
        let (x, y) = match anchor {
            Anchor::Centre => ((fw - w) / 2.0, (fh - h) / 2.0),
            Anchor::TopRight => (fw - w - margin, margin),
            Anchor::BottomCentre => ((fw - w) / 2.0, fh - h - margin),
        };
        let area = Rect { x: x.round(), y: y.round(), w: w.round(), h: h.round() };

        let (mut title, mut body) = (Vec::new(), Vec::new());
        self.body.font.push_rect(&mut body, area, BACKDROP);
        let mut top = area.y + padding;
        for line in lines {
            let face = self.face(line.tone);
            // Centred boxes centre their lines; the corner panel reads as a table.
            let left = match anchor {
                Anchor::TopRight => area.x + padding,
                _ => area.x + (area.w - face.font.measure(&line.text)) / 2.0,
            };
            let out = if line.tone == Tone::Title { &mut title } else { &mut body };
            face.font.push_text(out, left, top, &line.text, TextStyle { color: colour(line.tone) });
            top += face.font.line_height();
        }
        renderer.draw_ui(self.body.atlas, &body);
        renderer.draw_ui(self.title.atlas, &title);
    }

    /// Free the atlases. The renderer must be the one they were made with.
    pub fn release(self, renderer: &Renderer) {
        renderer.delete_atlas(self.title.atlas);
        renderer.delete_atlas(self.body.atlas);
    }

    fn face(&self, tone: Tone) -> &Face {
        if tone == Tone::Title { &self.title } else { &self.body }
    }
}

fn faces(renderer: &Renderer, file: &FontFile, scale: f32) -> Result<(Face, Face)> {
    let title = Face::new(renderer, file, (TITLE_PX * scale).round())?;
    let body = Face::new(renderer, file, (BODY_PX * scale).round())?;
    Ok((title, body))
}

fn colour(tone: Tone) -> Rgba {
    match tone {
        Tone::Title => BRIGHT,
        Tone::Normal => NORMAL,
        Tone::Dim => DIM,
    }
}
