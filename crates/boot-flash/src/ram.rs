//! `RamFlash`: an in-memory [`Flash`] with real flash physics, for host tests.
//!
//! What it models:
//!
//! * programming only clears bits (`new = old & data`); only an erase sets them;
//! * **uneven erase units**, from a list of `(unit_size, count)` runs;
//! * **program alignment** (`ALIGN`, a const generic that becomes `PROGRAM_ALIGN`);
//! * a **memory-mapped view** that can be bank-swapped, separate from the abstract
//!   offsets every [`Flash`] call uses (a swap changes what the CPU sees at an
//!   address, not which unit `erase_unit(off)` erases);
//! * **power loss at any program word or erase**, in two flavours ([`CutMode`]).
//!
//! A *step* is one programmed word (`ALIGN` bytes) or one erased unit. The
//! simulator counts completed steps; [`RamFlash::arm_power_cut`] makes the step
//! with a given index fail.

use core::ops::Range;
use std::vec;
use std::vec::Vec;

use crate::{Flash, FlashError};

/// 4 MiB of 4 KiB units, the layout of the SPI NOR on the H750 NetLite.
pub const UNIFORM_4K_4MB: &[(u32, u32)] = &[(4096, 1024)];

/// STM32F42x/F43x 2 MiB dual-bank sector map (RM0090): per bank four 16 KiB sectors,
/// one 64 KiB and seven 128 KiB.
pub const F42X_DUAL_BANK_2MB: &[(u32, u32)] = &[
    (16 * 1024, 4),
    (64 * 1024, 1),
    (128 * 1024, 7),
    (16 * 1024, 4),
    (64 * 1024, 1),
    (128 * 1024, 7),
];

/// What a power cut leaves behind in the step it hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutMode {
    /// The step did not happen at all.
    Clean,
    /// The step half happened. A torn program clears only some of the bits it should
    /// have; a torn erase sets only some of the bits it should have. Bits never move
    /// the other way. (Which bits is a deterministic pseudo-random choice.)
    Torn,
}

/// In-memory flash simulator. See the module documentation.
#[derive(Clone, Debug)]
pub struct RamFlash<const ALIGN: u32> {
    mem: Vec<u8>,
    /// Start offset of every unit, then the capacity as the last element.
    bounds: Vec<u32>,
    dual_bank: bool,
    swapped: bool,
    steps: u64,
    cut: Option<(u64, CutMode)>,
    dead: bool,
}

impl<const ALIGN: u32> RamFlash<ALIGN> {
    /// Builds an erased device from `(unit_size, count)` runs.
    ///
    /// Panics on an empty or zero-sized geometry or a unit that is not a multiple
    /// of `ALIGN` (it is a test fixture; a bad one should stop the test).
    pub fn new(geometry: &[(u32, u32)]) -> Self {
        assert!(ALIGN > 0, "ALIGN must be non-zero");
        assert!(!geometry.is_empty(), "empty geometry");
        let mut bounds = Vec::new();
        let mut at = 0u32;
        for &(size, count) in geometry {
            assert!(size > 0 && count > 0, "zero-sized run");
            assert!(size % ALIGN == 0, "unit size must be a multiple of ALIGN");
            for _ in 0..count {
                bounds.push(at);
                at = at.checked_add(size).expect("geometry exceeds 4 GiB");
            }
        }
        bounds.push(at);
        Self {
            mem: vec![0xFF; at as usize],
            bounds,
            dual_bank: false,
            swapped: false,
            steps: 0,
            cut: None,
            dead: false,
        }
    }

