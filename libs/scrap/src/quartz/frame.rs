use std::{ops, ptr, slice};

use super::ffi::*;

pub struct Frame {
    surface: IOSurfaceRef,
    inner: &'static [u8],
    bgra: Vec<u8>,
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
            bgra: Vec::new(),
            bgra_stride: 0,
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

    pub fn surface_to_bgra<'a>(&'a mut self, h: usize) {
        unsafe {
            let plane0 = IOSurfaceGetBaseAddressOfPlane(self.surface, 0);
            self.bgra_stride = IOSurfaceGetBytesPerRowOfPlane(self.surface, 0);
            self.bgra.resize(self.bgra_stride * h, 0);
            std::ptr::copy_nonoverlapping(
                plane0 as _,
                self.bgra.as_mut_ptr(),
                self.bgra_stride * h,
            );
        }
    }
}

impl ops::Deref for Frame {
    type Target = [u8];
    fn deref<'a>(&'a self) -> &'a [u8] {
        &self.bgra
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
