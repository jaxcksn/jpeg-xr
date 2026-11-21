//! A decoder for JPEG XR (ITU-T T.832 | ISO/IEC 29199-2) images, also known as HD Photo and Windows Media Photo:
//! the `.jxr`, `.wdp` and `.hdp` files Microsoft Office and Windows write.
//!
//! The decoder is written from the Recommendation, in safe Rust with no dependencies. It reads the tag-based file
//! format (Annex A) or a bare codestream, and decodes 8-bit grey, RGB and RGB with alpha (interleaved or as a separate
//! alpha plane): every internal colour format those use (Y only, 4:2:0, 4:2:2, 4:4:4), lossless and lossy
//! quantization, both overlap levels, hard and soft tiles, and the spatial and frequency codestream orders.
//!
//! ```no_run
//! let bytes = std::fs::read("picture.jxr").unwrap();
//! let info = jpeg_xr::probe(&bytes).unwrap();
//! let image = jpeg_xr::decode_rgba8(&bytes, &jpeg_xr::Limits::default()).unwrap();
//! assert_eq!(image.data.len(), info.width as usize * info.height as usize * 4);
//! ```
//!
//! Input is untrusted: every count, size and offset is bounded, the [`Limits`] cap memory and work before anything
//! is allocated, and malformed data is an [`Error`], never a panic.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod bits;
mod container;
mod decode;
mod header;
mod plane;
mod transform;
mod vlc;

use std::fmt;

use container::Container;
use decode::{Codestream, Role, write_plane};
use header::Output;

/// Why bytes did not decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The bytes are neither a JPEG XR file nor a JPEG XR codestream.
    NotJpegXr,
    /// The data ends early.
    Truncated,
    /// The data breaks the format's rules.
    Invalid(&'static str),
    /// A valid feature this decoder does not implement, such as 16-bit, floating point or CMYK output.
    Unsupported(&'static str),
    /// The image is larger, or would take more work, than the [`Limits`] allow.
    Limit,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotJpegXr => f.write_str("not a JPEG XR image"),
            Error::Truncated => f.write_str("JPEG XR data ends early"),
            Error::Invalid(what) => write!(f, "invalid JPEG XR data: {what}"),
            Error::Unsupported(what) => write!(f, "unsupported JPEG XR feature: {what}"),
            Error::Limit => f.write_str("JPEG XR image exceeds the decoding limits"),
        }
    }
}

impl std::error::Error for Error {}

/// Caps on what one decode may cost, checked before any image memory is allocated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most output pixels, width × height. Every pixel is at least one coded sample, so this is the tighter cap
    /// only for grey images at a [`Limits::max_work`] at or above it; colour images reach `max_work` first.
    pub max_pixels: u64,
    /// The most samples the codestream may code across all its planes and components, padding to whole
    /// macroblocks included. Decoding time and memory (4 bytes a sample) are proportional to it, and so are the
    /// tile counts the decoder will read an index table for.
    pub max_work: u64,
}

impl Default for Limits {
    /// 2^26 pixels and 2^26 samples. The samples take 4 bytes each while decoding, so the default holds the working
    /// memory to 256 MiB, which a 32-bit target such as `wasm32` can find beside its output: RGB up to about 22
    /// million pixels at 4:4:4 and 44 million at 4:2:0, grey up to 67 million. The two caps match because a pixel
    /// is at least a sample: raising only `max_pixels` changes nothing, so callers that want larger images raise
    /// both.
    fn default() -> Self {
        Limits { max_pixels: 1 << 26, max_work: 1 << 26 }
    }
}

/// How decoded pixels are laid out: 8 bits a channel, rows packed top to bottom, no padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layout {
    /// One grey channel.
    Gray8,
    /// Red, green, blue.
    Rgb8,
    /// Red, green, blue, alpha.
    Rgba8,
}

impl Layout {
    /// Bytes a pixel.
    pub fn channels(self) -> usize {
        match self {
            Layout::Gray8 => 1,
            Layout::Rgb8 => 3,
            Layout::Rgba8 => 4,
        }
    }
}

/// What an image says about itself, read from its headers alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Info {
    /// Width in pixels, as stored (before [`Info::orientation`]).
    pub width: u32,
    /// Height in pixels, as stored.
    pub height: u32,
    /// The layout [`decode`] produces.
    pub layout: Layout,
    /// Whether colour is premultiplied by alpha.
    pub premultiplied: bool,
    /// The turn or flip the image asks to be shown with, as an EXIF/TIFF orientation, 1 (as stored) to 8.
    /// Decoding does not apply it.
    pub orientation: u8,
    /// Horizontal and vertical resolution in pixels per inch, when the file states them.
    pub resolution: Option<(f32, f32)>,
}

