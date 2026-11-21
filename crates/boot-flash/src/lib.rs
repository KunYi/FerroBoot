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
}
