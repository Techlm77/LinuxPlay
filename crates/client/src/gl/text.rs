//! Text for the on-screen overlay: one of the system's fonts, rasterised once
//! into an atlas, then laid out as quads.

use std::collections::HashMap;

use anyhow::{Context as _, Result, anyhow};

use super::renderer::{Rgba, Vertex, push_quad};
use crate::layout::Rect;

/// Sans-serif families to look for, most preferred first; whatever the
/// system calls "sans-serif" is the fallback.
const FAMILIES: [&str; 7] =
    ["Inter", "Noto Sans", "Adwaita Sans", "Cantarell", "DejaVu Sans", "Liberation Sans", "Ubuntu"];
/// Characters beyond printable ASCII the overlay uses.
const EXTRA_CHARS: &str = "×·…–•";
/// Stands in for a character the atlas doesn't have.
const FALLBACK_CHAR: char = '?';
/// Empty pixels around each glyph, so neighbours don't bleed into each other.
const PADDING: usize = 1;

#[derive(Debug, Clone, Copy)]
struct Glyph {
    /// Where the bitmap sits in the atlas, in texture coordinates.
    uv: [f32; 4],
    /// Bitmap size in pixels.
    size: [f32; 2],
    /// From the pen position on the baseline to the bitmap's top-left.
    offset: [f32; 2],
    advance: f32,
}

/// A coverage bitmap holding every glyph, plus one solid pixel for panels.
pub struct Atlas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// How a run of text is drawn.
#[derive(Debug, Clone, Copy)]
pub struct TextStyle {
    pub color: Rgba,
}

/// One font at one pixel size.
pub struct Font {
    atlas: Atlas,
    glyphs: HashMap<char, Glyph>,
    /// Texture coordinates of the solid pixel.
    solid: [f32; 4],
    ascent: f32,
    line_height: f32,
}

/// A font file's contents, found once and rasterised at any number of sizes.
pub struct FontFile {
    data: Vec<u8>,
    /// Which face of a font collection to use.
    index: u32,
}

impl FontFile {
    /// Find a sans-serif font on this system that can actually be drawn:
    /// the preferred families first, then anything else installed.
    pub fn system() -> Result<Self> {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let preferred = FAMILIES
            .iter()
            .map(|name| fontdb::Family::Name(name))
            .chain([fontdb::Family::SansSerif])
            .filter_map(|family| db.query(&fontdb::Query { families: &[family], ..Default::default() }));
        let any = db.faces().map(|face| face.id);
        preferred
            .chain(any)
            .filter_map(|id| db.with_face_data(id, |data, index| Self { data: data.to_vec(), index }))
            .find(Self::draws_text)
            .context("no usable font is installed")
    }

    /// Does this font produce visible letters? Some formats load and report
    /// sizes yet rasterise to nothing, and symbol fonts have no letters.
    fn draws_text(&self) -> bool {
        const PROBE_PX: f32 = 16.0;
        let settings = fontdue::FontSettings { collection_index: self.index, scale: PROBE_PX, ..Default::default() };
        let Ok(font) = fontdue::Font::from_bytes(self.data.as_slice(), settings) else { return false };
        "Ag0".chars().all(|c| font.lookup_glyph_index(c) != 0 && font.rasterize(c, PROBE_PX).1.iter().any(|v| *v > 0))
    }
}

