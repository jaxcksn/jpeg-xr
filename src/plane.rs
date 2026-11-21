//! An image plane: macroblock parsing (T.832 8.7, 8.9 to 8.12) and transform coefficient decoding (9.5 to 9.8)

// Loops over blocks and components index several parallel tables at once.
#![allow(clippy::needless_range_loop)]

use crate::Error;
use crate::bits::Bits;
use crate::header::{Bands, Format, MAX_COMPONENTS, PlaneHeader, Qp, QpSets, read_qp};
use crate::vlc::*;

/// One component's samples: transform coefficients while parsing, then reconstructed values.
pub(crate) struct Component {
    pub width: usize,
    pub height: usize,
    /// 4×4 blocks per macroblock, across and down.
    pub bw: usize,
    pub bh: usize,
    // ponytail: the whole component at 4 bytes a sample; decoding by macroblock rows would hold a few rows instead.
    pub data: Vec<i32>,
}

impl Component {
    /// The sample index of coefficient `k` of block `b` (raster order) in macroblock (`x`, `y`).
    #[inline]
    pub fn at(&self, x: usize, y: usize, b: usize, k: usize) -> usize {
        let px = (x * self.bw + b % self.bw) * 4 + (k & 3);
        let py = (y * self.bh + b / self.bw) * 4 + (k >> 2);
        py * self.width + px
    }
}

/// Where a macroblock sits in its tile.
#[derive(Clone, Copy, Debug)]
pub(crate) struct At {
    pub x: usize,
    pub y: usize,
    /// On the tile's left and top edges.
    pub left: bool,
    pub top: bool,
    /// The scan totals reset before this macroblock.
    pub reset: bool,
    /// The code tables adapt after this macroblock.
    pub adapt: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct MbInfo {
    dc_mode: u8,
    hp_mode: u8,
    lp_qp: u8,
    hp_qp: u8,
    /// ModelBitsMBHP
    bits: [u8; 2],
    cbphp: [u16; MAX_COMPONENTS],
}

/// The adaptive code tables of the LP or HP band.
#[derive(Clone, Copy)]
struct BandCoder {
    first: [Adapt; 2],
    /// By chroma, then context.
    index: [[Adapt; 2]; 2],
    /// By context.
    level: [Adapt; 2],
}

impl BandCoder {
    fn new() -> Self {
        BandCoder { first: [Adapt::many(); 2], index: [[Adapt::many(); 2]; 2], level: [Adapt::two(); 2] }
    }

    fn adapt(&mut self) {
        for a in &mut self.first {
            a.adapt_many(4);
        }
        for a in self.index.iter_mut().flatten() {
            a.adapt_many(3);
        }
        for a in &mut self.level {
            a.adapt_two();
        }
    }
}

/// Adaptive coefficient normalization (8.12).
#[derive(Clone, Copy, Default)]
struct Model {
    state: [i32; 2],
    bits: [i32; 2],
}

impl Model {
    fn new(band: i32) -> Self {
        Model { state: [0; 2], bits: [(2 - band) * 4; 2] }
    }
}

/// An adaptive inverse scan (8.11): positions 1 to 15.
#[derive(Clone, Copy)]
struct Scan {
    order: [u8; 16],
    totals: [u32; 16],
}

const SCAN_ORDER_0: [u8; 16] = [0, 4, 1, 5, 8, 2, 9, 6, 12, 3, 10, 13, 7, 14, 11, 15];
const SCAN_ORDER_1: [u8; 16] = [0, 1, 2, 5, 4, 3, 6, 9, 8, 7, 12, 15, 13, 10, 11, 14];
const SCAN_TOTALS: [u32; 16] = [0, 32, 30, 28, 26, 24, 22, 20, 18, 16, 14, 12, 10, 8, 6, 4];

impl Scan {
    fn new(order: [u8; 16]) -> Self {
        Scan { order, totals: SCAN_TOTALS }
    }

    /// The raster position of scan position `i`, which then moves up if it has become more frequent.
    #[inline]
    fn place(&mut self, i: usize) -> usize {
        let k = usize::from(self.order[i]);
        self.totals[i] += 1;
        if i > 1 && self.totals[i] > self.totals[i - 1] {
            self.totals.swap(i, i - 1);
            self.order.swap(i, i - 1);
        }
        k
    }
}

const TRANSPOSE_444: [usize; 16] = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15];
const TRANSPOSE_422: [usize; 8] = [0, 2, 1, 3, 4, 6, 5, 7];
const TRANSPOSE_420: [usize; 4] = [0, 2, 1, 3];
const HIER_SCAN: [usize; 16] = [0, 1, 4, 5, 2, 3, 6, 7, 8, 9, 12, 13, 10, 11, 14, 15];

/// The parsing state of a tile, reset at its first macroblock.
struct State {
    dc: [Adapt; 2],
    lp: BandCoder,
    hp: BandCoder,
    num_cbphp: Adapt,
    num_blk_cbphp: Adapt,
    model_dc: Model,
    model_lp: Model,
    model_hp: Model,
    cbphp_state: [i32; 2],
    count_ones: [i32; 2],
    count_zeroes: [i32; 2],
    count_zero_cbplp: i32,
    count_max_cbplp: i32,
    lp_scan: Scan,
    hp_hor: Scan,
    hp_ver: Scan,
}

