//! No-op stand-in for `surface_diag` on builds without the macOS surface path.
//!
//! Keeping the same API means the call sites do not need `cfg` of their own; the
//! compiler drops these bodies entirely. They are unreferenced on those builds by
//! design, hence the module-wide `allow(dead_code)`.

#![allow(dead_code)]

pub fn note_surface_frame() {}
pub fn note_software() {}
pub fn note_renderer_refused() {}
pub fn reset() {}

pub fn counts() -> (u64, u64, u64) {
    (0, 0, 0)
}

pub fn summary() -> String {
    String::from("frames_surface=0 frames_software=0 renderer_refused=0")
}
