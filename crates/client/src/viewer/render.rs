//! The render thread: draws the newest frame the moment it is decoded.
//!
//! Drawing has a thread of its own so that nothing else can hold a frame up:
//! the decoder wakes it directly, it draws and hands the frame to the
//! display, and goes back to waiting. The main thread only tells it what else
//! is on screen (the [`Scene`]) and which stream to show.

use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use tracing::warn;

use super::hud::{Anchor, Hud, Line};
use super::window::RenderTarget;
use crate::decode::{Frame, FrameSource};
use crate::gl::{GlBridge, Renderer};
use crate::latency::LatencyTracker;
use crate::layout;

/// While frames are flowing, a change to the scene waits for the next frame
/// to be drawn with it, rather than cost a second presentation in the same
/// refresh. If no frame comes within this time, it is drawn on its own.
const SCENE_PATIENCE: Duration = Duration::from_millis(40);
/// Frames count as flowing if the last one was no longer ago than this.
const FLOWING: Duration = Duration::from_millis(100);

/// Everything on screen besides the picture.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scene {
    /// Size of the window's framebuffer in pixels.
    pub frame_size: (u32, u32),
    /// Framebuffer pixels per unscaled pixel; text is sized by it.
    pub scale: f32,
    /// The box in the middle of the window; empty for none.
    pub notice: Vec<Line>,
    /// The statistics panel; empty for none.
    pub stats: Vec<Line>,
    /// Lines along the bottom edge; empty for none.
    pub hint: Vec<Line>,
}

/// A stream's picture: where its frames appear and who times them.
#[derive(Clone)]
pub struct Video {
    pub frames: FrameSource,
    pub tracker: Arc<LatencyTracker>,
}

struct State {
    scene: Arc<Scene>,
    video: Option<Video>,
    /// A newly decoded frame is waiting.
    frame_ready: bool,
    /// The scene or the stream changed at this moment and hasn't been drawn.
    stale_since: Option<Instant>,
    quit: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn update(&self, change: impl FnOnce(&mut State)) {
        let mut state = self.state.lock().unwrap();
        change(&mut state);
        drop(state);
        self.wake.notify_one();
    }

    /// Wait until there is something to draw and return what to draw, or
    /// `None` when the thread should stop. A new frame is drawn at once; a
    /// change to the scene alone waits a moment for a frame while frames are
    /// `flowing`.
    fn next_draw(&self, flowing: bool) -> Option<(Arc<Scene>, Option<Video>)> {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.quit {
                return None;
            }
            let stale_for = state.stale_since.map(|since| since.elapsed());
            let patience = if flowing && state.video.is_some() { SCENE_PATIENCE } else { Duration::ZERO };
            if state.frame_ready || stale_for.is_some_and(|d| d >= patience) {
                state.frame_ready = false;
                state.stale_since = None;
                return Some((state.scene.clone(), state.video.clone()));
            }
            state = match stale_for {
                Some(waited) => self.wake.wait_timeout(state, patience - waited).unwrap().0,
                None => self.wake.wait(state).unwrap(),
            };
        }
    }

    /// The scene or stream changed: redraw (soon).
    fn change(&self, change: impl FnOnce(&mut State)) {
        self.update(|state| {
            change(state);
            state.stale_since.get_or_insert_with(Instant::now);
        });
    }
}

/// Owns the render thread; dropping it stops the thread and waits for it.
pub struct RenderThread {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl RenderThread {
    /// Start drawing to `target`. Returns once the thread has set up OpenGL,
    /// with the bridge decoders need to deliver frames to it.
    pub fn start(target: RenderTarget, scene: Scene) -> Result<(Self, GlBridge)> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                scene: Arc::new(scene),
                video: None,
                frame_ready: false,
                stale_since: Some(Instant::now()),
                quit: false,
            }),
            wake: Condvar::new(),
        });
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("linuxplay-render".into())
            .spawn({
                let shared = shared.clone();
                move || run(target, &shared, ready_tx)
            })
            .context("starting the render thread")?;
        let this = Self { shared, thread: Some(thread) };
        let bridge = ready_rx.recv().map_err(|_| anyhow!("the render thread stopped while starting"))??;
        Ok((this, bridge))
    }

    /// Replace what is drawn besides the picture.
    pub fn set_scene(&self, scene: Scene) {
        self.shared.change(|state| state.scene = Arc::new(scene));
    }

    /// Show this stream's picture, or none.
    pub fn set_video(&self, video: Option<Video>) {
        self.shared.change(|state| state.video = video);
    }

    /// A callback, for any thread, that says a new frame is waiting.
    pub fn frame_notifier(&self) -> Arc<dyn Fn() + Send + Sync> {
        let shared = self.shared.clone();
        Arc::new(move || shared.update(|state| state.frame_ready = true))
    }
}

