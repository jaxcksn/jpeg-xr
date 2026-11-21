# jpeg-xr

A decoder for JPEG XR (ITU-T T.832 | ISO/IEC 29199-2), the format also known as HD Photo and Windows Media Photo:
the `.jxr`, `.wdp` and `.hdp` files that Windows and Microsoft Office write.

- Pure Rust, `#![forbid(unsafe_code)]`, no dependencies, builds for `wasm32-unknown-unknown`.
- Written from the Recommendation (the 06/2019 edition); no code is taken from the reference software, jxrlib.
- Untrusted input is expected: counts, sizes and offsets are bounded, memory and work are capped by `Limits` before
  anything is allocated, and malformed data is an `Error`, never a panic.

```rust
let bytes = std::fs::read("picture.jxr")?;
let info = jpeg_xr::probe(&bytes)?;                  // size, layout, orientation; headers only
let image = jpeg_xr::decode_rgba8(&bytes, &jpeg_xr::Limits::default())?;
```

`probe` reads the size, layout, orientation and resolution from the headers. `decode` returns the image's own
layout (`Gray8`, `Rgb8` or `Rgba8`); `decode_rgba8` always returns RGBA. Neither applies the orientation the file
asks for (`Info::orientation`, in EXIF terms), and premultiplied images (`Info::premultiplied`) stay premultiplied.

## Supported

- The tag-based file format (Annex A), or a bare codestream.
- 8-bit output: grey (`8bppGray`), RGB and BGR (`24bppRGB`, `24bppBGR`, `32bppBGR`), and RGB with alpha
  (`32bppBGRA`, `32bppRGBA`, `32bppPBGRA`).
- Internal colour formats Y only, YUV 4:2:0, 4:2:2 and 4:4:4; scaled and unscaled arithmetic.
- Lossless and lossy quantization, with uniform, per-tile and per-macroblock QPs.
- No, one and two levels of overlap filtering.
- Soft and hard tiles, the index table, and the spatial and frequency codestream orders, with every subset of bands
  and trimmed flexbits.
- Windowing (margins).
- Alpha as an interleaved image plane or as a separate codestream.

## Not supported

These return `Error::Unsupported`:

- Output bit depths other than 8: 1-bit black and white, 5-, 10- and 16-bit integer, 5-6-5, half and single
  float, and fixed point.
- CMYK, n-channel and RGBE images.

4:2:0 and 4:2:2 chroma is upsampled with the filter T.832 gives as its example (Table 180), at the chroma position
the codestream states.

## Where this follows the reference software

Images are decoded as jxrlib decodes them, since that code (Microsoft's) wrote the files found in the wild. In three
places that differs from the Recommendation's text:

- The highpass prediction direction (`CalcHPPredMode( )`, 9.6.3.2) is computed from the lowpass coefficients
  before dequantization, not after. The two differ when chroma and luma are scaled differently.
- With hard tiles, overlap filtering stops at the tile edges with the two lists swapped: columns stop at the edges
  of the tile rows, and rows at the edges of the tile columns.
- An interleaved alpha plane in scaled arithmetic rounds halves up (adds 4 before the final shift), not down (3).

## Tests

`tests/vectors` holds images made by jxrlib through the `imagecodecs` Python package (2026.3.6) from PNGs in the
Twip repository, with codec parameters `imagecodecs` does not expose set by calling its bundled jxrlib directly.
Each comes with jxrlib's own decode as the expected PNG (for the premultiplied image, which `imagecodecs` cannot
decode, the lossless input). `windowed-tile-qp` is a codestream built by hand to cover margins and per-tile QPs,
which jxrlib's encoder does not write; per-macroblock QP sets have no vector yet. This decoder matches every vector
exactly; the tests allow lossy images 2 levels a channel. `tests/hostile.rs` covers truncated and corrupted files,
absurd dimensions, tile offsets past the end, mismatched colour formats, orientations, work bounded by the input's
size, and a mutation loop over every vector (`JXR_FUZZ=50000` runs 1.1 million; run in release with overflow checks
on).

## Licence

MIT or Apache-2.0, at your option.
