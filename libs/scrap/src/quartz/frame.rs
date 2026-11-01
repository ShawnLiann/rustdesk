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
            let mut v = vec![0u8; self.bgra_stride * h];
            unsafe {
                ptr::copy_nonoverlapping(
                    IOSurfaceGetBaseAddressOfPlane(self.surface, 0) as *const u8,
                    v.as_mut_ptr(),
                    v.len(),
                );
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
