//! Malformed and hostile input: errors, never panics, and work bounded by the input's size and the limits.

use std::time::{Duration, Instant};

use jpeg_xr::{Error, Limits};

const VECTORS: [&str; 6] = [
    "gray-lossless",
    "rgb-420-overlap2",
    "tiles-hard-420",
    "frequency-tiles-trim",
    "alpha-planar",
    "alpha-interleaved",
];

fn vector(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/vectors/{name}.jxr")).unwrap()
}

/// Writes bits most significant first.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    used: u32,
}

impl BitWriter {
    fn put(&mut self, v: u64, n: u32) {
        for i in (0..n).rev() {
            if self.used % 8 == 0 {
                self.bytes.push(0);
            }
            let bit = ((v >> i) & 1) as u8;
            *self.bytes.last_mut().unwrap() |= bit << (7 - self.used % 8);
            self.used += 1;
        }
    }

    fn align(&mut self) {
        self.used = self.used.div_ceil(8) * 8;
    }
}

/// A grey, DC-only codestream of `tiles` columns of the given macroblock widths (the last one implied), its tile
/// packets at `index` (when given), followed by `body`.
fn codestream(width: u32, height: u32, tile_widths: &[u32], index: Option<&[u16]>, body: &[u8]) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.bytes.extend_from_slice(b"WMPHOTO\0");
    w.used = 64;
    let tiling = !tile_widths.is_empty();
    w.put(1, 4); // RESERVED_B
    w.put(0, 1); // HARD_TILING_FLAG
    w.put(1, 3); // RESERVED_C
    w.put(u64::from(tiling), 1);
    w.put(0, 1); // spatial
    w.put(0, 3); // SPATIAL_XFRM_SUBORDINATE
    w.put(u64::from(index.is_some()), 1);
    w.put(0, 2); // OVERLAP_MODE
    w.put(0, 1); // SHORT_HEADER_FLAG
    w.put(1, 1); // LONG_WORD_FLAG
    w.put(0, 6); // WINDOWING, TRIM_FLEXBITS, RESERVED_D, RED_BLUE_NOT_SWAPPED, PREMULTIPLIED, ALPHA_IMAGE_PLANE
    w.put(0, 4); // YONLY
    w.put(1, 4); // BD8
    w.put(u64::from(width - 1), 32);
    w.put(u64::from(height - 1), 32);
    if tiling {
        w.put(tile_widths.len() as u64, 12);
        w.put(0, 12);
        for &t in tile_widths {
            w.put(u64::from(t), 16);
        }
    }
    // IMAGE_PLANE_HEADER: YONLY, unscaled, DC only, one DC QP
    w.put(0, 3);
    w.put(0, 1);
    w.put(3, 4);
    w.put(1, 1);
    w.put(1, 8);
    w.align();
    if let Some(index) = index {
        w.put(1, 16);
        for &o in index {
            w.put(u64::from(o), 16);
        }
    }
    w.put(0, 16); // SubsequentBytes
    w.bytes.extend_from_slice(body);
    w.bytes
}

/// A tile of `mbs` macroblocks whose DC values are all zero.
fn dc_tile(mbs: usize) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.put(1, 24);
    w.put(0, 8);
    for _ in 0..mbs {
        w.put(0, 1 + 8); // IS_DC_CH_FLAG, then 8 bits of DC_REF
    }
    w.bytes
}

#[test]
fn a_handmade_codestream_decodes() {
    let image = jpeg_xr::decode(&codestream(16, 16, &[], None, &dc_tile(1)), &Limits::default()).unwrap();
    assert_eq!((image.width, image.height), (16, 16));
    assert!(image.data.iter().all(|&g| g == 128));
    let two = [dc_tile(1), dc_tile(1)].concat();
    let image = jpeg_xr::decode(&codestream(32, 16, &[1], Some(&[0, 6]), &two), &Limits::default()).unwrap();
    assert!(image.data.iter().all(|&g| g == 128));
}

#[test]
fn truncation_anywhere_is_an_error() {
    for name in VECTORS {
        let bytes = vector(name);
        let step = (bytes.len() / 300).max(1);
        for len in (0..bytes.len()).step_by(step).chain(0..200.min(bytes.len())) {
            let _ = jpeg_xr::probe(&bytes[..len]);
            let _ = jpeg_xr::decode(&bytes[..len], &Limits::default());
        }
    }
}

