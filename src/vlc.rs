//! Code tables (T.832 8.7) and adaptive code table selection (8.8)

use crate::bits::{Lut, lut};

// Table 51
pub(crate) const VAL_DC_YUV: Lut = lut(&["10", "001", "00001", "0001", "11", "010", "00000", "011"]);

// Table 52
pub(crate) const ABS_LEVEL_INDEX: [Lut; 2] = [
    lut(&["01", "10", "11", "001", "0001", "00000", "00001"]),
    lut(&["1", "01", "001", "0001", "00001", "000000", "000001"]),
];

// Tables 55 and 56
pub(crate) const CBPLP_YUV1_444: Lut = lut(&["0", "100", "1010", "1011", "1100", "1101", "1110", "1111"]);
pub(crate) const CBPLP_YUV1_42X: Lut = lut(&["0", "10", "110", "111"]);

// Tables 59 and 60
pub(crate) const NUM_CBPHP: [Lut; 2] =
    [lut(&["1", "01", "001", "0000", "0001"]), lut(&["1", "000", "001", "010", "011"])];

// Table 61
pub(crate) const NUM_BLKCBPHP_YUV: [Lut; 2] = [
    lut(&["010", "00000", "0010", "00001", "00010", "1", "011", "00011", "0011"]),
    lut(&["1", "001", "010", "0001", "000001", "011", "00001", "0000000", "0000001"]),
];

// Table 62
pub(crate) const CHR_CBPHP: Lut = lut(&["1", "01", "00"]);

// Table 63
pub(crate) const NUM_CH_BLK: Lut = lut(&["1", "01", "000", "001"]);

// Table 64, by code value
pub(crate) const REF_CBPHP1: Lut = lut(&["00", "01", "100", "101", "110", "111"]);
pub(crate) const REF_CBPHP1_VALUE: [u32; 6] = [3, 5, 6, 9, 10, 12];

// Tables 76 to 78, values minus 1
pub(crate) const RUN_VALUE: [Lut; 3] = [lut(&["1", "0"]), lut(&["1", "01", "00"]), lut(&["1", "01", "001", "000"])];

// Table 79
pub(crate) const RUN_INDEX: Lut = lut(&["1", "01", "001", "0000", "0001"]);

// Table 80
pub(crate) const INDEX_A: [Lut; 4] = [
    lut(&["1", "00000", "001", "00001", "01", "0001"]),
    lut(&["01", "0000", "10", "0001", "11", "001"]),
    lut(&["0000", "0001", "01", "10", "11", "001"]),
    lut(&["00000", "00001", "01", "1", "0001", "001"]),
];

// Table 81
pub(crate) const INDEX_B: Lut = lut(&["0", "110", "10", "111"]);

// Table 82
pub(crate) const FIRST_INDEX: [Lut; 5] = [
    lut(&["00001", "000001", "0000000", "0000001", "00100", "010", "00101", "1", "00110", "0001", "00111", "011"]),
    lut(&["0010", "00010", "000000", "000001", "0011", "010", "00011", "11", "011", "100", "00001", "101"]),
    lut(&["11", "001", "0000000", "0000001", "00001", "010", "0000010", "011", "100", "101", "0000011", "0001"]),
    lut(&["001", "11", "0000000", "00001", "00010", "010", "0000001", "011", "00011", "100", "000001", "101"]),
    lut(&["010", "1", "0000001", "0001", "0000010", "011", "00000000", "0010", "0000011", "0011", "00000001", "00001"]),
];

// Tables 86 to 91
pub(crate) const ABS_LEVEL_DELTA: [i32; 7] = [1, 0, -1, -1, -1, -1, -1];
pub(crate) const FIRST_INDEX_DELTA: [[i32; 12]; 4] = [
    [1, 1, 1, 1, 1, 0, 0, -1, 2, 1, 0, 0],
    [2, 2, -1, -1, -1, 0, -2, -1, 0, 0, -2, -1],
    [-1, 1, 0, 2, 0, 0, 0, 0, -2, 0, 1, 1],
    [0, 1, 0, 1, -2, 0, -1, -1, -2, -1, -2, -2],
];
pub(crate) const INDEX_A_DELTA: [[i32; 6]; 3] = [[-1, 1, 1, 1, 0, 1], [-2, 0, 0, 2, 0, 0], [-1, -1, 0, 1, -2, 0]];
pub(crate) const NUM_CBPHP_DELTA: [i32; 5] = [0, -1, 0, 1, 1];
pub(crate) const NUM_BLKCBPHP_DELTA_YUV: [i32; 9] = [2, 2, 1, 1, -1, -2, -2, -2, -3];

/// AdaptiveVLC (5.5.5): which code table a syntax element uses, and the discriminants that move it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Adapt {
    pub table: usize,
    pub delta: usize,
    pub delta2: usize,
    pub disc1: i32,
    pub disc2: i32,
}

impl Adapt {
    /// InitializeVLCTable1( )
    pub fn two() -> Self {
        Adapt::default()
    }

    /// InitializeVLCTable2( )
    pub fn many() -> Self {
        Adapt { table: 1, delta: 0, delta2: 1, disc1: 0, disc2: 0 }
    }

    /// AdaptVLCTable1( )
    pub fn adapt_two(&mut self) {
        if self.disc1 < -8 && self.table != 0 {
            self.table -= 1;
            self.disc1 = 0;
        } else if self.disc1 > 8 && self.table != 1 {
            self.table += 1;
            self.disc1 = 0;
        } else {
            self.disc1 = self.disc1.clamp(-64, 64);
        }
    }

    /// AdaptVLCTable2( )
    pub fn adapt_many(&mut self, max: usize) {
        let change = if self.disc1 < -8 && self.table != 0 {
            self.table -= 1;
            true
        } else if self.disc2 > 8 && self.table != max {
            self.table += 1;
            true
        } else {
            false
        };
        if change {
            self.disc1 = 0;
            self.disc2 = 0;
            if self.table == max {
                self.delta = self.table - 1;
                self.delta2 = self.table - 1;
            } else if self.table == 0 {
                self.delta = 0;
                self.delta2 = 0;
            } else {
                self.delta = self.table - 1;
                self.delta2 = self.table;
            }
        } else {
            self.disc1 = self.disc1.clamp(-64, 64);
            self.disc2 = self.disc2.clamp(-64, 64);
        }
    }
}
