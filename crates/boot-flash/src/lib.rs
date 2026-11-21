//! Flash abstraction for FerroBoot.
//!
//! [`Flash`] describes a flash device the way the journal, the updater and the
//! rescue protocol need to see it, without assuming how it is organised:
//!
//! * **Offsets are abstract.** `off` is a byte offset inside the device
//!   (`0..capacity()`), the same flash-backend offset space `boot_layout::Layout`
//!   uses. It is not a CPU address. A backend that can remap its memory-mapped
//!   view (bank swap, FlexSPI remap) keeps *these* offsets fixed and does the
//!   translation itself.
//! * **Erase units may be uneven.** STM32F42x has 16 KiB, 64 KiB and 128 KiB
//!   sectors in one device; a SPI NOR has 4 KiB sectors. [`Flash::unit_at`] is a
//!   pure query (testable on the host) that returns the unit containing an offset.
//! * **Programming has an alignment.** [`Flash::PROGRAM_ALIGN`] is the smallest
//!   programmable word (1 for SPI NOR, 4 for STM32F4 at x32, larger on other
//!   families). Callers must not assume 1.
//!
//! Flash physics are the same everywhere this trait applies: programming can only
//! turn a bit from 1 to 0, and only an erase turns it back to 1, a whole unit at a
//! time.
//!
//! Nothing in the firmware depends on this crate yet. The `std` feature adds
//! [`RamFlash`], a simulator with those physics, uneven units, alignment,
//! a swappable memory-mapped view and power-cut injection at every program word
//! and every erase.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(any(test, feature = "std"))]
extern crate std;

use core::ops::Range;

#[cfg(any(test, feature = "std"))]
mod ram;
#[cfg(any(test, feature = "std"))]
pub use ram::{CutMode, RamFlash, F42X_DUAL_BANK_2MB, UNIFORM_4K_4MB};

/// Why a flash operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FlashError {
    /// The transport or the controller reported a failure.
    IoError,
    /// The platform provides no access to the flash (it is owned by someone else).
    NoTransport,
    /// The device stayed busy longer than allowed.
    Timeout,
    /// The target is write- or erase-protected.
    Protected,
    /// The access reaches outside `0..capacity()`.
    OutOfRange,
    /// Offset or length is not a multiple of [`Flash::PROGRAM_ALIGN`].
    Unaligned,
    /// Power was lost during the operation. Only simulators return this; it is in the
    /// enum so that `?` paths need no `cfg`.
    PowerCut,
}

/// A flash device addressed by abstract byte offsets.
pub trait Flash {
    /// Smallest programmable unit in bytes: both the offset and the length of a
    /// [`program`](Flash::program) call must be multiples of it.
    const PROGRAM_ALIGN: u32;

    /// Size of the device in bytes.
    fn capacity(&self) -> u32;

    /// Reads `buf.len()` bytes starting at `off`.
    ///
    /// Fails with [`FlashError::OutOfRange`] if the range does not fit in the device.
    fn read(&mut self, off: u32, buf: &mut [u8]) -> Result<(), FlashError>;

    /// Programs `data` at `off`. Bits can only go from 1 to 0.
    ///
    /// `off` and `data.len()` must be multiples of [`PROGRAM_ALIGN`](Flash::PROGRAM_ALIGN)
    /// ([`FlashError::Unaligned`] otherwise), and the range must fit in the device
    /// ([`FlashError::OutOfRange`]). A rejected call changes nothing. An empty `data` is
    /// accepted and does nothing. The range may span several erase units.
    fn program(&mut self, off: u32, data: &[u8]) -> Result<(), FlashError>;

    /// The erase unit that contains `off`, as a range of offsets.
    ///
    /// A pure query: it never touches the device. For an `off` outside the device
    /// it returns an empty range, so callers can stop on `is_empty()`.
    fn unit_at(&self, off: u32) -> Range<u32>;

    /// Erases the whole unit that contains `off` (every byte becomes `0xFF`).
    ///
    /// `off` does not have to be the start of the unit. Fails with
    /// [`FlashError::OutOfRange`] if `off >= capacity()`.
    fn erase_unit(&mut self, off: u32) -> Result<(), FlashError>;