#[test]
fn corrupted_bytes_never_panic() {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for name in VECTORS {
        let bytes = vector(name);
        for _ in 0..150 {
            let mut b = bytes.clone();
            for _ in 0..1 + next() % 4 {
                let at = (next() % b.len() as u64) as usize;
                b[at] ^= 1 << (next() % 8);
            }
            let _ = jpeg_xr::decode(&b, &Limits::default());
        }
    }
}

#[test]
fn absurd_dimensions_are_refused_before_allocating() {
    let tile = dc_tile(1);
    // 2^32 × 2^32 pixels: more macroblocks than the data could hold.
    let huge = codestream(u32::MAX, u32::MAX, &[], None, &tile);
    assert_eq!(jpeg_xr::probe(&huge).err(), Some(Error::Truncated));
    assert_eq!(jpeg_xr::decode(&huge, &Limits::default()).err(), Some(Error::Truncated));
    // Enough data for the macroblocks, more pixels than allowed.
    let wide = codestream(16 * 64, 16, &[], None, &dc_tile(64));
    assert!(jpeg_xr::decode(&wide, &Limits { max_pixels: 16 * 16 * 63, ..Limits::default() }).is_err());
    assert!(jpeg_xr::decode(&wide, &Limits { max_work: 16 * 16 * 63, ..Limits::default() }).is_err());
    assert!(jpeg_xr::decode(&wide, &Limits::default()).is_ok());
}

#[test]
fn tile_index_outside_the_file_is_an_error() {
    let two = [dc_tile(1), dc_tile(1)].concat();
    let past = codestream(32, 16, &[1], Some(&[0, 60_000]), &two);
    assert_eq!(jpeg_xr::decode(&past, &Limits::default()).err(), Some(Error::Truncated));
    // Tiles at the same offset read the same bytes, as the reference decoder does.
    let same = codestream(32, 16, &[1], Some(&[0, 0]), &two);
    assert!(jpeg_xr::decode(&same, &Limits::default()).is_ok());
    // Tile widths that run past the image.
    let wide = codestream(32, 16, &[2], Some(&[0, 13]), &two);
    assert!(jpeg_xr::decode(&wide, &Limits::default()).is_err());
}

#[test]
fn truncated_tiles_are_an_error() {
    let tile = dc_tile(4);
    let short = codestream(64, 16, &[], None, &tile[..tile.len() - 2]);
    assert_eq!(jpeg_xr::decode(&short, &Limits::default()).err(), Some(Error::Truncated));
}

