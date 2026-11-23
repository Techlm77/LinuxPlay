//! Sharing the window's OpenGL context with GStreamer.
//!
//! Decoded frames stay on the GPU: GStreamer's GL elements run in their own
//! context that shares textures with ours, so a frame reaches the screen
//! without ever being copied through system memory. GStreamer asks for the
//! display and for a context to share with through `need-context` messages;
//! [`GlBridge::attach`] answers them for one pipeline.

use std::ptr;

use anyhow::{Context as _, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_gl as gst_gl;
use gstreamer_gl::prelude::*;
use sdl3::sys;
use sdl3::video::Window;

/// Context type GStreamer's GL elements use to ask for a context to share with.
const APP_CONTEXT_TYPE: &str = "gst.gl.app_context";

/// Which flavour of OpenGL the window's context speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlApi {
    /// Desktop OpenGL 3.3 core.
    Core,
    /// OpenGL ES 3.0.
    Es,
}

impl GlApi {
    fn gst(self) -> gst_gl::GLAPI {
        match self {
            Self::Core => gst_gl::GLAPI::OPENGL3,
            Self::Es => gst_gl::GLAPI::GLES2,
        }
    }
}

/// The window's display and context, as GStreamer sees them.
#[derive(Clone)]
pub struct GlBridge {
    display: gst_gl::GLDisplay,
    context: gst_gl::GLContext,
}

impl GlBridge {
    /// Wrap the EGL context that is current on this thread, which is the
    /// only thread the bridge's context may then be used on. `wayland_display`
    /// is the window's `wl_display` when it is a Wayland window (see
    /// [`wayland_display`]).
    pub fn wrap_current(wayland_display: Option<usize>, api: GlApi) -> Result<Self> {
        // SAFETY: plain getters; a null result is handled below.
        let (egl_display, egl_context) =
            unsafe { (sys::video::SDL_EGL_GetCurrentDisplay(), sys::video::SDL_GL_GetCurrentContext()) };
        if egl_display.is_null() || egl_context.is_null() {
            return Err(anyhow!("the window has no EGL context"));
        }

        let display = match wayland_display {
            // SAFETY: the pointer is the live wl_display the window is on.
            Some(wl) => unsafe { gstreamer_gl_wayland::GLDisplayWayland::with_display(wl) }?.upcast(),
            // SAFETY: the pointer is the live EGLDisplay of the current context.
            None => unsafe { gstreamer_gl_egl::GLDisplayEGL::with_egl_display(egl_display as usize) }?.upcast(),
        };
        // SAFETY: `egl_context` is a live EGLContext on `display`, of the API given.
        let context = unsafe {
            gst_gl::GLContext::new_wrapped(&display, egl_context as usize, gst_gl::GLPlatform::EGL, api.gst())
        }
        .context("GStreamer could not wrap the window's OpenGL context")?;
        context.activate(true).context("activating the wrapped OpenGL context")?;
        context.fill_info().context("reading the OpenGL context's capabilities")?;
        Ok(Self { display, context })
    }

    /// The window's context, for waiting on frames GStreamer rendered.
    pub fn context(&self) -> &gst_gl::GLContext {
        &self.context
    }

    /// Make `pipeline`'s GL elements use the window's display and share
    /// textures with its context.
    pub fn attach(&self, pipeline: &gst::Pipeline) {
        let bridge = self.clone();
        let bus = pipeline.bus().expect("a pipeline always has a bus");
        bus.set_sync_handler(move |_, msg| {
            if let gst::MessageView::NeedContext(need) = msg.view()
                && let Some(element) = msg.src().and_then(|s| s.downcast_ref::<gst::Element>()) {
                    bridge.answer(element, need.context_type());
                }
            gst::BusSyncReply::Pass
        });
    }

    fn answer(&self, element: &gst::Element, context_type: &str) {
        if context_type == *gst_gl::GL_DISPLAY_CONTEXT_TYPE {
            let context = gst::Context::new(context_type, true);
            context.set_gl_display(&self.display);
            element.set_context(&context);
        } else if context_type == APP_CONTEXT_TYPE {
            let mut context = gst::Context::new(context_type, true);
            let fields = context.get_mut().expect("a new context is not shared yet").structure_mut();
            fields.set("context", &self.context);
            element.set_context(&context);
        }
    }
}

/// The `wl_display` the window lives on, when running on Wayland.
pub fn wayland_display(window: &Window) -> Option<usize> {
    // SAFETY: `window.raw()` is a live window; the property is a plain pointer
    // read that yields null when the window isn't a Wayland one.
    let display = unsafe {
        let props = sys::video::SDL_GetWindowProperties(window.raw());
        sys::properties::SDL_GetPointerProperty(
            props,
            sys::video::SDL_PROP_WINDOW_WAYLAND_DISPLAY_POINTER,
            ptr::null_mut(),
        )
    };
    (!display.is_null()).then_some(display as usize)
}
