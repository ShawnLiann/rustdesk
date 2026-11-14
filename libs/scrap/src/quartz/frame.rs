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

    /// The surface this frame wraps, already retained and locked by `Frame::new`.
    #[inline]
    pub(crate) fn surface(&self) -> IOSurfaceRef {
        self.surface
    }

    pub fn surface_to_bgra<'a>(&'a mut self, h: usize) {
        unsafe {
            let plane0 = IOSurfaceGetBaseAddressOfPlane(self.surface, 0);
            self.bgra_stride = IOSurfaceGetBytesPerRowOfPlane(self.surface, 0);
            let len = self.bgra_stride * h;
            // Copy into uninitialised memory: `resize(len, 0)` memsets the whole 4K frame (~37 MB)
            // and the very next line overwrites every byte.
            self.bgra.clear();
            self.bgra.reserve(len);
            std::ptr::copy_nonoverlapping(plane0 as _, self.bgra.as_mut_ptr(), len);
            self.bgra.set_len(len);
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

/// A reference to a captured surface that holds no lock on it.
///
/// The dedupe compare has to answer "is this frame the same as the last one". Keeping a heap copy
/// of the last frame answers it at the cost of a full-frame copy for every frame that turns out to
/// be different (~37 MB at 4096x2304); retaining the surface answers it where the bytes already
/// are. No lock is held between frames on purpose: the compositor writes into its own pool buffer,
/// and a lock held across frames would stall it with nothing to show for it in the counters.
pub struct RetainedSurface(IOSurfaceRef);

impl RetainedSurface {
    /// Retain the surface `frame` wraps for as long as the returned value lives, or `None` when
    /// there is nothing to retain. Callers must read `None` as "no previous frame", never as
    /// "identical".
    pub fn new(frame: &Frame) -> Option<Self> {
        let surface = frame.surface();
        if surface.is_null() {
            return None;
        }
        unsafe {
            CFRetain(surface);
            IOSurfaceIncrementUseCount(surface);
        }
        Some(Self(surface))
    }

    /// Is `frame` byte-identical to the surface held here, over the whole allocation? That is the
    /// same span `would_block_if_equal` compares, so both dedupe paths agree on what identical
    /// means.
    ///
    /// Takes the `Frame` rather than its pointer on purpose: `Frame::new` is what maps the bytes,
    /// and its lifetime is the only thing that says the mapping is still there.
    ///
    /// Two handles on one buffer are reported as a difference, never compared. The compositor
    /// rewrites that buffer in place, so comparing it with itself would always answer "identical"
    /// and the peer would freeze on a picture that keeps changing.
    ///
    /// Only this side is locked. The frame's surface is already locked by `Frame::new` for as long
    /// as the frame lives, and taking that lock again would count it twice without buying a thing.
    pub fn matches(&self, frame: &Frame) -> bool {
        if self.0 == frame.surface() {
            return false;
        }
        let Some(_guard) = Locked::read(self.0) else {
            // The previous frame cannot be read; say "different" so this one is sent.
            return false;
        };
        !crate::surfaces_differ(self.view(), Some(frame.inner()))
    }

    /// The bytes this reference holds, or `None` when the surface has no buffer to read. Only
    /// valid while the lock taken by `matches` is held.
    fn view(&self) -> Option<&'static [u8]> {
        unsafe { surface_bytes(self.0) }
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

/// Holds a read lock on a surface and gives it back on drop.
///
/// A guard rather than paired calls: the compare it protects is a plain slice comparison today,
/// and a panic between lock and unlock would leave the surface locked for good.
struct Locked(IOSurfaceRef);

impl Locked {
    fn read(surface: IOSurfaceRef) -> Option<Self> {
        if unsafe { IOSurfaceLock(surface, SURFACE_LOCK_READ_ONLY, ptr::null_mut()) } != 0 {
            return None;
        }
        Some(Self(surface))
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        unsafe {
            IOSurfaceUnlock(self.0, SURFACE_LOCK_READ_ONLY, ptr::null_mut());
        }
    }
}

/// The bytes of `surface`, or `None` when it has no buffer to read.
///
/// The caller must hold a lock on `surface`, or own the `Frame` that holds it.
unsafe fn surface_bytes(surface: IOSurfaceRef) -> Option<&'static [u8]> {
    let len = IOSurfaceGetAllocSize(surface);
    let base = IOSurfaceGetBaseAddress(surface) as *const u8;
    if len == 0 || base.is_null() {
        return None;
    }
    Some(slice::from_raw_parts(base, len))
}