#[test]
fn work_is_bounded_by_the_input() {
    // A header claiming 65536 × 65536 pixels over a few bytes, with limits that would allow it: the decoder must
    // refuse at once rather than allocate for, or loop over, 16 million macroblocks.
    let claim = codestream(65_536, 65_536, &[], None, &dc_tile(64));
    let generous = Limits { max_pixels: u64::MAX, max_work: u64::MAX };
    let start = Instant::now();
    assert!(jpeg_xr::decode(&claim, &generous).is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
    // The real vectors decode within limits their size implies.
    for name in VECTORS {
        let bytes = vector(name);
        let start = Instant::now();
        jpeg_xr::decode(&bytes, &generous).unwrap();
        assert!(start.elapsed() < Duration::from_secs(5), "{name}");
    }
}

/// The vector's bytes with the codestream's OUTPUT_CLR_FMT (the high nibble of its 12th byte) set to `fmt`.
fn with_output(name: &str, fmt: u8) -> Vec<u8> {
    let mut b = vector(name);
    let at = b.windows(8).position(|w| w == b"WMPHOTO\0").unwrap() + 11;
    b[at] = (b[at] & 0x0F) | (fmt << 4);
    b
}

#[test]
fn grey_output_of_a_colour_codestream_is_an_error() {
    for name in ["rgb-lossless", "rgb-420", "rgb-422", "rgb-444-lossy", "alpha-interleaved"] {
        assert_eq!(
            jpeg_xr::decode(&with_output(name, 0), &Limits::default()).err(),
            Some(Error::Invalid("grey output of a colour codestream")),
            "{name}"
        );
    }
    // RGB output of a Y-only plane spreads the grey (InvColorFmtConvert1( )).
    let grey = jpeg_xr::decode(&vector("gray-lossless"), &Limits::default()).unwrap();
    let rgb = jpeg_xr::decode(&with_output("gray-lossless", 7), &Limits::default()).unwrap();
    assert_eq!(rgb.layout, jpeg_xr::Layout::Rgb8);
    assert!(rgb.data.chunks_exact(3).zip(&grey.data).all(|(p, &g)| p == [g, g, g]));
    // Every other output colour format is refused before decoding.
    for fmt in [1, 2, 3, 4, 5, 6, 8, 9, 15] {
        assert!(jpeg_xr::decode(&with_output("rgb-lossless", fmt), &Limits::default()).is_err(), "{fmt}");
        assert!(jpeg_xr::decode(&with_output("gray-lossless", fmt), &Limits::default()).is_err(), "{fmt}");
    }
}

#[test]
fn orientations_map_to_exif() {
    // T.832 8.3.8: SPATIAL_XFRM_SUBORDINATE 0 to 7 are the TIFF orientations 1, 4, 2, 3, 6, 7, 5, 8 (4, RCW: 6;
    // 5, RCW then FlipV, row 0 the right edge and column 0 the bottom: 7; 6, RCW then FlipH, row 0 the left edge and
    // column 0 the top: 5).
    const TIFF: [u8; 8] = [1, 4, 2, 3, 6, 7, 5, 8];
    let file = vector("gray-lossless");
    let start = file.windows(8).position(|w| w == b"WMPHOTO\0").unwrap();
    for (v, want) in TIFF.into_iter().enumerate() {
        // A bare codestream: the subordinate transform, bits 2 to 4 of its 10th byte.
        let mut cs = file[start..].to_vec();
        cs[9] = (cs[9] & !0b0011_1000) | ((v as u8) << 3);
        let info = jpeg_xr::probe(&cs).unwrap();
        assert_eq!((info.orientation, info.width, info.height), (want, 47, 39), "subordinate {v}");
        // In a file, SPATIAL_XFRM_PRIMARY (tag 0xBC02, a USHORT) overrides it.
        let mut f = file.clone();
        f[start + 9] = (f[start + 9] & !0b0011_1000) | (((7 - v) as u8) << 3);
        let ifd = u32::from_le_bytes(f[4..8].try_into().unwrap()) as usize;
        let n = usize::from(u16::from_le_bytes([f[ifd], f[ifd + 1]]));
        let at =
            (0..n).map(|e| ifd + 2 + e * 12).find(|&at| f[at..at + 2] == [0x02, 0xBC]).expect("a primary transform");
        f[at + 8] = v as u8;
        assert_eq!(jpeg_xr::probe(&f).unwrap().orientation, want, "primary {v}");
    }
}

/// Mutated vectors decode or fail, never panic. `JXR_FUZZ` sets the mutations per vector.
#[test]
fn mutation_fuzz() {
    let per: usize = std::env::var("JXR_FUZZ").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    let mut names: Vec<String> = std::fs::read_dir("tests/vectors")
        .unwrap()
        .filter_map(|e| e.unwrap().file_name().to_str()?.strip_suffix(".jxr").map(str::to_string))
        .collect();
    names.sort();
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let limits = Limits { max_pixels: 1 << 24, max_work: 1 << 24 };
    for name in &names {
        let bytes = vector(name);
        for _ in 0..per {
            let mut b = bytes.clone();
            match next() % 4 {
                0 => b.truncate((next() % b.len() as u64) as usize),
                _ => {
                    for _ in 0..1 + next() % 6 {
                        let at = (next() % b.len() as u64) as usize;
                        b[at] = match next() % 3 {
                            0 => b[at] ^ (1 << (next() % 8)),
                            1 => next() as u8,
                            _ => [0, 0xFF][(next() % 2) as usize],
                        };
                    }
                }
            }
            let _ = jpeg_xr::probe(&b);
            let _ = jpeg_xr::decode(&b, &limits);
        }
    }
}
