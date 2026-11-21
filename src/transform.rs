//! Sample reconstruction (T.832 9.9): the two inverse transform levels and their overlap filters

use std::num::Wrapping;

use crate::plane::Plane;

type V = Wrapping<i32>;

const ONE: V = Wrapping(1);
const TWO: V = Wrapping(2);
const THREE: V = Wrapping(3);
const FOUR: V = Wrapping(4);

/// Runs `f` on the samples of `x` at `idx`, in that order.
#[inline]
fn apply<const N: usize>(x: &mut [i32], idx: [usize; N], f: impl FnOnce([V; N]) -> [V; N]) {
    let out = f(idx.map(|i| Wrapping(x[i])));
    for (i, v) in idx.into_iter().zip(out) {
        x[i] = v.0;
    }
}

// Basic operations (9.9.7, 9.9.8)

fn t2x2h([mut a, mut b, mut c, mut d]: [V; 4], round: i32) -> [V; 4] {
    a += d;
    b -= c;
    let t1 = (a - b + Wrapping(round)) >> 1;
    let t2 = c;
    c = t1 - d;
    d = t1 - t2;
    a -= d;
    b += c;
    [a, b, c, d]
}

fn inv_todd([mut a, mut b, mut c, mut d]: [V; 4]) -> [V; 4] {
    b += d;
    a -= c;
    d -= b >> 1;
    c += (a + ONE) >> 1;
    a -= (b * THREE + FOUR) >> 3;
    b += (a * THREE + FOUR) >> 3;
    c -= (d * THREE + FOUR) >> 3;
    d += (c * THREE + FOUR) >> 3;
    c -= (b + ONE) >> 1;
    d = ((a + ONE) >> 1) - d;
    b += c;
    a -= d;
    [a, b, c, d]
}

fn inv_toddodd([mut a, mut b, mut c, mut d]: [V; 4]) -> [V; 4] {
    d += a;
    c -= b;
    let t1 = d >> 1;
    let t2 = c >> 1;
    a -= t1;
    b += t2;
    a -= (b * THREE + THREE) >> 3;
    b += (a * THREE + THREE) >> 2;
    a -= (b * THREE + FOUR) >> 3;
    b -= t2;
    a += t1;
    c += b;
    d -= a;
    [a, -b, -c, d]
}

const INV_PERMUTE: [usize; 16] = [0, 8, 4, 13, 2, 15, 3, 14, 1, 12, 5, 9, 7, 11, 6, 10];

/// ICT4x4( ) on the samples of `x` at `idx`, raster order.
fn ict4x4(x: &mut [i32], idx: [usize; 16]) {
    let mut c = [0i32; 16];
    for (i, &p) in idx.iter().enumerate() {
        c[INV_PERMUTE[i]] = x[p];
    }
    apply(&mut c, [0, 1, 4, 5], |v| t2x2h(v, 1));
    apply(&mut c, [2, 3, 6, 7], inv_todd);
    apply(&mut c, [8, 12, 9, 13], inv_todd);
    apply(&mut c, [10, 11, 14, 15], inv_toddodd);
    for q in [[0, 3, 12, 15], [5, 6, 9, 10], [1, 2, 13, 14], [4, 7, 8, 11]] {
        apply(&mut c, q, |v| t2x2h(v, 0));
    }
    for (i, &p) in idx.iter().enumerate() {
        x[p] = c[i];
    }
}

fn inv_rotate([mut a, mut b]: [V; 2]) -> [V; 2] {
    a -= (b + ONE) >> 1;
    b += (a + ONE) >> 1;
    [a, b]
}

fn inv_scale([mut a, mut b]: [V; 2]) -> [V; 2] {
    a += b;
    b = (a >> 1) - b;
    a += (b * THREE) >> 3;
    b += (a * THREE) >> 4;
    b += a >> 7;
    b -= a >> 10;
    [a, b]
}

fn t2x2h_post([mut a, mut b, c, mut d]: [V; 4]) -> [V; 4] {
    b -= c;
    a += (d * THREE + FOUR) >> 3;
    d -= b >> 1;
    let c = ((a - b) >> 1) - c;
    let (c, d) = (d, c);
    a -= d;
    b += c;
    [a, b, c, d]
}

fn inv_toddodd_post([mut a, mut b, mut c, mut d]: [V; 4]) -> [V; 4] {
    d += a;
    c -= b;
    let t1 = d >> 1;
    let t2 = c >> 1;
    a -= t1;
    b += t2;
    a -= (b * THREE + Wrapping(6)) >> 3;
    b += (a * THREE + TWO) >> 2;
    a -= (b * THREE + FOUR) >> 3;
    b -= t2;
    a += t1;
    c += b;
    d -= a;
    [a, b, c, d]
}