impl Font {
    /// Rasterise `file` at `px` pixels.
    pub fn new(file: &FontFile, px: f32) -> Result<Self> {
        let (data, index) = (file.data.as_slice(), file.index);
        let settings = fontdue::FontSettings { collection_index: index, scale: px, ..Default::default() };
        let font = fontdue::Font::from_bytes(data, settings).map_err(|e| anyhow!("unusable font: {e}"))?;
        let metrics = font.horizontal_line_metrics(px).context("the font has no horizontal metrics")?;

        let chars: Vec<char> = (' '..='~').chain(EXTRA_CHARS.chars()).collect();
        let rasters: Vec<(char, fontdue::Metrics, Vec<u8>)> = chars
            .into_iter()
            .filter(|c| *c == ' ' || font.lookup_glyph_index(*c) != 0)
            .map(|c| {
                let (m, bitmap) = font.rasterize(c, px);
                (c, m, bitmap)
            })
            .collect();

        let mut packer = Packer::new(rasters.iter().map(|(_, m, _)| (m.width, m.height)));
        let solid = packer.place(1, 1, &[255]);
        let placed: Vec<PixelRect> = rasters.iter().map(|(_, m, bitmap)| packer.place(m.width, m.height, bitmap)).collect();
        let atlas = packer.finish();

        let glyphs = rasters
            .iter()
            .zip(placed)
            .map(|((c, m, _), rect)| {
                let glyph = Glyph {
                    uv: atlas.uv(rect),
                    size: [m.width as f32, m.height as f32],
                    offset: [m.xmin as f32, -(m.ymin as f32 + m.height as f32)],
                    advance: m.advance_width,
                };
                (*c, glyph)
            })
            .collect();
        // Sample the middle of the solid pixel so filtering can't reach its
        // transparent neighbours.
        let [u0, v0, u1, v1] = atlas.uv(solid);
        let centre = [(u0 + u1) / 2.0, (v0 + v1) / 2.0];
        Ok(Self {
            atlas,
            glyphs,
            solid: [centre[0], centre[1], centre[0], centre[1]],
            ascent: metrics.ascent.ceil(),
            line_height: metrics.new_line_size.ceil(),
        })
    }

    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// Distance between the tops of consecutive lines, in pixels.
    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    /// Width of `text` on one line, in pixels.
    pub fn measure(&self, text: &str) -> f32 {
        text.chars().filter_map(|c| self.glyph(c)).map(|g| g.advance).sum()
    }

    /// Append `text` with the top of its line at `(x, y)`.
    pub fn push_text(&self, out: &mut Vec<Vertex>, x: f32, y: f32, text: &str, style: TextStyle) {
        let baseline = (y + self.ascent).round();
        let mut pen = x.round();
        for g in text.chars().filter_map(|c| self.glyph(c)) {
            if g.size[0] > 0.0 {
                let rect = Rect { x: pen + g.offset[0], y: baseline + g.offset[1], w: g.size[0], h: g.size[1] };
                push_quad(out, rect, g.uv, style.color);
            }
            pen += g.advance;
        }
    }

    /// Append a filled rectangle.
    pub fn push_rect(&self, out: &mut Vec<Vertex>, rect: Rect, color: Rgba) {
        push_quad(out, rect, self.solid, color);
    }

    fn glyph(&self, c: char) -> Option<&Glyph> {
        self.glyphs.get(&c).or_else(|| self.glyphs.get(&FALLBACK_CHAR))
    }
}

/// A bitmap's place in the atlas, in pixels: left, top, right, bottom.
type PixelRect = [usize; 4];

/// Packs bitmaps into rows of a fixed-width atlas that grows downwards.
struct Packer {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
    cursor: (usize, usize),
    row_height: usize,
}

impl Packer {
    /// Choose a width so bitmaps of these sizes end up roughly square
    /// overall, and never narrower than the widest of them.
    fn new(sizes: impl Iterator<Item = (usize, usize)> + Clone) -> Self {
        let area: usize = sizes.clone().map(|(w, h)| (w + PADDING) * (h + PADDING)).sum();
        let widest = sizes.map(|(w, _)| w).max().unwrap_or(0) + 2 * PADDING;
        let width = ((area as f64).sqrt().ceil() as usize + PADDING).max(widest).next_power_of_two();
        Self { width, height: 0, pixels: Vec::new(), cursor: (PADDING, PADDING), row_height: 0 }
    }

