//! What the host's encoding side and the client's decoding side share: the
//! clock both measure latency against, how each codec looks on the wire, and
//! small helpers for building GStreamer pipelines that differ between
//! machines and GStreamer versions.

pub mod bus;
pub mod clock;
pub mod codec;
pub mod element;
