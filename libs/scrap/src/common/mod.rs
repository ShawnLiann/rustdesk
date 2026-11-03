pub use self::vpxcodec::*;
use base::message_proto::{video_frame, Chroma, VideoFrame};
use hbb_common::{bail, log, ResultType};
use std::{ffi::c_void, slice};

cfg_if! {
    if #[cfg(quartz)] {
        mod quartz;
        pub use self::quartz::*;
    } else if #[cfg(x11)] {
        cfg_if! {
            if #[cfg(feature="wayland")] {
                mod linux;
                mod wayland;
                mod x11;
                #[cfg(all(target_os = "linux", feature = "drm"))]
                pub mod drmtap_dl;
                #[cfg(all(target_os = "linux", feature = "drm"))]
                pub mod drm_reader;
                #[cfg(all(target_os = "linux", feature = "drm"))]
                pub mod drm_render;
                pub use self::linux::*;
                pub use self::wayland::set_map_err;
                pub use self::x11::PixelBuffer;
            } else {
                mod x11;
                pub use self::x11::*;
            }
        }
    } else if #[cfg(dxgi)] {
        mod dxgi;
        pub use self::dxgi::*;
    } else if #[cfg(target_os = "android")] {
        mod android;
        pub use self::android::*;
    }else {
        //TODO: Fallback implementation.
    }
}

pub mod codec;
pub mod convert;
#[cfg(feature = "hwcodec")]
pub mod hwcodec;
#[cfg(feature = "mediacodec")]
pub mod mediacodec;
pub mod vpxcodec;
#[cfg(feature = "vram")]
pub mod vram;
pub use self::convert::*;
pub const STRIDE_ALIGN: usize = 64; // commonly used in libvpx vpx_img_alloc caller
pub const HW_STRIDE_ALIGN: usize = 0; // recommended by av_frame_get_buffer

pub mod aom;
#[cfg(not(any(target_os = "ios")))]
pub mod camera;
pub mod record;
mod vpx;

#[repr(usize)]
#[derive(Debug, Copy, Clone)]
pub enum ImageFormat {
    Raw,
    ABGR,
    ARGB,
}

#[repr(C)]
#[derive(Clone)]
pub struct ImageRgb {
    pub raw: Vec<u8>,
    pub w: usize,
    pub h: usize,
    pub fmt: ImageFormat,
    pub align: usize,
}

impl ImageRgb {
    pub fn new(fmt: ImageFormat, align: usize) -> Self {
        Self {
            raw: Vec::new(),
            w: 0,
            h: 0,
            fmt,
            align,
        }
    }

    #[inline]
    pub fn fmt(&self) -> ImageFormat {
        self.fmt
    }

    #[inline]
    pub fn align(&self) -> usize {
        self.align
    }

    #[inline]
    pub fn set_align(&mut self, align: usize) {
        self.align = align;
    }
}

pub struct ImageTexture {
    pub texture: *mut c_void,
    pub w: usize,
    pub h: usize,
}

impl Default for ImageTexture {
    fn default() -> Self {
        Self {
            texture: std::ptr::null_mut(),
            w: 0,
            h: 0,
        }
    }
}

#[inline]
pub fn would_block_if_equal(old: &mut Vec<u8>, b: &[u8]) -> std::io::Result<()> {
    // does this really help?
    if b == &old[..] {
        return Err(std::io::ErrorKind::WouldBlock.into());
    }
    old.resize(b.len(), 0);
    old.copy_from_slice(b);
    Ok(())
}

pub trait TraitCapturer {
    // We doesn't support
    #[cfg(not(any(target_os = "ios")))]
    fn frame<'a>(&'a mut self, timeout: std::time::Duration) -> std::io::Result<Frame<'a>>;

    #[cfg(windows)]
    fn is_gdi(&self) -> bool;
    #[cfg(windows)]
    fn set_gdi(&mut self) -> bool;

    #[cfg(feature = "vram")]
    fn device(&self) -> AdapterDevice;

    #[cfg(feature = "vram")]
    fn set_output_texture(&mut self, texture: bool);
}

#[derive(Debug, Clone, Copy)]
pub struct AdapterDevice {
    pub device: *mut c_void,
    pub vendor_id: ::std::os::raw::c_uint,
    pub luid: i64,
}

impl Default for AdapterDevice {
    fn default() -> Self {
        Self {
            device: std::ptr::null_mut(),
            vendor_id: Default::default(),
            luid: Default::default(),
        }
    }
}

pub trait TraitPixelBuffer {
    fn data(&self) -> &[u8];

    fn width(&self) -> usize;

    fn height(&self) -> usize;

    fn stride(&self) -> Vec<usize>;

    fn pixfmt(&self) -> Pixfmt;

    /// Cheap emptiness test: a surface-backed frame must not have to copy itself just to answer it.
    fn is_empty(&self) -> bool {
        self.data().is_empty()
    }
}

#[cfg(not(any(target_os = "ios")))]
pub enum Frame<'a> {
    PixelBuffer(PixelBuffer<'a>),
    Texture((*mut c_void, usize)),
}

#[cfg(not(any(target_os = "ios")))]
impl Frame<'_> {
    pub fn valid<'a>(&'a self) -> bool {
        match self {
            Frame::PixelBuffer(pixelbuffer) => !pixelbuffer.is_empty(),
            Frame::Texture((texture, _)) => !texture.is_null(),
        }
    }

    pub fn to<'a>(
        &'a self,
        yuvfmt: EncodeYuvFormat,
        yuv: &'a mut Vec<u8>,
        mid_data: &mut Vec<u8>,
    ) -> ResultType<EncodeInput<'a>> {
        match self {
            Frame::PixelBuffer(pixelbuffer) => {
                #[cfg(target_os = "macos")]
                if encode_pixbuf_enabled() && wraps_video_toolbox_codec() {
                    return pixbuf_or_yuv(
                        true,
                        !nv12_capture_enabled(),
                        || pixelbuffer.create_cvpixelbuffer(),
                        |yuv| convert_to_yuv(&pixelbuffer, yuvfmt, yuv, mid_data),
                        yuv,
                    );
                }
                convert_to_yuv(&pixelbuffer, yuvfmt, yuv, mid_data)?;
                Ok(EncodeInput::YUV(yuv))
            }
            Frame::Texture(texture) => Ok(EncodeInput::Texture(*texture)),
        }
    }
}

