//! Drawing: the video frame as one textured quad, and text and panels as a
//! batch of quads over it.

use std::num::NonZeroU32;

use anyhow::{Result, anyhow};
use glow::HasContext;

use super::GlApi;
use crate::layout::Rect;

pub type Rgba = [f32; 4];

/// One corner of a quad, in framebuffer pixels from the top left.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: Rgba,
}

/// Append the two triangles of an axis-aligned quad.
pub fn push_quad(out: &mut Vec<Vertex>, rect: Rect, uv: [f32; 4], color: Rgba) {
    let (x0, y0, x1, y1) = (rect.x, rect.y, rect.x + rect.w, rect.y + rect.h);
    let [u0, v0, u1, v1] = uv;
    let v = |x, y, u, v| Vertex { pos: [x, y], uv: [u, v], color };
    out.extend([v(x0, y0, u0, v0), v(x1, y0, u1, v0), v(x0, y1, u0, v1), v(x1, y0, u1, v0), v(x1, y1, u1, v1), v(x0, y1, u0, v1)]);
}

const VERTEX_SHADER: &str = r"
uniform vec2 u_size;
in vec2 a_pos;
in vec2 a_uv;
in vec4 a_color;
out vec2 v_uv;
out vec4 v_color;
void main() {
    v_uv = a_uv;
    v_color = a_color;
    vec2 clip = a_pos / u_size * 2.0 - 1.0;
    gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
}
";

/// The frame as GStreamer delivers it: RGBA, first row at the top.
const VIDEO_SHADER: &str = r"
uniform sampler2D u_tex;
in vec2 v_uv;
in vec4 v_color;
out vec4 o_color;
void main() {
    o_color = vec4(texture(u_tex, v_uv).rgb, 1.0);
}
";

/// Text and panels: the atlas holds coverage, the vertex holds the colour.
const UI_SHADER: &str = r"
uniform sampler2D u_tex;
in vec2 v_uv;
in vec4 v_color;
out vec4 o_color;
void main() {
    o_color = vec4(v_color.rgb, v_color.a * texture(u_tex, v_uv).r);
}
";

struct Program {
    id: glow::Program,
    size: Option<glow::UniformLocation>,
}

pub struct Renderer {
    gl: glow::Context,
    video: Program,
    ui: Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    size: (u32, u32),
}

impl Renderer {
    /// Set up drawing on the OpenGL context current on this thread. `loader`
    /// resolves OpenGL function names to addresses.
    pub fn new(api: GlApi, loader: impl FnMut(&std::ffi::CStr) -> *const std::ffi::c_void) -> Result<Self> {
        // SAFETY: the loader resolves functions of the context current on
        // this thread, which every later call in this type runs on.
        let gl = unsafe { glow::Context::from_loader_function_cstr(loader) };
        // SAFETY (for every GL call in this type): the context is current on
        // this thread and all objects passed in were created by it.
        unsafe {
            let video = link(&gl, api, VIDEO_SHADER)?;
            let ui = link(&gl, api, UI_SHADER)?;
            let vao = gl.create_vertex_array().map_err(|e| anyhow!("creating a vertex array: {e}"))?;
            let vbo = gl.create_buffer().map_err(|e| anyhow!("creating a vertex buffer: {e}"))?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            let stride = size_of::<Vertex>() as i32;
            // Attribute locations are bound in `link`: 0 pos, 1 uv, 2 colour.
            for (index, components, offset) in [(0, 2, 0), (1, 2, 8), (2, 4, 16)] {
                gl.enable_vertex_attrib_array(index);
                gl.vertex_attrib_pointer_f32(index, components, glow::FLOAT, false, stride, offset);
            }
            gl.disable(glow::DEPTH_TEST);
            gl.blend_func_separate(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA, glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            Ok(Self { gl, video, ui, vao, vbo, size: (1, 1) })
        }
    }

    /// Start a frame: a black framebuffer of this size in pixels.
    pub fn begin(&mut self, size: (u32, u32)) {
        self.size = (size.0.max(1), size.1.max(1));
        unsafe {
            self.gl.viewport(0, 0, self.size.0 as i32, self.size.1 as i32);
            self.gl.clear_color(0.0, 0.0, 0.0, 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
    }

    /// Draw a video frame into `rect`. `texture` is the OpenGL name of an
    /// RGBA texture owned by GStreamer; `coverage` is how much of it, from
    /// the top-left corner, is picture.
    pub fn draw_video(&self, texture: u32, coverage: (f32, f32), rect: Rect) {
        let Some(texture) = NonZeroU32::new(texture).map(glow::NativeTexture) else { return };
        let mut quad = Vec::with_capacity(6);
        push_quad(&mut quad, rect, [0.0, 0.0, coverage.0, coverage.1], [1.0; 4]);
        unsafe {
            self.gl.disable(glow::BLEND);
            self.draw(&self.video, texture, glow::LINEAR, &quad);
        }
    }

    /// Draw text and panels built against `atlas`.
    pub fn draw_ui(&self, atlas: glow::Texture, vertices: &[Vertex]) {
        if vertices.is_empty() {
            return;
        }
        unsafe {
            self.gl.enable(glow::BLEND);
            self.draw(&self.ui, atlas, glow::NEAREST, vertices);
        }
    }

    /// Upload a one-channel coverage atlas for [`draw_ui`](Self::draw_ui).
    pub fn create_atlas(&self, width: u32, height: u32, pixels: &[u8]) -> Result<glow::Texture> {
        assert_eq!(pixels.len(), (width * height) as usize, "atlas size and pixel count differ");
        unsafe {
            let texture = self.gl.create_texture().map_err(|e| anyhow!("creating the text atlas: {e}"))?;
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::R8 as i32,
                width as i32,
                height as i32,
                0,
                glow::RED,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(pixels)),
            );
            for wrap in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
                self.gl.tex_parameter_i32(glow::TEXTURE_2D, wrap, glow::CLAMP_TO_EDGE as i32);
            }
            Ok(texture)
        }
    }

