//! LOCAL MEASUREMENT ONLY — a capture source with no screen behind it.
//!
//! Exists so the host-side path can be priced while the screen is locked, where a real capture
//! drops to ~1 fps and the numbers mean nothing. Each frame is the same thing the compositor
//! callback hands over (an IOSurface-backed `Frame`), so everything downstream — the still-frame
//! compare, the BGRA materialisation, the encoder, the `[FRAME]` counters — runs the real code path.
//!
//! Pull, not push: the capturer asks for a frame on its own thread, so nothing has to cross a
//! thread boundary and `Frame`'s raw pointers stay where they belong.
//!
//! Never proposed upstream and never part of a PR: local-only branch, numbers labelled synthetic,
//! enabled by `RD_FAKE_CAPTURE=<pattern>:<fps>:<width>x<height>`, pattern ∈ static|clock|scroll|full

use super::ffi::IOSurfaceGetAllocSize;
use super::Frame;
use std::ffi::c_void;
use std::ptr;
use std::time::{Duration, Instant};

const PIXEL_FORMAT_BGRA: u32 = 0x4247_5241; // 'BGRA'
const LOCK_READ_WRITE: u64 = 0;

extern "C" {
    fn CVPixelBufferCreate(
        allocator: *const c_void,
        width: usize,
        height: usize,
        pixel_format: u32,
        attributes: *const c_void,
        out: *mut *mut c_void,
    ) -> i32;
    fn CVPixelBufferRelease(pixel_buffer: *mut c_void);
    fn CVPixelBufferGetIOSurface(pixel_buffer: *mut c_void) -> *mut c_void;
    fn CVPixelBufferLockBaseAddress(pixel_buffer: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pixel_buffer: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pixel_buffer: *mut c_void) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRow(pixel_buffer: *mut c_void) -> usize;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// A new buffer with identical bytes every frame: the compare should answer "same".
    Static,
    /// A small box changes every frame, like a clock: the compare finds it early.
    Clock,
    /// Every row shifts: the whole picture differs.
    Scroll,
    /// The first byte changes: the cheapest possible "different".
    Full,
}

pub struct Spec {
    pub width: usize,
    pub height: usize,
    pub fps: u32,
    pub pattern: Pattern,
}

impl Spec {
    pub fn from_env(var: &str) -> Option<Spec> {
        let raw = std::env::var(var).ok()?;
        let mut parts = raw.split(':');
        let pattern = match parts.next()?.trim() {
            "clock" => Pattern::Clock,
            "scroll" => Pattern::Scroll,
            "full" => Pattern::Full,
            _ => Pattern::Static,
        };
        let fps = parts.next().and_then(|s| s.trim().parse().ok()).unwrap_or(30);
        let size = parts.next().unwrap_or("4096x2304");
        let (w, h) = size.split_once('x').unwrap_or(("4096", "2304"));
        Some(Spec {
            width: w.trim().parse().ok()?,
            height: h.trim().parse().ok()?,
            fps,
            pattern,
        })
    }
}

/// Generator state: one frame per `1/fps`, produced on demand.
pub struct Source {
    spec: Spec,
    interval: Duration,
    next_at: Option<Instant>,
    seq: u64,
}

impl Source {
    pub fn new(spec: Spec) -> Source {
        let interval = Duration::from_micros(1_000_000 / u64::from(spec.fps.max(1)));
        Source {
            spec,
            interval,
            next_at: None,
            seq: 0,
        }
    }

    /// The next frame, or `None` while the pacing says it is too early — the caller answers
    /// `WouldBlock`, which is exactly what a compositor with nothing new does.
    ///
    /// A fresh buffer every frame on purpose: reusing one would hand the same surface handle back,
    /// and the capturer treats a repeated handle as "different" without comparing, so the compare
    /// under test would never run.
    pub fn next_frame(&mut self) -> Option<Frame> {
        let now = Instant::now();
        match self.next_at {
            Some(at) if now < at => return None,
            Some(at) => self.next_at = Some(at + self.interval),
            None => self.next_at = Some(now + self.interval),
        }
        let spec = &self.spec;
        unsafe {
            let mut pb: *mut c_void = ptr::null_mut();
            let status = CVPixelBufferCreate(
                ptr::null(),
                spec.width,
                spec.height,
                PIXEL_FORMAT_BGRA,
                ptr::null(),
                &mut pb,
            );
            if status != 0 || pb.is_null() {
                eprintln!("[FAKE] CVPixelBufferCreate failed: {status}");
                self.next_at = Some(now + Duration::from_secs(1));
                return None;
            }
            let surface = CVPixelBufferGetIOSurface(pb);
            let mut frame = None;
            if !surface.is_null() && CVPixelBufferLockBaseAddress(pb, LOCK_READ_WRITE) == 0 {
                let base = CVPixelBufferGetBaseAddress(pb) as *mut u8;
                let bpr = CVPixelBufferGetBytesPerRow(pb);
                let alloc = IOSurfaceGetAllocSize(surface);
                fill(base, bpr, alloc, spec, self.seq);
                CVPixelBufferUnlockBaseAddress(pb, LOCK_READ_WRITE);
                // `Frame::new` retains the surface, so releasing the buffer below keeps it alive.
                frame = Some(Frame::new(surface));
            }
            CVPixelBufferRelease(pb);
            self.seq = self.seq.wrapping_add(1);
            frame
        }
    }
}

unsafe fn fill(base: *mut u8, bytes_per_row: usize, alloc: usize, spec: &Spec, seq: u64) {
    // The whole allocation, not just the visible rows: the compare reads `IOSurfaceGetAllocSize`
    // bytes, so untouched padding would read as a difference and `static` would never be equal.
    ptr::write_bytes(base, 0x40, alloc);
    let w = spec.width;
    match spec.pattern {
        Pattern::Static => {}
        Pattern::Full => *base = (seq as u8) | 1,
        Pattern::Clock => {
            let (bx, by) = (w / 2, spec.height / 2);
            let bw = 240.min(w);
            let bh = 80.min(spec.height);
            for y in by..(by + bh) {
                let row = base.add(y * bytes_per_row + bx * 4);
                for x in 0..bw {
                    *row.add(x * 4) = seq as u8;
                }
            }
        }
        Pattern::Scroll => {
            for y in 0..spec.height {
                let row = base.add(y * bytes_per_row);
                let v = (y as u64).wrapping_add(seq) as u8;
                ptr::write_bytes(row, v, w * 4);
            }
        }
    }
}
