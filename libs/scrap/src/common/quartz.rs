use crate::{quartz, Frame, Pixfmt};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};
use std::{io, mem};

/// Measurement-only budget, the same one `ci/n5a-instr2-20261115` carries on the in-place branch,
/// so the two builds print comparable columns. Scaffold for `ci/port-instr-*`, never part of a PR.
///
/// The window is wall time, so it also counts the time the server did not poll this capturer (it
/// waits for the peer to fetch a frame before asking for the next one). A window that prints much
/// more than 5s therefore has stalled time in it and must not be compared with a clean one.
#[derive(Default)]
struct FrameBudget {
    dedupe: Duration,
    bgra: Duration,
    none: u64,
    sent: u64,
    dropped: u64,
    since: Option<Instant>,
}

enum FrameOutcome {
    Empty,
    Dropped(Duration),
    Sent(Duration, Duration),
}

impl FrameBudget {
    /// Add one poll to the current window and, every five seconds, say how the frames were spent.
    ///
    /// `dedupe=` is `would_block_if_equal` on this branch: the compare, plus on a frame that
    /// differs the copy into `saved_raw_data`. That copy is what the in-place branch removes, so
    /// this is the column to difference against `ci/n5a-instr2-*`. A frame that compares identical
    /// pays the compare only, so the copy shows up in the windows where frames differ, not in the
    /// dead-still ones. `bgra=` is the surface-to-BGRA copy, `sent`/`dropped` the frames handed on
    /// and the frames that compared identical.
    ///
    /// `none` counts polls that produced no frame at all, so a window with `sent=0 dropped=0` is
    /// void: it says nothing about the screen. Polls that lost the mutex are not counted at all.
    fn note(&mut self, outcome: FrameOutcome) {
        let now = Instant::now();
        let since = *self.since.get_or_insert(now);
        match outcome {
            FrameOutcome::Empty => self.none += 1,
            FrameOutcome::Dropped(dedupe) => {
                self.dedupe += dedupe;
                self.dropped += 1;
            }
            FrameOutcome::Sent(dedupe, copy) => {
                self.dedupe += dedupe;
                self.bgra += copy;
                self.sent += 1;
            }
        }
        let window = now - since;
        if window < Duration::from_secs(5) {
            return;
        }
        let secs = window.as_secs_f32();
        hbb_common::log::info!(
            "[FRAME] {:.1}s none={} sent={} dropped={} dedupe={:.1} bgra={:.1} ms/s",
            secs,
            self.none,
            self.sent,
            self.dropped,
            self.dedupe.as_secs_f32() * 1000.0 / secs,
            self.bgra.as_secs_f32() * 1000.0 / secs,
        );
        *self = FrameBudget::default();
        self.since = Some(now);
    }
}

pub struct Capturer {
    inner: quartz::Capturer,
    frame: Arc<Mutex<Option<quartz::Frame>>>,
    saved_raw_data: Vec<u8>, // for faster compare and copy
    budget: FrameBudget,
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
            saved_raw_data: Vec::new(),
            budget: FrameBudget::default(),
        })
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
        match self.frame.try_lock() {
            Ok(mut handle) => {
                let mut frame = None;
                mem::swap(&mut frame, &mut handle);

                match frame {
                    Some(mut frame) => {
                        let dedupe = Instant::now();
                        let verdict =
                            crate::would_block_if_equal(&mut self.saved_raw_data, frame.inner());
                        let dedupe = dedupe.elapsed();
                        if let Err(e) = verdict {
                            self.budget.note(FrameOutcome::Dropped(dedupe));
                            return Err(e);
                        }
                        let copy = Instant::now();
                        frame.surface_to_bgra(self.height());
                        self.budget.note(FrameOutcome::Sent(dedupe, copy.elapsed()));
                        Ok(Frame::PixelBuffer(PixelBuffer {
                            frame,
                            data: PhantomData,
                            width: self.width(),
                            height: self.height(),
                        }))
                    }

                    None => {
                        self.budget.note(FrameOutcome::Empty);
                        Err(io::ErrorKind::WouldBlock.into())
                    }
                }
            }

            Err(TryLockError::WouldBlock) => Err(io::ErrorKind::WouldBlock.into()),

            Err(TryLockError::Poisoned(..)) => Err(io::ErrorKind::Other.into()),
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
