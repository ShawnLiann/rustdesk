use crate::{quartz, Frame, Pixfmt};
use std::io;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

/// Measurement-only switch for the synthetic frame source; see `quartz::fake`.
const FAKE_CAPTURE_ENV: &str = "RD_FAKE_CAPTURE";

/// Measurement-only budget: the copy this branch removes is only visible as a share of the wall
/// second, so the capturer reports what it spent them on. Scaffold for `ci/n5a-instr-*`, never part
/// of a PR branch.
///
/// The window is wall time, so it also counts the time the server did not poll this capturer (it
/// waits for the peer to fetch a frame before asking for the next one). A window that prints much
/// more than 5s therefore has stalled time in it and must not be compared with a clean one.
#[derive(Default)]
struct FrameBudget {
    dedupe: Duration,
    bgra: Duration,
    /// Time the synthetic source spent producing the frame (fill + pacing). Only the scaffold has
    /// one; the real compositor's cost is not observable from here and must not be invented.
    gen: Duration,
    none: u64,
    sent: u64,
    dropped: u64,
    /// Attribution for `sent` on this branch. `surf=` distinct handles seen in the window,
    /// `same=` frames whose handle equals the immediately previous frame's (proves the pool is
    /// rotating), `id=` frames the identity rule answered "different" without comparing —
    /// i.e. the retained previous surface came round again.
    distinct: u64,
    repeats: u64,
    identity: u64,
    seen: Vec<u32>,
    last_id: Option<u32>,
    since: Option<Instant>,
}

enum FrameOutcome {
    Empty,
    Dropped(Duration),
    Sent(Duration, Duration),
}

pub struct Capturer {
    /// `None` when the frame source is the synthetic one (`RD_FAKE_CAPTURE`), which has no
    /// compositor stream behind it.
    inner: Option<quartz::Capturer>,
    /// Measurement-only synthetic source: the size it reports, plus the generator itself.
    fake: Option<(usize, usize, quartz::fake::Source)>,
    frame: Arc<Mutex<Option<quartz::Frame>>>,
    /// The previous frame's surface, retained instead of copied: the compare reads both ranges
    /// where they already live.
    prev_surface: Option<quartz::RetainedSurface>,
    budget: FrameBudget,
}

impl Capturer {
    pub fn new(display: Display) -> io::Result<Capturer> {
        let frame = Arc::new(Mutex::new(None));

        if let Some(spec) = quartz::fake::Spec::from_env(FAKE_CAPTURE_ENV) {
            let (w, h) = (spec.width, spec.height);
            let (pattern, fps, pool) = (spec.pattern.name(), spec.fps, spec.pool);
            let source = quartz::fake::Source::new(spec);
            hbb_common::log::warn!(
                "[FAKE] synthetic capture source {w}x{h} pattern={pattern} fps={fps} pool={pool}; numbers are not from a real screen"
            );
            return Ok(Capturer {
                inner: None,
                fake: Some((w, h, source)),
                frame,
                prev_surface: None,
                budget: FrameBudget::default(),
            });
        }

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
            inner: Some(inner),
            fake: None,
            frame,
            prev_surface: None,
            budget: FrameBudget::default(),
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
        let id_hit = match &self.prev_surface {
            Some(prev) => prev.same_handle(frame),
            None => false,
        };
        if id_hit {
            self.budget.identity += 1;
        }
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

    /// Add one poll to the current window and, every five seconds, say how the frames were spent:
    /// `dedupe=` is the compare and the retain, `bgra=` the surface-to-BGRA copy, `dropped` the
    /// frames that compared identical, `sent` the frames handed on. `none` counts polls that
    /// produced no frame at all - either the compositor had nothing new or its callback held the
    /// mutex - so it cannot be read as "the screen is still".
    /// Time the synthetic source spent producing a frame (fill + pacing).
    fn note_gen(&mut self, took: Duration) {
        self.budget.gen += took;
    }

    /// Record the surface a frame arrived on.
    fn note_surface(&mut self, id: u32) {
        if !self.budget.seen.contains(&id) {
            self.budget.seen.push(id);
            self.budget.distinct = self.budget.seen.len() as u64;
        }
        if self.budget.last_id == Some(id) {
            self.budget.repeats += 1;
        }
        self.budget.last_id = Some(id);
    }

    fn note(&mut self, outcome: FrameOutcome) {
        let now = Instant::now();
        let since = *self.budget.since.get_or_insert(now);
        match outcome {
            FrameOutcome::Empty => self.budget.none += 1,
            FrameOutcome::Dropped(compare) => {
                self.budget.dedupe += compare;
                self.budget.dropped += 1;
            }
            FrameOutcome::Sent(compare, copy) => {
                self.budget.dedupe += compare;
                self.budget.bgra += copy;
                self.budget.sent += 1;
            }
        }
        let window = now - since;
        if window < Duration::from_secs(5) {
            return;
        }
        let secs = window.as_secs_f32();
        hbb_common::log::info!(
            "[FRAME] {:.1}s none={} sent={} dropped={} dedupe={:.1} bgra={:.1} gen={:.1} surf={} same={} id={} ms/s",
            secs,
            self.budget.none,
            self.budget.sent,
            self.budget.dropped,
            self.budget.dedupe.as_secs_f32() * 1000.0 / secs,
            self.budget.bgra.as_secs_f32() * 1000.0 / secs,
            self.budget.gen.as_secs_f32() * 1000.0 / secs,
            self.budget.distinct,
            self.budget.repeats,
            self.budget.identity,
        );
        self.budget = FrameBudget::default();
        self.budget.since = Some(now);
    }

    pub fn width(&self) -> usize {
        match &self.fake {
            Some((w, _, _)) => *w,
            None => self.inner.as_ref().map(|i| i.width()).unwrap_or(0),
        }
    }

    pub fn height(&self) -> usize {
        match &self.fake {
            Some((_, h, _)) => *h,
            None => self.inner.as_ref().map(|i| i.height()).unwrap_or(0),
        }
    }
}

impl crate::TraitCapturer for Capturer {
    fn frame<'a>(&'a mut self, _timeout_ms: std::time::Duration) -> io::Result<Frame<'a>> {
        // Take the frame out and let the guard go before touching `self`: keeping the reference to
        // the previous surface needs `&mut self`, and the guard borrows `self.frame` while it lives.
        let mut gen = None;
        let frame = if let Some((_, _, source)) = self.fake.as_mut() {
            let started = Instant::now();
            let frame = source.next_frame();
            gen = Some(started.elapsed());
            frame
        } else {
            match self.frame.try_lock() {
                Ok(mut handle) => handle.take(),
                Err(TryLockError::WouldBlock) => return Err(io::ErrorKind::WouldBlock.into()),
                Err(TryLockError::Poisoned(..)) => return Err(io::ErrorKind::Other.into()),
            }
        };

        if let Some(gen) = gen {
            self.note_gen(gen);
        }
        match frame {
            Some(mut frame) => {
                self.note_surface(frame.surface_id());
                let compare = Instant::now();
                let same = self.same_as_previous(&frame);
                let compare = compare.elapsed();
                if same {
                    self.note(FrameOutcome::Dropped(compare));
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                let copy = Instant::now();
                frame.surface_to_bgra(self.height());
                self.note(FrameOutcome::Sent(compare, copy.elapsed()));
                Ok(Frame::PixelBuffer(PixelBuffer {
                    frame,
                    data: PhantomData,
                    width: self.width(),
                    height: self.height(),
                }))
            }

            None => {
                self.note(FrameOutcome::Empty);
                Err(io::ErrorKind::WouldBlock.into())
            }
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
