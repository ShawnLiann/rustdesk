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
}

impl Drop for RetainedSurface {
    fn drop(&mut self) {
        unsafe {
            IOSurfaceDecrementUseCount(self.0);
            CFRelease(self.0);
        }
    }
}
