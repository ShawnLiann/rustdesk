pub use self::capturer::Capturer;
pub use self::config::Config;
pub use self::display::Display;
pub use self::ffi::{CGError, PixelFormat};
pub use self::frame::Frame;

/// Counters for one question: does the byte compare in `common::quartz` still earn its per-frame
/// surface copy, given that the compositor already reports `FrameIdle` when nothing changed?
pub mod stats {
    use std::sync::atomic::{AtomicU64, Ordering};

    pub static FRAME_COMPLETE: AtomicU64 = AtomicU64::new(0);
    pub static FRAME_IDLE: AtomicU64 = AtomicU64::new(0);
    pub static DEDUPE_CHECKED: AtomicU64 = AtomicU64::new(0);
    pub static DEDUPE_BLOCKED: AtomicU64 = AtomicU64::new(0);

    #[inline]
    pub fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}

mod capturer;
mod config;
mod display;
pub mod ffi;
mod frame;

use std::sync::{Arc, Mutex};

lazy_static::lazy_static! {
    pub static ref ENABLE_RETINA: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
}
