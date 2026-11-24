//! Pure helpers for the UART rescue protocol (version 2).
//!
//! The command loop itself needs the UART and the flash driver and lives in
//! `bootloader-h750`. Everything that decides *what is allowed* lives here so it can be
//! tested on the host: how the slot argument is read, whether a write stays inside its slot
//! and fits the device's program word, how much payload to discard after a rejected write,
//! which erase operations "erase slot" takes, and what the `i` (info) line says.
//!
//! Protocol version 2 (version 1 had no way to ask; its `i` printed a fixed banner):
//! * `i` answers one `INFO` line (see [`write_info`]) with the protocol version and what the
//!   host needs to know about the device;
//! * `w` needs an offset and length that are multiples of the program word (`align`);
//! * `e` answers one `.` per completed erase step, then `OK` or `ERR`, so the host can time
//!   out per step instead of guessing how long a whole slot takes.
#![no_std]

use boot_flash::{Flash, FlashError};
use boot_layout::{ExecModel, Layout};
use core::fmt;
use core::ops::Range;

/// Version announced in the `INFO` line. Bump it when a command changes incompatibly.
pub const PROTO_VERSION: u32 = 2;
/// Largest payload of one `w` command.
pub const MAX_WRITE_CHUNK: u32 = 512;
/// Most payload bytes read and thrown away after a rejected `w`, so the host's data is not
/// interpreted as commands (it contains arbitrary bytes, including `e`, `w`, `m`, `r`).
pub const MAX_DRAIN: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescueError {
    /// Slot argument is not `0` or `1`.
    BadSlot,
    /// `w` length is 0 or larger than [`MAX_WRITE_CHUNK`].
    BadLength,
    /// `w` would write outside its slot.
    OutOfRange,
    /// `w` offset or length is not a multiple of the device's program word, or the slot to
    /// erase is not made of whole erase units.
    Unaligned,
}

impl RescueError {
    /// Reply line (without CRLF) sent to the host.
    pub const fn reply(self) -> &'static str {
        match self {
            RescueError::BadSlot => "ERR_SLOT",
            RescueError::BadLength => "ERR_LEN",
            RescueError::OutOfRange => "ERR_RANGE",
            RescueError::Unaligned => "ERR_ALIGN",
        }
    }
}

/// Slot argument: exactly `'0'` or `'1'`. (The old code treated every other byte as slot 1.)
pub const fn parse_slot(c: u8) -> Result<u8, RescueError> {
    match c {
        b'0' => Ok(0),
        b'1' => Ok(1),
        _ => Err(RescueError::BadSlot),
    }
}

/// Flash offset for a `w` of `len` bytes at slot-relative `offset` on a device with program
/// word `align`, or why it is refused. The whole write must lie inside the slot, and both the
/// offset and the length must be multiples of `align`.
pub fn write_address(
    layout: &Layout,
    slot: u8,
    offset: u32,
    len: u32,
    align: u32,
) -> Result<u32, RescueError> {
    let region = layout.slot(slot).ok_or(RescueError::BadSlot)?;
    if len == 0 || len > MAX_WRITE_CHUNK {
        return Err(RescueError::BadLength);
    }
    let end = offset.checked_add(len).ok_or(RescueError::OutOfRange)?;
    if end > region.len {
        return Err(RescueError::OutOfRange);
    }
    let align = align.max(1);
    if offset % align != 0 || len % align != 0 {
        return Err(RescueError::Unaligned);
    }
    Ok(region.offset + offset)
}

/// Largest `w` payload that is a whole number of program words: what the host should send.
pub const fn chunk_for(align: u32) -> u32 {
    if align == 0 {
        MAX_WRITE_CHUNK
    } else {
        MAX_WRITE_CHUNK / align * align
    }
}

/// Payload bytes to read and discard after refusing a `w` that announced `len` bytes.
pub const fn drain_len(len: u32) -> u32 {
    if len > MAX_DRAIN {
        MAX_DRAIN
    } else {
        len
    }
}

