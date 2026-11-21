//! CODED_IMAGE( ) (T.832 8.2), its tiles (8.7.1 to 8.7.9), and output formatting (9.10)

use crate::{Error, Limits};
use crate::bits::Bits;
use crate::header::{Bands, Format, ImageHeader, Output, PlaneHeader};
use crate::plane::{At, Plane};
use crate::transform::reconstruct;

/// The samples the planes hold, padding to whole macroblocks included.
fn work(header: &ImageHeader, planes: &[PlaneHeader]) -> u64 {
    let mbs = header.mb_width as u64 * header.mb_height as u64;
    let per_mb = |f: Format| match f {
        Format::YOnly => 256,
        Format::Yuv420 => 384,
        Format::Yuv422 => 512,
        Format::Yuv444 => 768,
    };
    planes.iter().map(|p| mbs * per_mb(p.format)).sum()
}

/// A parsed codestream header and where its tile packets are.
pub(crate) struct Codestream<'a> {
    pub header: ImageHeader,
    pub planes: Vec<PlaneHeader>,
    /// Each tile packet's bytes, in index table order.
    packets: Vec<&'a [u8]>,
}

impl<'a> Codestream<'a> {
    pub fn parse(data: &'a [u8], limits: &Limits) -> Result<Self, Error> {
        let mut b = Bits::new(data);
        let header = ImageHeader::read(&mut b)?;
        // Each macroblock takes at least one bit of the DC band, so a codestream holds at most 8 per byte.
        let mbs = header.mb_width.checked_mul(header.mb_height).ok_or(Error::Limit)?;
        if mbs / 8 > data.len() {
            return Err(Error::Truncated);
        }
        let mut planes = vec![PlaneHeader::read(&mut b, false)?];
        // Grey output is defined of a Y-only plane alone (T.832 Tables 179 and 183); RGB of any of them.
        if header.output == Output::Gray && planes[0].format != Format::YOnly {
            return Err(Error::Invalid("grey output of a colour codestream"));
        }
        if header.alpha_plane {
            let alpha = PlaneHeader::read(&mut b, true)?;
            if alpha.bands > planes[0].bands {
                return Err(Error::Invalid("alpha plane has more bands than the primary plane"));
            }
            planes.push(alpha);
        }
        // Size, tile and macroblock counts are checked against the limits before the index table, the one
        // allocation that scales with the input's claims: tiles are at least a macroblock, so the entries below
        // number at most 4 per macroblock, and `max_work` bounds the macroblocks.
        if u64::from(header.width) * u64::from(header.height) > limits.max_pixels
            || work(&header, &planes) > limits.max_work
        {
            return Err(Error::Limit);
        }
        let bands = planes[0].bands.count();
        let count = if header.frequency { header.tiles() * bands } else { header.tiles() };
        let mut starts = Vec::new();
        if header.index_table {
            if b.read(16)? != 1 {
                return Err(Error::Invalid("index table start code"));
            }
            // Each entry takes at least a byte.
            if count > data.len() {
                return Err(Error::Truncated);
            }
            starts.reserve_exact(count);
            for _ in 0..count {
                starts.push(usize::try_from(b.vlw()?).map_err(|_| Error::Truncated)?);
            }
        } else if count == 1 {
            starts.push(0);
        } else {
            return Err(Error::Invalid("no index table"));
        }
        let skip = b.vlw()?;
        b.skip_bytes(skip)?;
        let start = b.byte_pos();
        for o in &mut starts {
            *o = o.checked_add(start).filter(|&o| o < data.len()).ok_or(Error::Truncated)?;
        }
        // A packet runs to the next one in the file. Packets may share an offset (the reference encoder points an
        // empty flexbits packet at the first one): each MB reads a bounded number of bits, and the MB count is
        // bounded by the data's size above, so sharing cannot multiply the work.
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        sorted.dedup();
        let packets = starts
            .iter()
            .map(|&s| &data[s..sorted.get(sorted.partition_point(|&o| o <= s)).copied().unwrap_or(data.len())])
            .collect();
        Ok(Codestream { header, planes, packets })
    }

    /// The samples the planes hold, which bounds the work and memory of decoding.
    pub fn work(&self) -> u64 {
        work(&self.header, &self.planes)
    }

    /// Parses every tile and reconstructs the planes' samples.
    pub fn decode(&self) -> Result<Vec<Plane>, Error> {
        let h = &self.header;
        let mut planes: Vec<Plane> =
            self.planes.iter().map(|p| Plane::new(p.clone(), h.mb_width, h.mb_height)).collect::<Result<_, _>>()?;
        let bands = self.planes[0].bands;
        let cols = h.tile_x.len() - 1;
        for (n, (ty, tx)) in (0..h.tile_y.len() - 1).flat_map(|y| (0..cols).map(move |x| (y, x))).enumerate() {
            let tile = Tile { x0: h.tile_x[tx], x1: h.tile_x[tx + 1], y0: h.tile_y[ty], y1: h.tile_y[ty + 1] };
            if h.frequency {
                let packet = |band: usize| self.packets[n * bands.count() + band];
                self.dc_packet(&mut planes, &tile, packet(0))?;
                if bands > Bands::DcOnly {
                    self.lp_packet(&mut planes, &tile, packet(1))?;
                }
                if bands > Bands::NoHighpass {
                    self.hp_packet(&mut planes, &tile, packet(2))?;
                }
                if bands == Bands::All {
                    self.flex_packet(&mut planes, &tile, packet(3))?;
                }
            } else {
                self.spatial_packet(&mut planes, &tile, self.packets[n])?;
            }
            for at in tile.mbs() {
                for p in planes.iter_mut().filter(|p| p.bands() > Bands::NoHighpass) {
                    p.mb_hp_finish(&at);
                }
            }
        }
        for p in &mut planes {
            reconstruct(p, h.overlap, h.hard_tiling, &h.tile_x, &h.tile_y);
        }
        Ok(planes)
    }