impl State {
    fn new() -> Self {
        State {
            dc: [Adapt::two(); 2],
            lp: BandCoder::new(),
            hp: BandCoder::new(),
            num_cbphp: Adapt::two(),
            num_blk_cbphp: Adapt::two(),
            model_dc: Model::new(0),
            model_lp: Model::new(1),
            model_hp: Model::new(2),
            cbphp_state: [0; 2],
            count_ones: [-4; 2],
            count_zeroes: [4; 2],
            count_zero_cbplp: 1,
            count_max_cbplp: 1,
            lp_scan: Scan::new(SCAN_ORDER_0),
            hp_hor: Scan::new(SCAN_ORDER_0),
            hp_ver: Scan::new(SCAN_ORDER_1),
        }
    }
}

pub(crate) struct Plane {
    pub header: PlaneHeader,
    pub components: Vec<Component>,
    mb_width: usize,
    mbs: Vec<MbInfo>,
    /// PredDCLP: by macroblock, then component.
    pred: Vec<[i32; 7]>,
    st: State,
    dc_qp: Qp,
    lp_qp: QpSets,
    hp_qp: QpSets,
    use_dc_qp: bool,
    use_lp_qp: bool,
}

impl Plane {
    /// A plane of `mb_width` × `mb_height` macroblocks; the caller has bounded the size, and sizes past `usize` are
    /// [`Error::Limit`].
    pub fn new(header: PlaneHeader, mb_width: usize, mb_height: usize) -> Result<Self, Error> {
        let n = header.format.components();
        let mul = |a: usize, b: usize| a.checked_mul(b).ok_or(Error::Limit);
        let mbs = mul(mb_width, mb_height)?;
        let components = (0..n)
            .map(|i| {
                let (bw, bh) = match (i, header.format) {
                    (0, _) | (_, Format::Yuv444) | (_, Format::YOnly) => (4, 4),
                    (_, Format::Yuv422) => (2, 4),
                    (_, Format::Yuv420) => (2, 2),
                };
                let (width, height) = (mul(mb_width, bw * 4)?, mul(mb_height, bh * 4)?);
                Ok(Component { width, height, bw, bh, data: vec![0; mul(width, height)?] })
            })
            .collect::<Result<_, Error>>()?;
        let zero = [0; MAX_COMPONENTS];
        Ok(Plane {
            components,
            mb_width,
            mbs: vec![MbInfo::default(); mbs],
            pred: vec![[0; 7]; mul(mbs, n)?],
            st: State::new(),
            dc_qp: header.dc.unwrap_or(zero),
            lp_qp: QpSets::one(header.lp.unwrap_or(zero)),
            hp_qp: QpSets::one(header.hp.unwrap_or(zero)),
            use_dc_qp: false,
            use_lp_qp: false,
            header,
        })
    }

    fn format(&self) -> Format {
        self.header.format
    }

    pub fn bands(&self) -> Bands {
        self.header.bands
    }

    fn n(&self) -> usize {
        self.components.len()
    }

    fn mb(&self, x: usize, y: usize) -> usize {
        y * self.mb_width + x
    }

    // Tile headers (8.7.4, 8.7.6, 8.7.8)

    pub fn tile_header_dc(&mut self, b: &mut Bits) -> Result<(), Error> {
        if self.header.dc.is_none() {
            self.dc_qp = read_qp(b, self.n())?;
        }
        Ok(())
    }

    pub fn tile_header_lp(&mut self, b: &mut Bits) -> Result<(), Error> {
        if self.header.lp.is_none() {
            self.use_dc_qp = b.bit()?;
            self.lp_qp = if self.use_dc_qp {
                QpSets::one(self.dc_qp)
            } else {
                let count = b.read(4)? as usize + 1;
                QpSets::read(b, count, self.n())?
            };
        }
        Ok(())
    }

    pub fn tile_header_hp(&mut self, b: &mut Bits) -> Result<(), Error> {
        if self.header.hp.is_none() {
            self.use_lp_qp = b.bit()?;
            self.hp_qp = if self.use_lp_qp {
                self.lp_qp
            } else {
                let count = b.read(4)? as usize + 1;
                QpSets::read(b, count, self.n())?
            };
        }
        Ok(())
    }

    /// LP_QP_INDEX, when present.
    pub fn lp_qp_index(&mut self, b: &mut Bits, at: &At) -> Result<(), Error> {
        let i = if self.lp_qp.count > 1 && !self.use_dc_qp { qp_index(b, self.lp_qp.count)? } else { 0 };
        let m = self.mb(at.x, at.y);
        self.mbs[m].lp_qp = i;
        Ok(())
    }

    /// HP_QP_INDEX, when present, or the one it is inferred to be.
    pub fn hp_qp_index(&mut self, b: &mut Bits, at: &At) -> Result<(), Error> {
        let m = self.mb(at.x, at.y);
        self.mbs[m].hp_qp = if self.use_lp_qp {
            self.mbs[m].lp_qp
        } else if self.hp_qp.count > 1 {
            qp_index(b, self.hp_qp.count)?
        } else {
            0
        };
        Ok(())
    }

    // DC (8.7.11, 9.5.1, 9.6.1, 9.8.1)