    pub fn delete_atlas(&self, atlas: glow::Texture) {
        unsafe { self.gl.delete_texture(atlas) };
    }

    unsafe fn draw(&self, program: &Program, texture: glow::Texture, filter: u32, vertices: &[Vertex]) {
        let gl = &self.gl;
        // SAFETY: `Vertex` is `repr(C)` and made only of `f32`s, so it has no
        // padding and any byte view of it is valid.
        let bytes = unsafe { std::slice::from_raw_parts(vertices.as_ptr().cast::<u8>(), size_of_val(vertices)) };
        unsafe {
            gl.use_program(Some(program.id));
            gl.uniform_2_f32(program.size.as_ref(), self.size.0 as f32, self.size.1 as f32);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            for parameter in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, parameter, filter as i32);
            }
            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STREAM_DRAW);
            gl.draw_arrays(glow::TRIANGLES, 0, vertices.len() as i32);
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_program(self.video.id);
            self.gl.delete_program(self.ui.id);
            self.gl.delete_vertex_array(self.vao);
            self.gl.delete_buffer(self.vbo);
        }
    }
}

fn shader_header(api: GlApi) -> &'static str {
    match api {
        GlApi::Core => "#version 330 core\n",
        GlApi::Es => "#version 300 es\nprecision mediump float;\n",
    }
}

unsafe fn compile(gl: &glow::Context, api: GlApi, kind: u32, source: &str) -> Result<glow::Shader> {
    unsafe {
        let shader = gl.create_shader(kind).map_err(|e| anyhow!("creating a shader: {e}"))?;
        gl.shader_source(shader, &format!("{}{source}", shader_header(api)));
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            let log = gl.get_shader_info_log(shader);
            gl.delete_shader(shader);
            return Err(anyhow!("a shader failed to compile: {log}"));
        }
        Ok(shader)
    }
}

unsafe fn link(gl: &glow::Context, api: GlApi, fragment_source: &str) -> Result<Program> {
    unsafe {
        let vertex = compile(gl, api, glow::VERTEX_SHADER, VERTEX_SHADER)?;
        let fragment = compile(gl, api, glow::FRAGMENT_SHADER, fragment_source)?;
        let id = gl.create_program().map_err(|e| anyhow!("creating a shader program: {e}"))?;
        gl.attach_shader(id, vertex);
        gl.attach_shader(id, fragment);
        for (index, name) in [(0, "a_pos"), (1, "a_uv"), (2, "a_color")] {
            gl.bind_attrib_location(id, index, name);
        }
        gl.link_program(id);
        gl.delete_shader(vertex);
        gl.delete_shader(fragment);
        if !gl.get_program_link_status(id) {
            let log = gl.get_program_info_log(id);
            gl.delete_program(id);
            return Err(anyhow!("a shader program failed to link: {log}"));
        }
        gl.use_program(Some(id));
        gl.uniform_1_i32(gl.get_uniform_location(id, "u_tex").as_ref(), 0);
        Ok(Program { id, size: gl.get_uniform_location(id, "u_size") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quad_is_two_triangles_covering_the_rectangle() {
        let mut out = Vec::new();
        push_quad(&mut out, Rect { x: 10.0, y: 20.0, w: 30.0, h: 40.0 }, [0.0, 0.0, 1.0, 1.0], [1.0; 4]);
        assert_eq!(out.len(), 6);
        let xs: Vec<f32> = out.iter().map(|v| v.pos[0]).collect();
        let ys: Vec<f32> = out.iter().map(|v| v.pos[1]).collect();
        assert_eq!(xs.iter().cloned().fold(f32::MAX, f32::min), 10.0);
        assert_eq!(xs.iter().cloned().fold(f32::MIN, f32::max), 40.0);
        assert_eq!(ys.iter().cloned().fold(f32::MIN, f32::max), 60.0);
        // Top-left corner carries the top-left texture coordinate.
        assert_eq!(out[0], Vertex { pos: [10.0, 20.0], uv: [0.0, 0.0], color: [1.0; 4] });
    }

    #[test]
    fn the_vertex_layout_matches_the_attribute_offsets() {
        assert_eq!(size_of::<Vertex>(), 32);
        assert_eq!(std::mem::offset_of!(Vertex, uv), 8);
        assert_eq!(std::mem::offset_of!(Vertex, color), 16);
    }
}