    fn trim(&self, b: &mut Bits) -> Result<u32, Error> {
        if self.header.trim_flexbits { b.read(4) } else { Ok(0) }
    }

    /// TILE_SPATIAL( )
    fn spatial_packet(&self, planes: &mut [Plane], tile: &Tile, data: &[u8]) -> Result<(), Error> {
        let mut b = Bits::new(data);
        start_code(&mut b)?;
        let trim = self.trim(&mut b)?;
        for p in planes.iter_mut() {
            p.tile_header_dc(&mut b)?;
            if p.bands() > Bands::DcOnly {
                p.tile_header_lp(&mut b)?;
                if p.bands() > Bands::NoHighpass {
                    p.tile_header_hp(&mut b)?;
                }
            }
        }
        for at in tile.mbs() {
            for p in planes.iter_mut() {
                let bands = p.bands();
                if bands > Bands::DcOnly {
                    p.lp_qp_index(&mut b, &at)?;
                    if bands > Bands::NoHighpass {
                        p.hp_qp_index(&mut b, &at)?;
                    }
                }
                p.mb_dc(&mut b, &at)?;
                if bands > Bands::DcOnly {
                    p.mb_lp(&mut b, &at)?;
                    if bands > Bands::NoHighpass {
                        p.mb_cbphp(&mut b, &at)?;
                        p.mb_hp(&mut b, &at, (bands == Bands::All).then_some(trim))?;
                    }
                }
            }
        }
        Ok(())
    }

    /// TILE_DC( )
    fn dc_packet(&self, planes: &mut [Plane], tile: &Tile, data: &[u8]) -> Result<(), Error> {
        let mut b = Bits::new(data);
        start_code(&mut b)?;
        for p in planes.iter_mut() {
            p.tile_header_dc(&mut b)?;
        }
        for at in tile.mbs() {
            for p in planes.iter_mut() {
                p.mb_dc(&mut b, &at)?;
            }
        }
        Ok(())
    }

    /// TILE_LOWPASS( )
    fn lp_packet(&self, planes: &mut [Plane], tile: &Tile, data: &[u8]) -> Result<(), Error> {
        let mut b = Bits::new(data);
        start_code(&mut b)?;
        for p in planes.iter_mut().filter(|p| p.bands() > Bands::DcOnly) {
            p.tile_header_lp(&mut b)?;
        }
        for at in tile.mbs() {
            for p in planes.iter_mut().filter(|p| p.bands() > Bands::DcOnly) {
                p.lp_qp_index(&mut b, &at)?;
                p.mb_lp(&mut b, &at)?;
            }
        }
        Ok(())
    }

    /// TILE_HIGHPASS( )
    fn hp_packet(&self, planes: &mut [Plane], tile: &Tile, data: &[u8]) -> Result<(), Error> {
        let mut b = Bits::new(data);
        start_code(&mut b)?;
        for p in planes.iter_mut().filter(|p| p.bands() > Bands::NoHighpass) {
            p.tile_header_hp(&mut b)?;
        }
        for at in tile.mbs() {
            for p in planes.iter_mut().filter(|p| p.bands() > Bands::NoHighpass) {
                p.hp_qp_index(&mut b, &at)?;
                p.mb_cbphp(&mut b, &at)?;
                p.mb_hp(&mut b, &at, None)?;
            }
        }
        Ok(())
    }

    /// TILE_FLEXBITS( )
    fn flex_packet(&self, planes: &mut [Plane], tile: &Tile, data: &[u8]) -> Result<(), Error> {
        let mut b = Bits::new(data);
        start_code(&mut b)?;
        let trim = self.trim(&mut b)?;
        for at in tile.mbs() {
            for p in planes.iter_mut().filter(|p| p.bands() == Bands::All) {
                p.mb_flex(&mut b, &at, trim)?;
            }
        }
        Ok(())
    }
}

/// TILE_STARTCODE and ARBITRARY_BYTE
fn start_code(b: &mut Bits) -> Result<(), Error> {
    if b.read(24)? != 1 {
        return Err(Error::Invalid("tile start code"));
    }
    b.read(8)?;
    Ok(())
}

/// A tile's macroblock columns `x0..x1` and rows `y0..y1`.
struct Tile {
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
}

