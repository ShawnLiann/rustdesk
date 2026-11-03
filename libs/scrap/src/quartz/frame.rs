use hbb_common::log;
use std::{cell::OnceCell, ptr, slice};

use super::ffi::*;

pub struct Frame {
    surface: IOSurfaceRef,
    inner: &'static [u8],
    bgra: OnceCell<Vec<u8>>,
    bgra_stride: usize,
}

impl Frame {
    pub unsafe fn new(surface: IOSurfaceRef) -> Frame {
        CFRetain(surface);
        IOSurfaceIncrementUseCount(surface);

        IOSurfaceLock(surface, SURFACE_LOCK_READ_ONLY, ptr::null_mut());

        let inner = slice::from_raw_parts(
            IOSurfaceGetBaseAddress(surface) as *const u8,
            IOSurfaceGetAllocSize(surface),
        );

        Frame {
            surface,
            inner,
            bgra: OnceCell::new(),
            bgra_stride: IOSurfaceGetBytesPerRowOfPlane(surface, 0),
        }
    }

    #[inline]
    pub fn inner(&self) -> &[u8] {
        self.inner
    }

    pub fn stride(&self) -> usize {
        self.bgra_stride
    }

    /// A handle that keeps this surface alive without holding a lock on it, for the next frame's
    /// dedupe comparison.
    pub fn retained_surface(&self) -> RetainedSurface {
        unsafe { RetainedSurface::new(self.surface) }
    }

    /// Prove which format the surface really carries, once per process. A capture that was asked for
    /// 420v and silently got ARGB8888 would otherwise be indistinguishable from "the change made no
    /// difference", which is the one conclusion an A/B arm must never reach by accident.
    pub fn log_surface_format_once(&self) {
        static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            let fourcc = unsafe { IOSurfaceGetPixelFormat(self.surface) };
            let name: String = [24u32, 16, 8, 0]
                .iter()
                .map(|shift| ((fourcc >> shift) & 0xff) as u8 as char)
                .collect();
            log::info!("capture surface format in effect: {name}");
        });
    }

    /// Wrap the surface without copying, with the matrix VideoToolbox should convert with. The
    /// attachment is not written into the bitstream (no VUI in the SPS), so the peer still sees
    /// whatever its decoder assumes. The caller owns one reference and must CFRelease it.
    pub fn create_cvpixelbuffer(&self) -> *mut std::ffi::c_void {
        unsafe {
            let mut pb: *mut std::ffi::c_void = ptr::null_mut();
            if CVPixelBufferCreateWithIOSurface(
                ptr::null_mut(),
                self.surface,
                ptr::null_mut(),
                &mut pb,
            ) != 0
            {
                return ptr::null_mut();
            }
            if !pb.is_null() {
                // 1 == kCVAttachmentMode_ShouldPropagate
                CVBufferSetAttachment(
                    pb,
                    kCVImageBufferYCbCrMatrixKey,
                    kCVImageBufferYCbCrMatrix_ITU_R_601_4 as *mut _,
                    1,
                );
            }
            pb
        }
    }

    /// Materialise the BGRA copy on first use. The zero-copy path hands the surface to
    /// VideoToolbox and never reads the pixels, so a frame that only takes that path must not
    /// pay for a full surface copy. The yuv conversion and screenshots pay for it here, under
    /// the read-only lock that new() already holds.
    pub fn bgra(&self, h: usize) -> &[u8] {
        self.bgra.get_or_init(|| {
            let len = self.bgra_stride * h;
            // Copy into uninitialised memory: `vec![0u8; len]` memsets the whole 37 MB frame just
            // to overwrite every byte on the next line, and `sample` shows that memset (bzero) as
            // a per-frame cost on the 4K path.
            let mut v: Vec<u8> = Vec::with_capacity(len);
            unsafe {
                ptr::copy_nonoverlapping(
                    IOSurfaceGetBaseAddressOfPlane(self.surface, 0) as *const u8,
                    v.as_mut_ptr(),
                    len,
                );
                v.set_len(len);
            }
            v
        })
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            IOSurfaceUnlock(self.surface, SURFACE_LOCK_READ_ONLY, ptr::null_mut());

            IOSurfaceDecrementUseCount(self.surface);
            CFRelease(self.surface);
        }
    }
}

/// A surface kept alive *without* holding a lock on it, so the dedupe compare can read the previous
/// frame where it already lives instead of keeping a heap copy of it. Holding the read lock across
/// frames would block the compositor from writing into its own pool buffer, which is a stall nobody
/// could see in the counters; the lock is therefore taken only for the duration of a comparison.
pub struct RetainedSurface(IOSurfaceRef);

impl RetainedSurface {
    pub unsafe fn new(surface: IOSurfaceRef) -> Self {
        CFRetain(surface);
        IOSurfaceIncrementUseCount(surface);
        Self(surface)
    }

    /// Two handles on the same buffer cannot be compared: the compositor rewrites it in place, so
    /// the answer would always be "identical" and the peer would freeze. The caller must treat the
    /// same-buffer case as a difference and does not need this method to say so.
    pub fn same_buffer(&self, frame: &Frame) -> bool {
        self.0 == frame.surface
    }