/// Owns one reference on a CVPixelBufferRef and releases it when the frame is done being encoded.
#[cfg(target_os = "macos")]
pub struct CVPixelBufferHandle(pub *mut c_void);

#[cfg(target_os = "macos")]
impl Drop for CVPixelBufferHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { crate::quartz::ffi::CFRelease(self.0 as *const c_void) }
        }
    }
}

#[cfg(target_os = "macos")]
impl CVPixelBufferHandle {
    /// Take one more reference on the same buffer so it can be encoded again later.
    ///
    /// A reference count bump, not a copy: this is what lets the pixel-buffer path re-encode the
    /// previous frame without keeping its pixels in RAM.
    pub fn retained(&self) -> Self {
        if !self.0.is_null() {
            unsafe { crate::quartz::ffi::CFRetain(self.0 as *const c_void) };
        }
        Self(self.0)
    }
}

#[cfg(target_os = "macos")]
fn announce_pixbuf_once() {
    use std::sync::OnceLock;
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        log::info!("encode path: CVPixelBuffer -> VideoToolbox (zero copy), RD_ENCODE_PIXBUF=1")
    });
}

/// The decode side of this feature logs a running refusal count so a session can prove which path it
/// took. The encode side had only a one-shot announcement, which cannot answer "how many frames
/// actually went direct" in an A/B window. These two counters are that answer.
#[cfg(target_os = "macos")]
mod pixbuf_stats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static WRAPPED: AtomicU64 = AtomicU64::new(0);
    static FELL_BACK: AtomicU64 = AtomicU64::new(0);

    pub fn wrapped() {
        WRAPPED.fetch_add(1, Ordering::Relaxed);
    }

    pub fn fell_back() {
        FELL_BACK.fetch_add(1, Ordering::Relaxed);
    }

    pub fn counts() -> (u64, u64) {
        (
            WRAPPED.load(Ordering::Relaxed),
            FELL_BACK.load(Ordering::Relaxed),
        )
    }
}

/// Wrap the captured surface as a `CVPixelBuffer` when that works, otherwise convert to YUV.
///
/// The two halves are arguments so that the fallback can be tested without an IOSurface the system
/// refuses to wrap: a regression that returns early on a null handle would drop every frame while
/// every other test still passed, which is the failure this signature exists to make visible.
#[cfg(target_os = "macos")]
/// A software encoder takes a yuv slice and fails on a pixel buffer, so only the VideoToolbox
/// codecs may be handed one.
#[cfg(target_os = "macos")]
fn wraps_video_toolbox_codec() -> bool {
    matches!(
        crate::codec::Encoder::negotiated_codec(),
        CodecFormat::H264 | CodecFormat::H265
    )
}

fn pixbuf_or_yuv<'a>(
    enabled: bool,
    software_ok: bool,
    wrap: impl FnOnce() -> *mut std::ffi::c_void,
    convert: impl FnOnce(&mut Vec<u8>) -> ResultType<()>,
    yuv: &'a mut Vec<u8>,
) -> ResultType<EncodeInput<'a>> {
    if enabled {
        let pb = wrap();
        if !pb.is_null() {
            pixbuf_stats::wrapped();
            announce_pixbuf_once();
            return Ok(EncodeInput::PixelBuffer(CVPixelBufferHandle(pb)));
        }
        pixbuf_stats::fell_back();
        // Frame-driven failure: throttle it, an unthrottled warn here would emit one line per frame
        // on a machine that cannot wrap the surface.
        hbb_common::throttled_log!(
            std::time::Duration::from_secs(30),
            warn,
            "CVPixelBufferCreateWithIOSurface failed, falling back to yuv ({} wrapped / {} fell back so far)",
            pixbuf_stats::counts().0,
            pixbuf_stats::counts().1
        );
        // A biplanar surface has no packed BGRA bytes to convert from, so the yuv fallback would
        // hand the encoder mis-typed pixels. Dropping the frame is the honest outcome: the loop
        // already treats "no frame" as a hiccup, and a picture built from a luma plane would not
        // look like an error to anyone watching the peer.
        if !software_ok {
            bail!("nv12 surface could not be wrapped, frame dropped");
        }
    }
    convert(yuv)?;
    Ok(EncodeInput::YUV(yuv))
}

/// The direct encode path changes what the encoder is fed, so it stays off unless the
/// environment names it exactly. Anything else - unset, empty, "true", " 1" - is off.
#[cfg(target_os = "macos")]
fn parse_pixbuf_env(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

#[cfg(target_os = "macos")]
fn encode_pixbuf_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        parse_pixbuf_env(std::env::var("RD_ENCODE_PIXBUF").ok().as_deref())
    })
}

/// The byte compare can only answer "same as last frame" by keeping a heap copy of the surface, so
/// an arm with `RD_NO_DEDUPE=1` drops both the compare and that copy to price what it buys.
#[cfg(target_os = "macos")]
fn parse_no_dedupe_env(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

/// `RD_DEDUPE_ZC=1` compares the new surface against a retained reference to the previous one
/// instead of against a heap copy: same verdict, no 37 MB copy per frame. At 4096x2304 the copy is
/// ~26 ms per frame, which on a nearly-static screen is the largest single item in the loop - larger
/// than the encode it protects.
#[cfg(target_os = "macos")]
fn parse_dedupe_zc_env(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

#[cfg(target_os = "macos")]
pub fn dedupe_zc_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| parse_dedupe_zc_env(std::env::var("RD_DEDUPE_ZC").ok().as_deref()))
}

