//! Pure helpers for the UART rescue protocol.
//!
//! The command loop itself needs the UART and the NOR driver and lives in
//! `bootloader-h750`. Everything that decides *what is allowed* lives here so it can be
//! tested on the host: how the slot argument is read, whether a write stays inside its slot,
//! how much payload to discard after a rejected write, and which blocks "erase slot" covers.
#![no_std]

use boot_layout::Layout;

/// Largest payload of one `w` command.
pub const MAX_WRITE_CHUNK: u32 = 512;
/// Most payload bytes read and thrown away after a rejected `w`, so the host's data is not
/// interpreted as commands (it contains arbitrary bytes, including `e`, `w`, `m`, `r`).
pub const MAX_DRAIN: u32 = 4096;
/// NOR block erase unit used by the rescue `e` command.
pub const ERASE_BLOCK: u32 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescueError {
    /// Slot argument is not `0` or `1`.
    BadSlot,
    /// `w` length is 0 or larger than [`MAX_WRITE_CHUNK`].
    BadLength,
    /// `w` would write outside its slot.
    OutOfRange,
    /// The slot is not a whole number of [`ERASE_BLOCK`]s starting on a block boundary.
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

/// Absolute NOR address for a `w` of `len` bytes at slot-relative `offset`, or why it is
/// refused. The whole write must lie inside the slot.
pub fn write_address(layout: &Layout, slot: u8, offset: u32, len: u32) -> Result<u32, RescueError> {
    let region = layout.slot(slot).ok_or(RescueError::BadSlot)?;
    if len == 0 || len > MAX_WRITE_CHUNK {
        return Err(RescueError::BadLength);
    }
    let end = offset.checked_add(len).ok_or(RescueError::OutOfRange)?;
    if end > region.len {
        return Err(RescueError::OutOfRange);
    }
    Ok(region.offset + offset)
}

/// Payload bytes to read and discard after refusing a `w` that announced `len` bytes.
pub const fn drain_len(len: u32) -> u32 {
    if len > MAX_DRAIN {
        MAX_DRAIN
    } else {
        len
    }
}

/// Start addresses of the [`ERASE_BLOCK`]s that make up a slot, in ascending order.
#[derive(Debug, Clone, Copy)]
pub struct SlotErase {
    next: u32,
    end: u32,
}

impl Iterator for SlotErase {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        if self.next >= self.end {
            return None;
        }
        let addr = self.next;
        self.next += ERASE_BLOCK;
        Some(addr)
    }
}

/// Every erase block of `slot` (the whole slot, not just its first block).
pub fn slot_erase_blocks(layout: &Layout, slot: u8) -> Result<SlotErase, RescueError> {
    let region = layout.slot(slot).ok_or(RescueError::BadSlot)?;
    if region.len == 0 || region.offset % ERASE_BLOCK != 0 || region.len % ERASE_BLOCK != 0 {
        return Err(RescueError::Unaligned);
    }
    Ok(SlotErase { next: region.offset, end: region.offset + region.len })
}

#[cfg(test)]
mod tests {
    use super::*;
    use boot_layout::{Layout, Region, H750_NETLITE};

    #[test]
    fn slot_argument_is_strictly_zero_or_one() {
        assert_eq!(parse_slot(b'0'), Ok(0));
        assert_eq!(parse_slot(b'1'), Ok(1));
        for c in [b'2', b'a', b' ', b'\n', b'\r', 0, 0xFF, b'e'] {
            assert_eq!(parse_slot(c), Err(RescueError::BadSlot), "byte {c:#x}");
        }
    }

    #[test]
    fn write_inside_the_slot_maps_to_its_absolute_address() {
        let l = &H750_NETLITE;
        assert_eq!(write_address(l, 0, 0, 512), Ok(l.slots[0].offset));
        assert_eq!(write_address(l, 1, 0x200, 16), Ok(l.slots[1].offset + 0x200));
        // Last byte of the slot is writable, one more is not.
        let last = l.slots[1].len - 1;
        assert_eq!(write_address(l, 1, last, 1), Ok(l.slots[1].offset + last));
        assert_eq!(write_address(l, 1, last + 1, 1), Err(RescueError::OutOfRange));
        // A chunk that starts inside but ends past the slot is refused as a whole.
        assert_eq!(write_address(l, 0, l.slots[0].len - 100, 512), Err(RescueError::OutOfRange));
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
                let addr = write_address(l, slot, off, 1).unwrap();
                assert!(addr >= r.offset && addr < r.end(), "slot {slot} off {off:#x}");
                off += 0x1_0000;
            }
        }
        assert_eq!(write_address(l, 0, u32::MAX, 1), Err(RescueError::OutOfRange));
        assert_eq!(write_address(l, 0, u32::MAX - 100, 512), Err(RescueError::OutOfRange));
        assert_eq!(write_address(l, 2, 0, 1), Err(RescueError::BadSlot));
    }

    #[test]
    fn write_length_is_limited_and_never_silently_truncated() {
        let l = &H750_NETLITE;
        assert_eq!(write_address(l, 0, 0, 0), Err(RescueError::BadLength));
        assert_eq!(write_address(l, 0, 0, MAX_WRITE_CHUNK), Ok(l.slots[0].offset));
        assert_eq!(write_address(l, 0, 0, MAX_WRITE_CHUNK + 1), Err(RescueError::BadLength));
    }

    #[test]
    fn drained_payload_is_bounded() {
        assert_eq!(drain_len(0), 0);
        assert_eq!(drain_len(512), 512);
        assert_eq!(drain_len(MAX_DRAIN), MAX_DRAIN);
        assert_eq!(drain_len(u32::MAX), MAX_DRAIN);
    }

    #[test]
    fn erase_covers_exactly_the_whole_slot() {
        let l = &H750_NETLITE;
        for slot in 0..2u8 {
            let r = l.slot(slot).unwrap();
            let mut count = 0u32;
            let mut prev_end = r.offset;
            for addr in slot_erase_blocks(l, slot).unwrap() {
                assert_eq!(addr, prev_end, "blocks are contiguous");
                assert!(addr >= r.offset && addr + ERASE_BLOCK <= r.end(), "inside the slot");
                prev_end = addr + ERASE_BLOCK;
                count += 1;
            }
            assert_eq!(prev_end, r.end(), "reaches the end of the slot");
            assert_eq!(count, r.len / ERASE_BLOCK);
            // The old `e` erased one block; the slot has more than that.
            assert!(count > 1, "slot {slot}: {count} blocks");
        }
    }

    #[test]
    fn erase_refuses_a_slot_that_is_not_block_aligned() {
        let mut l = H750_NETLITE;
        l.slots[0] = Region::new(0x1_0000, 0xA_0000 - 0x1000);
        assert_eq!(slot_erase_blocks(&l, 0).err(), Some(RescueError::Unaligned));
        let mut l: Layout = H750_NETLITE;
        l.slots[1] = Region::new(0xB_0000 + 0x1000, 0xA_0000);
        assert_eq!(slot_erase_blocks(&l, 1).err(), Some(RescueError::Unaligned));
        assert_eq!(slot_erase_blocks(&H750_NETLITE, 2).err(), Some(RescueError::BadSlot));
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
}
