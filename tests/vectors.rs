//! Images encoded by jxrlib, the reference software, through the `imagecodecs` Python package, with jxrlib's own
//! decodes as the expected pixels (for the premultiplied one, which `imagecodecs` does not decode, the lossless
//! input).

use jpeg_xr::{Layout, Limits};

/// The expected pixels and their layout.
fn expected(name: &str) -> (u32, u32, Layout, Vec<u8>) {
    let file = std::fs::File::open(format!("tests/vectors/{name}.png")).unwrap();
    let mut reader = png::Decoder::new(std::io::BufReader::new(file)).read_info().unwrap();
    let mut data = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut data).unwrap();
    data.truncate(info.buffer_size());
    let layout = match info.color_type {
        png::ColorType::Grayscale => Layout::Gray8,
        png::ColorType::Rgb => Layout::Rgb8,
        png::ColorType::Rgba => Layout::Rgba8,
        c => panic!("{name}: {c:?}"),
    };
    (info.width, info.height, layout, data)
}

/// Decodes `name` and checks every sample is within `tolerance` of jxrlib's.
fn check(name: &str, tolerance: u8) {
    let bytes = std::fs::read(format!("tests/vectors/{name}.jxr")).unwrap();
    let image = jpeg_xr::decode(&bytes, &Limits::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
    let (width, height, layout, data) = expected(name);
    assert_eq!((image.width, image.height, image.layout), (width, height, layout), "{name}");
    let info = jpeg_xr::probe(&bytes).unwrap();
    assert_eq!((info.width, info.height, info.layout), (width, height, layout), "{name}");
    let worst = image.data.iter().zip(&data).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(worst <= tolerance, "{name}: a sample is {worst} off");
}

#[test]
fn lossless_gray() {
    check("gray-lossless", 0);
    check("gray-lossless-overlap", 0);
}

#[test]
fn colour_formats() {
    check("rgb-lossless", 0);
    check("bgr-lossless", 0);
    check("rgb-422", 2);
    check("rgb-420", 2);
}

#[test]
fn lossy_and_overlap() {
    check("gray-lossy", 2);
    check("rgb-444-lossy", 2);
    check("rgb-420-overlap2", 2);
    check("rgb-444-overlap2", 2);
}

#[test]
fn tiles_and_codestream_order() {
    check("tiles-soft", 2);
    check("tiles-hard-420", 2);
    check("tiles-hard-422", 2);
    check("frequency", 2);
    check("frequency-tiles-trim", 2);
    check("frequency-lossless", 0);
}

#[test]
fn alpha() {
    check("alpha-planar", 2);
    check("alpha-interleaved", 2);
    check("alpha-interleaved-frequency", 2);
    check("alpha-premultiplied", 0);
    let bytes = std::fs::read("tests/vectors/alpha-premultiplied.jxr").unwrap();
    assert!(jpeg_xr::probe(&bytes).unwrap().premultiplied);
    let bytes = std::fs::read("tests/vectors/alpha-planar.jxr").unwrap();
    assert!(!jpeg_xr::probe(&bytes).unwrap().premultiplied);
    assert_eq!(jpeg_xr::probe(&bytes).unwrap().resolution, Some((96.0, 96.0)));
}

#[test]
fn rgba8_spreads_grey_and_fills_alpha() {
    let bytes = std::fs::read("tests/vectors/gray-lossless.jxr").unwrap();
    let image = jpeg_xr::decode_rgba8(&bytes, &Limits::default()).unwrap();
    let (_, _, _, grey) = expected("gray-lossless");
    assert_eq!(image.layout, Layout::Rgba8);
    assert!(image.data.chunks_exact(4).zip(grey).all(|(p, g)| p == [g, g, g, 255]));
}

#[test]
fn windowing_and_tile_qps() {
    // Hand-built: margins on all four sides, hard tiles, and a different DC QP in each tile.
    check("windowed-tile-qp", 0);
}
