use crate::{quartz, Frame, Pixfmt};
use hbb_common::log;
use std::io;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, TryLockError};

/// Request the surface format the encode path can actually consume. The pixel-buffer path hands the
/// surface itself to VideoToolbox, so a biplanar 420v surface saves the conversion VT would otherwise
/// do per frame; every other path converts from packed ARGB8888 and would read a 420v surface's luma
/// plane as if it were BGRA, so it keeps the packed surface.
fn capture_pixel_format() -> quartz::PixelFormat {
    if crate::nv12_capture_enabled() {
        log::info!(
            "capture surface: requesting 420v (RD_PIXBUF_NV12=1) - screenshots and the yuv \
             fallback are not supported on this arm"
        );
        quartz::PixelFormat::YCbCr420Video
    } else {
        quartz::PixelFormat::Argb8888
    }
}

pub struct Capturer {
    inner: quartz::Capturer,
    frame: Arc<Mutex<Option<quartz::Frame>>>,
    saved_raw_data: Vec<u8>, // for faster compare and copy
    /// The previous frame's surface, retained instead of copied, for `RD_DEDUPE_ZC=1`.
    prev_surface: Option<quartz::RetainedSurface>,
    /// Change accumulated since the last delivered frame, for `RD_DIFF_ACCUM`.
    diff_accum: u64,
    /// Frames skipped in a row, for `RD_DIFF_MAX_SKIP`.
    diff_skipped: usize,
}

impl Capturer {
    pub fn new(display: Display) -> io::Result<Capturer> {
        let frame = Arc::new(Mutex::new(None));

        let f = frame.clone();
        let inner = quartz::Capturer::new(
            display.0,
            display.width(),
            display.height(),
            capture_pixel_format(),
            Default::default(),
            move |inner| {
                if let Ok(mut f) = f.lock() {
                    *f = Some(inner);
                }
            },
        )
        .map_err(|_| io::Error::from(io::ErrorKind::Other))?;

        Ok(Capturer {
            inner,
            frame,
            saved_raw_data: Vec::new(),
            prev_surface: None,
            diff_accum: 0,
            diff_skipped: 0,
        })
    }

    /// "Same as the last frame", answered without a heap copy: the previous surface is still
    /// referenced, so the two byte ranges can be compared where they already are. A compositor that
    /// hands the same buffer back makes the question meaningless, and the answer there has to be
    /// "different" - dropping a static frame recovers, presenting a stale one does not.
    fn dedupe_zc_equal(&mut self, frame: &quartz::Frame) -> bool {
        let same = match &self.prev_surface {
            Some(prev) if prev.same_buffer(frame) => false,
            Some(prev) => prev.bytes_equal(frame),
            None => false,
        };
        if !same {
            self.prev_surface = Some(frame.retained_surface());
        }
        same
    }

    /// `RD_DIFF_ACCUM`: the same compare, but the answer is a budget rather than a verdict. Each frame
    /// contributes the number of differing sampled bytes it finds; a frame is delivered only when the
    /// total since the last delivered frame covers the threshold, and a delivery restarts it. The
    /// retained reference therefore stays on the last *delivered* surface, so nothing that was skipped
    /// is lost from the accounting.
    fn dedupe_zc_budget(&mut self, frame: &quartz::Frame, threshold: u64) -> bool {
        let diff = match &self.prev_surface {
            // One handle on the compositor's own buffer cannot be compared with itself, and holding it
            // as the baseline would freeze the peer, so it counts as a full change.
            Some(prev) if prev.same_buffer(frame) => u64::MAX,
            Some(prev) => prev.sampled_diff(frame, crate::diff_sample_stride()),
            None => u64::MAX,
        };
        let (deliver, accum, skipped) = crate::common::diff_budget_decision(
            self.diff_accum,
            diff,
            threshold,
            self.diff_skipped,
            crate::diff_max_skip(),
        );
        self.diff_accum = accum;
        self.diff_skipped = skipped;
        if deliver {
            self.prev_surface = Some(frame.retained_surface());
            false
        } else {
            true
        }
    }

