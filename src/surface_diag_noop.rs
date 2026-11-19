//! No-op stand-in for `surface_diag` on builds without the macOS surface path.
//!
//! Keeping the same API means the call sites do not need `cfg` of their own; the
//! compiler drops these bodies entirely.

pub fn note_surface_accepted() {}
pub fn note_surface_refused() {}
pub fn note_software() {}

pub fn counts() -> (u64, u64, u64) {
    (0, 0, 0)
}

pub fn summary() -> String {
    String::from("accepted=0 refused=0 software=0")
}
