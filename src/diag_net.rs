//! Where the client's per-frame budget goes *before* the decoder sees it.
//!
//! The `[DEC]` line already splits decode / present / wait once a frame reaches the video thread. This
//! module covers the path ahead of that: protobuf parse plus the io_loop's own bookkeeping for a video
//! frame (format, thread lookup, queue push, channel send). Counters are always accumulated; the line is
//! printed only when `RD_LOG_SPLIT=1`, the same switch the host uses, so one build can be either side.
use hbb_common::log;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static PARSE_US: AtomicU64 = AtomicU64::new(0);
static ARM_US: AtomicU64 = AtomicU64::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static EPOCH: OnceLock<Instant> = OnceLock::new();
static WINDOW_START_US: AtomicU64 = AtomicU64::new(0);
static PRINTING: AtomicBool = AtomicBool::new(false);

/// `RD_DROP_AFTER_DECODE=1` decodes every frame and then throws it away instead of handing it to the UI.
/// It prices the work that happens *after* `handle_frame`: the pixel copy, the texture upload and whatever
/// rasters behind it. [DEC]'s decode= and present= together account for only about 4 of the 18.6 percentage
/// points that zero-copy removes on an M2 client, so the remainder gets pinned down by taking that work out
/// and measuring what is left. Unset = normal behaviour.
pub fn drop_after_decode() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("RD_DROP_AFTER_DECODE").map(|v| v == "1").unwrap_or(false))
}

fn now_us() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// One video frame crossed `handle_msg_from_peer`: `parse` is the protobuf decode that produced it,
/// `arm` is the io_loop work done by the VideoFrame arm itself.
pub fn record(parse: Duration, arm: Duration, bytes: usize) {
    PARSE_US.fetch_add(parse.as_micros() as u64, Ordering::Relaxed);
    ARM_US.fetch_add(arm.as_micros() as u64, Ordering::Relaxed);
    BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    FRAMES.fetch_add(1, Ordering::Relaxed);

    let now = now_us();
    let window_us = now.saturating_sub(WINDOW_START_US.load(Ordering::Relaxed));
    if window_us < 5_000_000 || !scrap::log_split_enabled() {
        return;
    }
    // One reporter per window; a thread that loses the race simply keeps counting.
    if PRINTING.swap(true, Ordering::AcqRel) {
        return;
    }
    let secs = window_us as f64 / 1_000_000.0;
    let parse_us = PARSE_US.swap(0, Ordering::Relaxed);
    let arm_us = ARM_US.swap(0, Ordering::Relaxed);
    let frames = FRAMES.swap(0, Ordering::Relaxed);
    let bytes = BYTES.swap(0, Ordering::Relaxed);
    WINDOW_START_US.store(now, Ordering::Relaxed);
    let ms_per_s = |v: u64| v as f64 / 1000.0 / secs;
    log::info!(
        "[NET] {:.1}s frames={} ({:.1} fps) parse={:.0} arm={:.0} rate={:.0} kB/s",
        secs,
        frames,
        frames as f64 / secs,
        ms_per_s(parse_us),
        ms_per_s(arm_us),
        bytes as f64 / secs / 1024.0
    );
    PRINTING.store(false, Ordering::Release);
}