    pub fn mb_dc(&mut self, b: &mut Bits, at: &At) -> Result<(), Error> {
        if at.left && at.top {
            self.st.dc = [Adapt::two(); 2];
            self.st.model_dc = Model::new(0);
        }
        let mut lap = [0i32; 2];
        let mut dc = [0i32; MAX_COMPONENTS];
        if self.format() == Format::YOnly {
            let abs = b.bit()?;
            lap[0] += i32::from(abs);
            dc[0] = self.decode_dc(b, 0, false, abs)?;
        } else {
            let v = b.vlc(&VAL_DC_YUV)?;
            for (n, d) in dc.iter_mut().enumerate() {
                let abs = v & (4 >> n) != 0;
                let m = n.min(1);
                lap[m] += i32::from(abs);
                *d = self.decode_dc(b, m, n > 0, abs)?;
            }
        }
        let (fmt, n) = (self.format(), self.n());
        update_model(&mut self.st.model_dc, lap, 0, fmt, n);
        if at.adapt {
            for a in &mut self.st.dc {
                a.adapt_two();
            }
        }
        self.dc_predict(at, &mut dc);
        let m = self.mb(at.x, at.y);
        for (i, d) in dc.iter().enumerate().take(n) {
            self.pred[m * n + i][0] = *d;
            let q = quant_map(self.dc_qp[i], i == 0, self.header.scaled);
            let c = &mut self.components[i];
            let p = c.at(at.x, at.y, 0, 0);
            c.data[p] = d.wrapping_mul(q);
        }
        Ok(())
    }

    fn decode_dc(&mut self, b: &mut Bits, model: usize, chroma: bool, abs: bool) -> Result<i32, Error> {
        let mut dc = 0i32;
        if abs {
            dc = abs_level(b, &mut self.st.dc[usize::from(chroma)])? - 1;
        }
        let bits = self.st.model_dc.bits[model] as u32;
        if bits > 0 {
            dc = (dc << bits) | b.read(bits)? as i32;
        }
        if dc != 0 && b.bit()? {
            dc = dc.wrapping_neg();
        }
        Ok(dc)
    }

    /// CalcDCPredMode( ), DCCoefficientPrediction( )
    fn dc_predict(&mut self, at: &At, dc: &mut [i32; MAX_COMPONENTS]) {
        let n = self.n();
        let m = self.mb(at.x, at.y);
        let mode = match (at.left, at.top) {
            (true, true) => 3,
            (true, false) => 1,
            (false, true) => 0,
            (false, false) => {
                let l = (m - 1) * n;
                let t = (m - self.mb_width) * n;
                let tl = t - n;
                let d = |a: usize, b: usize| u64::from(self.pred[a][0].wrapping_sub(self.pred[b][0]).unsigned_abs());
                let (mut hor, mut ver) = (d(tl, l), d(tl, t));
                if n > 1 {
                    let scale = match self.format() {
                        Format::Yuv420 => 8,
                        Format::Yuv422 => 4,
                        _ => 2,
                    };
                    hor = hor * scale + d(tl + 1, l + 1) + d(tl + 2, l + 2);
                    ver = ver * scale + d(tl + 1, t + 1) + d(tl + 2, t + 2);
                }
                if hor * 4 < ver {
                    1
                } else if ver * 4 < hor {
                    0
                } else {
                    2
                }
            }
        };
        self.mbs[m].dc_mode = mode;
        if mode == 3 {
            return;
        }
        for (i, d) in dc.iter_mut().enumerate().take(n) {
            let left = || self.pred[(m - 1) * n + i][0];
            let top = || self.pred[(m - self.mb_width) * n + i][0];
            let p = match mode {
                0 => left(),
                1 => top(),
                _ if i == 0 || !self.format().is_42x() => left().wrapping_add(top()) >> 1,
                _ => left().wrapping_add(top()).wrapping_add(1) >> 1,
            };
            *d = d.wrapping_add(p);
        }
    }

    // LP (8.7.16, 9.5.2, 9.6.2, 9.8.2)