    /// Copy a `w`×`h` bitmap in and say where it went.
    fn place(&mut self, w: usize, h: usize, bitmap: &[u8]) -> PixelRect {
        if self.cursor.0 + w + PADDING > self.width {
            self.cursor = (PADDING, self.cursor.1 + self.row_height + PADDING);
            self.row_height = 0;
        }
        let (x, y) = self.cursor;
        let needed = y + h + PADDING;
        if needed > self.height {
            self.height = needed;
            self.pixels.resize(self.width * self.height, 0);
        }
        for row in 0..h {
            let dst = (y + row) * self.width + x;
            self.pixels[dst..dst + w].copy_from_slice(&bitmap[row * w..(row + 1) * w]);
        }
        self.cursor.0 += w + PADDING;
        self.row_height = self.row_height.max(h);
        [x, y, x + w, y + h]
    }

    fn finish(self) -> Atlas {
        Atlas { width: self.width as u32, height: self.height as u32, pixels: self.pixels }
    }
}

impl Atlas {
    /// Texture coordinates of a pixel rectangle.
    fn uv(&self, rect: PixelRect) -> [f32; 4] {
        let (w, h) = (self.width as f32, self.height as f32);
        [rect[0] as f32 / w, rect[1] as f32 / h, rect[2] as f32 / w, rect[3] as f32 / h]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_bitmaps_keep_their_pixels_and_never_overlap() {
        let sizes = [(5usize, 7usize), (3, 3), (9, 2), (1, 1), (6, 6), (9, 9), (2, 8)];
        let mut packer = Packer::new(sizes.iter().copied());
        let rects: Vec<PixelRect> = sizes
            .iter()
            .enumerate()
            .map(|(i, (w, h))| packer.place(*w, *h, &vec![i as u8 + 1; w * h]))
            .collect();
        let atlas = packer.finish();
        assert_eq!(atlas.pixels.len(), (atlas.width * atlas.height) as usize);

        let mut covered = 0;
        for (i, r) in rects.iter().enumerate() {
            for y in r[1]..r[3] {
                for x in r[0]..r[2] {
                    assert_eq!(atlas.pixels[y * atlas.width as usize + x], i as u8 + 1, "bitmap {i} was overwritten");
                    covered += 1;
                }
            }
        }
        // Everything else is padding: no stray pixels.
        assert_eq!(atlas.pixels.iter().filter(|p| **p != 0).count(), covered);
    }

    /// Needs a font on the machine; machines without one have no overlay.
    #[test]
    fn the_system_font_found_draws_visible_letters() {
        let Ok(file) = FontFile::system() else { return };
        assert!(file.draws_text());
        let font = Font::new(&file, 16.0).unwrap();
        let atlas = font.atlas();
        // More than the one solid pixel: real glyph coverage made it in.
        assert!(atlas.pixels.iter().filter(|p| **p > 0).count() > 500);
    }

    #[test]
    fn a_system_font_lays_out_text() {
        let Ok(font) = FontFile::system().and_then(|file| Font::new(&file, 16.0)) else { return };
        assert!(font.line_height() >= 16.0);
        let width = font.measure("LinuxPlay 60 fps");
        assert!(width > 40.0 && width < 400.0);
        assert!(font.measure("mm") > font.measure("ii"));

        let mut out = Vec::new();
        font.push_text(&mut out, 10.0, 10.0, "Hi there", TextStyle { color: [1.0; 4] });
        // Seven visible glyphs, six vertices each; the space draws nothing.
        assert_eq!(out.len(), 7 * 6);
        assert!(out.iter().all(|v| v.pos[0] >= 9.0 && v.pos[1] >= 5.0));
        assert!(out.iter().all(|v| v.uv.iter().all(|c| (0.0..=1.0).contains(c))));
        // A character outside the atlas falls back instead of vanishing.
        assert!(font.measure("\u{4e2d}") > 0.0);
    }
}
