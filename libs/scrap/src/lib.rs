#[cfg(quartz)]
extern crate block;
#[macro_use]
extern crate cfg_if;
pub use hbb_common::libc;
#[cfg(dxgi)]
extern crate winapi;

pub use common::*;

/// Owned decoded surface, macOS only.
///
/// The macOS analogue of `hwcodec::vram::DecodeFrame::texture`, except owned
/// (retain on clone, release on drop) because nothing else keeps it alive.
/// What the renderer side actually shares with the Windows path is
/// `ImageTexture`, not this type.
#[cfg(all(target_os = "macos", feature = "hwcodec"))]
pub use ::hwcodec::ffmpeg_ram::pixelbuffer::PixelBufferFrame;
#[cfg(all(target_os = "macos", feature = "hwcodec"))]
pub type PixelBufferOutput = Option<PixelBufferFrame>;

#[cfg(quartz)]
pub mod quartz;

#[cfg(x11)]
pub mod x11;

#[cfg(all(x11, feature = "wayland"))]
pub mod wayland;

#[cfg(dxgi)]
pub mod dxgi;

#[cfg(target_os = "android")]
pub mod android;

mod common;