    pub fn mb_lp(&mut self, b: &mut Bits, at: &At) -> Result<(), Error> {
        let fmt = self.format();
        let n = self.n();
        if at.left && at.top {
            self.st.count_zero_cbplp = 1;
            self.st.count_max_cbplp = 1;
            self.st.lp = BandCoder::new();
            self.st.lp_scan = Scan::new(SCAN_ORDER_0);
            self.st.model_lp = Model::new(1);
        }
        if at.reset {
            self.st.lp_scan.totals = SCAN_TOTALS;
        }
        let full = if fmt.is_42x() { 2 } else { n };
        let cbplp = if fmt == Format::YOnly {
            u32::from(b.bit()?)
        } else {
            let max = (full * 4 - 5) as u32;
            let v = if self.st.count_zero_cbplp <= 0 || self.st.count_max_cbplp < 0 {
                let v = b.vlc(if fmt == Format::Yuv444 { &CBPLP_YUV1_444 } else { &CBPLP_YUV1_42X })? as u32;
                if self.st.count_max_cbplp < self.st.count_zero_cbplp { max - v } else { v }
            } else {
                b.read(full as u32)?
            };
            let st = &mut self.st;
            st.count_zero_cbplp = (st.count_zero_cbplp + 1 - 4 * i32::from(v == 0)).clamp(-8, 7);
            st.count_max_cbplp = (st.count_max_cbplp + 1 - 4 * i32::from(v == max)).clamp(-8, 7);
            v
        };
        let mut lp = [[0i32; 16]; MAX_COMPONENTS];
        let mut lap = [0i32; 2];
        for p in 0..full {
            let idx = p.min(1);
            let chroma42x = p > 0 && fmt.is_42x();
            let mut nnz = 0;
            if (cbplp >> p) & 1 != 0 {
                let mut rl = [0i32; 32];
                let location = match fmt {
                    Format::Yuv420 if p > 0 => 10,
                    Format::Yuv422 if p > 0 => 2,
                    _ => 1,
                };
                nnz = decode_block(b, idx == 1, &mut rl, &mut self.st.lp, location)?;
                if chroma42x {
                    let mut tmp = [0i32; 14];
                    let mut i = 0;
                    for k in 0..nnz {
                        i += rl[k * 2] as usize;
                        tmp[i] = rl[k * 2 + 1];
                        i += 1;
                    }
                    const REMAP: [usize; 7] = [4, 1, 2, 3, 5, 6, 7];
                    let (count, offset) = if fmt == Format::Yuv422 { (14, 0) } else { (6, 1) };
                    for (k, &t) in tmp.iter().enumerate().take(count) {
                        let r = REMAP[(k >> 1) + offset];
                        let pos = if fmt == Format::Yuv420 { TRANSPOSE_420[r] } else { TRANSPOSE_422[r] };
                        lp[(k & 1) + 1][pos] = t;
                    }
                } else {
                    let mut i = 1;
                    for k in 0..nnz {
                        i += rl[k * 2] as usize;
                        let pos = self.st.lp_scan.place(i);
                        lp[p][pos] = rl[k * 2 + 1];
                        i += 1;
                    }
                }
            }
            lap[idx] += nnz as i32;
            let bits = self.st.model_lp.bits[idx] as u32;
            if bits > 0 {
                if chroma42x {
                    let t: &[usize] = if fmt == Format::Yuv420 { &TRANSPOSE_420 } else { &TRANSPOSE_422 };
                    for &k in &t[1..] {
                        lp[1][k] = refine(b, lp[1][k], bits)?;
                        lp[2][k] = refine(b, lp[2][k], bits)?;
                    }
                } else {
                    for &k in &TRANSPOSE_444[1..] {
                        lp[p][k] = refine(b, lp[p][k], bits)?;
                    }
                }
            }
        }
        update_model(&mut self.st.model_lp, lap, 1, fmt, n);
        if at.adapt {
            self.st.lp.adapt();
        }
        self.lp_decode(at, &mut lp);
        Ok(())
    }

    /// LP prediction (9.6.2), dequantization (9.8.2), and placement in the sample arrays.
    fn lp_decode(&mut self, at: &At, lp: &mut [[i32; 16]; MAX_COMPONENTS]) {
        let n = self.n();
        let m = self.mb(at.x, at.y);
        let info = self.mbs[m];
        let mode = if info.dc_mode == 0 && info.lp_qp == self.mbs[m - 1].lp_qp {
            0
        } else if info.dc_mode == 1 && info.lp_qp == self.mbs[m - self.mb_width].lp_qp {
            1
        } else {
            2
        };
        let fmt = self.format();
        for (i, c) in lp.iter_mut().enumerate().take(n) {
            let me = m * n + i;
            let left = if mode == 0 { self.pred[(m - 1) * n + i] } else { [0; 7] };
            let top = if mode == 1 { self.pred[(m - self.mb_width) * n + i] } else { [0; 7] };
            let add = |c: &mut [i32; 16], k: usize, v: i32| c[k] = c[k].wrapping_add(v);
            match fmt {
                Format::Yuv420 if i > 0 => {
                    match mode {
                        0 => add(c, 2, left[2]),
                        1 => add(c, 1, top[1]),
                        _ => {}
                    }
                    self.pred[me][1] = c[1];
                    self.pred[me][2] = c[2];
                }
                Format::Yuv422 if i > 0 => {
                    match mode {
                        0 => {
                            add(c, 4, left[4]);
                            add(c, 2, left[2]);
                            add(c, 6, left[6]);
                        }
                        1 => {
                            add(c, 4, top[4]);
                            add(c, 1, top[5]);
                            add(c, 5, c[1]);
                        }
                        _ if info.dc_mode == 1 => add(c, 5, c[1]),
                        _ => {}
                    }
                    for k in [1, 2, 4, 5, 6] {
                        self.pred[me][k] = c[k];
                    }
                }
                _ => {
                    match mode {
                        0 => {
                            add(c, 4, left[4]);
                            add(c, 8, left[5]);
                            add(c, 12, left[6]);
                        }
                        1 => {
                            for k in 1..4 {
                                add(c, k, top[k]);
                            }
                        }
                        _ => {}
                    }
                    let p = &mut self.pred[me];
                    p[1..5].copy_from_slice(&c[1..5]);
                    p[5] = c[8];
                    p[6] = c[12];
                }
            }
            let q = quant_map(self.lp_qp.sets[usize::from(info.lp_qp)][i], i == 0, self.header.scaled);
            let comp = &mut self.components[i];
            for (j, &v) in c.iter().enumerate().take(comp.bw * comp.bh).skip(1) {
                let p = comp.at(at.x, at.y, j, 0);
                comp.data[p] = v.wrapping_mul(q);
            }
        }
        self.mbs[m].hp_mode = self.hp_mode(lp);
    }