#[cfg(target_os = "macos")]
pub fn no_dedupe_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| parse_no_dedupe_env(std::env::var("RD_NO_DEDUPE").ok().as_deref()))
}

/// `RD_DIFF_ACCUM=<n>` makes the dedupe a change budget instead of an exact compare: every frame adds
/// the number of differing *sampled* bytes it finds, a frame is delivered only once that running total
/// reaches `n`, and delivering resets it to zero. Unset, `0`, or anything unparsable keeps the exact
/// compare. The exact compare answers 0/1, so a clock second costs a full encode; this one answers
/// "has enough of the picture changed to be worth a frame yet".
#[cfg(any(target_os = "macos", test))]
fn parse_diff_accum_env(value: Option<&str>) -> Option<u64> {
    match value?.trim().parse::<u64>() {
        Ok(n) if n > 0 => Some(n),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
pub fn diff_accum_threshold() -> Option<u64> {
    use std::sync::OnceLock;
    static THRESHOLD: OnceLock<Option<u64>> = OnceLock::new();
    *THRESHOLD.get_or_init(|| parse_diff_accum_env(std::env::var("RD_DIFF_ACCUM").ok().as_deref()))
}

/// `RD_DIFF_SAMPLE=<n>` compares one contiguous 16 KB chunk out of every `n` of them, i.e. `1/n` of the
/// surface. Chunks rather than every n-th byte: a byte stride scatters one read per 64 bytes across
/// 37 MB, and measured here that costs the same as comparing the whole thing, while contiguous chunks
/// stay inside cache lines the CPU already fetched. `1` compares everything.
#[cfg(any(target_os = "macos", test))]
fn parse_diff_sample_env(value: Option<&str>) -> usize {
    match value.and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(n) if n > 0 => n.min(1024),
        _ => 16,
    }
}

#[cfg(target_os = "macos")]
pub fn diff_sample() -> usize {
    use std::sync::OnceLock;
    static SAMPLE: OnceLock<usize> = OnceLock::new();
    *SAMPLE.get_or_init(|| {
        // `RD_DIFF_STRIDE` was the old name for the same density; keep reading it so an arm recorded
        // against the previous build keeps meaning what it said.
        let value = std::env::var("RD_DIFF_SAMPLE")
            .ok()
            .or_else(|| std::env::var("RD_DIFF_STRIDE").ok());
        parse_diff_sample_env(value.as_deref())
    })
}

/// `RD_DIFF_MAX_SKIP=<n>` is the starvation guard: after `n` frames skipped in a row the next one is
/// delivered whatever the budget says. Without it a screen whose only motion is a small scratched
/// region could accumulate forever and never reach the threshold.
#[cfg(any(target_os = "macos", test))]
fn parse_diff_max_skip_env(value: Option<&str>) -> usize {
    match value.and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(n) if n > 0 => n,
        _ => 30,
    }
}

#[cfg(target_os = "macos")]
pub fn diff_max_skip() -> usize {
    use std::sync::OnceLock;
    static MAX_SKIP: OnceLock<usize> = OnceLock::new();
    *MAX_SKIP
        .get_or_init(|| parse_diff_max_skip_env(std::env::var("RD_DIFF_MAX_SKIP").ok().as_deref()))
}

/// The whole rule in one place: spend the accumulated change budget, and deliver only when it covers
/// the threshold or the skip guard fires. Returns `(deliver, next_accum, next_skipped)`; a delivery
/// restarts both counters.
#[cfg(any(target_os = "macos", test))]
/// The knobs the change-driven frame rate is built from, read once.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiffTune {
    /// Estimated bytes/second of change at which the cap is earned. Below it the rate is proportional.
    pub soft: f64,
    /// How often the surface may be compared while the picture is still, in Hz. This is the idle CPU
    /// cost: at 10 Hz a 4K compare that costs ~1 ms runs 10 times a second instead of 32-75.
    pub probe_hz: f64,
    /// Highest rate the change alone can earn; the client's own request and the host loop still cap it.
    pub max_fps: f64,
    /// The rate a frozen screen falls back to. One frame a second keeps the peer from looking dead.
    pub min_fps: f64,
}