/// Erases a whole slot one erase operation at a time, so the caller can kick the watchdog and
/// report progress between steps.
#[derive(Debug, Clone, Copy)]
pub struct SlotErase {
    next: u32,
    end: u32,
}

impl SlotErase {
    /// Prepares to erase `slot`. The slot has to be made of whole erase units of `flash` (the
    /// first unit starts where the slot does, the last one ends where it ends); otherwise
    /// [`RescueError::Unaligned`] and nothing has been touched.
    pub fn new<F: Flash>(flash: &F, layout: &Layout, slot: u8) -> Result<Self, RescueError> {
        let region = layout.slot(slot).ok_or(RescueError::BadSlot)?;
        let end = region
            .offset
            .checked_add(region.len)
            .ok_or(RescueError::Unaligned)?;
        if region.len == 0
            || flash.unit_at(region.offset).start != region.offset
            || flash.unit_at(end - 1).end != end
        {
            return Err(RescueError::Unaligned);
        }
        Ok(SlotErase {
            next: region.offset,
            end,
        })
    }

    /// Does the next erase operation (`Flash::erase_span`, so a device with a bigger erase uses
    /// it) and returns the range it cleared, or `None` once the slot is done. After an error
    /// the erase is over: later calls return `None` rather than carry on past a failure.
    pub fn step<F: Flash>(&mut self, flash: &mut F) -> Option<Result<Range<u32>, FlashError>> {
        if self.next >= self.end {
            return None;
        }
        match flash.erase_span(self.next, self.end) {
            Ok(r) if r.start == self.next && r.end > r.start && r.end <= self.end => {
                self.next = r.end;
                Some(Ok(r))
            }
            // A span that breaks the `Flash` contract would stall or run past the slot.
            Ok(_) => {
                self.next = self.end;
                Some(Err(FlashError::IoError))
            }
            Err(e) => {
                self.next = self.end;
                Some(Err(e))
            }
        }
    }
}

/// The `i` reply, without the line ending: one line of `key=value` pairs, all numbers in
/// decimal, separated by single spaces, the board name without spaces:
///
/// `INFO proto=2 board=h750-netlite bl=1 flash=4194304 align=1 chunk=512 slot0=655360 slot1=655360 window=524288`
///
/// `flash` is the device size in bytes, `align` its program word, `chunk` the largest `w`
/// payload the host should send (a whole number of program words), `slot0` and `slot1` the slot
/// sizes and `window` the largest image the board can execute (the RAM window; 0 if the board
/// has no such window).
pub fn write_info<W: fmt::Write>(
    w: &mut W,
    layout: &Layout,
    flash_capacity: u32,
    program_align: u32,
    bl_version: u32,
) -> fmt::Result {
    let window = match layout.exec {
        ExecModel::RamLoad { load, .. } => load.len,
        // An execution model without a RAM window (or one this code does not know yet).
        #[allow(unreachable_patterns)]
        _ => 0,
    };
    // No `write!` here: the formatting machinery behind it costs about 3 KiB of flash for a line
    // this simple, which matters in a bootloader.
    w.write_str("INFO proto=")?;
    put_u32(w, PROTO_VERSION)?;
    w.write_str(" board=")?;
    w.write_str(layout.name)?;
    for (key, value) in [
        (" bl=", bl_version),
        (" flash=", flash_capacity),
        (" align=", program_align),
        (" chunk=", chunk_for(program_align)),
        (" slot0=", layout.slots[0].len),
        (" slot1=", layout.slots[1].len),
        (" window=", window),
    ] {
        w.write_str(key)?;
        put_u32(w, value)?;
    }
    Ok(())
}