    // HP (8.7.17, 8.7.18, 9.5.3, 9.6.3, 9.8.3)

    /// CalcHPPredMode( ). It reads the LP coefficients before dequantization, as the reference software does: the
    /// Recommendation reads them after, which differs once chroma and luma QPs scale apart.
    fn hp_mode(&self, lp: &[[i32; 16]; MAX_COMPONENTS]) -> u8 {
        let a = |i: usize, j: usize| u64::from(lp[i][j].unsigned_abs());
        let mut hor = a(0, 1) + a(0, 2) + a(0, 3);
        let mut ver = a(0, 4) + a(0, 8) + a(0, 12);
        if self.n() > 1 {
            for i in 1..3 {
                hor += a(i, 1);
                match self.format() {
                    Format::Yuv420 => ver += a(i, 2),
                    Format::Yuv422 => {
                        ver += a(i, 2) + a(i, 6);
                        hor += a(i, 5);
                    }
                    _ => ver += a(i, 4),
                }
            }
        }
        if hor * 4 < ver {
            0
        } else if ver * 4 < hor {
            1
        } else {
            2
        }
    }

    pub fn mb_cbphp(&mut self, b: &mut Bits, at: &At) -> Result<(), Error> {
        const FLC: [u32; 6] = [0, 2, 1, 2, 2, 0];
        const OFF: [usize; 6] = [0, 4, 2, 8, 12, 1];
        const OUT: [u32; 16] = [0, 15, 3, 12, 1, 2, 4, 8, 5, 6, 9, 10, 7, 11, 13, 14];
        if at.left && at.top {
            self.st.num_cbphp = Adapt::two();
            self.st.num_blk_cbphp = Adapt::two();
        }
        let fmt = self.format();
        let mut diff = [0u32; MAX_COMPONENTS];
        let a = &mut self.st.num_cbphp;
        let num = b.vlc(&NUM_CBPHP[a.table])?;
        a.disc1 += NUM_CBPHP_DELTA[num];
        let groups = refine_cbphp(b, num)?;
        for blk in 0..4 {
            if groups & (1 << blk) == 0 {
                continue;
            }
            let a = &mut self.st.num_blk_cbphp;
            let v = if fmt == Format::YOnly {
                let v = b.vlc(&NUM_CBPHP[a.table])?;
                a.disc1 += NUM_CBPHP_DELTA[v];
                v
            } else {
                let v = b.vlc(&NUM_BLKCBPHP_YUV[a.table])?;
                a.disc1 += NUM_BLKCBPHP_DELTA_YUV[v];
                v
            };
            let mut val = v + 1;
            let mut cbp = 0u32;
            if val >= 6 {
                cbp = 0x10 * (b.vlc(&CHR_CBPHP)? as u32 + 1);
                if val >= 9 {
                    val += b.vlc(&CHR_CBPHP)?;
                }
                val -= 6;
            }
            let mut code = OFF[val];
            if FLC[val] > 0 {
                code += b.read(FLC[val])? as usize;
            }
            cbp += OUT[code];
            diff[0] |= (cbp & 0xF) << (blk * 4);
            match fmt {
                Format::Yuv444 => {
                    for k in 0..2 {
                        if (cbp >> (k + 4)) & 1 != 0 {
                            let num = b.vlc(&NUM_CH_BLK)? + 1;
                            diff[k + 1] |= refine_cbphp(b, num)? << (blk * 4);
                        }
                    }
                }
                Format::Yuv422 => {
                    const SHIFT: [u32; 4] = [0, 1, 4, 5];
                    for k in 0..2 {
                        if (cbp >> (k + 4)) & 1 != 0 {
                            let v = b.vlc(&CHR_CBPHP)?;
                            diff[k + 1] |= SHIFT[v + 1] << SHIFT[blk];
                        }
                    }
                }
                Format::Yuv420 => {
                    diff[1] |= ((cbp >> 4) & 1) << blk;
                    diff[2] |= ((cbp >> 5) & 1) << blk;
                }
                Format::YOnly => {}
            }
        }
        self.predict_cbphp(at, &diff);
        Ok(())
    }

