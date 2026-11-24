//! Element lookups and property setting that tolerate what a given machine's
//! GStreamer does not have.

use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::{debug, warn};

/// Is this element factory installed (and, for hardware elements, supported
/// by a device on this machine)?
pub fn exists(factory: &str) -> bool {
    gst::ElementFactory::find(factory).is_some()
}

/// Set a property from its string form if the element has it. Encoders and
/// decoders differ between vendors and GStreamer versions, so a missing knob
/// is skipped rather than treated as an error. Returns whether it was set.
pub fn set(el: &gst::Element, name: &str, value: &str) -> bool {
    let Some(pspec) = el.find_property(name) else {
        debug!("{} has no property {name}", el.name());
        return false;
    };
    if !pspec.flags().contains(gst::glib::ParamFlags::WRITABLE) {
        warn!("{}: {name} is read-only", el.name());
        return false;
    }
    // Parsing against the pspec yields the property's exact type, so the set
    // below can't hit a type mismatch.
    match gst::glib::Value::deserialize_with_pspec(value, &pspec) {
        Ok(v) => {
            el.set_property_from_value(name, &v);
            true
        }
        Err(e) => {
            warn!("{}: {name}={value}: {e}", el.name());
            false
        }
    }
}