impl DiffTune {
    pub const DEFAULT: DiffTune = DiffTune {
        soft: 2_000_000.0,
        probe_hz: 10.0,
        max_fps: 60.0,
        min_fps: 1.0,
    };
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn parse_diff_tune(
    soft: Option<&str>,
    probe_hz: Option<&str>,
    max_fps: Option<&str>,
    min_fps: Option<&str>,
) -> DiffTune {
    let number = |value: Option<&str>, fallback: f64, low: f64, high: f64| -> f64 {
        match value.and_then(|v| v.trim().parse::<f64>().ok()) {
            Some(n) if n > 0.0 && n.is_finite() => n.clamp(low, high),
            _ => fallback,
        }
    };
    DiffTune {
        soft: number(soft, DiffTune::DEFAULT.soft, 1.0, 1e12),
        probe_hz: number(probe_hz, DiffTune::DEFAULT.probe_hz, 0.2, 240.0),
        max_fps: number(max_fps, DiffTune::DEFAULT.max_fps, 1.0, 240.0),
        min_fps: number(min_fps, DiffTune::DEFAULT.min_fps, 0.2, 240.0),
    }
}

#[cfg(target_os = "macos")]
pub fn diff_tune() -> DiffTune {
    use std::sync::OnceLock;
    static TUNE: OnceLock<DiffTune> = OnceLock::new();
    *TUNE.get_or_init(|| {
        parse_diff_tune(
            std::env::var("RD_DIFF_SOFT").ok().as_deref(),
            std::env::var("RD_DIFF_PROBE_HZ").ok().as_deref(),
            std::env::var("RD_DIFF_MAX_FPS").ok().as_deref(),
            std::env::var("RD_DIFF_MIN_FPS").ok().as_deref(),
        )
    })
}

/// The rate a change of `rate` bytes/second has earned: `min_fps` when nothing moves, climbing to
/// `max_fps` once the change reaches `soft`. Straight line rather than a step, so a small change buys a
/// proportionally small number of frames instead of either none or all of them.
pub(crate) fn target_fps(rate: f64, tune: DiffTune) -> f64 {
    if !rate.is_finite() || rate <= 0.0 {
        return tune.min_fps;
    }
    let earned = (rate / tune.soft).clamp(0.0, 1.0);
    let span = (tune.max_fps - tune.min_fps).max(0.0);
    (tune.min_fps + span * earned).clamp(
        tune.min_fps.min(tune.max_fps),
        tune.max_fps.max(tune.min_fps),
    )
}

/// How long to wait before looking at the surface again. The compare happens at the frame rate the
/// change has earned, but never slower than `probe_hz` - a frozen screen still needs to be checked
/// often enough that a real change is noticed within ~`1000 / probe_hz` ms.
pub(crate) fn compare_interval_ms(target_fps: f64, tune: DiffTune) -> f64 {
    let looking_at = target_fps.max(tune.probe_hz);
    1000.0 / looking_at.max(0.2)
}

pub(crate) fn diff_budget_decision(
    accum: u64,
    diff: u64,
    threshold: u64,
    skipped: usize,
    max_skip: usize,
    force: bool,
) -> (bool, u64, usize) {
    let accum = accum.saturating_add(diff);
    // `force` is a change the eye cannot miss - one whole sampled chunk rewritten, or a surface that
    // could not be compared at all - and no budget should defer it.
    if force || accum >= threshold || skipped >= max_skip {
        (true, 0, 0)
    } else {
        (false, accum, skipped + 1)
    }
}

/// `RD_LAZY_BGRA=0` restores the eager surface copy taken at capture time, so an arm can price what
/// materialising the BGRA buffer on first use buys. Unset, or anything but `0`, stays lazy.
#[cfg(target_os = "macos")]
fn parse_lazy_bgra_env(value: Option<&str>) -> bool {
    !matches!(value.map(str::trim), Some("0"))
}

#[cfg(target_os = "macos")]
pub fn lazy_bgra_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| parse_lazy_bgra_env(std::env::var("RD_LAZY_BGRA").ok().as_deref()))
}

/// The per-frame budget lines repeat every five seconds for the life of a session; `RD_LOAD_LOG=0`
/// silences them for a build that is not being measured, without dropping the counters behind them.
#[cfg(target_os = "macos")]
fn parse_load_log_env(value: Option<&str>) -> bool {
    !matches!(value.map(str::trim), Some("0"))
}

#[cfg(target_os = "macos")]
pub fn load_log_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| parse_load_log_env(std::env::var("RD_LOAD_LOG").ok().as_deref()))
}

/// `RD_DIFF_LOG=1` prints the change-budget distribution once every five seconds. Off by default: a
/// build that is not being tuned keeps the log byte-identical to what it was before the budget existed,
/// which is the only way an A/B on the log itself stays honest.
fn parse_diff_log_env(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("1"))
}

#[cfg(target_os = "macos")]
pub fn diff_log_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| parse_diff_log_env(std::env::var("RD_DIFF_LOG").ok().as_deref()))
}

/// `RD_NO_REPEAT=1` stops the WouldBlock arm from re-encoding the previous frame, so both the cost
/// of the static-screen repeat and the peer's staleness without it can be priced in one build.
fn parse_no_repeat_env(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

pub fn no_repeat_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| parse_no_repeat_env(std::env::var("RD_NO_REPEAT").ok().as_deref()))
}

/// `RD_PIXBUF_NV12=1` asks the screen capture for a biplanar `420v` surface, so VideoToolbox encodes
/// the frame without converting BGRA to NV12 itself - at 4096x2304 that conversion runs on the same
/// serialised per-frame path the zero-copy work exists to shorten. Only the pixel-buffer encode path
/// can consume a surface without reading its pixels, so every other path keeps the packed ARGB8888
/// surface it converts from.
#[cfg(target_os = "macos")]
fn parse_nv12_capture_env(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

#[cfg(target_os = "macos")]
pub fn nv12_capture_env() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| parse_nv12_capture_env(std::env::var("RD_PIXBUF_NV12").ok().as_deref()))
}

/// Deliberately not cached: the negotiated codec can change mid-process, and with it whether the
/// encoder will take a surface at all.
#[cfg(target_os = "macos")]
pub fn nv12_capture_enabled() -> bool {
    nv12_capture_env() && encode_pixbuf_enabled() && wraps_video_toolbox_codec()
}

/// `RD_LOG_SPLIT=1` adds `enc=` / `send=` / `qos_fps=` to the per-frame budget line, splitting the
/// one `encode=` bucket so a slow frame can be blamed on VideoToolbox or on the send. The counters
/// are always accumulated; only the line changes.
#[cfg(target_os = "macos")]
fn parse_log_split_env(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

#[cfg(target_os = "macos")]
pub fn log_split_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| parse_log_split_env(std::env::var("RD_LOG_SPLIT").ok().as_deref()))
}

#[cfg(all(test, target_os = "macos"))]
mod switch_env_tests {
    use super::{parse_lazy_bgra_env, parse_load_log_env, parse_no_repeat_env};

    #[test]
    fn lazy_bgra_stays_on_unless_switched_off() {
        for value in [None, Some("1"), Some("true"), Some("")] {
            assert!(
                parse_lazy_bgra_env(value),
                "{value:?} must keep the copy lazy"
            );
        }
        assert!(!parse_lazy_bgra_env(Some("0")));
        assert!(!parse_lazy_bgra_env(Some(" 0 ")));
    }

