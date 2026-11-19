use crate::{quartz, Frame, Pixfmt};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};
use std::{io, mem};

/// Measurement-only switch for the synthetic frame source; see `quartz::fake`.
const FAKE_CAPTURE_ENV: &str = "RD_FAKE_CAPTURE";

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
    /// Time the synthetic source spent producing the frame (its own fill + pacing), when there is
    /// one. Without this column a window cannot tell "the pipeline is slow" from "our generator is
    /// slow", and the generator is the one thing whose cost is not in the real path at all.
    gen: Duration,
    none: u64,
    sent: u64,
    dropped: u64,
    /// Distinct surface handles seen in this window, and how often a frame arrived on the same
    /// handle as the one before it. Those two numbers are what attribute `sent`: a sent frame on a
    /// repeated handle is the identity rule answering "different" without comparing, which is the
    /// difference between "the content changed" and "the compositor recycled a buffer".
    distinct: u64,
    repeats: u64,
    seen: Vec<u32>,
    last_id: Option<u32>,
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
    /// On `ci/fakecap-m5-*` the hand-off no longer copies: the frame is handed on as the locked
    /// plane, so `bgra=` times the plane resolution instead and reads ≈0 by construction. Compare
    /// `dedupe=`, `sent=`, `dropped=` and the host process CPU, not `bgra=`.
    ///
    /// `none` counts polls that produced no frame at all, so a window with `sent=0 dropped=0` is
    /// void: it says nothing about the screen. Polls that lost the mutex are not counted at all.
    ///
    /// The two helpers below feed the same window before `note` runs. Time the synthetic source
    /// spent producing a frame (fill + pacing). Only the scaffold has
    /// one; the real compositor's cost is not observable from here and must not be invented.
    fn note_gen(&mut self, took: Duration) {
        self.gen += took;
    }

    /// Record the surface a frame arrived on: `surf=` counts distinct handles in this window,
    /// `same=` counts frames whose handle equals the previous frame's.
    fn note_surface(&mut self, id: u32) {
        if !self.seen.contains(&id) {
            self.seen.push(id);
            self.distinct = self.seen.len() as u64;
        }
        if self.last_id == Some(id) {
            self.repeats += 1;
        }
        self.last_id = Some(id);
    }

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
            "[FRAME] {:.1}s none={} sent={} dropped={} dedupe={:.1} bgra={:.1} gen={:.1} surf={} same={} ms/s",
            secs,
            self.none,
            self.sent,
            self.dropped,
            self.dedupe.as_secs_f32() * 1000.0 / secs,
            self.bgra.as_secs_f32() * 1000.0 / secs,
            self.gen.as_secs_f32() * 1000.0 / secs,
            self.distinct,
            self.repeats,
        );
        *self = FrameBudget::default();
        self.since = Some(now);
    }
}

pub struct Capturer {
    /// `None` when the frame source is the synthetic one (`RD_FAKE_CAPTURE`), which has no
    /// compositor stream behind it.
    inner: Option<quartz::Capturer>,
    /// Measurement-only synthetic source: the size it reports, plus the generator itself.
    fake: Option<(usize, usize, quartz::fake::Source)>,
    frame: Arc<Mutex<Option<quartz::Frame>>>,
    saved_raw_data: Vec<u8>, // for faster compare and copy
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
                saved_raw_data: Vec::new(),
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
            saved_raw_data: Vec::new(),
            budget: FrameBudget::default(),
        })
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
        let mut gen = None;
        let frame = if let Some((_, _, source)) = self.fake.as_mut() {
            let started = Instant::now();
            let frame = source.next_frame();
            gen = Some(started.elapsed());
            frame
        } else {
            match self.frame.try_lock() {
                Ok(mut handle) => {
                    let mut frame = None;
                    mem::swap(&mut frame, &mut handle);
                    frame
                }
                Err(TryLockError::WouldBlock) => return Err(io::ErrorKind::WouldBlock.into()),
                Err(TryLockError::Poisoned(..)) => return Err(io::ErrorKind::Other.into()),
            }
        };
        if let Some(gen) = gen {
            self.budget.note_gen(gen);
        }

        match frame {
            Some(frame) => {
                self.budget.note_surface(frame.surface_id());
                let dedupe = Instant::now();
                let verdict = crate::would_block_if_equal(&mut self.saved_raw_data, frame.inner());
                let dedupe = dedupe.elapsed();
                if let Err(e) = verdict {
                    self.budget.note(FrameOutcome::Dropped(dedupe));
                    return Err(e);
                }
                // M5 arm: the frame is handed on in place, so this bucket times the plane
                // resolution that replaces the copy instead of the copy itself.
                let handoff = Instant::now();
                let _ = frame.plane(self.height());
                self.budget.note(FrameOutcome::Sent(dedupe, handoff.elapsed()));
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
}

pub struct PixelBuffer<'a> {
    frame: quartz::Frame,
    data: PhantomData<&'a [u8]>,
    width: usize,
    height: usize,
}

impl<'a> crate::TraitPixelBuffer for PixelBuffer<'a> {
    fn data(&self) -> &[u8] {
        self.frame.plane(self.height).0
    }

    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn stride(&self) -> Vec<usize> {
        let mut v = Vec::new();
        v.push(self.frame.plane(self.height).1);
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