    /// PredCBPHP( )
    fn predict_cbphp(&mut self, at: &At, diff: &[u32; MAX_COMPONENTS]) {
        if at.left && at.top {
            self.st.cbphp_state = [0; 2];
            self.st.count_ones = [-4; 2];
            self.st.count_zeroes = [4; 2];
        }
        let m = self.mb(at.x, at.y);
        let fmt = self.format();
        // The neighbour's block whose status starts the prediction: (left, top) bit positions.
        let neighbour = |me: &Self, i: usize, left: u32, top: u32| -> u32 {
            if !at.left {
                (u32::from(me.mbs[m - 1].cbphp[i]) >> left) & 1
            } else if !at.top {
                (u32::from(me.mbs[m - me.mb_width].cbphp[i]) >> top) & 1
            } else {
                1
            }
        };
        for i in 0..self.n() {
            let c1 = usize::from(i > 0);
            let mut cbp = diff[i];
            let state = self.st.cbphp_state[c1];
            let ones = if i > 0 && fmt.is_42x() {
                let (top, mask, weight) = if fmt == Format::Yuv422 { (6, 0xFF, 2) } else { (2, 0xF, 4) };
                if state == 0 {
                    cbp ^= neighbour(self, i, 1, top);
                    if fmt == Format::Yuv422 {
                        cbp ^= (cbp & 0x01) << 1;
                        cbp ^= (cbp & 0x03) << 2;
                        cbp ^= (cbp & 0x0C) << 2;
                        cbp ^= (cbp & 0x30) << 2;
                    } else {
                        cbp ^= 0x02 & (cbp << 1);
                        cbp ^= (cbp & 0x3) << 2;
                    }
                } else if state == 2 {
                    cbp ^= mask;
                }
                cbp.count_ones() as i32 * weight
            } else {
                if state == 0 {
                    cbp ^= neighbour(self, i, 5, 10);
                    cbp ^= 0x02 & (cbp << 1);
                    cbp ^= 0x10 & (cbp << 3);
                    cbp ^= 0x20 & (cbp << 1);
                    cbp ^= (cbp & 0x33) << 2;
                    cbp ^= (cbp & 0x00CC) << 6;
                    cbp ^= (cbp & 0x3300) << 2;
                } else if state == 2 {
                    cbp ^= 0xFFFF;
                }
                cbp.count_ones() as i32
            };
            let st = &mut self.st;
            st.count_ones[c1] = (st.count_ones[c1] + ones - 3).clamp(-16, 15);
            st.count_zeroes[c1] = (st.count_zeroes[c1] + 16 - ones - 3).clamp(-16, 15);
            st.cbphp_state[c1] = if st.count_ones[c1] < 0 {
                if st.count_ones[c1] < st.count_zeroes[c1] { 1 } else { 2 }
            } else if st.count_zeroes[c1] < 0 {
                2
            } else {
                0
            };
            self.mbs[m].cbphp[i] = cbp as u16;
        }
    }

    /// MB_HP( ), or MB_HP_FLEX( ) when `flex` holds TRIM_FLEXBITS.
    pub fn mb_hp(&mut self, b: &mut Bits, at: &At, flex: Option<u32>) -> Result<(), Error> {
        if at.left && at.top {
            self.st.hp = BandCoder::new();
            self.st.hp_hor = Scan::new(SCAN_ORDER_0);
            self.st.hp_ver = Scan::new(SCAN_ORDER_1);
            self.st.model_hp = Model::new(2);
        }
        if at.reset {
            self.st.hp_hor.totals = SCAN_TOTALS;
            self.st.hp_ver.totals = SCAN_TOTALS;
        }
        let m = self.mb(at.x, at.y);
        let mode = self.mbs[m].hp_mode;
        let bits = self.st.model_hp.bits;
        let mut lap = [0i32; 2];
        for i in 0..self.n() {
            let chroma = i > 0;
            let mut cbp = self.mbs[m].cbphp[i];
            let blocks = self.components[i].bw * self.components[i].bh;
            for blk in 0..blocks {
                let map = if blocks == 16 { HIER_SCAN[blk] } else { blk };
                let mut nnz = 0;
                if cbp & 1 != 0 {
                    let mut rl = [0i32; 32];
                    nnz = decode_block(b, chroma, &mut rl, &mut self.st.hp, 1)?;
                    let scan = if mode == 1 { &mut self.st.hp_ver } else { &mut self.st.hp_hor };
                    let c = &mut self.components[i];
                    let mut k = 1;
                    for r in rl.chunks_exact(2).take(nnz) {
                        k += r[0] as usize;
                        let pos = scan.place(k);
                        let p = c.at(at.x, at.y, map, pos);
                        c.data[p] = r[1];
                        k += 1;
                    }
                }
                if let Some(trim) = flex {
                    self.block_flex(b, i, at, map, bits[usize::from(chroma)] as u32, trim)?;
                }
                lap[usize::from(chroma)] += nnz as i32;
                cbp >>= 1;
            }
        }
        self.mbs[m].bits = [bits[0] as u8, bits[1] as u8];
        let (fmt, n) = (self.format(), self.n());
        update_model(&mut self.st.model_hp, lap, 2, fmt, n);
        if at.adapt {
            self.st.hp.adapt();
            self.st.num_cbphp.adapt_two();
            self.st.num_blk_cbphp.adapt_two();
        }
        Ok(())
    }

    /// MB_FLEXBITS( )
    pub fn mb_flex(&mut self, b: &mut Bits, at: &At, trim: u32) -> Result<(), Error> {
        let m = self.mb(at.x, at.y);
        for i in 0..self.n() {
            let bits = u32::from(self.mbs[m].bits[usize::from(i > 0)]);
            let blocks = self.components[i].bw * self.components[i].bh;
            for blk in 0..blocks {
                let map = if blocks == 16 { HIER_SCAN[blk] } else { blk };
                self.block_flex(b, i, at, map, bits, trim)?;
            }
        }
        Ok(())
    }

