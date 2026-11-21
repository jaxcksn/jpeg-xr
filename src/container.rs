//! The tag-based file format (T.832 Annex A)

use crate::Error;

/// The parts of a file's first image file directory this decoder uses.
#[derive(Clone, Debug, Default)]
pub(crate) struct Container {
    pub pixel_format: Option<[u8; 16]>,
    /// SPATIAL_XFRM_PRIMARY
    pub transform: Option<u8>,
    /// WIDTH_RESOLUTION and HEIGHT_RESOLUTION, pixels per inch.
    pub resolution: Option<(f32, f32)>,
    pub image: (usize, usize),
    pub alpha: Option<(usize, usize)>,
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

impl Container {
    pub fn read(d: &[u8]) -> Result<Self, Error> {
        if !d.starts_with(&[0x49, 0x49, 0xBC]) {
            return Err(Error::NotJpegXr);
        }
        if d.get(3) != Some(&1) {
            return Err(Error::Unsupported("file version"));
        }
        let ifd = u32_at(d, 4).ok_or(Error::Truncated)? as usize;
        let count = usize::from(u16_at(d, ifd).ok_or(Error::Truncated)?);
        let mut c = Container::default();
        let (mut image_offset, mut image_count, mut alpha_offset, mut alpha_count) = (None, None, None, None);
        let (mut res_x, mut res_y) = (None, None);
        for e in 0..count {
            let at = ifd + 2 + e * 12;
            let tag = u16_at(d, at).ok_or(Error::Truncated)?;
            let kind = u16_at(d, at + 2).ok_or(Error::Truncated)?;
            let n = u32_at(d, at + 4).ok_or(Error::Truncated)?;
            // An unsigned scalar: BYTE, USHORT or ULONG, held in the entry.
            let scalar = || -> Option<u32> {
                match (kind, n) {
                    (1, 1) => d.get(at + 8).map(|&b| u32::from(b)),
                    (3, 1) => u16_at(d, at + 8).map(u32::from),
                    (4, 1) => u32_at(d, at + 8),
                    _ => None,
                }
            };
            match tag {
                0xBC01 if kind == 1 && n == 16 => {
                    let off = u32_at(d, at + 8).ok_or(Error::Truncated)? as usize;
                    let bytes = d.get(off..off.checked_add(16).ok_or(Error::Truncated)?).ok_or(Error::Truncated)?;
                    c.pixel_format = Some(bytes.try_into().map_err(|_| Error::Truncated)?);
                }
                0xBC02 => c.transform = scalar().map(|v| (v & 7) as u8),
                0xBC82 | 0xBC83 if kind == 11 && n == 1 => {
                    let v = u32_at(d, at + 8).map(f32::from_bits).filter(|v| v.is_finite() && *v > 0.0);
                    if tag == 0xBC82 { res_x = v } else { res_y = v }
                }
                0xBCC0 => image_offset = scalar(),
                0xBCC1 => image_count = scalar(),
                0xBCC2 => alpha_offset = scalar(),
                0xBCC3 => alpha_count = scalar(),
                _ => {}
            }
        }
        let span = |off: Option<u32>, count: Option<u32>| -> Result<Option<(usize, usize)>, Error> {
            let Some(off) = off else { return Ok(None) };
            let off = off as usize;
            // A zero or missing byte count runs to the end of the file, and so does one past it: some encoders write
            // the alpha plane's end there.
            let end = match count {
                Some(n) if n > 0 => off.saturating_add(n as usize).min(d.len()),
                _ => d.len(),
            };
            if off >= end {
                return Err(Error::Truncated);
            }
            Ok(Some((off, end)))
        };
        c.resolution = res_x.zip(res_y);
        c.image = span(image_offset, image_count)?.ok_or(Error::Invalid("no image offset"))?;
        c.alpha = span(alpha_offset, alpha_count)?;
        Ok(c)
    }

    /// Whether the pixel format names premultiplied alpha (32bppPBGRA, 64bppPRGBA, 128bppPRGBAFloat).
    pub fn premultiplied(&self) -> bool {
        const PREFIX: [u8; 15] =
            [0x24, 0xC3, 0xDD, 0x6F, 0x03, 0x4E, 0xFE, 0x4B, 0xB1, 0x85, 0x3D, 0x77, 0x76, 0x8D, 0xC9];
        matches!(self.pixel_format, Some(p) if p[..15] == PREFIX && matches!(p[15], 0x10 | 0x17 | 0x1A))
    }
}
