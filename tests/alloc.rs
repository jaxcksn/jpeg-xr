//! A hostile index table is refused before anything scaling with its claims is allocated.

#![allow(unsafe_code)] // a counting allocator; the library itself forbids unsafe

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use jpeg_xr::{Error, Limits};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator, only counting the bytes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(l.size(), Relaxed) + l.size();
        PEAK.fetch_max(live, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

#[derive(Default)]
struct Bits(Vec<u8>, u32);

impl Bits {
    fn put(&mut self, v: u64, n: u32) {
        for i in (0..n).rev() {
            if self.1 % 8 == 0 {
                self.0.push(0);
            }
            *self.0.last_mut().unwrap() |= (((v >> i) & 1) as u8) << (7 - self.1 % 8);
            self.1 += 1;
        }
    }
}

/// 65536 x 65536 grey, 4096 x 4096 tiles of one macroblock, all four bands in frequency order, an index table
/// of 4 x 4096^2 entries that are all 0xFF (offset 0): 67 MB.
fn crafted() -> Vec<u8> {
    let mut b = Bits::default();
    b.0.extend_from_slice(b"WMPHOTO\0");
    b.1 = 64;
    b.put(1, 4);
    b.put(0, 1); // hard tiling
    b.put(1, 3);
    b.put(1, 1); // tiling
    b.put(1, 1); // frequency order
    b.put(0, 3);
    b.put(1, 1); // index table
    b.put(0, 2); // overlap
    b.put(0, 1); // long header
    b.put(1, 1); // long words
    b.put(0, 6);
    b.put(0, 4); // Y only
    b.put(1, 4); // 8 bits
    b.put(65_535, 32);
    b.put(65_535, 32);
    b.put(4095, 12);
    b.put(4095, 12);
    for _ in 1..4096 * 2 {
        b.put(1, 16); // tile width and height, in macroblocks
    }
    b.put(0, 3); // Y only, unscaled
    b.put(0, 1);
    b.put(0, 4); // all bands
    b.put(1, 1); // DC QP
    b.put(0, 8);
    for _ in 0..2 {
        b.put(0, 1); // LP, then HP QP: same as the DC one
        b.put(1, 1);
        b.put(0, 8);
    }
    b.1 = b.1.div_ceil(8) * 8;
    b.put(1, 16); // index table start code
    let mut out = b.0;
    out.resize(out.len() + 4 * 4096 * 4096, 0xFF);
    out.extend_from_slice(&[0xFF, 0, 0, 1, 0, 0, 0, 0]);
    out
}

#[test]
fn a_huge_index_table_is_refused_before_it_is_read() {
    let data = crafted();
    for probe in [true, false] {
        let base = LIVE.load(Relaxed);
        PEAK.store(base, Relaxed);
        let err = if probe {
            jpeg_xr::probe(&data).err()
        } else {
            jpeg_xr::decode_rgba8(&data, &Limits::default()).err()
        };
        let peak = PEAK.load(Relaxed) - base;
        assert_eq!(err, Some(Error::Limit));
        assert!(peak < 1 << 20, "peak {peak} bytes");
    }
}
