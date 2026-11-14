use crate::{quartz, Frame, Pixfmt};
use std::io;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, TryLockError};

pub struct Capturer {
    inner: quartz::Capturer,
    frame: Arc<Mutex<Option<quartz::Frame>>>,
    /// The previous frame's surface, retained instead of copied: the compare reads both ranges
    /// where they already live.
    prev_surface: Option<quartz::RetainedSurface>,
}

impl Capturer {
    pub fn new(display: Display) -> io::Result<Capturer> {
        let frame = Arc::new(Mutex::new(None));

        let f = frame.clone();
        let inner = quartz::Capturer::new(
            display.0,
            display.width(),
            display.height(),
            quartz::PixelFormat::Argb8888,
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
            prev_surface: None,
        })
    }

    /// Is this frame byte-identical to the one last handed out?
    ///
    /// Answered against a retained reference to the previous surface instead of a heap copy of it,
    /// so a frame that turns out to be different no longer pays a full-frame copy to be told apart
    /// from its predecessor. The reference is replaced whenever the answer is "different", which
    /// also covers a compositor that hands the same buffer back: that has to be reported as a
    /// difference, because dropping a static frame recovers and presenting a stale one does not.
    fn same_as_previous(&mut self, frame: &quartz::Frame) -> bool {
        let same = match &self.prev_surface {
            Some(prev) => prev.matches(frame),
            None => false,
        };
        if !same {
            // Retain the new one before the old is released, so the surface is never briefly
            // unowned. `None` here means there is nothing to retain, i.e. the next frame is sent.
            self.prev_surface = quartz::RetainedSurface::new(frame);
        }
        same
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
        // Take the frame out and let the guard go before touching `self`: keeping the reference to
        // the previous surface needs `&mut self`, and the guard borrows `self.frame` while it lives.
        let frame = match self.frame.try_lock() {
            Ok(mut handle) => handle.take(),
            Err(TryLockError::WouldBlock) => return Err(io::ErrorKind::WouldBlock.into()),
            Err(TryLockError::Poisoned(..)) => return Err(io::ErrorKind::Other.into()),
        };

        match frame {
            Some(mut frame) => {
                if self.same_as_previous(&frame) {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                frame.surface_to_bgra(self.height());
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

impl<'a> crate::TraitPixelBuffer for PixelBuffer<'a> {
    fn data(&self) -> &[u8] {
        &*self.frame
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