/// OverlapPostFilter4x4( ) on the samples of `x` at `idx`, raster order.
fn overlap4x4(x: &mut [i32], idx: [usize; 16]) {
    let mut c = idx.map(|p| x[p]);
    const QUADS: [[usize; 4]; 4] = [[0, 3, 12, 15], [1, 2, 13, 14], [4, 7, 8, 11], [5, 6, 9, 10]];
    for q in QUADS {
        apply(&mut c, q, |v| t2x2h(v, 0));
    }
    for p in [[13, 12], [9, 8], [7, 3], [6, 2]] {
        apply(&mut c, p, inv_rotate);
    }
    apply(&mut c, [10, 11, 14, 15], inv_toddodd_post);
    for p in [[0, 15], [1, 14], [4, 11], [5, 10]] {
        apply(&mut c, p, inv_scale);
    }
    for q in QUADS {
        apply(&mut c, q, t2x2h_post);
    }
    for (i, &p) in idx.iter().enumerate() {
        x[p] = c[i];
    }
}

/// OverlapPostFilter4( )
fn overlap4([mut a, mut b, mut c, mut d]: [V; 4]) -> [V; 4] {
    a += d;
    b += c;
    d -= (a + ONE) >> 1;
    c -= (b + ONE) >> 1;
    [a, d] = inv_scale([a, d]);
    [b, c] = inv_scale([b, c]);
    a += (d * THREE + FOUR) >> 3;
    b += (c * THREE + FOUR) >> 3;
    d -= a >> 1;
    c -= b >> 1;
    a += d;
    b += c;
    d = -d;
    c = -c;
    [c, d] = inv_rotate([c, d]);
    d += (a + ONE) >> 1;
    c += (b + ONE) >> 1;
    a -= d;
    b -= c;
    [a, b, c, d]
}

/// OverlapPostFilter2x2( )
fn overlap2x2([mut a, mut b, mut c, mut d]: [V; 4]) -> [V; 4] {
    a += d;
    b += c;
    d -= (a + ONE) >> 1;
    c -= (b + ONE) >> 1;
    [a, b] = overlap2([a, b]);
    d += (a + ONE) >> 1;
    c += (b + ONE) >> 1;
    a -= d;
    b -= c;
    [a, b, c, d]
}

/// OverlapPostFilter2( )
fn overlap2([mut a, mut b]: [V; 2]) -> [V; 2] {
    b += (a + TWO) >> 2;
    a += (b + ONE) >> 1;
    a += b >> 5;
    a += b >> 9;
    a += b >> 13;
    b += (a + TWO) >> 2;
    [a, b]
}

/// A view of a component in which sample (x, y) is at `(y * stride + x) * step`: the plane itself (`step` 1), or
/// its DC-LP array (`step` 4, the first sample of each 4×4 block).
struct Grid<'a> {
    data: &'a mut [i32],
    stride: usize,
    step: usize,
}

impl Grid<'_> {
    #[inline]
    fn at(&self, x: usize, y: usize) -> usize {
        (y * self.stride + x) * self.step
    }

    fn block<const N: usize, const M: usize>(&self, x: usize, y: usize) -> [usize; M] {
        std::array::from_fn(|i| self.at(x + i % N, y + i / N))
    }

    fn column(&self, x: usize, y: usize) -> [usize; 4] {
        std::array::from_fn(|i| self.at(x, y + i))
    }

    fn row(&self, x: usize, y: usize) -> [usize; 4] {
        std::array::from_fn(|i| self.at(x + i, y))
    }

    /// The overlap filtering of a luma or full-resolution region (9.9.3.2, 9.9.6), `x0..x1` × `y0..y1`.
    fn overlap_full(&mut self, x0: usize, x1: usize, y0: usize, y1: usize) {
        for y in (y0 + 2..y1.saturating_sub(2)).step_by(4) {
            for x in (x0 + 2..x1.saturating_sub(2)).step_by(4) {
                let idx = self.block::<4, 16>(x, y);
                overlap4x4(self.data, idx);
            }
            for x in [x0, x0 + 1, x1 - 2, x1 - 1] {
                let idx = self.column(x, y);
                apply(self.data, idx, overlap4);
            }
        }
        for x in (x0 + 2..x1.saturating_sub(2)).step_by(4) {
            for y in [y0, y0 + 1, y1 - 2, y1 - 1] {
                let idx = self.row(x, y);
                apply(self.data, idx, overlap4);
            }
        }
        for (x, y) in [(x0, y0), (x1 - 2, y0), (x0, y1 - 2), (x1 - 2, y1 - 2)] {
            let idx = self.block::<2, 4>(x, y);
            apply(self.data, idx, overlap4);
        }
    }

    /// The first level overlap filtering of a 4:2:0 or 4:2:2 chroma DC-LP region (9.9.3.3, 9.9.3.4).
    fn overlap_chroma(&mut self, x0: usize, x1: usize, y0: usize, y1: usize) {
        let corners = [(x0, y0, x0 + 1), (x1 - 1, y0, x1 - 2), (x0, y1 - 1, x0 + 1), (x1 - 1, y1 - 1, x1 - 2)];
        for (x, y, from) in corners {
            let (p, q) = (self.at(x, y), self.at(from, y));
            self.data[p] = self.data[p].wrapping_sub(self.data[q]);
        }
        for y in (y0 + 1..y1 - 1).step_by(2) {
            for x in (x0 + 1..x1 - 1).step_by(2) {
                let idx = self.block::<2, 4>(x, y);
                apply(self.data, idx, overlap2x2);
            }
            for x in [x0, x1 - 1] {
                let idx = [self.at(x, y), self.at(x, y + 1)];
                apply(self.data, idx, overlap2);
            }
        }
        for x in (x0 + 1..x1 - 1).step_by(2) {
            for y in [y0, y1 - 1] {
                let idx = [self.at(x, y), self.at(x + 1, y)];
                apply(self.data, idx, overlap2);
            }
        }
        for (x, y, from) in corners {
            let (p, q) = (self.at(x, y), self.at(from, y));
            self.data[p] = self.data[p].wrapping_add(self.data[q]);
        }
    }
}

