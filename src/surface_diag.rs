//! Which presentation path actually ran, counted per process.
//!
//! The surface path used to have no positive log line: a session could ask for
//! surfaces (`decoded surfaces wanted: true`) and still present nothing, and the
//! log looked the same either way. These counters are what a measurement quotes
//! instead of an absence of warnings: how many frames the renderer accepted as a
//! surface, how many it refused, and how many went out as software pixels.
//!
//! Diagnostic only -- nothing here changes behaviour, and reading them costs an
//! atomic load.

use std::sync::atomic::{AtomicU64, Ordering};

static SURFACE_ACCEPTED: AtomicU64 = AtomicU64::new(0);
static SURFACE_REFUSED: AtomicU64 = AtomicU64::new(0);
static SOFTWARE_PRESENTED: AtomicU64 = AtomicU64::new(0);

/// The renderer took a decoded surface and returned non-zero.
pub fn note_surface_accepted() {
    SURFACE_ACCEPTED.fetch_add(1, Ordering::Relaxed);
}

/// The renderer refused a decoded surface, or the session had to drop one.
pub fn note_surface_refused() {
    SURFACE_REFUSED.fetch_add(1, Ordering::Relaxed);
}

/// A frame was presented as software pixels (RGBA or a decoder without a surface).
pub fn note_software() {
    SOFTWARE_PRESENTED.fetch_add(1, Ordering::Relaxed);
}

/// `(accepted, refused, software)` since the process started.
pub fn counts() -> (u64, u64, u64) {
    (
        SURFACE_ACCEPTED.load(Ordering::Relaxed),
        SURFACE_REFUSED.load(Ordering::Relaxed),
        SOFTWARE_PRESENTED.load(Ordering::Relaxed),
    )
}

/// One line for a log message, so every call site prints the same fields.
pub fn summary() -> String {
    let (accepted, refused, software) = counts();
    format!("accepted={accepted} refused={refused} software={software}")
}