    pub fn width(&self) -> usize {
        self.inner.width()
    }

    pub fn height(&self) -> usize {
        self.inner.height()
    }
}

impl crate::TraitCapturer for Capturer {
    fn frame<'a>(&'a mut self, _timeout_ms: std::time::Duration) -> io::Result<Frame<'a>> {
        // Take the frame out and drop the guard before touching `self`: the zero-copy dedupe compare
        // needs `&mut self` to keep its reference to the previous surface, and holding the lock across
        // that would be a borrow conflict - and a lock held longer than it needs to be.
        let frame = {
            let mut handle = match self.frame.try_lock() {
                Ok(handle) => handle,
                Err(TryLockError::WouldBlock) => return Err(io::ErrorKind::WouldBlock.into()),
                Err(TryLockError::Poisoned(..)) => return Err(io::ErrorKind::Other.into()),
            };
            handle.take()
        };

        match frame {
            Some(frame) => {
                quartz::stats::bump(&quartz::stats::DEDUPE_CHECKED);
                if !crate::no_dedupe_enabled() {
                    let started = std::time::Instant::now();
                    let same = if crate::dedupe_zc_enabled() {
                        match crate::diff_accum_threshold() {
                            Some(threshold) => self.dedupe_zc_budget(&frame, threshold),
                            None => self.dedupe_zc_equal(&frame),
                        }
                    } else {
                        crate::would_block_if_equal(&mut self.saved_raw_data, frame.inner())
                            .is_err()
                    };
                    quartz::stats::add_micros(&quartz::stats::DEDUPE_MICROS, started.elapsed());
                    if same {
                        quartz::stats::bump(&quartz::stats::DEDUPE_BLOCKED);
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                }
                if !crate::lazy_bgra_enabled() {
                    let _ = frame.bgra(self.height());
                }
                Ok(Frame::PixelBuffer(PixelBuffer {
                    frame,
                    data: PhantomData,
                    width: self.width(),
                    height: self.height(),
                }))
            }

            None => Err(io::ErrorKind::WouldBlock.into()),
        }
    }
}

pub struct PixelBuffer<'a> {
    frame: quartz::Frame,
    data: PhantomData<&'a [u8]>,
    width: usize,
    height: usize,
}

impl<'a> PixelBuffer<'a> {
    /// CVPixelBufferRef wrapping the same IOSurface; caller owns one reference.
    pub fn create_cvpixelbuffer(&self) -> *mut std::ffi::c_void {
        self.frame.log_surface_format_once();
        self.frame.create_cvpixelbuffer()
    }
}

impl<'a> crate::TraitPixelBuffer for PixelBuffer<'a> {
    fn data(&self) -> &[u8] {
        self.frame.bgra(self.height)
    }

    fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.frame.stride() == 0
    }

    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn stride(&self) -> Vec<usize> {
        let mut v = Vec::new();
        v.push(self.frame.stride());
        v
    }

    fn pixfmt(&self) -> Pixfmt {
        Pixfmt::BGRA
    }
}

pub struct Display(quartz::Display);

impl Display {
    pub fn primary() -> io::Result<Display> {
        Ok(Display(quartz::Display::primary()))
    }

    pub fn all() -> io::Result<Vec<Display>> {
        Ok(quartz::Display::online()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))?
            .into_iter()
            .map(Display)
            .collect())
    }

    pub fn width(&self) -> usize {
        self.0.width()
    }

    pub fn height(&self) -> usize {
        self.0.height()
    }

    pub fn scale(&self) -> f64 {
        self.0.scale()
    }

    pub fn name(&self) -> String {
        self.0.id().to_string()
    }

    pub fn is_online(&self) -> bool {
        self.0.is_online()
    }

    pub fn origin(&self) -> (i32, i32) {
        let o = self.0.bounds().origin;
        (o.x as _, o.y as _)
    }

    pub fn is_primary(&self) -> bool {
        self.0.is_primary()
    }
}
