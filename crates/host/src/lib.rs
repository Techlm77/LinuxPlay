//! The LinuxPlay host: shares this computer's screen with clients that have
//! been paired or invited, and lets the person at it decide who gets in.
//!
//! [`Host`] is the whole of it; the `linuxplay-host` program and the
//! `linuxplay` engine both run one.

mod approval;
mod audio;
mod capture;
mod clipboard;
pub mod control;
mod input;
mod invites;
mod limits;
mod monitor;
pub mod netif;
mod notify;
pub mod password;
pub mod portal;
mod quality;
mod server;
mod sessions;
mod tray;
mod trust;
pub mod video;

#[cfg(test)]
mod tests;

pub use approval::Decision;
pub use capture::{CaptureKind, TestPattern, TestPicture};
pub use server::{Host, HostConfig};
pub use trust::TrustStore;