    /// Declares the device a dual-bank one: two equal halves with identical unit
    /// layouts, so that [`set_bank_swap`](Self::set_bank_swap) is meaningful.
    pub fn dual_bank(mut self) -> Self {
        let cap = self.capacity();
        assert!(cap % 2 == 0, "dual bank needs an even capacity");
        let half = cap / 2;
        let split = self
            .bounds
            .iter()
            .position(|&b| b == half)
            .expect("bank boundary must be a unit boundary");
        let (lo, hi) = (&self.bounds[..=split], &self.bounds[split..]);
        assert_eq!(
            lo.len(),
            hi.len(),
            "banks must have the same number of units"
        );
        for (l, h) in lo.windows(2).zip(hi.windows(2)) {
            assert_eq!(
                l[1] - l[0],
                h[1] - h[0],
                "banks must have the same unit sizes"
            );
        }
        self.dual_bank = true;
        self
    }

    /// The simulated cells, by abstract offset (not the swapped view).
    pub fn bytes(&self) -> &[u8] {
        &self.mem
    }

    /// Direct access to the cells, bypassing flash physics. For injecting corruption.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.mem
    }

    /// Completed steps (programmed words and erased units) since creation.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// True after a power cut fired and until [`power_on`](Self::power_on).
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Arms a power cut: after `after` more steps have completed, the next step is
    /// the one that fails. `after` is counted from creation (see [`steps`](Self::steps)),
    /// so it must not be in the past.
    pub fn arm_power_cut(&mut self, after: u64, mode: CutMode) {
        assert!(after >= self.steps, "cannot arm a power cut in the past");
        self.cut = Some((after, mode));
    }

    /// Power is back: clears a fired or pending cut. The cells keep whatever the
    /// cut left in them, as real flash does.
    pub fn power_on(&mut self) {
        self.dead = false;
        self.cut = None;
    }

    /// Swaps (or restores) the two banks in the memory-mapped view.
    /// Only [`mapped_read`](Self::mapped_read) is affected.
    pub fn set_bank_swap(&mut self, swapped: bool) {
        assert!(self.dual_bank, "set_bank_swap needs a dual-bank device");
        self.swapped = swapped;
    }

    /// Whether the memory-mapped view is currently bank-swapped.
    pub fn is_bank_swapped(&self) -> bool {
        self.swapped
    }

    /// Reads as the CPU would through the memory-mapped window: with the banks
    /// swapped, view offset `v` shows the cell at `(v + capacity / 2) % capacity`.
    pub fn mapped_read(&self, view_off: u32, buf: &mut [u8]) -> Result<(), FlashError> {
        if self.dead {
            return Err(FlashError::PowerCut);
        }
        let cap = self.capacity();
        if u64::from(view_off) + buf.len() as u64 > u64::from(cap) {
            return Err(FlashError::OutOfRange);
        }
        let half = cap / 2;
        for (i, b) in buf.iter_mut().enumerate() {
            let v = view_off + i as u32;
            let phys = if self.swapped { (v + half) % cap } else { v };
            *b = self.mem[phys as usize];
        }
        Ok(())
    }

    /// Deterministic stand-in for "which bits got there before the power went":
    /// a splitmix64-style hash of the step and the byte position.
    fn torn_mask(step: u64, pos: u32) -> u8 {
        let mut z = step.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ u64::from(pos).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z ^= z >> 30;
        z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z ^= z >> 27;
        z = z.wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 24) as u8
    }
}

impl<const ALIGN: u32> Flash for RamFlash<ALIGN> {
    const PROGRAM_ALIGN: u32 = ALIGN;

    fn capacity(&self) -> u32 {
        self.mem.len() as u32
    }

    fn read(&mut self, off: u32, buf: &mut [u8]) -> Result<(), FlashError> {
        if self.dead {
            return Err(FlashError::PowerCut);
        }
        let end = u64::from(off) + buf.len() as u64;
        if end > u64::from(self.capacity()) {
            return Err(FlashError::OutOfRange);
        }
        buf.copy_from_slice(&self.mem[off as usize..end as usize]);
        Ok(())
    }

