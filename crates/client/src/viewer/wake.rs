//! Waking the window's event loop from other threads.

use std::sync::atomic::{AtomicBool, Ordering};

use sdl3::EventSubsystem;
use sdl3::event::EventSender;

/// The event that carries a wake-up; it has no content of its own.
pub struct Wake;

/// Pushes at most one [`Wake`] into the event queue at a time, however many
/// threads ask, so a burst of frames can't flood the queue.
pub struct Waker {
    sender: EventSender,
    pending: AtomicBool,
}

impl Waker {
    pub fn new(events: &EventSubsystem) -> Result<Self, sdl3::Error> {
        events.register_custom_event::<Wake>()?;
        Ok(Self { sender: events.event_sender(), pending: AtomicBool::new(false) })
    }

    /// Ask the event loop to look at its queues. Callable from any thread.
    pub fn wake(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            // Fails only while SDL is shutting down, when nobody is listening.
            let _ = self.sender.push_custom_event(Wake);
        }
    }

    /// The event loop saw the wake-up. Call before draining the queues, so
    /// anything queued afterwards wakes the loop again.
    pub fn acknowledge(&self) {
        self.pending.store(false, Ordering::Release);
    }
}
