//! Which presentation path each frame took, counted per process.
//!
//! The surface path had no positive log line: a session could ask for surfaces
//! (`decoded surfaces wanted: true`) and still present nothing, and the log
//! looked the same either way. These counters are what a measurement quotes
//! instead of an absence of warnings.
//!
//! Two bases, and the field names say which is which:
//!
//! * `frames_surface` / `frames_software` are counted on the video thread, once
//!   per decoded frame, so they add up to the frames the session handed to the renderer; a refused frame still counts as handed over.
//! * `renderer_refused` is counted per presentation call, once per UI window, so
//!   with two windows open it can exceed `frames_surface`. Do not add it to the
//!   other two; read it as "how often the renderer said no".
//!
//! Counters are process-wide and cleared at the start of every UI session, so a
//! measurement that opens one session gets that session's numbers. Two connections in one process share them: the later session's reset clears the earlier one's numbers.
//!
//! Diagnostic only -- nothing here changes behaviour, and reading them costs an
//! atomic load. The three numbers are read separately, so `counts()` is not an atomic snapshot.

use std::sync::atomic::{AtomicU64, Ordering};

static FRAMES_SURFACE: AtomicU64 = AtomicU64::new(0);
static FRAMES_SOFTWARE: AtomicU64 = AtomicU64::new(0);
static RENDERER_REFUSED: AtomicU64 = AtomicU64::new(0);

/// One decoded frame was handed to the renderer as a decoded surface.
pub fn note_surface_frame() {
    FRAMES_SURFACE.fetch_add(1, Ordering::Relaxed);
}

/// One decoded frame was handed over as software pixels (RGBA or no surface).
pub fn note_software() {
    FRAMES_SOFTWARE.fetch_add(1, Ordering::Relaxed);
}

/// The renderer refused a surface. Frames dropped before presentation are not counted here.
pub fn note_renderer_refused() {
    RENDERER_REFUSED.fetch_add(1, Ordering::Relaxed);
}

/// Start a session's numbers from zero.
pub fn reset() {
    FRAMES_SURFACE.store(0, Ordering::Relaxed);
    FRAMES_SOFTWARE.store(0, Ordering::Relaxed);
    RENDERER_REFUSED.store(0, Ordering::Relaxed);
}

/// `(frames_surface, frames_software, renderer_refused)` since the last reset.
pub fn counts() -> (u64, u64, u64) {
    (
        FRAMES_SURFACE.load(Ordering::Relaxed),
        FRAMES_SOFTWARE.load(Ordering::Relaxed),
        RENDERER_REFUSED.load(Ordering::Relaxed),
    )
}

/// One line for a log message, so every call site prints the same fields.
pub fn summary() -> String {
    let (surface, software, refused) = counts();
    format!("frames_surface={surface} frames_software={software} renderer_refused={refused}")
}