/// Writes `n` in decimal, one digit at a time (no `str` conversion: that code costs a few
/// hundred bytes of flash, and a bootloader writes its reply straight to the UART).
fn put_u32<W: fmt::Write>(w: &mut W, mut n: u32) -> fmt::Result {
    let mut digits = [0u8; 10];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    for &d in &digits[i..] {
        w.write_char(char::from(d))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use boot_abi::IoOps;
    use boot_flash::{CutMode, RamFlash, F42X_DUAL_BANK_2MB};
    use boot_layout::{Layout, Region, ALL, H750_NETLITE};
    use boot_nor::{RamNor, SpiNorIoOpsDriver};
    use core::cell::RefCell;
    use std::string::String;
    use std::thread_local;
    use std::vec;
    use std::vec::Vec;

    /// Fixed-size text sink for the tests: fails, like a UART that cannot take more would, rather
    /// than truncating.
    struct LineBuf<const N: usize> {
        buf: [u8; N],
        len: usize,
    }
    impl<const N: usize> LineBuf<N> {
        fn new() -> Self {
            LineBuf {
                buf: [0; N],
                len: 0,
            }
        }
        fn as_str(&self) -> &str {
            core::str::from_utf8(&self.buf[..self.len]).unwrap()
        }
    }
    impl<const N: usize> fmt::Write for LineBuf<N> {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let end = self.len + s.len();
            if end > N {
                return Err(fmt::Error);
            }
            self.buf[self.len..end].copy_from_slice(s.as_bytes());
            self.len = end;
            Ok(())
        }
    }

    #[test]
    fn slot_argument_is_strictly_zero_or_one() {
        assert_eq!(parse_slot(b'0'), Ok(0));
        assert_eq!(parse_slot(b'1'), Ok(1));
        for c in [b'2', b'a', b' ', b'\n', b'\r', 0, 0xFF, b'e'] {
            assert_eq!(parse_slot(c), Err(RescueError::BadSlot), "byte {c:#x}");
        }
    }

    #[test]
    fn write_inside_the_slot_maps_to_its_flash_offset() {
        let l = &H750_NETLITE;
        assert_eq!(write_address(l, 0, 0, 512, 1), Ok(l.slots[0].offset));
        assert_eq!(
            write_address(l, 1, 0x200, 16, 1),
            Ok(l.slots[1].offset + 0x200)
        );
        // Last byte of the slot is writable, one more is not.
        let last = l.slots[1].len - 1;
        assert_eq!(
            write_address(l, 1, last, 1, 1),
            Ok(l.slots[1].offset + last)
        );
        assert_eq!(
            write_address(l, 1, last + 1, 1, 1),
            Err(RescueError::OutOfRange)
        );
        // A chunk that starts inside but ends past the slot is refused as a whole.
        assert_eq!(
            write_address(l, 0, l.slots[0].len - 100, 512, 1),
            Err(RescueError::OutOfRange)
        );
    }

    #[test]
    fn write_cannot_reach_outside_its_slot() {
        // The old handler had no range check: slot 0 + a large offset hit slot 1 or the
        // journal. Check every region this layout has.
        let l = &H750_NETLITE;
        for slot in 0..2u8 {
            let r = l.slot(slot).unwrap();
            let mut off = 0u32;
            while off < r.len {
                let addr = write_address(l, slot, off, 1, 1).unwrap();
                assert!(
                    addr >= r.offset && addr < r.end(),
                    "slot {slot} off {off:#x}"
                );
                off += 0x1_0000;
            }
        }
        assert_eq!(
            write_address(l, 0, u32::MAX, 1, 1),
            Err(RescueError::OutOfRange)
        );
        assert_eq!(
            write_address(l, 0, u32::MAX - 100, 512, 1),
            Err(RescueError::OutOfRange)
        );
        assert_eq!(write_address(l, 2, 0, 1, 1), Err(RescueError::BadSlot));
    }

    #[test]
    fn write_length_is_limited_and_never_silently_truncated() {
        let l = &H750_NETLITE;
        assert_eq!(write_address(l, 0, 0, 0, 1), Err(RescueError::BadLength));
        assert_eq!(
            write_address(l, 0, 0, MAX_WRITE_CHUNK, 1),
            Ok(l.slots[0].offset)
        );
        assert_eq!(
            write_address(l, 0, 0, MAX_WRITE_CHUNK + 1, 1),
            Err(RescueError::BadLength)
        );
    }

    #[test]
    fn write_must_be_whole_program_words() {
        let l = &H750_NETLITE;
        // 4-byte words (STM32F4): offset and length must both be multiples of 4.
        assert_eq!(write_address(l, 0, 0, 512, 4), Ok(l.slots[0].offset));
        assert_eq!(write_address(l, 0, 4, 16, 4), Ok(l.slots[0].offset + 4));
        for (off, len) in [(2u32, 16u32), (1, 4), (0, 15), (0, 1), (4, 6), (3, 3)] {
            assert_eq!(
                write_address(l, 0, off, len, 4),
                Err(RescueError::Unaligned),
                "{off} {len}"
            );
        }
        // byte programming (the H750's NOR) accepts everything the range rules accept
        assert_eq!(write_address(l, 0, 3, 5, 1), Ok(l.slots[0].offset + 3));
        // a 32-byte word
        assert_eq!(write_address(l, 0, 32, 64, 32), Ok(l.slots[0].offset + 32));
        assert_eq!(write_address(l, 0, 16, 32, 32), Err(RescueError::Unaligned));
        // length and range are reported before alignment
        assert_eq!(write_address(l, 0, 0, 0, 4), Err(RescueError::BadLength));
        assert_eq!(
            write_address(l, 0, l.slots[0].len - 2, 3, 4),
            Err(RescueError::OutOfRange)
        );
        // an alignment of 0 is a bug in the caller; it is treated as 1, not a division by zero
        assert_eq!(write_address(l, 0, 1, 1, 0), Ok(l.slots[0].offset + 1));
    }

    #[test]
    fn the_chunk_the_host_is_told_to_send_is_whole_words_and_at_most_512() {
        for (align, chunk) in [
            (1u32, 512u32),
            (2, 512),
            (4, 512),
            (8, 512),
            (16, 512),
            (32, 512),
            (3, 510),
            (5, 510),
            (7, 511),
        ] {
            assert_eq!(chunk_for(align), chunk, "align {align}");
        }
        for align in 1..=64u32 {
            let c = chunk_for(align);
            assert!(c <= MAX_WRITE_CHUNK && c % align == 0, "align {align}: {c}");
        }
        assert_eq!(
            chunk_for(600),
            0,
            "a word wider than the largest chunk cannot be sent"
        );
        assert_eq!(chunk_for(0), MAX_WRITE_CHUNK);
        // and the largest chunk is accepted by write_address
        assert_eq!(
            write_address(&H750_NETLITE, 0, 0, chunk_for(3), 3),
            Ok(H750_NETLITE.slots[0].offset)
        );
    }

    #[test]
    fn drained_payload_is_bounded() {
        assert_eq!(drain_len(0), 0);
        assert_eq!(drain_len(512), 512);
        assert_eq!(drain_len(MAX_DRAIN), MAX_DRAIN);
        assert_eq!(drain_len(u32::MAX), MAX_DRAIN);
    }

    #[test]
    fn replies_are_distinct_single_tokens() {
        let all = [
            RescueError::BadSlot,
            RescueError::BadLength,
            RescueError::OutOfRange,
            RescueError::Unaligned,
        ];
        for (i, a) in all.iter().enumerate() {
            assert!(a.reply().starts_with("ERR_") && !a.reply().contains(' '));
            for b in &all[i + 1..] {
                assert_ne!(a.reply(), b.reply());
            }
        }
    }

    // --- Erasing a slot, step by step -----------------------------------------------------------

    /// Runs the erase to the end and returns the ranges it cleared.
    fn erase_all<F: Flash>(flash: &mut F, e: &mut SlotErase) -> Vec<Range<u32>> {
        let mut spans = Vec::new();
        while let Some(r) = e.step(flash) {
            spans.push(r.unwrap());
        }
        spans
    }

    fn assert_exactly_erased(mem: &[u8], r: Range<u32>, fill: u8) {
        for (a, &b) in mem.iter().enumerate() {
            let inside = (r.start as usize..r.end as usize).contains(&a);
            assert_eq!(b, if inside { 0xFF } else { fill }, "byte {a:#x}");
        }
    }

    #[test]
    fn the_h750_erases_each_slot_in_ten_64k_blocks() {
        let l = &H750_NETLITE;
        for slot in 0..2u8 {
            let r = l.slot(slot).unwrap();
            let mut nor = RamNor::new(4 * 1024 * 1024);
            nor.mem.fill(0x00);
            let mut e = SlotErase::new(&nor, l, slot).unwrap();
            let spans = erase_all(&mut nor, &mut e);
            assert_eq!(spans.len(), 10, "slot {slot}");
            assert!(spans.iter().all(|s| s.end - s.start == 0x1_0000));
            assert!(
                spans.windows(2).all(|w| w[0].end == w[1].start),
                "contiguous"
            );
            assert_eq!((spans[0].start, spans[9].end), (r.offset, r.end()));
            assert_exactly_erased(&nor.mem, r.offset..r.end(), 0x00);
            assert!(e.step(&mut nor).is_none(), "finished stays finished");
        }
    }

    #[test]
    fn a_slot_on_4k_edges_but_not_64k_edges_is_erased_with_sectors_then_blocks() {
        // The old rule (every slot must be whole 64 KiB blocks) refused this; the rule is now
        // "whole erase units of the device", and the device still uses a block where it can.
        let mut l = H750_NETLITE;
        l.slots[0] = Region::new(0x1_1000, 0x9_F000); // ends at 0xB_0000
        let mut nor = RamNor::new(4 * 1024 * 1024);
        nor.mem.fill(0x00);
        let mut e = SlotErase::new(&nor, &l, 0).unwrap();
        let spans = erase_all(&mut nor, &mut e);
        let sectors = (0x1_1000u32..0x2_0000)
            .step_by(0x1000)
            .map(|a| a..a + 0x1000);
        let blocks = (0x2_0000u32..0xB_0000)
            .step_by(0x1_0000)
            .map(|a| a..a + 0x1_0000);
        let expected: Vec<Range<u32>> = sectors.chain(blocks).collect();
        assert_eq!(
            (spans.len(), expected.len()),
            (24, 24),
            "15 sectors, then 9 blocks"
        );
        assert_eq!(spans, expected);
        assert_exactly_erased(&nor.mem, 0x1_1000..0xB_0000, 0x00);
    }

    #[test]
    fn a_slot_that_is_not_whole_erase_units_is_refused_and_nothing_is_touched() {
        let l = &H750_NETLITE;
        let nor = RamNor::new(4 * 1024 * 1024);
        let with = |r: Region| {
            let mut l = *l;
            l.slots[0] = r;
            l
        };
        let cases = [
            (
                "ends inside a sector",
                Region::new(0x1_0000, 0xA_0000 - 0x800),
            ),
            ("starts inside a sector", Region::new(0x1_0800, 0xA_0000)),
            // starts inside a sector but ends exactly on a sector edge (0xB_0000)
            (
                "starts inside a sector, ends on one",
                Region::new(0x1_0800, 0x9_F800),
            ),
            ("empty", Region::new(0x1_0000, 0)),
            (
                "offset + length overflows",
                Region::new(0xFFFF_F000, 0x2000),
            ),
            (
                "past the end of the device",
                Region::new(0x40_0000, 0x1_0000),
            ),
        ];
        for (what, r) in cases {
            assert_eq!(
                SlotErase::new(&nor, &with(r), 0).err(),
                Some(RescueError::Unaligned),
                "{what}"
            );
        }
        assert_eq!(SlotErase::new(&nor, l, 2).err(), Some(RescueError::BadSlot));
        assert!(
            nor.mem.iter().all(|&b| b == 0xFF),
            "nothing was erased (or even programmed)"
        );
    }

    /// STM32F42x geometry; slot 1 is two 128 KiB sectors, slot 0 the 16K x4 + 64K sectors.
    fn f42x_layout() -> Layout {
        let mut l = H750_NETLITE;
        l.slots = [Region::new(0x0, 0x2_0000), Region::new(0xA_0000, 0x4_0000)];
        l
    }

    #[test]
    fn f42x_slots_are_erased_one_whole_sector_per_step() {
        let l = f42x_layout();
        let mut f = RamFlash::<4>::new(F42X_DUAL_BANK_2MB).dual_bank();
        f.bytes_mut().fill(0x00);
        // slot 1: two 128 KiB sectors
        let mut e = SlotErase::new(&f, &l, 1).unwrap();
        let spans = erase_all(&mut f, &mut e);
        assert_eq!(spans, vec![0xA_0000..0xC_0000, 0xC_0000..0xE_0000]);
        assert_exactly_erased(f.bytes(), 0xA_0000..0xE_0000, 0x00);
        // slot 0: uneven units, 16 KiB x 4 then 64 KiB, each in one step
        let mut e = SlotErase::new(&f, &l, 0).unwrap();
        let spans = erase_all(&mut f, &mut e);
        assert_eq!(
            spans,
            vec![
                0x0..0x4000,
                0x4000..0x8000,
                0x8000..0xC000,
                0xC000..0x1_0000,
                0x1_0000..0x2_0000
            ]
        );
        let m = f.bytes();
        assert!(m[..0x2_0000].iter().all(|&b| b == 0xFF));
        assert!(
            m[0x2_0000..0xA_0000].iter().all(|&b| b == 0x00),
            "between the slots untouched"
        );
    }

    #[test]
    fn the_h750_layout_on_a_device_with_128k_sectors_is_refused() {
        let f = RamFlash::<4>::new(F42X_DUAL_BANK_2MB);
        // slot 0 is 0x1_0000..0xB_0000: it starts on the 64K sector but ends inside a 128K one
        assert_eq!(
            SlotErase::new(&f, &H750_NETLITE, 0).err(),
            Some(RescueError::Unaligned)
        );
    }

    #[test]
    fn a_failed_step_ends_the_erase_and_the_rest_is_left_alone() {
        let l = f42x_layout();
        let mut f = RamFlash::<4>::new(F42X_DUAL_BANK_2MB);
        f.bytes_mut().fill(0x00);
        let mut e = SlotErase::new(&f, &l, 0).unwrap(); // five steps
        f.arm_power_cut(2, CutMode::Clean); // the third erase fails
        assert!(matches!(e.step(&mut f), Some(Ok(_))));
        assert!(matches!(e.step(&mut f), Some(Ok(_))));
        assert_eq!(e.step(&mut f), Some(Err(FlashError::PowerCut)));
        f.power_on();
        assert!(e.step(&mut f).is_none(), "no carrying on past a failure");
        assert_exactly_erased(f.bytes(), 0x0..0x8000, 0x00);
    }

    /// A `Flash` whose `erase_span` breaks the contract in a chosen way.
    struct Bad(fn(u32, u32) -> Range<u32>);
    impl Flash for Bad {
        const PROGRAM_ALIGN: u32 = 1;
        fn capacity(&self) -> u32 {
            0x10_0000
        }
        fn read(&mut self, _: u32, _: &mut [u8]) -> Result<(), FlashError> {
            Ok(())
        }
        fn program(&mut self, _: u32, _: &[u8]) -> Result<(), FlashError> {
            Ok(())
        }
        fn unit_at(&self, off: u32) -> Range<u32> {
            let s = off & !0xFFF;
            s..s + 0x1000
        }
        fn erase_unit(&mut self, _: u32) -> Result<(), FlashError> {
            Ok(())
        }
        fn erase_span(&mut self, off: u32, limit: u32) -> Result<Range<u32>, FlashError> {
            Ok((self.0)(off, limit))
        }
    }

    #[test]
    fn a_flash_that_breaks_the_span_contract_cannot_stall_or_overrun_the_erase() {
        let mut l = H750_NETLITE;
        l.slots[0] = Region::new(0x1_0000, 0x4000);
        let cases: [(&str, fn(u32, u32) -> Range<u32>); 4] = [
            ("empty span (would loop forever)", |o, _| o..o),
            ("backwards span", |o, _| o..o.saturating_sub(1)),
            ("span past the limit", |_, limit| 0x1_0000..limit + 0x1000),
            ("span that starts elsewhere", |o, _| o + 0x1000..o + 0x2000),
        ];
        for (what, f) in cases {
            let mut bad = Bad(f);
            let mut e = SlotErase::new(&bad, &l, 0).unwrap();
            assert_eq!(e.step(&mut bad), Some(Err(FlashError::IoError)), "{what}");
            assert!(e.step(&mut bad).is_none(), "{what}: stopped");
        }
        // A slot whose end wraps around 2^32 must not look like "nothing to erase": this device
        // has units everywhere, so only the overflow check can refuse it.
        let mut wraps = H750_NETLITE;
        wraps.slots[0] = Region::new(0xFFFF_F000, 0x2000);
        assert_eq!(
            SlotErase::new(&Bad(|o, _| o..o + 0x1000), &wraps, 0).err(),
            Some(RescueError::Unaligned)
        );
        // and a well-behaved one in the same harness finishes in the expected four steps
        let mut ok = Bad(|o, _| o..o + 0x1000);
        let mut e = SlotErase::new(&ok, &l, 0).unwrap();
        assert_eq!(erase_all(&mut ok, &mut e).len(), 4);
    }

    // --- The real SPI driver: what `e` puts on the bus -----------------------------------------

    thread_local! {
        /// Every chip-select frame as (first byte, 3-byte address if sent, bytes clocked).
        static FRAMES: RefCell<(Vec<(u8, u32, u32)>, bool)> = const { RefCell::new((Vec::new(), false)) };
    }
    unsafe extern "C" fn bus_cs(assert: bool) {
        if assert {
            FRAMES.with(|f| {
                let mut f = f.borrow_mut();
                f.0.push((0, 0, 0));
                f.1 = true;
            });
        }
    }
    unsafe extern "C" fn bus_spi(tx: *const u8, rx: *mut u8, len: u32) -> i32 {
        core::slice::from_raw_parts_mut(rx, len as usize).fill(0); // never busy
        FRAMES.with(|f| {
            let mut f = f.borrow_mut();
            let first = f.1;
            let fr = f.0.last_mut().unwrap();
            fr.2 += len;
            if first && !tx.is_null() {
                let t = core::slice::from_raw_parts(tx, len as usize);
                fr.0 = t[0];
                if t.len() >= 4 {
                    fr.1 = u32::from(t[1]) << 16 | u32::from(t[2]) << 8 | u32::from(t[3]);
                }
            }
            f.1 = false;
        });
        0
    }
    unsafe extern "C" fn bus_millis() -> u32 {
        0
    }
    unsafe extern "C" fn bus_yield() {}
    static BUS_IO: IoOps = IoOps {
        spi_transfer: Some(bus_spi),
        cs: Some(bus_cs),
        millis: bus_millis,
        yield_: bus_yield,
        reserved: [0; 4],
    };

    /// Recorded from the rescue `e` as it was before it moved to `Flash` (commit 1142777's
    /// `slot_erase_blocks` + `erase_block_64k`): `e 0` then `e 1` on the H750 layout, 60 chip-select
    /// frames, CRC-32 over (command, 3 address bytes big endian, clocked bytes u32 LE) per frame,
    /// computed outside this crate with Python `zlib.crc32`.
    const GOLDEN_E_FRAMES: usize = 60;
    const GOLDEN_E_FRAMES_CRC: u32 = 0x1240_3EC2;

    #[test]
    fn e_puts_the_same_commands_on_the_spi_bus_as_before_the_flash_trait() {
        let mut bytes = Vec::new();
        let mut erase_addrs = Vec::new();
        for slot in 0..2u8 {
            FRAMES.with(|f| *f.borrow_mut() = (Vec::new(), false));
            let mut nor = SpiNorIoOpsDriver::new(&BUS_IO);
            let mut e = SlotErase::new(&nor, &H750_NETLITE, slot).unwrap();
            while let Some(r) = e.step(&mut nor) {
                r.unwrap();
            }
            FRAMES.with(|f| {
                for &(cmd, addr, len) in &f.borrow().0 {
                    bytes.push(cmd);
                    bytes.extend_from_slice(&addr.to_be_bytes()[1..]);
                    bytes.extend_from_slice(&len.to_le_bytes());
                    if cmd == 0xD8 {
                        erase_addrs.push(addr);
                    }
                }
            });
        }
        assert_eq!(bytes.len() / 8, GOLDEN_E_FRAMES);
        assert_eq!(boot_crc_of(&bytes), GOLDEN_E_FRAMES_CRC);
        // by hand: twenty block erases (0xD8), ten per slot, 64 KiB apart, and no sector erase
        let expected: Vec<u32> = (0x1u32..=0xA).chain(0xB..=0x14).map(|n| n << 16).collect();
        assert_eq!(erase_addrs, expected);
        assert!(bytes.chunks(8).all(|f| f[0] != 0x20));
    }

    /// CRC-32 (IEEE) of `data`, bitwise, so this test does not depend on the code under test.
    fn boot_crc_of(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    // --- The `i` line ---------------------------------------------------------------------------

    fn info(layout: &Layout, cap: u32, align: u32, bl: u32) -> String {
        let mut b = LineBuf::<192>::new();
        write_info(&mut b, layout, cap, align, bl).unwrap();
        String::from(b.as_str())
    }

    /// The same literal is asserted by `tools/test_rescue_upload.py` against its parser.
    const H750_INFO: &str = "INFO proto=2 board=h750-netlite bl=1 flash=4194304 align=1 chunk=512 slot0=655360 slot1=655360 window=524288";

    #[test]
    fn the_h750_info_line() {
        assert_eq!(info(&H750_NETLITE, 4 * 1024 * 1024, 1, 1), H750_INFO);
        assert_eq!(PROTO_VERSION, 2);
    }

    #[test]
    fn info_reports_what_the_host_needs_to_pad_and_size() {
        let s = info(&f42x_layout(), 2 * 1024 * 1024, 4, 7);
        assert_eq!(
            s,
            "INFO proto=2 board=h750-netlite bl=7 flash=2097152 align=4 chunk=512 slot0=131072 slot1=262144 window=524288"
        );
        assert!(info(&H750_NETLITE, 4 << 20, 3, 1).contains(" align=3 chunk=510 "));
    }

    #[test]
    fn every_board_has_an_info_line_that_is_one_clean_line() {
        for l in ALL {
            let s = info(l, 4 << 20, 1, 1);
            assert!(
                !l.name.is_empty() && !l.name.contains(char::is_whitespace),
                "{}",
                l.name
            );
            assert!(
                s.is_ascii() && !s.contains('\n') && !s.contains('\r'),
                "{s}"
            );
            // all values are plain tokens; none is empty
            assert!(s.split(' ').skip(1).all(|kv| kv
                .split_once('=')
                .is_some_and(|(k, v)| !k.is_empty() && !v.is_empty())));
            assert!(
                s.len() < 160,
                "{} bytes does not fit the bootloader's 160-byte line buffer",
                s.len()
            );
        }
    }

    #[test]
    fn decimal_numbers_match_the_standard_formatter() {
        let mut values = vec![
            0u32,
            1,
            9,
            10,
            11,
            99,
            100,
            255,
            256,
            4095,
            4096,
            u32::MAX,
            u32::MAX - 1,
        ];
        let mut p = 1u32;
        while let Some(next) = p.checked_mul(10) {
            values.extend([p - 1, p, p + 1]);
            p = next;
        }
        for v in values {
            let mut b = LineBuf::<12>::new();
            put_u32(&mut b, v).unwrap();
            assert_eq!(b.as_str(), std::format!("{v}"), "{v}");
        }
        // a buffer that cannot hold the number reports it
        assert!(put_u32(&mut LineBuf::<3>::new(), 1000).is_err());
    }

    #[test]
    fn a_sink_that_cannot_take_the_line_makes_write_info_fail() {
        let mut small = LineBuf::<40>::new();
        assert!(write_info(&mut small, &H750_NETLITE, 4 << 20, 1, 1).is_err());
        let mut exact = LineBuf::<{ H750_INFO.len() }>::new();
        assert!(write_info(&mut exact, &H750_NETLITE, 4 << 20, 1, 1).is_ok());
        assert_eq!(exact.as_str(), H750_INFO);
    }
}
