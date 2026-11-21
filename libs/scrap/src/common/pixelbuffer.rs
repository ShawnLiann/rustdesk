//! An owned reference to a decoded `CVPixelBuffer`, macOS only.
//!
//! hwcodec lends the surface for one call: it stays valid only until the next
//! decode on the same decoder. The renderer reads the pointer after the decode
//! returns, so the caller takes its own reference; this wrapper owns exactly
//! one retain and releases it on drop.

use std::{ffi::c_void, ptr::NonNull};

/// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange` ('420v').
const PIXEL_FORMAT_420V: u32 = 0x3432_3076;
/// `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange` ('420f').
const PIXEL_FORMAT_420F: u32 = 0x3432_3066;

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferRetain(pixel_buffer: *mut c_void) -> *mut c_void;
    fn CVPixelBufferRelease(pixel_buffer: *mut c_void);
    fn CVPixelBufferGetWidth(pixel_buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetHeight(pixel_buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetPixelFormatType(pixel_buffer: *mut c_void) -> u32;
    fn CVPixelBufferGetIOSurface(pixel_buffer: *mut c_void) -> *mut c_void;
}

pub struct PixelBufferFrame {
    buffer: NonNull<c_void>,
    width: i32,
    height: i32,
    pixfmt: u32,
}

impl PixelBufferFrame {
    /// Take ownership of a borrowed `CVPixelBufferRef` by retaining it.
    ///
    /// `None` when the pointer is null, the format is not one the renderer
    /// samples directly, or it is not IOSurface-backed.
    /// # Safety
    /// `pixel_buffer` must be null or a valid `CVPixelBufferRef`.
    pub unsafe fn retain_from_borrowed(pixel_buffer: *mut c_void) -> Option<Self> {
        let buffer = NonNull::new(pixel_buffer)?;
        let pixfmt = CVPixelBufferGetPixelFormatType(buffer.as_ptr());
        if !matches!(pixfmt, PIXEL_FORMAT_420V | PIXEL_FORMAT_420F)
            || CVPixelBufferGetIOSurface(buffer.as_ptr()).is_null()
        {
            return None;
        }
        let retained = CVPixelBufferRetain(buffer.as_ptr());
        NonNull::new(retained).map(|buffer| Self {
            buffer,
            width: CVPixelBufferGetWidth(buffer.as_ptr()) as i32,
            height: CVPixelBufferGetHeight(buffer.as_ptr()) as i32,
            pixfmt,
        })
    }

    /// Borrowed view of the underlying `CVPixelBufferRef`; valid while `self` is.
    pub fn as_ptr(&self) -> *mut c_void {
        self.buffer.as_ptr()
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    pub fn pixfmt(&self) -> u32 {
        self.pixfmt
    }
}

impl Clone for PixelBufferFrame {
    fn clone(&self) -> Self {
        // Two owned wrappers, two references.
        let retained = unsafe { CVPixelBufferRetain(self.buffer.as_ptr()) };
        debug_assert!(!retained.is_null());
        Self {
            buffer: self.buffer,
            width: self.width,
            height: self.height,
            pixfmt: self.pixfmt,
        }
    }
}

impl Drop for PixelBufferFrame {
    fn drop(&mut self) {
        unsafe { CVPixelBufferRelease(self.buffer.as_ptr()) }
    }
}

// SAFETY: only a CoreVideo handle moves; pixel data is never touched here, and
// CVPixelBuffer retain/release is thread-safe.
unsafe impl Send for PixelBufferFrame {}