    fn program(&mut self, off: u32, data: &[u8]) -> Result<(), FlashError> {
        if self.dead {
            return Err(FlashError::PowerCut);
        }
        let align = ALIGN as usize;
        if off % ALIGN != 0 || data.len() % align != 0 {
            return Err(FlashError::Unaligned);
        }
        if u64::from(off) + data.len() as u64 > u64::from(self.capacity()) {
            return Err(FlashError::OutOfRange);
        }
        let words = (data.len() / align) as u64;
        // Words that may still complete before an armed cut fires.
        let budget = self.cut.map_or(u64::MAX, |(at, _)| at - self.steps);
        let done = words.min(budget);
        let done_bytes = done as usize * align;
        for (i, &b) in data[..done_bytes].iter().enumerate() {
            self.mem[off as usize + i] &= b;
        }
        self.steps += done;
        if done < words {
            // The cut fires on word index `done` of this call.
            if let Some((_, CutMode::Torn)) = self.cut {
                let step = self.steps;
                for i in done_bytes..done_bytes + align {
                    let cell = &mut self.mem[off as usize + i];
                    // Bits to clear may or may not have been cleared yet.
                    *cell &= data[i] | Self::torn_mask(step, off + i as u32);
                }
            }
            self.dead = true;
            return Err(FlashError::PowerCut);
        }
        Ok(())
    }

    fn unit_at(&self, off: u32) -> Range<u32> {
        let cap = self.capacity();
        if off >= cap {
            return cap..cap;
        }
        // `bounds[0] == 0 <= off`, so the partition point is at least 1.
        let idx = self.bounds.partition_point(|&b| b <= off) - 1;
        self.bounds[idx]..self.bounds[idx + 1]
    }