impl Drop for RenderThread {
    fn drop(&mut self) {
        self.shared.update(|state| state.quit = true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the thread draws with. Dropped on the thread, context still current.
struct Gpu {
    renderer: Renderer,
    bridge: GlBridge,
    hud: Option<Hud>,
    /// Loading the fonts failed once; don't try again for every frame.
    hud_failed: bool,
}

impl Gpu {
    fn new(target: &RenderTarget) -> Result<Self> {
        target.make_current()?;
        let renderer = Renderer::new(target.api, |name| target.load(name))?;
        let bridge = GlBridge::wrap_current(target.wayland_display, target.api)?;
        Ok(Self { renderer, bridge, hud: None, hud_failed: false })
    }

    /// Have the text overlay ready at `scale`, loading its fonts on first use.
    fn prepare_hud(&mut self, scale: f32) {
        match &mut self.hud {
            Some(hud) => hud.rescale(&self.renderer, scale),
            None if self.hud_failed => {}
            None => match Hud::new(&self.renderer, scale) {
                Ok(hud) => self.hud = Some(hud),
                Err(e) => {
                    warn!("no text can be shown in the window: {e:#}");
                    self.hud_failed = true;
                }
            },
        }
    }

    fn draw(&mut self, scene: &Scene, frame: Option<&Frame>) {
        self.renderer.begin(scene.frame_size);
        if let Some(frame) = frame
            && let Some(rect) = layout::letterbox(scene.frame_size, frame.size()) {
                self.renderer.draw_video(frame.texture(), frame.coverage(), rect);
            }
        if scene.notice.is_empty() && scene.stats.is_empty() && scene.hint.is_empty() {
            return;
        }
        self.prepare_hud(scene.scale);
        let Some(hud) = &self.hud else { return };
        hud.draw(&self.renderer, scene.frame_size, Anchor::Centre, &scene.notice);
        hud.draw(&self.renderer, scene.frame_size, Anchor::TopRight, &scene.stats);
        hud.draw(&self.renderer, scene.frame_size, Anchor::BottomCentre, &scene.hint);
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        if let Some(hud) = self.hud.take() {
            hud.release(&self.renderer);
        }
    }
}

fn run(target: RenderTarget, shared: &Shared, ready: mpsc::Sender<Result<GlBridge>>) {
    let mut gpu = match Gpu::new(&target) {
        Ok(gpu) => gpu,
        Err(e) => {
            target.release();
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(gpu.bridge.clone()));

    // The frame on screen; it keeps its texture alive until replaced.
    let mut shown: Option<Frame> = None;
    let mut last_frame_at: Option<Instant> = None;
    loop {
        let flowing = last_frame_at.is_some_and(|t| t.elapsed() < FLOWING);
        let Some((scene, video)) = shared.next_draw(flowing) else { break };

        let newest = video.as_ref().and_then(|v| v.frames.take(&gpu.bridge));
        let is_new = newest.is_some();
        match (&video, newest) {
            (None, _) => shown = None,
            (Some(_), Some(frame)) => shown = Some(frame),
            (Some(_), None) => {}
        }
        if scene.frame_size.0 == 0 || scene.frame_size.1 == 0 {
            // The window's size isn't known yet.
            continue;
        }
        gpu.draw(&scene, shown.as_ref());
        target.swap();
        if let (true, Some(video), Some(frame)) = (is_new, &video, &shown) {
            video.tracker.presented(frame.rtp());
            last_frame_at = Some(Instant::now());
        }
    }

    // Everything that lives in the context goes before the context does.
    drop(shown);
    drop(gpu);
    target.release();
}