impl Tile {
    fn mbs(&self) -> impl Iterator<Item = At> + '_ {
        (self.y0..self.y1).flat_map(move |y| {
            (self.x0..self.x1).map(move |x| {
                let reset = (x - self.x0) % 16 == 0;
                At { x, y, left: x == self.x0, top: y == self.y0, reset, adapt: reset || x == self.x1 - 1 }
            })
        })
    }
}

// Output formatting

/// What a plane becomes in the output pixels.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// Grey or RGB, `rgb` spreading grey to three channels.
    Colour { rgb: bool },
    /// The fourth channel; `interleaved` when the plane shares the colour codestream.
    Alpha { interleaved: bool },
}

/// Rounds a reconstructed sample to 8 bits (AddBias( ), ComputeScaling( ), ClippingBasic( )); `round` is `None` for
/// an unscaled plane.
#[inline]
fn to_u8(v: i64, round: Option<i64>) -> u8 {
    let v = match round {
        Some(r) => (v + (128 << 3) + r) >> 3,
        None => v + 128,
    };
    v.clamp(0, 255) as u8
}

/// Upsample( ) filter taps by CHROMA_CENTERING (Table 181); unknown positions take 0.
fn taps(centering: u8) -> [i64; 4] {
    const H: [[i64; 4]; 5] = [[4, 4, 0, 8], [5, 3, 1, 7], [6, 2, 2, 6], [7, 1, 3, 5], [8, 0, 4, 4]];
    *H.get(usize::from(centering)).unwrap_or(&H[0])
}

/// One row of a component upsampled 2× along it (Upsample( ), 9.10.3.2).
fn upsample(src: &[i64], dst: &mut [i64], h: [i64; 4]) {
    let n = src.len();
    for k in 0..n {
        dst[2 * k] = (h[2] * src[k.saturating_sub(1)] + h[3] * src[k] + 4) >> 3;
        dst[2 * k + 1] = (h[0] * src[k] + h[1] * src[(k + 1).min(n - 1)] + 4) >> 3;
    }
}

/// Formats a reconstructed plane into `out`, pixels `stride` bytes apart, cropped to the output window.
pub(crate) fn write_plane(plane: &Plane, h: &ImageHeader, out: &mut [u8], stride: usize, role: Role) {
    let (w, ht) = (h.width as usize, h.height as usize);
    let (top, left) = (h.top as usize, h.left as usize);
    // The reference software rounds an interleaved alpha plane up from the half, other planes down.
    let round = plane.header.scaled.then_some(if role == (Role::Alpha { interleaved: true }) { 4 } else { 3 });
    let y = &plane.components[0];
    let format = plane.header.format;
    if format == Format::YOnly {
        let (offset, rgb) = match role {
            Role::Colour { rgb } => (0, rgb),
            Role::Alpha { .. } => (3, false),
        };
        for row in 0..ht {
            let src = &y.data[(row + top) * y.width + left..][..w];
            for (x, &v) in src.iter().enumerate() {
                let g = to_u8(i64::from(v), round);
                let o = &mut out[(row * w + x) * stride + offset..];
                o[0] = g;
                if rgb {
                    o[1] = g;
                    o[2] = g;
                }
            }
        }
        return;
    }
    // SamplingConversion( ) a row at a time: down the column first for 4:2:0, then along the row.
    let (u, v) = (&plane.components[1], &plane.components[2]);
    let mut rows = [vec![0i64; y.width], vec![0i64; y.width]];
    let mut half = vec![0i64; u.width];
    let (hx, hy) = (taps(plane.header.centering_x), taps(plane.header.centering_y));
    for row in 0..ht {
        let ry = row + top;
        for (c, dst) in [u, v].into_iter().zip(&mut rows) {
            let line = |r: usize| c.data[r * c.width..][..c.width].iter().map(|&s| i64::from(s));
            let src: &mut [i64] = if format == Format::Yuv444 { dst } else { &mut half };
            if format == Format::Yuv420 {
                let k = ry / 2;
                let (a, b, wa, wb) = if ry % 2 == 0 {
                    (k.saturating_sub(1), k, hy[2], hy[3])
                } else {
                    (k, (k + 1).min(c.height - 1), hy[0], hy[1])
                };
                for ((s, p), q) in src.iter_mut().zip(line(a)).zip(line(b)) {
                    *s = (wa * p + wb * q + 4) >> 3;
                }
            } else {
                for (s, p) in src.iter_mut().zip(line(ry)) {
                    *s = p;
                }
            }
            if format != Format::Yuv444 {
                upsample(&half, dst, hx);
            }
        }
        // InvColorFmtConvert2( )
        let src = &y.data[ry * y.width + left..][..w];
        for (x, &l) in src.iter().enumerate() {
            let (yy, uu, vv) = (i64::from(l), rows[0][x + left], rows[1][x + left]);
            let t = -uu;
            let g = yy - (t >> 1);
            let r = t + g - ((vv + 1) >> 1);
            let b = vv + r;
            let o = &mut out[(row * w + x) * stride..];
            o[0] = to_u8(r, round);
            o[1] = to_u8(g, round);
            o[2] = to_u8(b, round);
        }
    }
}