    /// BLOCK_FLEXBITS( ) and the HP remapping of the block (9.5.4): each coefficient becomes its VLC part shifted
    /// up by `bits`, plus its flexbits.
    fn block_flex(&mut self, b: &mut Bits, i: usize, at: &At, blk: usize, bits: u32, trim: u32) -> Result<(), Error> {
        let left = bits.saturating_sub(trim);
        let c = &mut self.components[i];
        for &k in &TRANSPOSE_444[1..] {
            let p = c.at(at.x, at.y, blk, k);
            let v = c.data[p];
            let mut f = 0i32;
            if left > 0 {
                let r = b.read(left)? as i32;
                f = match v.signum() {
                    1 => r,
                    -1 => -r,
                    _ if r != 0 && b.bit()? => -r,
                    _ => r,
                };
            }
            c.data[p] = (v << bits).wrapping_add(f << trim);
        }
        Ok(())
    }

    /// The rest of HP decoding once a macroblock is parsed: remapping without flexbits, dequantization and
    /// prediction.
    pub fn mb_hp_finish(&mut self, at: &At) {
        let m = self.mb(at.x, at.y);
        let info = self.mbs[m];
        let shift = self.bands() != Bands::All;
        for i in 0..self.n() {
            let bits = u32::from(info.bits[usize::from(i > 0)]);
            let q = quant_map(self.hp_qp.sets[usize::from(info.hp_qp)][i], true, self.header.scaled);
            let c = &mut self.components[i];
            let blocks = c.bw * c.bh;
            for blk in 0..blocks {
                for k in 1..16 {
                    let p = c.at(at.x, at.y, blk, k);
                    let v = if shift { c.data[p] << bits } else { c.data[p] };
                    c.data[p] = v.wrapping_mul(q);
                }
            }
            let ks: [usize; 3] = match info.hp_mode {
                0 => [4, 8, 12],
                1 => [1, 2, 3],
                _ => continue,
            };
            for blk in 0..blocks {
                let (bx, by) = (blk % c.bw, blk / c.bw);
                let from = match info.hp_mode {
                    0 if bx > 0 => blk - 1,
                    1 if by > 0 => blk - c.bw,
                    _ => continue,
                };
                for k in ks {
                    let (p, q) = (c.at(at.x, at.y, blk, k), c.at(at.x, at.y, from, k));
                    c.data[p] = c.data[p].wrapping_add(c.data[q]);
                }
            }
        }
    }
}

/// DECODE_QP_INDEX( )
fn qp_index(b: &mut Bits, count: usize) -> Result<u8, Error> {
    const BITS: [u32; 17] = [0, 0, 1, 1, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4];
    if !b.bit()? {
        return Ok(0);
    }
    let i = b.read(BITS[count])? as usize + 1;
    if i >= count {
        return Err(Error::Invalid("QP index"));
    }
    Ok(i as u8)
}

/// DECODE_ABS_LEVEL( )
fn abs_level(b: &mut Bits, a: &mut Adapt) -> Result<i32, Error> {
    const REMAP: [i32; 6] = [2, 3, 4, 6, 10, 14];
    const FIXED: [u32; 6] = [0, 0, 1, 2, 2, 2];
    let idx = b.vlc(&ABS_LEVEL_INDEX[a.table])?;
    a.disc1 += ABS_LEVEL_DELTA[idx];
    if idx < 6 {
        return Ok(REMAP[idx] + b.read(FIXED[idx])? as i32);
    }
    let mut fixed = b.read(4)? + 4;
    if fixed == 19 {
        fixed += b.read(2)?;
        if fixed == 22 {
            fixed += b.read(3)?;
        }
    }
    Ok(2 + (1 << fixed) + b.read(fixed)? as i32)
}

/// DECODE_BLOCK( ): run/level pairs into `rl`, starting at scan position `location`; the number of pairs.
fn decode_block(
    b: &mut Bits,
    chroma: bool,
    rl: &mut [i32; 32],
    coder: &mut BandCoder,
    mut location: usize,
) -> Result<usize, Error> {
    let a = &mut coder.first[usize::from(chroma)];
    let first = b.vlc(&FIRST_INDEX[a.table])?;
    a.disc1 += FIRST_INDEX_DELTA[a.delta][first];
    a.disc2 += FIRST_INDEX_DELTA[a.delta2][first];
    let negative = b.bit()?;
    let sr = first & 1;
    let mut srn = first >> 2;
    let mut ctx = sr & srn;
    let mut level = if first & 2 != 0 { abs_level(b, &mut coder.level[ctx])? } else { 1 };
    rl[1] = if negative { -level } else { level };
    rl[0] = if sr == 0 { run(b, location)? } else { 0 };
    location += rl[0] as usize + 1;
    let mut count = 1;
    while srn != 0 {
        if location > 16 {
            return Err(Error::Invalid("coefficients past the end of a block"));
        }
        let r = if srn & 1 == 0 { run(b, location)? } else { 0 };
        rl[count * 2] = r;
        location += r as usize + 1;
        if location > 16 {
            return Err(Error::Invalid("coefficients past the end of a block"));
        }
        let index = if location < 15 {
            let a = &mut coder.index[usize::from(chroma)][ctx];
            let v = b.vlc(&INDEX_A[a.table])?;
            a.disc1 += INDEX_A_DELTA[a.delta][v];
            a.disc2 += INDEX_A_DELTA[a.delta2][v];
            v
        } else if location == 15 {
            b.vlc(&INDEX_B)?
        } else {
            usize::from(b.bit()?)
        };
        srn = index >> 1;
        ctx &= srn;
        let negative = b.bit()?;
        level = if index & 1 != 0 { abs_level(b, &mut coder.level[ctx])? } else { 1 };
        rl[count * 2 + 1] = if negative { -level } else { level };
        count += 1;
    }
    if location > 16 {
        return Err(Error::Invalid("coefficients past the end of a block"));
    }
    Ok(count)
}