    #[test]
    fn load_log_stays_on_unless_switched_off() {
        for value in [None, Some("1"), Some("true")] {
            assert!(parse_load_log_env(value), "{value:?} must keep logging");
        }
        assert!(!parse_load_log_env(Some("0")));
    }

    #[test]
    fn no_repeat_only_the_exact_value_one_enables_it() {
        assert!(!parse_no_repeat_env(None));
        assert!(parse_no_repeat_env(Some("1")));
        for value in ["0", "", "true", "yes", " 1"] {
            assert!(
                !parse_no_repeat_env(Some(value)),
                "{value:?} must not disable the repeat"
            );
        }
    }

    #[test]
    fn log_split_only_the_exact_value_one_enables_it() {
        assert!(!super::parse_log_split_env(None));
        assert!(super::parse_log_split_env(Some("1")));
        for value in ["0", "", "true", "on", " 1"] {
            assert!(
                !super::parse_log_split_env(Some(value)),
                "{value:?} must not split the line"
            );
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod no_dedupe_env_tests {
    use super::parse_no_dedupe_env;

    #[test]
    fn no_dedupe_off_when_the_variable_is_absent() {
        assert!(!parse_no_dedupe_env(None));
    }

    #[test]
    fn no_dedupe_only_the_exact_value_one_enables_it() {
        assert!(parse_no_dedupe_env(Some("1")));
        for value in ["0", "", "true", "yes", "2", " 1", "1 "] {
            assert!(
                !parse_no_dedupe_env(Some(value)),
                "{value:?} must not enable it"
            );
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod pixbuf_env_tests {
    use super::parse_pixbuf_env;

    #[test]
    fn off_when_the_variable_is_absent() {
        assert!(!parse_pixbuf_env(None));
    }

    #[test]
    fn only_the_exact_value_one_enables_it() {
        assert!(parse_pixbuf_env(Some("1")));
        for value in ["0", "", "true", "yes", "2", " 1", "1 "] {
            assert!(!parse_pixbuf_env(Some(value)), "{:?} must not enable it", value);
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod nv12_capture_env_tests {
    use super::parse_nv12_capture_env;

    #[test]
    fn nv12_capture_only_the_exact_value_one_enables_it() {
        assert!(!parse_nv12_capture_env(None));
        assert!(parse_nv12_capture_env(Some("1")));
        for value in ["0", "", "true", "yes", "2", " 1", "1 "] {
            assert!(
                !parse_nv12_capture_env(Some(value)),
                "{value:?} must keep the packed surface"
            );
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod dedupe_zc_env_tests {
    use super::parse_dedupe_zc_env;

    #[test]
    fn dedupe_zc_only_the_exact_value_one_enables_it() {
        assert!(!parse_dedupe_zc_env(None));
        assert!(parse_dedupe_zc_env(Some("1")));
        for value in ["0", "", "true", "yes", "2", " 1", "1 "] {
            assert!(
                !parse_dedupe_zc_env(Some(value)),
                "{value:?} must keep the copy path"
            );
        }
    }
}

#[cfg(test)]
mod diff_budget_tests {
    use super::{
        compare_interval_ms, diff_budget_decision, parse_diff_accum_env, parse_diff_max_skip_env,
        parse_diff_sample_env, parse_diff_tune, target_fps, DiffTune,
    };

    #[test]
    fn the_budget_only_delivers_once_the_accumulated_change_covers_it() {
        let (deliver, accum, skipped) = diff_budget_decision(0, 100, 300, 0, 30, false);
        assert!(!deliver, "a third of the budget is not a frame");
        assert_eq!((accum, skipped), (100, 1));

        let (deliver, accum, _) = diff_budget_decision(accum, 100, 300, skipped, 30, false);
        assert!(!deliver);
        assert_eq!(accum, 200);

        // The third frame crosses it, and a delivery restarts both counters.
        let (deliver, accum, skipped) = diff_budget_decision(accum, 100, 300, 2, 30, false);
        assert!(deliver);
        assert_eq!((accum, skipped), (0, 0));
    }

    #[test]
    fn one_fully_changed_frame_is_worth_a_frame_on_its_own() {
        let (deliver, _, _) = diff_budget_decision(0, u64::MAX, 300, 0, 30, false);
        assert!(
            deliver,
            "an incomparable surface must not be swallowed by the budget"
        );
    }

    #[test]
    fn the_skip_guard_fires_even_if_nothing_ever_accumulates() {
        let (deliver, _, _) = diff_budget_decision(0, 0, 300, 30, 30, false);
        assert!(
            deliver,
            "30 idempotent frames in a row still need one delivered"
        );
        let (deliver, accum, _) = diff_budget_decision(0, 0, 300, 29, 30, false);
        assert!(!deliver);
        assert_eq!(accum, 0);
    }

    #[test]
    fn a_frozen_screen_gets_the_floor_rate_and_a_big_change_earns_the_cap() {
        let tune = DiffTune::DEFAULT;
        assert_eq!(target_fps(0.0, tune), tune.min_fps);
        assert_eq!(target_fps(-5.0, tune), tune.min_fps, "no negative rates");
        assert_eq!(target_fps(f64::NAN, tune), tune.min_fps);
        assert_eq!(target_fps(tune.soft, tune), tune.max_fps);
        assert_eq!(
            target_fps(tune.soft * 10.0, tune),
            tune.max_fps,
            "no overshoot"
        );
        let half = target_fps(tune.soft / 2.0, tune);
        assert!(
            half > tune.min_fps && half < tune.max_fps,
            "{half} must sit between"
        );
        // Monotonic: more change never earns fewer frames.
        let mut previous = 0.0;
        for step in 0..20 {
            let fps = target_fps(tune.soft * step as f64 / 10.0, tune);
            assert!(fps >= previous);
            previous = fps;
        }
    }

    #[test]
    fn the_look_is_never_slower_than_the_probe_rate() {
        let tune = DiffTune::DEFAULT;
        // A frozen screen earns the floor rate, but we still look `probe_hz` times a second.
        assert_eq!(
            compare_interval_ms(tune.min_fps, tune),
            1000.0 / tune.probe_hz
        );
        // Once the change earns more than the probe rate, the frame rate wins.
        assert_eq!(
            compare_interval_ms(tune.max_fps, tune),
            1000.0 / tune.max_fps
        );
    }

    #[test]
    fn the_tuning_knobs_reject_nonsense_and_keep_their_defaults() {
        let tune = parse_diff_tune(None, None, None, None);
        assert_eq!(tune, DiffTune::DEFAULT);
        for bad in ["0", "-1", "abc", "", "inf", "NaN"] {
            let tune = parse_diff_tune(Some(bad), Some(bad), Some(bad), Some(bad));
            assert_eq!(tune, DiffTune::DEFAULT, "{bad:?} must fall back");
        }
        let tune = parse_diff_tune(Some("500000"), Some("5"), Some("30"), Some("0.5"));
        assert_eq!(tune.soft, 500_000.0);
        assert_eq!(tune.probe_hz, 5.0);
        assert_eq!(tune.max_fps, 30.0);
        assert_eq!(tune.min_fps, 0.5);
    }

    #[test]
    fn a_forced_change_is_never_deferred_by_the_budget() {
        // A whole sampled chunk rewritten, or an incomparable surface, must buy a frame even when the
        // accumulator is nowhere near the threshold.
        let (deliver, accum, skipped) = diff_budget_decision(0, 1, 1_000_000, 0, 30, true);
        assert!(deliver);
        assert_eq!(
            (accum, skipped),
            (0, 0),
            "a forced delivery restarts the counters"
        );
    }

    #[test]
    fn the_accumulator_saturates_instead_of_wrapping() {
        let (deliver, _, _) = diff_budget_decision(u64::MAX - 1, 5, u64::MAX, 0, 30, false);
        assert!(deliver);
    }

    #[test]
    fn only_a_positive_number_turns_the_budget_on() {
        assert_eq!(parse_diff_accum_env(None), None);
        assert_eq!(parse_diff_accum_env(Some("0")), None);
        assert_eq!(parse_diff_accum_env(Some("")), None);
        assert_eq!(parse_diff_accum_env(Some("lots")), None);
        assert_eq!(parse_diff_accum_env(Some(" 4096 ")), Some(4096));
    }

    #[test]
    fn the_sample_density_and_the_skip_guard_fall_back_to_their_defaults() {
        assert_eq!(parse_diff_sample_env(None), 16);
        assert_eq!(parse_diff_sample_env(Some("0")), 16);
        assert_eq!(parse_diff_sample_env(Some("1")), 1);
        assert_eq!(parse_diff_sample_env(Some("99999")), 1024);
        assert_eq!(parse_diff_max_skip_env(None), 30);
        assert_eq!(parse_diff_max_skip_env(Some("-3")), 30);
        assert_eq!(parse_diff_max_skip_env(Some("5")), 5);
    }
}

/// The grid arithmetic lives in `quartz`, which only exists on macOS, so this test travels with it.
#[cfg(all(test, target_os = "macos"))]
mod diff_grid_tests {
    #[test]
    fn the_sample_grid_is_page_aligned_and_density_one_reads_everything() {
        use crate::quartz::{diff_interval, scale_to_surface, DIFF_CHUNK};
        assert_eq!(DIFF_CHUNK % 4096, 0, "multiple of the Intel page size");
        assert_eq!(
            DIFF_CHUNK % 16384,
            0,
            "multiple of the Apple Silicon page size"
        );
        assert_eq!(
            diff_interval(1),
            DIFF_CHUNK,
            "density 1 compares every chunk"
        );
        assert_eq!(diff_interval(16), 16 * DIFF_CHUNK);
        assert_eq!(diff_interval(0), DIFF_CHUNK, "zero must not divide by zero");
        // Scaling back to the whole surface keeps a threshold meaningful across densities.
        assert_eq!(scale_to_surface(1234, 4096, 4096), 1234);
        assert_eq!(scale_to_surface(100, 4096, 65536), 1600);
        assert_eq!(
            scale_to_surface(0, 0, 4096),
            0,
            "nothing sampled is nothing changed"
        );
        assert_eq!(scale_to_surface(5, 4096, 0), 0);
    }
}

pub enum EncodeInput<'a> {
    YUV(&'a [u8]),
    Texture((*mut c_void, usize)),
    #[cfg(target_os = "macos")]
    PixelBuffer(CVPixelBufferHandle),
    #[cfg(all(windows, feature = "vram"))]
    Repeat,
}

impl<'a> EncodeInput<'a> {
    pub fn yuv(&self) -> ResultType<&'_ [u8]> {
        match self {
            Self::YUV(f) => Ok(f),
            #[cfg(target_os = "macos")]
            Self::PixelBuffer(_) => Err(crate::Error::BadPtr(
                "pixel buffer input has no yuv slice".to_owned(),
            )
            .into()),
            _ => bail!("not pixelfbuffer frame"),
        }
    }

    pub fn texture(&self) -> ResultType<(*mut c_void, usize)> {
        match self {
            Self::Texture(f) => Ok(*f),
            _ => bail!("not texture frame"),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Pixfmt {
    BGRA,
    RGBA,
    RGB565LE,
    I420,
    NV12,
    I444,
}

impl Pixfmt {
    pub fn bpp(&self) -> usize {
        match self {
            Pixfmt::BGRA | Pixfmt::RGBA => 32,
            Pixfmt::RGB565LE => 16,
            Pixfmt::I420 | Pixfmt::NV12 => 12,
            Pixfmt::I444 => 24,
        }
    }

    pub fn bytes_per_pixel(&self) -> usize {
        (self.bpp() + 7) / 8
    }
}

#[derive(Debug, Clone)]
pub struct EncodeYuvFormat {
    pub pixfmt: Pixfmt,
    pub w: usize,
    pub h: usize,
    pub stride: Vec<usize>,
    pub u: usize,
    pub v: usize,
}

#[cfg(x11)]
#[inline]
pub fn is_x11() -> bool {
    base::platform::linux::is_x11_or_headless()
}

#[cfg(x11)]
#[inline]
pub fn is_cursor_embedded() -> bool {
    if is_x11() {
        x11::IS_CURSOR_EMBEDDED
    } else {
        false
    }
}

#[cfg(not(x11))]
#[inline]
pub fn is_cursor_embedded() -> bool {
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecName {
    VP8,
    VP9,
    AV1,
    H264RAM(String),
    H265RAM(String),
    H264VRAM,
    H265VRAM,
}

#[derive(PartialEq, Debug, Clone, Copy)]
pub enum CodecFormat {
    VP8,
    VP9,
    AV1,
    H264,
    H265,
    Unknown,
}

impl From<&VideoFrame> for CodecFormat {
    fn from(it: &VideoFrame) -> Self {
        match it.union {
            Some(video_frame::Union::Vp8s(_)) => CodecFormat::VP8,
            Some(video_frame::Union::Vp9s(_)) => CodecFormat::VP9,
            Some(video_frame::Union::Av1s(_)) => CodecFormat::AV1,
            Some(video_frame::Union::H264s(_)) => CodecFormat::H264,
            Some(video_frame::Union::H265s(_)) => CodecFormat::H265,
            _ => CodecFormat::Unknown,
        }
    }
}

impl From<&video_frame::Union> for CodecFormat {
    fn from(it: &video_frame::Union) -> Self {
        match it {
            video_frame::Union::Vp8s(_) => CodecFormat::VP8,
            video_frame::Union::Vp9s(_) => CodecFormat::VP9,
            video_frame::Union::Av1s(_) => CodecFormat::AV1,
            video_frame::Union::H264s(_) => CodecFormat::H264,
            video_frame::Union::H265s(_) => CodecFormat::H265,
            _ => CodecFormat::Unknown,
        }
    }
}

impl From<&CodecName> for CodecFormat {
    fn from(value: &CodecName) -> Self {
        match value {
            CodecName::VP8 => Self::VP8,
            CodecName::VP9 => Self::VP9,
            CodecName::AV1 => Self::AV1,
            CodecName::H264RAM(_) | CodecName::H264VRAM => Self::H264,
            CodecName::H265RAM(_) | CodecName::H265VRAM => Self::H265,
        }
    }
}

impl ToString for CodecFormat {
    fn to_string(&self) -> String {
        match self {
            CodecFormat::VP8 => "VP8".into(),
            CodecFormat::VP9 => "VP9".into(),
            CodecFormat::AV1 => "AV1".into(),
            CodecFormat::H264 => "H264".into(),
            CodecFormat::H265 => "H265".into(),
            CodecFormat::Unknown => "Unknown".into(),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    FailedCall(String),
    BadPtr(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[macro_export]
macro_rules! generate_call_macro {
    ($func_name:ident, $allow_err:expr) => {
        macro_rules! $func_name {
            ($x:expr) => {{
                let result = unsafe { $x };
                let result_int = unsafe { std::mem::transmute::<_, i32>(result) };
                if result_int != 0 {
                    let message = format!(
                        "errcode={} {}:{}:{}:{}",
                        result_int,
                        module_path!(),
                        file!(),
                        line!(),
                        column!()
                    );
                    if $allow_err {
                        log::warn!("Failed to call {}, {}", stringify!($func_name), message);
                    } else {
                        return Err(crate::Error::FailedCall(message).into());
                    }
                }
                result
            }};
        }
    };
}

#[macro_export]
macro_rules! generate_call_ptr_macro {
    ($func_name:ident) => {
        macro_rules! $func_name {
            ($x:expr) => {{
                let result = unsafe { $x };
                let result_int = unsafe { std::mem::transmute::<_, isize>(result) };
                if result_int == 0 {
                    return Err(crate::Error::BadPtr(format!(
                        "errcode={} {}:{}:{}:{}",
                        result_int,
                        module_path!(),
                        file!(),
                        line!(),
                        column!()
                    ))
                    .into());
                }
                result
            }};
        }
    };
}

pub trait GoogleImage {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn stride(&self) -> Vec<i32>;
    fn planes(&self) -> Vec<*mut u8>;
    fn chroma(&self) -> Chroma;
    fn get_bytes_per_row(w: usize, fmt: ImageFormat, align: usize) -> usize {
        let bytes_per_pixel = match fmt {
            ImageFormat::Raw => 3,
            ImageFormat::ARGB | ImageFormat::ABGR => 4,
        };
        // https://github.com/lemenkov/libyuv/blob/6900494d90ae095d44405cd4cc3f346971fa69c9/source/convert_argb.cc#L128
        // https://github.com/lemenkov/libyuv/blob/6900494d90ae095d44405cd4cc3f346971fa69c9/source/convert_argb.cc#L129
        (w * bytes_per_pixel + align - 1) & !(align - 1)
    }
    // rgb [in/out] fmt and stride must be set in ImageRgb
    fn to(&self, rgb: &mut ImageRgb) {
        rgb.w = self.width();
        rgb.h = self.height();
        let bytes_per_row = Self::get_bytes_per_row(rgb.w, rgb.fmt, rgb.align());
        rgb.raw.resize(rgb.h * bytes_per_row, 0);
        let stride = self.stride();
        let planes = self.planes();
        unsafe {
            match (self.chroma(), rgb.fmt()) {
                (Chroma::I420, ImageFormat::Raw) => {
                    super::I420ToRAW(
                        planes[0],
                        stride[0],
                        planes[1],
                        stride[1],
                        planes[2],
                        stride[2],
                        rgb.raw.as_mut_ptr(),
                        bytes_per_row as _,
                        self.width() as _,
                        self.height() as _,
                    );
                }
                (Chroma::I420, ImageFormat::ARGB) => {
                    super::I420ToARGB(
                        planes[0],
                        stride[0],
                        planes[1],
                        stride[1],
                        planes[2],
                        stride[2],
                        rgb.raw.as_mut_ptr(),
                        bytes_per_row as _,
                        self.width() as _,
                        self.height() as _,
                    );
                }
                (Chroma::I420, ImageFormat::ABGR) => {
                    super::I420ToABGR(
                        planes[0],
                        stride[0],
                        planes[1],
                        stride[1],
                        planes[2],
                        stride[2],
                        rgb.raw.as_mut_ptr(),
                        bytes_per_row as _,
                        self.width() as _,
                        self.height() as _,
                    );
                }
                (Chroma::I444, ImageFormat::ARGB) => {
                    super::I444ToARGB(
                        planes[0],
                        stride[0],
                        planes[1],
                        stride[1],
                        planes[2],
                        stride[2],
                        rgb.raw.as_mut_ptr(),
                        bytes_per_row as _,
                        self.width() as _,
                        self.height() as _,
                    );
                }
                (Chroma::I444, ImageFormat::ABGR) => {
                    super::I444ToABGR(
                        planes[0],
                        stride[0],
                        planes[1],
                        stride[1],
                        planes[2],
                        stride[2],
                        rgb.raw.as_mut_ptr(),
                        bytes_per_row as _,
                        self.width() as _,
                        self.height() as _,
                    );
                }
                // (Chroma::I444, ImageFormat::Raw), new version libyuv have I444ToRAW
                _ => log::error!("unsupported pixfmt: {:?}", self.chroma()),
            }
        }
    }
    fn data(&self) -> (&[u8], &[u8], &[u8]) {
        unsafe {
            let stride = self.stride();
            let planes = self.planes();
            let h = (self.height() as usize + 1) & !1;
            let n = stride[0] as usize * h;
            let y = slice::from_raw_parts(planes[0], n);
            let n = stride[1] as usize * (h >> 1);
            let u = slice::from_raw_parts(planes[1], n);
            let v = slice::from_raw_parts(planes[2], n);
            (y, u, v)
        }
    }
}

#[cfg(target_os = "android")]
pub fn screen_size() -> (u16, u16, u16) {
    SCREEN_SIZE.lock().unwrap().clone()
}

#[cfg(target_os = "android")]
pub fn is_start() -> Option<bool> {
    android::is_start()
}

#[cfg(all(test, target_os = "macos"))]
mod pixbuf_fallback_tests {
    use super::{pixbuf_or_yuv, EncodeInput};

    #[test]
    fn a_failed_wrap_falls_through_to_the_conversion() {
        let mut yuv = Vec::new();
        let input = pixbuf_or_yuv(
            true,
            true,
            std::ptr::null_mut,
            |dst| {
                dst.extend_from_slice(&[9, 8, 7]);
                Ok(())
            },
            &mut yuv,
        )
        .expect("fallback must not error when the conversion succeeds");
        assert!(
            matches!(input, EncodeInput::YUV(_)),
            "a null CVPixelBuffer must produce YUV input, not a dropped frame"
        );
        assert_eq!(yuv, vec![9, 8, 7], "the conversion must have run");
    }

    #[test]
    fn the_counters_separate_direct_frames_from_fallbacks() {
        let (wrapped_before, fell_before) = super::pixbuf_stats::counts();
        let mut yuv = Vec::new();
        let _ = pixbuf_or_yuv(
            true,
            true,
            std::ptr::null_mut,
            |dst| {
                dst.push(0);
                Ok(())
            },
            &mut yuv,
        )
        .expect("fallback");
        let mut yuv2 = Vec::new();
        let _ = pixbuf_or_yuv(
            false,
            true,
            || panic!("not called"),
            |dst| {
                dst.push(0);
                Ok(())
            },
            &mut yuv2,
        )
        .expect("conversion");
        let (wrapped_after, fell_after) = super::pixbuf_stats::counts();
        assert_eq!(
            wrapped_after, wrapped_before,
            "a frame that never reached the encoder must not be counted as direct"
        );
        assert!(
            fell_after >= fell_before + 1,
            "the fallback that just ran must be visible in the counter"
        );
    }

    #[test]
    fn with_the_gate_off_the_wrap_is_never_attempted() {
        let mut yuv = Vec::new();
        let input = pixbuf_or_yuv(
            false,
            true,
            || panic!("the wrap must not be called when the gate is off"),
            |dst| {
                dst.push(1);
                Ok(())
            },
            &mut yuv,
        )
        .expect("conversion");
        assert!(matches!(input, EncodeInput::YUV(_)));
    }

    #[test]
    fn a_biplanar_surface_is_dropped_instead_of_converted() {
        let mut yuv = Vec::new();
        let err = match pixbuf_or_yuv(
            true,
            false,
            std::ptr::null_mut,
            |dst| {
                dst.push(0);
                Ok(())
            },
            &mut yuv,
        ) {
            Ok(_) => panic!("a biplanar surface must not be handed to the BGRA converter"),
            Err(err) => err,
        };
        assert!(
            yuv.is_empty(),
            "the conversion reads the luma plane as packed pixels and must not run"
        );
        assert!(err.to_string().contains("frame dropped"));
    }
}
