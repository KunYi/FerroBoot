//! CRC-32/ISO-HDLC (the "zlib" / `crc32fast` CRC), implemented with a
//! classic 256-entry (1 KiB) lookup table, one byte per step.
//!
//! Why this crate exists: `crc32fast` on targets without a hardware fast path
//! links a 16 x 256 x 4 = 16 KiB slice-by-16 table into `.rodata`. The
//! bootloader only checksums tiny records (journal headers/records, the NOR
//! seed record), so that table is pure waste. This crate produces
//! **bit-identical results**, so data already written to flash stays valid.
//!
//! Parameters: reflected, polynomial `0xEDB8_8320`, init `0xFFFF_FFFF`,
//! final XOR `0xFFFF_FFFF`. `crc32("123456789") == 0xCBF4_3926`.
//!
//! The API mirrors the subset of `crc32fast::Hasher` the workspace uses
//! (`new`, `update`, `finalize`), so call sites only change their `use` line.
//!
//! The table is evaluated at compile time by a `const fn` and lives in flash
//! (`.rodata`); it costs no RAM and no start-up work. One byte per step is
//! fast enough that this can also serve larger regions if ever needed.

#![no_std]
#![forbid(unsafe_code)]

/// Reflected CRC-32 polynomial (ISO-HDLC / zlib).
const POLY: u32 = 0xEDB8_8320;

/// CRC contribution of each byte value, computed at compile time into flash.
static TABLE: [u32; 256] = build_table();

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
            bit += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

/// Incremental CRC-32 state.
#[derive(Clone, Copy, Debug)]
pub struct Hasher {
    state: u32,
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher {
    /// Start a new checksum.
    pub const fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    /// Feed more bytes. May be called any number of times; splitting the input
    /// arbitrarily gives the same result as one call.
    pub fn update(&mut self, data: &[u8]) {
        let mut crc = self.state;
        for &byte in data {
            crc = TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
        self.state = crc;
    }

    /// Finish and return the checksum. The hasher is consumed by value (it is
    /// `Copy`, so a clone can keep being updated if a running value is needed).
    pub const fn finalize(self) -> u32 {
        !self.state
    }
}

/// One-shot convenience.
pub fn checksum(data: &[u8]) -> u32 {
    let mut h = Hasher::new();
    h.update(data);
    h.finalize()
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    #[test]
    fn known_answer_vectors() {
        assert_eq!(checksum(b""), 0x0000_0000);
        assert_eq!(checksum(b"123456789"), 0xCBF4_3926);
        assert_eq!(checksum(b"a"), 0xE8B7_BE43);
        assert_eq!(checksum(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }

    #[test]
    fn table_matches_reference_values() {
        // Well-known entries of the standard reflected 0xEDB88320 table.
        assert_eq!(TABLE[0], 0x0000_0000);
        assert_eq!(TABLE[1], 0x7707_3096);
        assert_eq!(TABLE[2], 0xEE0E_612C);
        assert_eq!(TABLE[255], 0x2D02_EF8D);
    }

    #[test]
    fn incremental_equals_one_shot() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        let whole = checksum(&data);
        for split in [0usize, 1, 2, 31, 32, 33, 500, 999, 1000] {
            let mut h = Hasher::new();
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(h.finalize(), whole, "split at {split}");
        }
    }

    /// Deterministic pseudo-random generator (no extra dependencies).
    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn matches_crc32fast_oracle_on_random_inputs() {
        let mut rng = 0x1234_5678_9ABC_DEF0u64;
        for len in (0..300usize).chain([511, 512, 513, 4096, 65_537]) {
            let data: Vec<u8> = (0..len).map(|_| xorshift(&mut rng) as u8).collect();
            let mut oracle = crc32fast::Hasher::new();
            oracle.update(&data);
            assert_eq!(checksum(&data), oracle.finalize(), "len {len}");
        }
    }

    #[test]
    fn matches_crc32fast_with_arbitrary_chunking() {
        let mut rng = 0xDEAD_BEEF_CAFE_F00Du64;
        let data: Vec<u8> = (0..5000).map(|_| xorshift(&mut rng) as u8).collect();
        let mut ours = Hasher::new();
        let mut oracle = crc32fast::Hasher::new();
        let mut pos = 0;
        while pos < data.len() {
            let n = (xorshift(&mut rng) as usize % 97 + 1).min(data.len() - pos);
            ours.update(&data[pos..pos + n]);
            oracle.update(&data[pos..pos + n]);
            pos += n;
        }
        assert_eq!(ours.finalize(), oracle.finalize());
    }
}
