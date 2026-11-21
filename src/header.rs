//! IMAGE_HEADER( ) and IMAGE_PLANE_HEADER( ) (T.832 8.3, 8.4)

use crate::Error;
use crate::bits::Bits;

/// The most components a supported internal colour format has.
pub(crate) const MAX_COMPONENTS: usize = 3;

/// INTERNAL_CLR_FMT
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    YOnly,
    Yuv420,
    Yuv422,
    Yuv444,
}

impl Format {
    pub fn is_42x(self) -> bool {
        matches!(self, Format::Yuv420 | Format::Yuv422)
    }

    pub fn components(self) -> usize {
        if self == Format::YOnly { 1 } else { 3 }
    }
}

/// OUTPUT_CLR_FMT values this decoder writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Output {
    Gray,
    Rgb,
}

/// BANDS_PRESENT
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Bands {
    DcOnly,
    NoHighpass,
    NoFlexbits,
    All,
}

impl Bands {
    /// NumBands
    pub fn count(self) -> usize {
        self as usize + 1
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ImageHeader {
    pub hard_tiling: bool,
    pub frequency: bool,
    pub transform: u8,
    pub index_table: bool,
    pub overlap: u8,
    pub trim_flexbits: bool,
    pub premultiplied: bool,
    pub alpha_plane: bool,
    pub output: Output,
    pub width: u32,
    pub height: u32,
    pub top: u32,
    pub left: u32,
    /// MB columns and rows.
    pub mb_width: usize,
    pub mb_height: usize,
    /// LeftMBIndexOfTile[ ], NumTileCols + 1 entries.
    pub tile_x: Vec<usize>,
    /// TopMBIndexOfTile[ ], NumTileRows + 1 entries.
    pub tile_y: Vec<usize>,
}

impl ImageHeader {
    pub fn read(b: &mut Bits) -> Result<Self, Error> {
        if b.read(32)? != 0x574D_5048 || b.read(32)? != 0x4F54_4F00 {
            return Err(Error::NotJpegXr);
        }
        if b.read(4)? != 1 {
            return Err(Error::Unsupported("codestream version"));
        }
        let hard_tiling = b.bit()?;
        b.read(3)?;
        let tiling = b.bit()?;
        let frequency = b.bit()?;
        let transform = b.read(3)? as u8;
        let index_table = b.bit()?;
        let overlap = b.read(2)? as u8;
        let short = b.bit()?;
        let _long_word = b.bit()?;
        let windowing = b.bit()?;
        let trim_flexbits = b.bit()?;
        b.read(2)?; // RESERVED_D, RED_BLUE_NOT_SWAPPED_FLAG
        let premultiplied = b.bit()?;
        let alpha_plane = b.bit()?;
        let output = match b.read(4)? {
            0 => Output::Gray,
            7 => Output::Rgb,
            _ => return Err(Error::Unsupported("output colour format")),
        };
        if b.read(4)? != 1 {
            return Err(Error::Unsupported("output bit depth other than 8"));
        }
        if overlap == 3 {
            return Err(Error::Invalid("overlap mode"));
        }
        let size_bits = if short { 16 } else { 32 };
        let width = b.read(size_bits)?.checked_add(1).ok_or(Error::Limit)?;
        let height = b.read(size_bits)?.checked_add(1).ok_or(Error::Limit)?;
        let (cols, rows) = if tiling { (b.read(12)? as usize + 1, b.read(12)? as usize + 1) } else { (1, 1) };
        let tile_bits = if short { 8 } else { 16 };
        let mut tile_w = Vec::with_capacity(cols - 1);
        for _ in 1..cols {
            tile_w.push(b.read(tile_bits)? as usize);
        }
        let mut tile_h = Vec::with_capacity(rows - 1);
        for _ in 1..rows {
            tile_h.push(b.read(tile_bits)? as usize);
        }
        let (top, left, bottom, right) = if windowing {
            (b.read(6)?, b.read(6)?, b.read(6)?, b.read(6)?)
        } else {
            (0, 0, (16 - height % 16) % 16, (16 - width % 16) % 16)
        };
        let ext_w = u64::from(width) + u64::from(left) + u64::from(right);
        let ext_h = u64::from(height) + u64::from(top) + u64::from(bottom);
        if ext_w % 16 != 0 || ext_h % 16 != 0 {
            return Err(Error::Invalid("extended size is not a whole number of macroblocks"));
        }
        let mb_width = usize::try_from(ext_w / 16).map_err(|_| Error::Limit)?;
        let mb_height = usize::try_from(ext_h / 16).map_err(|_| Error::Limit)?;
        let tile_x = boundaries(&tile_w, mb_width)?;
        let tile_y = boundaries(&tile_h, mb_height)?;
        Ok(ImageHeader {
            hard_tiling,
            frequency,
            transform,
            index_table,
            overlap,
            trim_flexbits,
            premultiplied,
            alpha_plane,
            output,
            width,
            height,
            top,
            left,
            mb_width,
            mb_height,
            tile_x,
            tile_y,
        })
    }

    pub fn tiles(&self) -> usize {
        (self.tile_x.len() - 1) * (self.tile_y.len() - 1)
    }
}

/// DetermineLeftBoundaryofTile( ): tile sizes to strictly increasing boundaries ending at `total`.
fn boundaries(sizes: &[usize], total: usize) -> Result<Vec<usize>, Error> {
    let mut out = Vec::with_capacity(sizes.len() + 2);
    out.push(0);
    let mut at = 0;
    for &s in sizes {
        at += s;
        if s == 0 || at >= total {
            return Err(Error::Invalid("tile boundaries"));
        }
        out.push(at);
    }
    out.push(total);
    Ok(out)
}

/// A QP for each component.
pub(crate) type Qp = [u8; MAX_COMPONENTS];

/// DC_QP( ), and one set of LP_QP( ) or HP_QP( ).
pub(crate) fn read_qp(b: &mut Bits, components: usize) -> Result<Qp, Error> {
    let mode = if components == 1 { 0 } else { b.read(2)? };
    Ok(match mode {
        0 => [b.read(8)? as u8; MAX_COMPONENTS],
        1 => {
            let luma = b.read(8)? as u8;
            let chroma = b.read(8)? as u8;
            [luma, chroma, chroma]
        }
        2 => {
            let mut q = [0; MAX_COMPONENTS];
            for v in q.iter_mut().take(components) {
                *v = b.read(8)? as u8;
            }
            q
        }
        _ => return Err(Error::Invalid("component mode")),
    })
}

/// Up to 16 QP sets for the LP or HP band.
#[derive(Clone, Copy, Debug)]
pub(crate) struct QpSets {
    pub count: usize,
    pub sets: [Qp; 16],
}

impl QpSets {
    pub fn one(q: Qp) -> Self {
        let mut sets = [[0; MAX_COMPONENTS]; 16];
        sets[0] = q;
        QpSets { count: 1, sets }
    }

    pub fn read(b: &mut Bits, count: usize, components: usize) -> Result<Self, Error> {
        let mut s = QpSets::one([0; MAX_COMPONENTS]);
        s.count = count;
        for q in s.sets.iter_mut().take(count) {
            *q = read_qp(b, components)?;
        }
        Ok(s)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PlaneHeader {
    pub format: Format,
    pub scaled: bool,
    pub bands: Bands,
    pub centering_x: u8,
    pub centering_y: u8,
    pub dc: Option<Qp>,
    pub lp: Option<Qp>,
    pub hp: Option<Qp>,
}

impl PlaneHeader {
    pub fn read(b: &mut Bits, alpha: bool) -> Result<Self, Error> {
        let format = match b.read(3)? {
            0 => Format::YOnly,
            1 => Format::Yuv420,
            2 => Format::Yuv422,
            3 => Format::Yuv444,
            _ => return Err(Error::Unsupported("internal colour format")),
        };
        if alpha && format != Format::YOnly {
            return Err(Error::Invalid("alpha plane colour format"));
        }
        let scaled = b.bit()?;
        let bands = match b.read(4)? {
            0 => Bands::All,
            1 => Bands::NoFlexbits,
            2 => Bands::NoHighpass,
            3 => Bands::DcOnly,
            _ => return Err(Error::Invalid("bands present")),
        };
        let (mut centering_x, mut centering_y) = (0, 0);
        if format != Format::YOnly {
            if format.is_42x() {
                b.read(1)?;
                centering_x = b.read(3)? as u8;
            } else {
                b.read(4)?;
            }
            if format == Format::Yuv420 {
                b.read(1)?;
                centering_y = b.read(3)? as u8;
            } else {
                b.read(4)?;
            }
        }
        let components = format.components();
        let dc = if b.bit()? { Some(read_qp(b, components)?) } else { None };
        let (mut lp, mut hp) = (None, None);
        if bands != Bands::DcOnly {
            b.read(1)?;
            if b.bit()? {
                lp = Some(read_qp(b, components)?);
            }
            if bands != Bands::NoHighpass {
                b.read(1)?;
                if b.bit()? {
                    hp = Some(read_qp(b, components)?);
                }
            }
        }
        b.align();
        Ok(PlaneHeader { format, scaled, bands, centering_x, centering_y, dc, lp, hp })
    }
}