    fn erase_unit(&mut self, off: u32) -> Result<(), FlashError> {
        if self.dead {
            return Err(FlashError::PowerCut);
        }
        if off >= self.capacity() {
            return Err(FlashError::OutOfRange);
        }
        let unit = self.unit_at(off);
        let cells = unit.start as usize..unit.end as usize;
        if let Some((at, mode)) = self.cut {
            if self.steps == at {
                if mode == CutMode::Torn {
                    let step = self.steps;
                    for (i, cell) in self.mem[cells].iter_mut().enumerate() {
                        // Bits to set may or may not have been set yet.
                        *cell |= Self::torn_mask(step, unit.start + i as u32);
                    }
                }
                self.dead = true;
                return Err(FlashError::PowerCut);
            }
        }
        self.mem[cells].fill(0xFF);
        self.steps += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type F4 = RamFlash<4>;
    type Nor = RamFlash<1>;

    fn f42x() -> F4 {
        F4::new(F42X_DUAL_BANK_2MB).dual_bank()
    }

    /// Unit start offsets of the F42x 2 MiB device, written out by hand from the
    /// sector table in RM0090 (not derived from `F42X_DUAL_BANK_2MB`).
    const F42X_STARTS: [u32; 24] = [
        0x00000, 0x04000, 0x08000, 0x0C000, 0x10000, 0x20000, 0x40000, 0x60000, 0x80000, 0xA0000,
        0xC0000, 0xE0000, // bank 1
        0x100000, 0x104000, 0x108000, 0x10C000, 0x110000, 0x120000, 0x140000, 0x160000, 0x180000,
        0x1A0000, 0x1C0000, 0x1E0000, // bank 2
    ];
    const F42X_END: u32 = 0x200000;

    #[test]
    fn f42x_units_match_the_reference_sector_map() {
        let f = f42x();
        assert_eq!(f.capacity(), F42X_END);
        for (i, &start) in F42X_STARTS.iter().enumerate() {
            let end = F42X_STARTS.get(i + 1).copied().unwrap_or(F42X_END);
            // first byte, a middle byte and the last byte of the unit all map to it
            for off in [start, start + (end - start) / 2, end - 1] {
                assert_eq!(f.unit_at(off), start..end, "offset {off:#x}");
            }
        }
    }

    #[test]
    fn uniform_units_are_4k() {
        let f = Nor::new(UNIFORM_4K_4MB);
        assert_eq!(f.capacity(), 4 * 1024 * 1024);
        assert_eq!(f.unit_at(0), 0..4096);
        assert_eq!(f.unit_at(4095), 0..4096);
        assert_eq!(f.unit_at(4096), 4096..8192);
        assert_eq!(f.unit_at(0x3F_FFFF), 0x3F_F000..0x40_0000);
    }

    #[test]
    fn unit_at_outside_the_device_is_empty() {
        let f = f42x();
        assert!(f.unit_at(F42X_END).is_empty());
        assert!(f.unit_at(u32::MAX).is_empty());
        assert_eq!(f.unit_at(F42X_END - 1), 0x1E0000..F42X_END);
    }

    /// The trait contract every geometry must satisfy: units tile the device with
    /// no gap or overlap, and each one is a whole number of program words.
    fn units_tile_the_device<F: Flash>(f: &F) {
        let mut at = 0;
        while at < f.capacity() {
            let u = f.unit_at(at);
            assert_eq!(u.start, at, "gap or overlap at {at:#x}");
            assert!(!u.is_empty());
            assert_eq!((u.end - u.start) % F::PROGRAM_ALIGN, 0);
            at = u.end;
        }
        assert_eq!(at, f.capacity());
    }

    #[test]
    fn every_geometry_tiles_the_device() {
        units_tile_the_device(&Nor::new(UNIFORM_4K_4MB));
        units_tile_the_device(&F4::new(UNIFORM_4K_4MB));
        units_tile_the_device(&f42x());
        units_tile_the_device(&RamFlash::<8>::new(&[(16, 3), (64, 2), (32, 5)]));
    }

    #[test]
    fn program_only_clears_bits() {
        let mut f = Nor::new(UNIFORM_4K_4MB);
        f.program(10, &[0xF0]).unwrap();
        f.program(10, &[0x3C]).unwrap();
        let mut b = [0u8; 1];
        f.read(10, &mut b).unwrap();
        assert_eq!(b[0], 0xF0 & 0x3C);
        // a 1 in the data cannot set a bit that is already 0
        f.program(10, &[0xFF]).unwrap();
        f.read(10, &mut b).unwrap();
        assert_eq!(b[0], 0x30);
    }

    #[test]
    fn erase_sets_exactly_one_unit() {
        let mut f = f42x();
        f.program(0, &vec![0u8; F42X_END as usize]).unwrap();
        // The 64 KiB sector sits between 16 KiB and 128 KiB neighbours.
        f.erase_unit(0x10000 + 5).unwrap();
        for (i, &b) in f.bytes().iter().enumerate() {
            let erased = (0x10000..0x20000).contains(&i);
            assert_eq!(b, if erased { 0xFF } else { 0x00 }, "byte {i:#x}");
        }
        assert_eq!(f.steps(), (F42X_END / 4) as u64 + 1);
    }

    #[test]
    fn erase_works_from_any_offset_in_the_unit() {
        for off in [0x4000, 0x4001, 0x7FFF] {
            let mut f = f42x();
            f.program(0, &vec![0u8; 0x10000]).unwrap();
            f.erase_unit(off).unwrap();
            assert!(f.bytes()[0x4000..0x8000].iter().all(|&b| b == 0xFF));
            assert!(f.bytes()[..0x4000].iter().all(|&b| b == 0));
            assert!(f.bytes()[0x8000..0x10000].iter().all(|&b| b == 0));
        }
    }

    #[test]
    fn unaligned_program_is_rejected_and_changes_nothing() {
        let mut f = F4::new(UNIFORM_4K_4MB);
        for (off, len) in [(2u32, 4usize), (1, 4), (0, 3), (0, 1), (4, 6)] {
            assert_eq!(f.program(off, &vec![0u8; len]), Err(FlashError::Unaligned));
        }
        assert!(f.bytes().iter().all(|&b| b == 0xFF));
        assert_eq!(f.steps(), 0);
        // aligned is fine
        f.program(4, &[0, 0, 0, 0]).unwrap();
        assert_eq!(&f.bytes()[..9], &[0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0xFF]);
    }

    #[test]
    fn out_of_range_is_rejected_and_changes_nothing() {
        let mut f = F4::new(&[(4096, 2)]);
        let cap = f.capacity();
        // starts inside, ends past the end: nothing may be written
        assert_eq!(f.program(cap - 4, &[0; 8]), Err(FlashError::OutOfRange));
        assert_eq!(f.program(cap, &[0; 4]), Err(FlashError::OutOfRange));
        assert!(f.bytes().iter().all(|&b| b == 0xFF));
        assert_eq!(f.steps(), 0);
        let mut b = [0u8; 8];
        assert_eq!(f.read(cap - 4, &mut b), Err(FlashError::OutOfRange));
        assert_eq!(f.read(u32::MAX, &mut b), Err(FlashError::OutOfRange));
        assert_eq!(f.erase_unit(cap), Err(FlashError::OutOfRange));
        // the last bytes are reachable
        assert_eq!(f.read(cap - 8, &mut b), Ok(()));
        f.program(cap - 4, &[0; 4]).unwrap();
    }

    #[test]
    fn empty_program_is_accepted_and_free() {
        let mut f = F4::new(UNIFORM_4K_4MB);
        f.program(0, &[]).unwrap();
        f.program(f.capacity(), &[]).unwrap(); // an empty range ending at the end fits
        assert_eq!(
            f.program(f.capacity() + 4, &[]),
            Err(FlashError::OutOfRange)
        );
        assert_eq!(f.steps(), 0);
    }

    #[test]
    fn clean_cut_in_a_program_stops_after_whole_words() {
        let mut f = F4::new(UNIFORM_4K_4MB);
        f.arm_power_cut(2, CutMode::Clean);
        assert_eq!(f.program(0, &[0u8; 16]), Err(FlashError::PowerCut));
        assert!(f.is_dead());
        assert_eq!(f.steps(), 2);
        // everything fails while the power is off
        let mut b = [0u8; 4];
        assert_eq!(f.read(0, &mut b), Err(FlashError::PowerCut));
        assert_eq!(f.program(32, &[0; 4]), Err(FlashError::PowerCut));
        assert_eq!(f.erase_unit(0), Err(FlashError::PowerCut));
        assert_eq!(f.mapped_read(0, &mut b), Err(FlashError::PowerCut));
        // power back: the two words stayed programmed, the third did not happen
        f.power_on();
        assert_eq!(
            &f.bytes()[..12],
            &[0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]
        );
        f.read(0, &mut b).unwrap();
        f.program(8, &[0; 4]).unwrap();
    }

    #[test]
    fn a_cut_armed_for_the_end_of_an_operation_does_not_fire_in_it() {
        let mut f = F4::new(UNIFORM_4K_4MB);
        f.arm_power_cut(4, CutMode::Clean);
        f.program(0, &[0u8; 16]).unwrap(); // exactly 4 words
        assert!(!f.is_dead());
        // the next step is the one that fails, and it has no effect
        assert_eq!(f.program(16, &[0; 4]), Err(FlashError::PowerCut));
        assert_eq!(&f.bytes()[16..20], &[0xFF; 4]);
    }

    #[test]
    fn a_cut_can_span_calls_and_erases_count_as_steps() {
        let mut f = F4::new(UNIFORM_4K_4MB);
        f.arm_power_cut(3, CutMode::Clean);
        f.program(0, &[0; 4]).unwrap(); // step 0
        f.erase_unit(0).unwrap(); // step 1
        f.program(0, &[0; 4]).unwrap(); // step 2
        assert_eq!(f.erase_unit(0), Err(FlashError::PowerCut)); // step 3 fails
        assert_eq!(
            &f.bytes()[..4],
            &[0; 4],
            "a clean cut leaves the unit untouched"
        );
    }

    #[test]
    fn clean_cut_in_an_erase_leaves_the_unit_untouched() {
        let mut f = F4::new(UNIFORM_4K_4MB);
        f.program(4096, &vec![0u8; 4096]).unwrap();
        let steps = f.steps();
        f.arm_power_cut(steps, CutMode::Clean);
        assert_eq!(f.erase_unit(4096), Err(FlashError::PowerCut));
        assert!(f.bytes()[4096..8192].iter().all(|&b| b == 0));
    }

    #[test]
    fn torn_erase_only_sets_bits_and_stays_within_its_unit() {
        let mut f = F4::new(UNIFORM_4K_4MB);
        let old: Vec<u8> = (0..4096u32).map(|i| (i * 7 + 3) as u8).collect();
        f.program(4096, &old).unwrap();
        let steps = f.steps();
        f.arm_power_cut(steps, CutMode::Torn);
        assert_eq!(f.erase_unit(5000), Err(FlashError::PowerCut));
        let now = &f.bytes()[4096..8192];
        // no bit went 1 -> 0
        assert!(now.iter().zip(&old).all(|(&n, &o)| n & o == o));
        // it really is partial: some byte changed, and not every byte is erased
        assert!(now.iter().zip(&old).any(|(&n, &o)| n != o));
        assert!(now.iter().any(|&n| n != 0xFF));
        // neighbours untouched
        assert!(f.bytes()[..4096].iter().all(|&b| b == 0xFF));
        assert!(f.bytes()[8192..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn torn_program_only_clears_bits_and_only_in_the_cut_word() {
        for cut_word in 0..4u64 {
            let mut f = F4::new(UNIFORM_4K_4MB);
            f.program(0, &[0xA5; 16]).unwrap();
            let base = f.steps();
            f.arm_power_cut(base + cut_word, CutMode::Torn);
            assert_eq!(f.program(0, &[0x0F; 16]), Err(FlashError::PowerCut));
            for (i, &b) in f.bytes()[..16].iter().enumerate() {
                let word = (i / 4) as u64;
                if word < cut_word {
                    assert_eq!(b, 0xA5 & 0x0F, "word {word} completed");
                } else if word > cut_word {
                    assert_eq!(b, 0xA5, "word {word} never started");
                } else {
                    // between "nothing happened" and "fully programmed"
                    assert_eq!(b & !0xA5, 0, "a bit was set");
                    assert_eq!(
                        b & (0xA5 & 0x0F),
                        0xA5 & 0x0F,
                        "a bit that stays 1 was cleared"
                    );
                }
            }
        }
    }

    #[test]
    fn torn_program_is_actually_partial_somewhere() {
        // Across cut positions the torn word is neither always untouched nor always done.
        let (mut untouched, mut done, mut partial) = (0, 0, 0);
        for cut in 0..64u64 {
            let mut f = Nor::new(UNIFORM_4K_4MB);
            f.arm_power_cut(cut, CutMode::Torn);
            let _ = f.program(0, &[0u8; 64]);
            match f.bytes()[cut as usize] {
                0xFF => untouched += 1,
                0x00 => done += 1,
                _ => partial += 1,
            }
        }
        assert!(partial > 0 && untouched + done + partial == 64);
    }

    #[test]
    fn power_on_keeps_the_cells_and_clears_the_cut() {
        let mut f = F4::new(UNIFORM_4K_4MB);
        f.arm_power_cut(1, CutMode::Clean);
        let _ = f.program(0, &[0; 8]);
        f.power_on();
        assert!(!f.is_dead());
        f.program(8, &[0; 8]).unwrap(); // no cut pending any more
                                        // word 0 survived the cut, word 1 never happened, words 2 and 3 are the new write
        assert_eq!(&f.bytes()[..4], &[0; 4]);
        assert_eq!(&f.bytes()[4..8], &[0xFF; 4]);
        assert_eq!(&f.bytes()[8..16], &[0; 8]);
    }

    #[test]
    fn bank_swap_changes_the_view_but_not_the_offsets_flash_calls_use() {
        let mut f = f42x();
        let half = f.capacity() / 2;
        f.program(0, &[0x11; 4]).unwrap();
        f.program(half, &[0x22; 4]).unwrap();
        let mut b = [0u8; 4];

        f.mapped_read(0, &mut b).unwrap();
        assert_eq!(b, [0x11; 4]);

        f.set_bank_swap(true);
        assert!(f.is_bank_swapped());
        f.mapped_read(0, &mut b).unwrap();
        assert_eq!(b, [0x22; 4], "the CPU now sees bank 2 at the start");
        f.mapped_read(half, &mut b).unwrap();
        assert_eq!(b, [0x11; 4]);

        // Flash calls are unaffected: offset 0 is still the first physical unit.
        f.read(0, &mut b).unwrap();
        assert_eq!(b, [0x11; 4]);
        f.erase_unit(0).unwrap();
        assert!(f.bytes()[..0x4000].iter().all(|&x| x == 0xFF));
        assert_eq!(&f.bytes()[half as usize..half as usize + 4], &[0x22; 4]);
        assert_eq!(f.unit_at(0), 0..0x4000);

        f.set_bank_swap(false);
        f.mapped_read(half, &mut b).unwrap();
        assert_eq!(b, [0x22; 4]);
    }

    #[test]
    fn mapped_read_across_the_bank_boundary_follows_the_swap() {
        let mut f = f42x();
        let cap = f.capacity() as usize;
        let half = cap / 2;
        // distinct, position-dependent content around both boundaries
        for i in 0..8u8 {
            f.bytes_mut()[half - 4 + i as usize] = 0x10 + i;
            f.bytes_mut()[cap - 4 + (i as usize % 4)] = 0x40 + (i % 4);
            f.bytes_mut()[i as usize % 4] = 0x50 + (i % 4);
        }
        let mut b = [0u8; 8];
        f.mapped_read((half - 4) as u32, &mut b).unwrap();
        assert_eq!(b, [0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17]);
        f.set_bank_swap(true);
        f.mapped_read((half - 4) as u32, &mut b).unwrap();
        // view [half-4, half) is physical [cap-4, cap); view [half, half+4) is physical [0, 4)
        assert_eq!(b, [0x40, 0x41, 0x42, 0x43, 0x50, 0x51, 0x52, 0x53]);
        assert_eq!(
            f.mapped_read(f.capacity() - 3, &mut b),
            Err(FlashError::OutOfRange)
        );
    }

    #[test]
    #[should_panic(expected = "same unit sizes")]
    fn dual_bank_rejects_banks_with_different_unit_layouts() {
        // bank 1 = 16+16+32 KiB, bank 2 = 32+16+16 KiB: the halves meet on a unit edge
        // and have the same number of units, but not the same sizes
        let _ = F4::new(&[(16 * 1024, 2), (32 * 1024, 2), (16 * 1024, 2)]).dual_bank();
    }

    #[test]
    #[should_panic(expected = "unit boundary")]
    fn dual_bank_needs_the_boundary_on_a_unit_edge() {
        let _ = F4::new(&[(8192, 3)]).dual_bank();
    }

    #[test]
    #[should_panic(expected = "dual-bank")]
    fn bank_swap_needs_a_dual_bank_device() {
        F4::new(UNIFORM_4K_4MB).set_bank_swap(true);
    }

    #[test]
    #[should_panic(expected = "in the past")]
    fn arming_a_cut_in_the_past_is_a_test_bug() {
        let mut f = F4::new(UNIFORM_4K_4MB);
        f.program(0, &[0; 8]).unwrap();
        f.arm_power_cut(1, CutMode::Clean);
    }
}