    /// Byte equality over the whole allocation - the same span `would_block_if_equal` compares, so
    /// the two dedupe paths agree on what "identical" means.
    pub fn bytes_equal(&self, frame: &Frame) -> bool {
        unsafe {
            if IOSurfaceLock(self.0, SURFACE_LOCK_READ_ONLY, ptr::null_mut()) != 0 {
                return false;
            }
            let len = IOSurfaceGetAllocSize(self.0);
            let a = slice::from_raw_parts(IOSurfaceGetBaseAddress(self.0) as *const u8, len);
            let same = frame.inner().len() == len && a == frame.inner();
            IOSurfaceUnlock(self.0, SURFACE_LOCK_READ_ONLY, ptr::null_mut());
            same
        }
    }
    /// How many bytes the surface changed by, estimated from a contiguous sample of it.
    ///
    /// Sampling *chunks* rather than every n-th byte is the whole point: a stride of 1 byte in 64 is
    /// 590k scattered reads over a 37 MB surface, i.e. a cache miss each, and measured here it costs
    /// the same as comparing the whole thing (8.2 ms against 9.1 ms per frame). Reading one 16 KB
    /// chunk out of every `sample * 16 KB` keeps every read inside cache lines the CPU already
    /// fetched, and it works the same on Intel and Apple Silicon because the chunk is a multiple of
    /// both page sizes.
    ///
    /// `frame_index` rotates the grid by one chunk per frame, so a small region cannot sit between
    /// samples forever and stay invisible to the budget.
    ///
    /// The result is scaled up to the whole surface, so a threshold keeps its meaning when the
    /// sampling density changes. `u64::MAX` means "cannot compare, everything changed".
    pub fn sampled_diff(&self, frame: &Frame, sample: usize, frame_index: u64) -> DiffSample {
        let interval = diff_interval(sample);
        unsafe {
            if IOSurfaceLock(self.0, SURFACE_LOCK_READ_ONLY, ptr::null_mut()) != 0 {
                return DiffSample::EVERYTHING;
            }
            let len = IOSurfaceGetAllocSize(self.0);
            let other = frame.inner();
            let sample = if len != other.len() {
                // A geometry change moves every byte that appeared or vanished.
                DiffSample::EVERYTHING
            } else if len == 0 {
                DiffSample::NOTHING
            } else {
                let a = slice::from_raw_parts(IOSurfaceGetBaseAddress(self.0) as *const u8, len);
                let mut sampled = 0usize;
                let mut differing = 0usize;
                let mut whole_chunk = false;
                let mut start = (frame_index as usize).wrapping_mul(DIFF_CHUNK) % interval;
                while start < len {
                    let end = (start + DIFF_CHUNK).min(len);
                    let (x, y) = (&a[start..end], &other[start..end]);
                    sampled += end - start;
                    // An equal chunk is one memcmp; only a changed one pays for counting bytes.
                    if x != y {
                        let changed = x.iter().zip(y.iter()).filter(|(p, q)| p != q).count();
                        differing += changed;
                        // A chunk rewritten end to end is a local change the eye cannot miss, so it
                        // buys a frame even when the accumulated budget says not yet.
                        whole_chunk |= changed == end - start;
                    }
                    start += interval;
                }
                DiffSample {
                    estimated: scale_to_surface(differing, sampled, len),
                    whole_chunk,
                }
            };
            IOSurfaceUnlock(self.0, SURFACE_LOCK_READ_ONLY, ptr::null_mut());
            sample
        }
    }
}

/// The verdict of one sampled comparison: how much of the surface changed (estimated), and whether one
/// whole chunk was rewritten, which is a change no threshold should swallow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffSample {
    pub estimated: u64,
    pub whole_chunk: bool,
}

impl DiffSample {
    pub const NOTHING: DiffSample = DiffSample {
        estimated: 0,
        whole_chunk: false,
    };
    pub const EVERYTHING: DiffSample = DiffSample {
        estimated: u64::MAX,
        whole_chunk: true,
    };
}

/// Bytes compared per sample. A multiple of 4 KB and of 16 KB, so it is page-friendly on Intel
/// (4 KB pages) and on Apple Silicon (16 KB pages, M1 through M6).
pub const DIFF_CHUNK: usize = 16 * 1024;

/// Distance between the starts of two sampled chunks. `sample` is the density: 1 compares everything,
/// 16 reads one sixteenth of the surface.
pub fn diff_interval(sample: usize) -> usize {
    sample.saturating_mul(DIFF_CHUNK).max(DIFF_CHUNK)
}

/// Scale a sample's differing byte count up to the whole surface. Pure, so the arithmetic is testable
/// without a surface.
pub fn scale_to_surface(differing: usize, sampled: usize, len: usize) -> u64 {
    if sampled == 0 || len == 0 {
        return 0;
    }
    let factor = (len / sampled).max(1) as u64;
    (differing as u64).saturating_mul(factor)
}

impl Drop for RetainedSurface {
    fn drop(&mut self) {
        unsafe {
            IOSurfaceDecrementUseCount(self.0);
            CFRelease(self.0);
        }
    }
}