/// The macroblock boundaries overlap filtering stops at: the image's edges, and with hard tiles the tiles' edges.
///
/// The reference software, and so the files it and Windows write, swaps the two lists of tile edges here: columns
/// of the filtering stop at the tile rows' edges, and rows at the tile columns'. Images with hard tiles decode as
/// their encoders meant only that way, so this follows it rather than T.832; edges past the image are dropped.
fn overlap_edges(hard_tiling: bool, across: &[usize], end: usize) -> Vec<usize> {
    let mut edges = vec![0];
    if hard_tiling {
        edges.extend(across[1..across.len() - 1].iter().copied().filter(|&e| e < end));
    }
    edges.push(end);
    edges
}

/// SampleReconstruction( ) of a plane whose coefficients are fully decoded.
pub(crate) fn reconstruct(plane: &mut Plane, overlap: u8, hard_tiling: bool, tile_x: &[usize], tile_y: &[usize]) {
    let format = plane.header.format;
    let scaled = plane.header.scaled;
    let (mb_width, mb_height) = (tile_x[tile_x.len() - 1], tile_y[tile_y.len() - 1]);
    let tx = overlap_edges(hard_tiling, tile_y, mb_width);
    let ty = overlap_edges(hard_tiling, tile_x, mb_height);
    for (i, c) in plane.components.iter_mut().enumerate() {
        let (bw, bh) = (c.bw, c.bh);
        let mbw = c.width / (bw * 4);
        let mbh = c.height / (bh * 4);
        let mut dc = Grid { data: &mut c.data, stride: c.width, step: 4 };
        // First level inverse transform
        for my in 0..mbh {
            for mx in 0..mbw {
                let (x, y) = (mx * bw, my * bh);
                match (bw, bh) {
                    (4, 4) => {
                        let idx = dc.block::<4, 16>(x, y);
                        ict4x4(dc.data, idx);
                    }
                    (2, 2) => {
                        let idx = dc.block::<2, 4>(x, y);
                        apply(dc.data, idx, |v| {
                            let [a, b, c, d] = t2x2h(v, 0);
                            [a, c, b, d]
                        });
                    }
                    _ => {
                        let idx = dc.block::<2, 8>(x, y);
                        apply(dc.data, idx, |mut v| {
                            v[0] -= (v[4] + ONE) >> 1;
                            v[4] += v[0];
                            let [a, b, c, d] = t2x2h([v[0], v[1], v[2], v[3]], 0);
                            [v[0], v[1], v[2], v[3]] = [a, c, b, d];
                            let [a, b, c, d] = t2x2h([v[4], v[6], v[5], v[7]], 0);
                            [v[4], v[6], v[5], v[7]] = [a, b, c, d];
                            v.swap(5, 6);
                            v
                        });
                    }
                }
                if i > 0 && scaled {
                    for j in 0..bw * bh {
                        let p = dc.at(x + j % bw, y + j / bw);
                        dc.data[p] = dc.data[p].wrapping_mul(2);
                    }
                }
            }
        }
        // First level overlap filtering
        let chroma42x = i > 0 && format.is_42x();
        if overlap == 2 {
            for t in ty.windows(2) {
                for s in tx.windows(2) {
                    let (x0, x1, y0, y1) = (s[0] * bw, s[1] * bw, t[0] * bh, t[1] * bh);
                    if chroma42x {
                        dc.overlap_chroma(x0, x1, y0, y1);
                    } else {
                        dc.overlap_full(x0, x1, y0, y1);
                    }
                }
            }
        }
        // Second level inverse transform
        let mut g = Grid { data: &mut c.data, stride: c.width, step: 1 };
        for y in (0..c.height).step_by(4) {
            for x in (0..c.width).step_by(4) {
                let idx = g.block::<4, 16>(x, y);
                ict4x4(g.data, idx);
            }
        }
        // Second level overlap filtering
        if overlap != 0 {
            for t in ty.windows(2) {
                for s in tx.windows(2) {
                    g.overlap_full(s[0] * bw * 4, s[1] * bw * 4, t[0] * bh * 4, t[1] * bh * 4);
                }
            }
        }
    }
}
