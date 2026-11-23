//! The window and its OpenGL context.
//!
//! The window belongs to the main thread, which handles its events. Its
//! OpenGL context is handed to the render thread as a [`RenderTarget`], so
//! drawing never waits for event handling or the other way round.

use std::ffi::{CStr, c_void};

use anyhow::{Result, anyhow};
use sdl3::sys;
use sdl3::video::{GLContext, GLProfile, Window};
use sdl3::{Sdl, VideoSubsystem};
use tracing::{debug, info};

use crate::gl::{self, GlApi};

/// What desktops group and label the window by.
const APP_ID: &str = "io.github.techlm77.LinuxPlay";
/// The window never takes more than this share of the screen on its own.
const MAX_SCREEN_SHARE: f32 = 0.85;

pub struct GlWindow {
    // Dropped in this order: the context goes before the window it draws to.
    _context: GLContext,
    pub window: Window,
    pub video: VideoSubsystem,
    pub sdl: Sdl,
    pub api: GlApi,
}

impl GlWindow {
    /// Open a window with an OpenGL context current on this thread: desktop
    /// OpenGL where available, OpenGL ES otherwise.
    pub fn open(title: &str, size: (u32, u32)) -> Result<Self> {
        // GStreamer shares our context through EGL, so use EGL on X11 too.
        sdl3::hint::set("SDL_VIDEO_FORCE_EGL", "1");
        sdl3::hint::set("SDL_APP_ID", APP_ID);
        // While the keyboard is grabbed, Alt+Tab belongs to the host.
        sdl3::hint::set("SDL_ALLOW_ALT_TAB_WHILE_GRABBED", "0");
        sdl3::hint::set("SDL_VIDEO_MINIMIZE_ON_FOCUS_LOSS", "0");

        let sdl = sdl3::init().map_err(|e| anyhow!("starting SDL: {e}"))?;
        let video = sdl.video().map_err(|e| anyhow!("no display to open a window on: {e}"))?;
        info!("video driver: {}", video.current_video_driver());

        let mut last_error = anyhow!("no OpenGL context could be created");
        for api in [GlApi::Core, GlApi::Es] {
            match create(&video, title, size, api) {
                Ok((window, context)) => return Ok(Self { _context: context, window, video, sdl, api }),
                Err(e) => last_error = e,
            }
        }
        Err(explain(last_error))
    }

    /// Hand the OpenGL context to another thread. The context is current on
    /// the calling thread when the window is opened and must be current
    /// nowhere while it changes hands, so this releases it here.
    ///
    /// The target refers to this window: it must be dropped (and its thread
    /// done) before the window is.
    pub fn render_target(&self) -> Result<RenderTarget> {
        // SAFETY: plain getter and a release of this thread's own context.
        let context = unsafe { sys::video::SDL_GL_GetCurrentContext() };
        if context.is_null() {
            return Err(anyhow!("the OpenGL context is not on this thread to hand over"));
        }
        if !unsafe { sys::video::SDL_GL_MakeCurrent(self.window.raw(), std::ptr::null_mut()) } {
            return Err(anyhow!("releasing the OpenGL context: {}", sdl3::get_error()));
        }
        Ok(RenderTarget {
            window: self.window.raw(),
            context,
            api: self.api,
            wayland_display: gl::wayland_display(&self.window),
        })
    }

    /// The refresh rate of the display the window is on, in Hz.
    pub fn refresh_rate(&self) -> Option<u32> {
        let mode = self.window.get_display().ok()?.get_mode().ok()?;
        (mode.refresh_rate >= 1.0).then_some(mode.refresh_rate.round() as u32)
    }

    /// Framebuffer pixels per window coordinate.
    pub fn pixel_scale(&self) -> (f32, f32) {
        let (w, h) = self.window.size();
        let (pw, ph) = self.window.size_in_pixels();
        (pw as f32 / w.max(1) as f32, ph as f32 / h.max(1) as f32)
    }