/// DECODE_RUN( ) for a block whose next coefficient may be at scan position `location` to 15.
fn run(b: &mut Bits, location: usize) -> Result<i32, Error> {
    const REMAP: [i32; 15] = [1, 2, 3, 5, 7, 1, 2, 3, 5, 7, 1, 2, 3, 4, 5];
    const BIN: [usize; 15] = [0, 0, 0, 0, 2, 2, 2, 1, 1, 1, 1, 0, 0, 0, 0];
    const FIXED: [u32; 15] = [0, 0, 1, 1, 3, 0, 0, 1, 1, 2, 0, 0, 0, 0, 1];
    let max = 15usize.checked_sub(location).ok_or(Error::Invalid("run"))?;
    Ok(match max {
        0 => return Err(Error::Invalid("run")),
        1 => 1,
        2..=4 => b.vlc(&RUN_VALUE[max - 2])? as i32 + 1,
        _ => {
            let i = b.vlc(&RUN_INDEX)? + 5 * BIN[max];
            REMAP[i] + b.read(FIXED[i])? as i32
        }
    })
}

/// REFINE_LP( )
fn refine(b: &mut Bits, c: i32, bits: u32) -> Result<i32, Error> {
    let r = b.read(bits)? as i32;
    Ok(match c.signum() {
        1 => (c << bits).wrapping_add(r),
        -1 => (c << bits).wrapping_sub(r),
        _ if r != 0 && b.bit()? => -r,
        _ => r,
    })
}

/// REFINE_CBPHP( )
fn refine_cbphp(b: &mut Bits, num: usize) -> Result<u32, Error> {
    Ok(match num {
        1 => 1 << b.read(2)?,
        2 => REF_CBPHP1_VALUE[b.vlc(&REF_CBPHP1)?],
        3 => 0xF ^ (1 << b.read(2)?),
        4 => 0xF,
        _ => 0,
    })
}

/// UpdateModelMB( )
fn update_model(model: &mut Model, mut lap: [i32; 2], band: usize, fmt: Format, n: usize) {
    const WEIGHT0: [i32; 3] = [240, 12, 1];
    const WEIGHT1: [[i32; 16]; 3] = [
        [0, 240, 120, 80, 60, 48, 40, 34, 30, 27, 24, 22, 20, 18, 17, 16],
        [0, 12, 6, 4, 3, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1],
        [0, 16, 8, 5, 4, 3, 3, 2, 2, 2, 2, 1, 1, 1, 1, 1],
    ];
    const WEIGHT2: [i32; 6] = [120, 37, 2, 120, 18, 1];
    lap[0] *= WEIGHT0[band];
    match fmt {
        Format::Yuv420 => lap[1] *= WEIGHT2[band],
        Format::Yuv422 => lap[1] *= WEIGHT2[3 + band],
        _ => {
            lap[1] *= WEIGHT1[band][n - 1];
            if band == 2 {
                lap[1] >>= 4;
            }
        }
    }
    let models = if fmt == Format::YOnly { 1 } else { 2 };
    for j in 0..models {
        let mut ms = model.state[j];
        let mut delta = (lap[j] - 70) >> 2;
        if delta <= -8 {
            delta = (delta + 4).max(-16);
            ms += delta;
            if ms < -8 {
                if model.bits[j] == 0 {
                    ms = -8;
                } else {
                    ms = 0;
                    model.bits[j] -= 1;
                }
            }
        } else if delta >= 8 {
            delta = (delta - 4).min(15);
            ms += delta;
            if ms > 8 {
                if model.bits[j] >= 15 {
                    model.bits[j] = 15;
                    ms = 8;
                } else {
                    ms = 0;
                    model.bits[j] += 1;
                }
            }
        }
        model.state[j] = ms;
    }
}

/// QuantMap( ): the dequantization factor of `qp`.
fn quant_map(qp: u8, luma_or_hp: bool, scaled: bool) -> i32 {
    let qp = i32::from(qp);
    if qp == 0 {
        return 1;
    }
    let (man, exp) = if !scaled {
        if qp < 32 {
            ((qp + 3) >> 2, 0)
        } else if qp < 48 {
            ((16 + qp % 16 + 1) >> 1, (qp >> 4) - 2)
        } else {
            (16 + qp % 16, (qp >> 4) - 3)
        }
    } else {
        let shift = i32::from(luma_or_hp);
        if qp < 16 { (qp, shift) } else { (16 + qp % 16, (qp >> 4) - 1 + shift) }
    };
    man << exp
}