    /// Erases, starting at `off`, as much as one erase operation can clear efficiently
    /// without going past `limit`, and returns the range that was erased.
    ///
    /// This is how a caller clears a long range (a whole slot) in as few operations as
    /// the device allows: loop from the start of the range, continue at the end of the
    /// returned range, stop at `limit`. A backend with a faster, larger erase (a SPI NOR
    /// has 64 KiB blocks next to its 4 KiB sectors) overrides it; the default erases
    /// exactly one unit, so a device without such an operation needs nothing.
    ///
    /// Contract, for the default and every override:
    /// * `off` must be the start of a unit and `limit` at least the end of that unit,
    ///   otherwise [`FlashError::Unaligned`] and nothing is erased;
    /// * `off >= capacity()` is [`FlashError::OutOfRange`];
    /// * on success the returned range starts at `off`, is made of whole units, ends at
    ///   or before `limit`, and every byte of it is erased.
    ///
    /// One call is one blocking operation (the longest the device needs for a unit, or
    /// for the larger span an override picks), which is the granularity at which a caller
    /// can kick a watchdog or report progress.
    fn erase_span(&mut self, off: u32, limit: u32) -> Result<Range<u32>, FlashError> {
        let unit = self.unit_at(off);
        if unit.is_empty() {
            return Err(FlashError::OutOfRange);
        }
        if unit.start != off || unit.end > limit {
            return Err(FlashError::Unaligned);
        }
        self.erase_unit(off)?;
        Ok(unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type F4 = RamFlash<4>;

    fn f42x() -> F4 {
        F4::new(F42X_DUAL_BANK_2MB).dual_bank()
    }

    fn all_programmed(f: &mut F4) {
        let n = f.capacity() as usize;
        f.program(0, &std::vec![0u8; n]).unwrap();
    }

    /// Clears `start..end` the way a caller is meant to: loop on `erase_span`.
    fn clear_range<F: Flash>(f: &mut F, start: u32, end: u32) -> std::vec::Vec<Range<u32>> {
        let mut spans = std::vec::Vec::new();
        let mut at = start;
        while at < end {
            let r = f.erase_span(at, end).unwrap();
            assert_eq!(r.start, at, "a span starts where the caller asked");
            assert!(
                r.end > at && r.end <= end,
                "a span makes progress and stays inside"
            );
            at = r.end;
            spans.push(r);
        }
        assert_eq!(at, end);
        spans
    }

    #[test]
    fn default_span_is_exactly_one_unit() {
        let mut f = f42x();
        all_programmed(&mut f);
        let before = f.steps();
        // the 64 KiB sector, with a limit far beyond it
        assert_eq!(f.erase_span(0x10000, 0x20_0000), Ok(0x10000..0x20000));
        assert_eq!(f.steps(), before + 1, "one operation");
        for (i, &b) in f.bytes().iter().enumerate() {
            let erased = (0x10000..0x20000).contains(&i);
            assert_eq!(b, if erased { 0xFF } else { 0 }, "byte {i:#x}");
        }
    }

    #[test]
    fn span_needs_a_unit_start_and_room_for_the_whole_unit() {
        let mut f = f42x();
        all_programmed(&mut f);
        let before = f.steps();
        // not the start of a unit
        assert_eq!(f.erase_span(0x4001, 0x20_0000), Err(FlashError::Unaligned));
        assert_eq!(f.erase_span(0x10004, 0x20_0000), Err(FlashError::Unaligned));
        // limit cuts the unit (0x4000..0x8000) short
        assert_eq!(f.erase_span(0x4000, 0x7FFF), Err(FlashError::Unaligned));
        assert_eq!(f.erase_span(0x4000, 0x4000), Err(FlashError::Unaligned));
        assert_eq!(f.steps(), before, "a rejected call erases nothing");
        assert!(f.bytes().iter().all(|&b| b == 0));
        // limit exactly at the unit end is enough
        assert_eq!(f.erase_span(0x4000, 0x8000), Ok(0x4000..0x8000));
    }

    #[test]
    fn span_outside_the_device_is_out_of_range() {
        let mut f = f42x();
        let cap = f.capacity();
        assert_eq!(f.erase_span(cap, cap), Err(FlashError::OutOfRange));
        assert_eq!(f.erase_span(cap + 4, u32::MAX), Err(FlashError::OutOfRange));
        assert_eq!(
            f.erase_span(u32::MAX, u32::MAX),
            Err(FlashError::OutOfRange)
        );
        assert_eq!(f.steps(), 0);
        // the last unit is reachable
        assert_eq!(f.erase_span(0x1E0000, cap), Ok(0x1E0000..cap));
    }

    #[test]
    fn looping_on_spans_clears_exactly_the_range_on_every_geometry() {
        // (geometry, start, end): whole device, and ranges that begin and end on unit edges
        let cases: [(&[(u32, u32)], u32, u32); 4] = [
            (UNIFORM_4K_4MB, 0, 4 * 1024 * 1024),
            (UNIFORM_4K_4MB, 0x10_0000, 0x10_0000 + 640 * 1024),
            (F42X_DUAL_BANK_2MB, 0, 0x20_0000),
            (F42X_DUAL_BANK_2MB, 0x10000, 0x80000), // 64K + 3 x 128K
        ];
        for (geometry, start, end) in cases {
            let mut f = RamFlash::<4>::new(geometry);
            let n = f.capacity() as usize;
            f.program(0, &std::vec![0u8; n]).unwrap();
            let before = f.steps();
            let spans = clear_range(&mut f, start, end);
            for (i, &b) in f.bytes().iter().enumerate() {
                let inside = (start as usize..end as usize).contains(&i);
                assert_eq!(b, if inside { 0xFF } else { 0 }, "byte {i:#x}");
            }
            // the default is one unit per call
            assert_eq!(f.steps() - before, spans.len() as u64);
            assert!(spans.windows(2).all(|w| w[0].end == w[1].start));
        }
    }

    #[test]
    fn a_range_that_ends_inside_a_unit_cannot_be_cleared_by_looping() {
        // The 128 KiB sector at 0x20000 does not fit in a range that ends at 0x30000.
        let mut f = f42x();
        assert_eq!(f.erase_span(0x20000, 0x30000), Err(FlashError::Unaligned));
    }
}
