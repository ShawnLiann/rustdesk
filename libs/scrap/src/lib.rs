#[cfg(quartz)]
extern crate block;
#[macro_use]
extern crate cfg_if;
pub use hbb_common::libc;
#[cfg(dxgi)]
extern crate winapi;

pub use common::*;

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

/// GPU frame of one decode call: `Some` must be presented instead of the software pixels.
#[cfg(all(target_os = "macos", feature = "hwcodec"))]
pub use ::hwcodec::ffmpeg_ram::pixelbuffer::PixelBufferFrame;
#[cfg(all(target_os = "macos", feature = "hwcodec"))]
pub type PixelBufferOutput = Option<::hwcodec::ffmpeg_ram::pixelbuffer::PixelBufferFrame>;
#[cfg(not(all(target_os = "macos", feature = "hwcodec")))]
pub type PixelBufferOutput = Option<std::convert::Infallible>;