    /// Resize the window to show a `video`-sized picture at full size, or as
    /// large as comfortably fits the screen.
    pub fn fit_to(&mut self, video: (u32, u32)) {
        let Some(screen) = self.window.get_display().ok().and_then(|d| d.get_usable_bounds().ok()) else { return };
        let density = self.window.display_scale().max(1.0);
        let (vw, vh) = (video.0 as f32 / density, video.1 as f32 / density);
        let limit = (screen.width() as f32 * MAX_SCREEN_SHARE, screen.height() as f32 * MAX_SCREEN_SHARE);
        let shrink = (limit.0 / vw).min(limit.1 / vh).min(1.0);
        let size = ((vw * shrink).round() as u32, (vh * shrink).round() as u32);
        if let Err(e) = self.window.set_size(size.0.max(1), size.1.max(1)) {
            debug!("couldn't resize the window: {e}");
        }
    }
}

fn create(video: &VideoSubsystem, title: &str, size: (u32, u32), api: GlApi) -> Result<(Window, GLContext)> {
    let attr = video.gl_attr();
    match api {
        GlApi::Core => {
            attr.set_context_profile(GLProfile::Core);
            attr.set_context_version(3, 3);
        }
        GlApi::Es => {
            attr.set_context_profile(GLProfile::GLES);
            attr.set_context_version(3, 0);
        }
    }
    // The picture is drawn as-is: no depth, stencil or multisampling needed.
    attr.set_depth_size(0);
    attr.set_stencil_size(0);
    let window = video
        .window(title, size.0, size.1)
        .opengl()
        .resizable()
        .high_pixel_density()
        .build()
        .map_err(|e| anyhow!("opening the window: {e}"))?;
    let context = window.gl_create_context().map_err(|e| anyhow!("creating an OpenGL context ({api:?}): {e}"))?;
    Ok((window, context))
}

/// A window's OpenGL context, for the one thread that draws to it.
pub struct RenderTarget {
    window: *mut sys::video::SDL_Window,
    context: sys::video::SDL_GLContext,
    pub api: GlApi,
    /// The window's `wl_display`, when it is a Wayland window.
    pub wayland_display: Option<usize>,
}

// SAFETY: the pointers are only used through the SDL calls below, which SDL
// allows from any thread as long as the context is current on one thread at
// a time; `GlWindow::render_target` documents that the window outlives this.
unsafe impl Send for RenderTarget {}

impl RenderTarget {
    /// Make the context current on the calling thread, with vsync off: the
    /// newest frame should replace an older one that hasn't been shown yet,
    /// never queue behind it.
    pub fn make_current(&self) -> Result<()> {
        // SAFETY: the window and context are alive (see the `Send` note).
        if !unsafe { sys::video::SDL_GL_MakeCurrent(self.window, self.context) } {
            return Err(anyhow!("taking the OpenGL context: {}", sdl3::get_error()));
        }
        if !unsafe { sys::video::SDL_GL_SetSwapInterval(0) } {
            debug!("couldn't turn vsync off: {}", sdl3::get_error());
        }
        Ok(())
    }

    /// Give the context up again, so the window can destroy it.
    pub fn release(&self) {
        // SAFETY: as above; releasing a context is always allowed.
        unsafe { sys::video::SDL_GL_MakeCurrent(self.window, std::ptr::null_mut()) };
    }

    /// Show what was drawn.
    pub fn swap(&self) {
        // SAFETY: as above, with the context current on this thread.
        unsafe { sys::video::SDL_GL_SwapWindow(self.window) };
    }

    /// The address of an OpenGL function, for the renderer.
    pub fn load(&self, name: &CStr) -> *const c_void {
        // SAFETY: `name` is a valid C string; the context is current.
        match unsafe { sys::video::SDL_GL_GetProcAddress(name.as_ptr()) } {
            Some(function) => function as *const c_void,
            None => std::ptr::null(),
        }
    }
}

/// Turn a failure to get an OpenGL context into advice where we know the
/// cause: a Flatpak whose graphics drivers are missing.
fn explain(error: anyhow::Error) -> anyhow::Error {
    let in_flatpak = std::env::var_os("FLATPAK_ID").is_some();
    let drivers = std::path::Path::new("/usr/lib/x86_64-linux-gnu/GL/default");
    if in_flatpak && !drivers.exists() {
        return error.context(
            "parts of LinuxPlay's Flatpak runtime are missing. Install them with: \
             flatpak install flathub org.freedesktop.Platform.GL.default//26.08 \
             org.freedesktop.Platform.codecs-extra//26.08-extra",
        );
    }
    error
}