/// Decoded pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The layout of [`Image::data`].
    pub layout: Layout,
    /// Whether colour is premultiplied by alpha.
    pub premultiplied: bool,
    /// The turn or flip the image asks to be shown with, as in [`Info::orientation`].
    pub orientation: u8,
    /// `width × height × layout.channels()` bytes.
    pub data: Vec<u8>,
}

/// The primary codestream and, when the alpha channel is stored separately, the alpha codestream.
struct Parts<'a> {
    image: Codestream<'a>,
    alpha: Option<Codestream<'a>>,
    container: Container,
}

fn parts<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Parts<'a>, Error> {
    let container = if bytes.starts_with(b"WMPHOTO\0") {
        Container { image: (0, bytes.len()), ..Container::default() }
    } else {
        Container::read(bytes)?
    };
    let image = Codestream::parse(&bytes[container.image.0..container.image.1], limits)?;
    let alpha = match (container.alpha, image.header.alpha_plane) {
        (Some((start, end)), false) => {
            let a = Codestream::parse(&bytes[start..end], limits)?;
            if a.planes[0].format != header::Format::YOnly || a.header.alpha_plane {
                return Err(Error::Invalid("alpha codestream is not one plane"));
            }
            if (a.header.width, a.header.height) != (image.header.width, image.header.height) {
                return Err(Error::Invalid("alpha codestream size"));
            }
            Some(a)
        }
        _ => None,
    };
    Ok(Parts { image, alpha, container })
}

impl Parts<'_> {
    fn info(&self) -> Info {
        let h = &self.image.header;
        let alpha = h.alpha_plane || self.alpha.is_some();
        let layout = match (h.output, alpha) {
            (_, true) => Layout::Rgba8,
            (Output::Rgb, false) => Layout::Rgb8,
            (Output::Gray, false) => Layout::Gray8,
        };
        let premultiplied = alpha
            && (self.container.premultiplied()
                || h.premultiplied
                || self.alpha.as_ref().is_some_and(|a| a.header.premultiplied));
        const TIFF: [u8; 8] = [1, 4, 2, 3, 6, 7, 5, 8];
        let orientation = TIFF[usize::from(self.container.transform.unwrap_or(h.transform))];
        Info {
            width: h.width,
            height: h.height,
            layout,
            premultiplied,
            orientation,
            resolution: self.container.resolution,
        }
    }
}

/// Reads an image's headers: its size, layout and orientation.
///
/// The image's size and tile counts are held to the default [`Limits`], so a hostile header costs nothing; use
/// [`probe_with`] to raise them.
pub fn probe(bytes: &[u8]) -> Result<Info, Error> {
    probe_with(bytes, &Limits::default())
}

/// [`probe`] with the given [`Limits`]: an image over them is [`Error::Limit`].
pub fn probe_with(bytes: &[u8], limits: &Limits) -> Result<Info, Error> {
    Ok(parts(bytes, limits)?.info())
}

/// Decodes an image into its own [`Layout`].
pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Image, Error> {
    let parts = parts(bytes, limits)?;
    let info = parts.info();
    let pixels = u64::from(info.width) * u64::from(info.height);
    let work = parts.image.work() + parts.alpha.as_ref().map_or(0, |a| a.work());
    if pixels > limits.max_pixels || work > limits.max_work {
        return Err(Error::Limit);
    }
    let h = &parts.image.header;
    let channels = info.layout.channels();
    let bytes = usize::try_from(pixels).ok().and_then(|p| p.checked_mul(channels)).ok_or(Error::Limit)?;
    let mut data = vec![0u8; bytes];
    let planes = parts.image.decode()?;
    write_plane(&planes[0], h, &mut data, channels, Role::Colour { rgb: channels >= 3 });
    if let Some(alpha) = planes.get(1) {
        write_plane(alpha, h, &mut data, channels, Role::Alpha { interleaved: true });
    } else if let Some(a) = &parts.alpha {
        let planes = a.decode()?;
        write_plane(&planes[0], &a.header, &mut data, channels, Role::Alpha { interleaved: false });
    }
    Ok(Image {
        width: info.width,
        height: info.height,
        layout: info.layout,
        premultiplied: info.premultiplied,
        orientation: info.orientation,
        data,
    })
}

/// Decodes an image into RGBA, 8 bits a channel; grey is spread to all three colours and opaque images get an
/// alpha of 255.
pub fn decode_rgba8(bytes: &[u8], limits: &Limits) -> Result<Image, Error> {
    let image = decode(bytes, limits)?;
    let data = match image.layout {
        Layout::Rgba8 => return Ok(image),
        Layout::Rgb8 => image.data.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        Layout::Gray8 => image.data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
    };
    Ok(Image { layout: Layout::Rgba8, data, ..image })
}
