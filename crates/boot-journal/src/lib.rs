//! Flash Ping-Pong State Journal
//!
//! Stores firmware slot state transitions across two ping-pong sectors, each exactly one
//! erase unit of the flash device (4 KiB on the H750's SPI NOR), with power-cut tolerance
//! and snapshot rollover.

#![no_std]

use boot_abi::{ImageVersion, SLOT_STATE_EMPTY};
use boot_crc::Hasher;
use boot_flash::{Flash, FlashError};
use boot_layout::Layout;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub const JOURNAL_SECTOR_MAGIC: u32 = 0x4A4E_524C; // 'JRNL'
pub const RECORD_MAGIC: u16 = 0x5243; // 'RC'
pub const RECORD_SIZE: usize = 32;

pub const RECORD_KIND_STATE: u8 = 0;
pub const RECORD_KIND_SNAPSHOT: u8 = 1;
pub const RECORD_KIND_BL_UPDATE_PENDING: u8 = 2;
pub const RECORD_KIND_BL_UPDATE_DONE: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalError {
    /// The flash device reported an error (in a simulator: lost power).
    Flash(FlashError),
    Corrupted,
    NoValidSector,
    InvalidSlot,
    /// The layout does not fit this device: a journal region is not exactly one erase unit,
    /// the two regions differ in size or are the same one, or the device's program word does
    /// not divide the 16-byte header and 32-byte records.
    BadLayout,
}

impl From<FlashError> for JournalError {
    fn from(e: FlashError) -> Self {
        JournalError::Flash(e)
    }
}

/// Sector header stored at offset 0 of each journal sector.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct JournalSectorHeader {
    pub magic: u32,
    pub generation: u32,
    pub reserved: u32,
    pub crc32: u32,
}

impl JournalSectorHeader {
    pub fn new(generation: u32) -> Self {
        let mut hdr = Self {
            magic: JOURNAL_SECTOR_MAGIC,
            generation,
            reserved: 0xFFFF_FFFF,
            crc32: 0,
        };
        let mut hasher = Hasher::new();
        hasher.update(&hdr.magic.to_le_bytes());
        hasher.update(&hdr.generation.to_le_bytes());
        hasher.update(&hdr.reserved.to_le_bytes());
        hdr.crc32 = hasher.finalize();
        hdr
    }

    pub fn is_valid(&self) -> bool {
        if self.magic != JOURNAL_SECTOR_MAGIC {
            return false;
        }
        let mut hasher = Hasher::new();
        hasher.update(&self.magic.to_le_bytes());
        hasher.update(&self.generation.to_le_bytes());
        hasher.update(&self.reserved.to_le_bytes());
        self.crc32 == hasher.finalize()
    }
}

/// Journal record (exactly 32 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct JournalRecord {
    pub magic: u16,
    pub fmt_ver: u8,
    pub kind: u8,
    pub seq: u32,
    pub slot: u8,
    pub state: u8,
    pub trial: u8,
    pub flags: u8,
    pub img_ver: ImageVersion,
    pub min_version: u32,
    pub key_revoke_mask: u8,
    pub reserved: [u8; 3],
    pub crc32: u32,
}

const _: () = assert!(core::mem::size_of::<JournalRecord>() == RECORD_SIZE);

impl JournalRecord {
    pub fn new(
        kind: u8,
        seq: u32,
        slot: u8,
        state: u8,
        trial: u8,
        flags: u8,
        img_ver: ImageVersion,
        min_version: u32,
        key_revoke_mask: u8,
    ) -> Self {
        let mut rec = Self {
            magic: RECORD_MAGIC,
            fmt_ver: 1,
            kind,
            seq,
            slot,
            state,
            trial,
            flags,
            img_ver,
            min_version,
            key_revoke_mask,
            reserved: [0xFF; 3],
            crc32: 0,
        };
        rec.crc32 = rec.compute_crc();
        rec
    }

    fn compute_crc(&self) -> u32 {
        let mut hasher = Hasher::new();
        let bytes = self.as_bytes();
        hasher.update(&bytes[..28]);
        hasher.finalize()
    }

    pub fn is_valid(&self) -> bool {
        if self.magic != RECORD_MAGIC || self.fmt_ver != 1 {
            return false;
        }
        self.crc32 == self.compute_crc()
    }

    pub fn is_erased(&self) -> bool {
        self.as_bytes().iter().all(|&b| b == 0xFF)
    }
}

/// In-memory view of current slot state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotJournalState {
    pub state: u8,
    pub trial: u8,
    pub flags: u8,
    pub img_ver: ImageVersion,
    pub min_version: u32,
    pub key_revoke_mask: u8,
}

impl Default for SlotJournalState {
    fn default() -> Self {
        Self {
            state: SLOT_STATE_EMPTY,
            trial: 0,
            flags: 0,
            img_ver: ImageVersion::default(),
            min_version: 0,
            key_revoke_mask: 0,
        }
    }
}

pub struct JournalManager {
    /// The two ping-pong sector offsets, from `Layout::journal`.
    sectors: [u32; 2],
    /// Usable bytes per journal sector (`Layout::journal[..].len`).
    sector_len: u32,
    pub active_sector_addr: u32,
    pub generation: u32,
    pub next_seq: u32,
    pub next_record_offset: u32,
    pub slot_a: SlotJournalState,
    pub slot_b: SlotJournalState,
}

/// True if `off..off + len` is exactly one erase unit of `flash`.
fn is_one_erase_unit<F: Flash>(flash: &F, off: u32, len: u32) -> bool {
    match off.checked_add(len) {
        Some(end) => len > 0 && flash.unit_at(off) == (off..end),
        None => false,
    }
}

impl JournalManager {
    /// Loads or initializes the journal across the ping-pong sectors.
    pub fn init<F: Flash>(flash: &mut F, layout: &Layout) -> Result<Self, JournalError> {
        let sectors = [layout.journal[0].offset, layout.journal[1].offset];
        let sector_len = layout.journal[0].len;
        // A journal sector is erased and rolled over as a whole, so each region must be exactly
        // one erase unit of this device (whatever its size), the two must be the same size and
        // different, and the program word must divide the header and record sizes.
        if layout.journal[1].len != sector_len
            || sectors[0] == sectors[1]
            || !is_one_erase_unit(flash, sectors[0], sector_len)
            || !is_one_erase_unit(flash, sectors[1], sector_len)
            || 16 % F::PROGRAM_ALIGN != 0
            || RECORD_SIZE as u32 % F::PROGRAM_ALIGN != 0
        {
            return Err(JournalError::BadLayout);
        }

        let mut hdr0_buf = [0u8; 16];
        let mut hdr1_buf = [0u8; 16];

        flash.read(sectors[0], &mut hdr0_buf)?;
        flash.read(sectors[1], &mut hdr1_buf)?;

        let hdr0 = JournalSectorHeader::ref_from_prefix(&hdr0_buf).ok().map(|(h, _)| *h);
        let hdr1 = JournalSectorHeader::ref_from_prefix(&hdr1_buf).ok().map(|(h, _)| *h);

        let v0 = hdr0.is_some_and(|h| h.is_valid());
        let v1 = hdr1.is_some_and(|h| h.is_valid());

        let (active_addr, generation) = match (v0, v1) {
            (true, true) => {
                let g0 = hdr0.unwrap().generation;
                let g1 = hdr1.unwrap().generation;
                if g0 >= g1 {
                    (sectors[0], g0)
                } else {
                    (sectors[1], g1)
                }
            }
            (true, false) => (sectors[0], hdr0.unwrap().generation),
            (false, true) => (sectors[1], hdr1.unwrap().generation),
            (false, false) => {
                // Initialize clean journal on Sector 0
                flash.erase_unit(sectors[0])?;
                let new_hdr = JournalSectorHeader::new(1);
                flash.program(sectors[0], new_hdr.as_bytes())?;
                (sectors[0], 1)
            }
        };

        let mut mgr = Self {
            sectors,
            sector_len,
            active_sector_addr: active_addr,
            generation,
            next_seq: 1,
            next_record_offset: 16,
            slot_a: SlotJournalState::default(),
            slot_b: SlotJournalState::default(),
        };

        mgr.replay_active_sector(flash)?;
        Ok(mgr)
    }

    fn replay_active_sector<F: Flash>(&mut self, flash: &mut F) -> Result<(), JournalError> {
        let mut buf = [0u8; RECORD_SIZE];
        let mut offset = 16u32;

        while offset + (RECORD_SIZE as u32) <= self.sector_len {
            flash.read(self.active_sector_addr + offset, &mut buf)?;
            let (rec_ref, _) = match JournalRecord::ref_from_prefix(&buf) {
                Ok(r) => r,
                Err(_) => break,
            };

            if rec_ref.is_erased() {
                // Reached unused portion of sector
                self.next_record_offset = offset;
                return Ok(());
            }

            if rec_ref.is_valid() {
                let target_slot = match rec_ref.slot {
                    0 => &mut self.slot_a,
                    1 => &mut self.slot_b,
                    _ => &mut self.slot_a,
                };

                target_slot.state = rec_ref.state;
                target_slot.trial = rec_ref.trial;
                target_slot.flags = rec_ref.flags;
                target_slot.img_ver = rec_ref.img_ver;
                target_slot.min_version = rec_ref.min_version;
                target_slot.key_revoke_mask = rec_ref.key_revoke_mask;

                if rec_ref.seq >= self.next_seq {
                    self.next_seq = rec_ref.seq + 1;
                }
            }

            offset += RECORD_SIZE as u32;
        }

        self.next_record_offset = offset;
        Ok(())
    }

    /// Appends a new state record, triggering snapshot rollover if sector is full.
    pub fn append_record<F: Flash>(
        &mut self,
        flash: &mut F,
        slot: u8,
        state: u8,
        trial: u8,
        flags: u8,
        img_ver: ImageVersion,
        min_version: u32,
        key_revoke_mask: u8,
    ) -> Result<(), JournalError> {
        if slot > 1 {
            return Err(JournalError::InvalidSlot);
        }

        // Check if sector full
        if self.next_record_offset + (RECORD_SIZE as u32) > self.sector_len {
            self.rollover(flash)?;
        }

        let record = JournalRecord::new(
            RECORD_KIND_STATE,
            self.next_seq,
            slot,
            state,
            trial,
            flags,
            img_ver,
            min_version,
            key_revoke_mask,
        );

        flash.program(
            self.active_sector_addr + self.next_record_offset,
            record.as_bytes(),
        )?;

        // Update in-memory state
        let target = if slot == 0 {
            &mut self.slot_a
        } else {
            &mut self.slot_b
        };
        target.state = state;
        target.trial = trial;
        target.flags = flags;
        target.img_ver = img_ver;
        target.min_version = min_version;
        target.key_revoke_mask = key_revoke_mask;

        self.next_seq += 1;
        self.next_record_offset += RECORD_SIZE as u32;

        Ok(())
    }

    /// Compacts current state into the backup sector and erases old sector.
    fn rollover<F: Flash>(&mut self, flash: &mut F) -> Result<(), JournalError> {
        let other_addr = if self.active_sector_addr == self.sectors[0] {
            self.sectors[1]
        } else {
            self.sectors[0]
        };

        flash.erase_unit(other_addr)?;

        // The header goes in last. A sector only counts as part of the journal once its header
        // is valid, and `init` prefers the higher generation, so a header written before the
        // snapshots would make a power cut in between promote an empty sector over the old,
        // complete one and lose every slot state.
        let new_gen = self.generation + 1;
        let new_hdr = JournalSectorHeader::new(new_gen);

        // Write Slot A snapshot
        let rec_a = JournalRecord::new(
            RECORD_KIND_SNAPSHOT,
            self.next_seq,
            0,
            self.slot_a.state,
            self.slot_a.trial,
            self.slot_a.flags,
            self.slot_a.img_ver,
            self.slot_a.min_version,
            self.slot_a.key_revoke_mask,
        );
        flash.program(other_addr + 16, rec_a.as_bytes())?;

        // Write Slot B snapshot
        let rec_b = JournalRecord::new(
            RECORD_KIND_SNAPSHOT,
            self.next_seq + 1,
            1,
            self.slot_b.state,
            self.slot_b.trial,
            self.slot_b.flags,
            self.slot_b.img_ver,
            self.slot_b.min_version,
            self.slot_b.key_revoke_mask,
        );
        flash.program(other_addr + 16 + RECORD_SIZE as u32, rec_b.as_bytes())?;

        // Both snapshots are in; this makes the new sector valid.
        flash.program(other_addr, new_hdr.as_bytes())?;

        // Verify written snapshot header before erasing old sector
        let mut verify_buf = [0u8; 16];
        flash.read(other_addr, &mut verify_buf)?;
        let (v_hdr, _) = JournalSectorHeader::ref_from_prefix(&verify_buf)
            .map_err(|_| JournalError::Corrupted)?;
        if !v_hdr.is_valid() {
            return Err(JournalError::Corrupted);
        }

        // Erase old sector
        flash.erase_unit(self.active_sector_addr)?;

        self.active_sector_addr = other_addr;
        self.generation = new_gen;
        self.next_seq += 2;
        self.next_record_offset = 16 + (2 * RECORD_SIZE as u32);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boot_abi::*;
    use boot_layout::H750_NETLITE;
    use boot_nor::RamNor;

    #[test]
    fn test_journal_init_and_state_transitions() {
        let mut nor = RamNor::new(64 * 1024);
        let mut mgr = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();

        assert_eq!(mgr.slot_a.state, SLOT_STATE_EMPTY);
        assert_eq!(mgr.slot_b.state, SLOT_STATE_EMPTY);

        let v1 = ImageVersion {
            major: 1,
            minor: 0,
            revision: 0,
            build_num: 1,
        };

        // Write Slot A Written
        mgr.append_record(&mut nor, 0, SLOT_STATE_WRITTEN, 0, 0, v1, 1, 0)
            .unwrap();
        // Mark Pending
        mgr.append_record(&mut nor, 0, SLOT_STATE_PENDING, 0, 0, v1, 1, 0)
            .unwrap();
        // Boot testing
        mgr.append_record(&mut nor, 0, SLOT_STATE_TESTING, 1, 0, v1, 1, 0)
            .unwrap();
        // Confirm
        mgr.append_record(&mut nor, 0, SLOT_STATE_CONFIRMED, 1, 0, v1, 1, 0)
            .unwrap();

        assert_eq!(mgr.slot_a.state, SLOT_STATE_CONFIRMED);
        assert_eq!(mgr.slot_a.trial, 1);

        // Re-read journal from NOR to simulate reboot
        let reloaded = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();
        assert_eq!(reloaded.slot_a.state, SLOT_STATE_CONFIRMED);
        assert_eq!(reloaded.slot_a.trial, 1);
        assert_eq!(reloaded.slot_a.img_ver.major, 1);
    }

    #[test]
    fn test_journal_rollover() {
        let mut nor = RamNor::new(64 * 1024);
        let mut mgr = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();

        let v = ImageVersion::default();
        // Append records until rollover occurs
        for i in 0..150 {
            mgr.append_record(&mut nor, 0, (i % 5) as u8, 0, 0, v, 0, 0)
                .unwrap();
        }

        assert!(mgr.generation > 1);

        // Verify that after reboot, the state is accurately recovered from the new active sector
        let reloaded = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();
        assert_eq!(reloaded.generation, mgr.generation);
        assert_eq!(reloaded.slot_a.state, mgr.slot_a.state);
    }

    // -----------------------------------------------------------------------
    // Characterization tests (P0): pin the on-flash format and the recovery
    // behaviour so later refactors (Layout, Flash trait, ...) cannot change them
    // silently. Golden bytes were computed independently with Python's zlib.
    // -----------------------------------------------------------------------

    /// JournalRecord::new(STATE, seq 7, slot B, PENDING, trial 0, flags 0,
    /// v1.2.0x0304+0x0A0B0C0D, min_version 5, revoke mask 0x03)
    const GOLDEN_RECORD: [u8; 32] = [
        0x43, 0x52, 0x01, 0x00, 0x07, 0x00, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00, 0x01, 0x02, 0x04,
        0x03, 0x0D, 0x0C, 0x0B, 0x0A, 0x05, 0x00, 0x00, 0x00, 0x03, 0xFF, 0xFF, 0xFF, 0x16, 0x0B,
        0xD0, 0x08,
    ];

    /// JournalSectorHeader::new(generation 5)
    const GOLDEN_HEADER: [u8; 16] = [
        0x4C, 0x52, 0x4E, 0x4A, 0x05, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xF6, 0xC8, 0xFA,
        0x33,
    ];

    fn golden_inputs() -> JournalRecord {
        JournalRecord::new(
            RECORD_KIND_STATE,
            7,
            1,
            SLOT_STATE_PENDING,
            0,
            0,
            ImageVersion { major: 1, minor: 2, revision: 0x0304, build_num: 0x0A0B_0C0D },
            5,
            0x03,
        )
    }

    #[test]
    fn golden_record_bytes_are_stable() {
        let rec = golden_inputs();
        assert_eq!(rec.as_bytes(), &GOLDEN_RECORD[..]);
        assert!(rec.is_valid());
    }

    #[test]
    fn golden_sector_header_bytes_are_stable() {
        let hdr = JournalSectorHeader::new(5);
        assert_eq!(hdr.as_bytes(), &GOLDEN_HEADER[..]);
        assert!(hdr.is_valid());
    }

    #[test]
    fn any_single_bit_flip_invalidates_a_record() {
        for byte in 0..RECORD_SIZE {
            for bit in 0..8 {
                let mut raw = GOLDEN_RECORD;
                raw[byte] ^= 1 << bit;
                let rec = JournalRecord::read_from_bytes(&raw).unwrap();
                assert!(!rec.is_valid(), "byte {byte} bit {bit} went unnoticed");
            }
        }
    }

    #[test]
    fn any_single_bit_flip_invalidates_a_sector_header() {
        for byte in 0..16 {
            for bit in 0..8 {
                let mut raw = GOLDEN_HEADER;
                raw[byte] ^= 1 << bit;
                let hdr = JournalSectorHeader::read_from_bytes(&raw).unwrap();
                assert!(!hdr.is_valid(), "byte {byte} bit {bit} went unnoticed");
            }
        }
    }

    fn v1() -> ImageVersion {
        ImageVersion { major: 1, minor: 0, revision: 0, build_num: 1 }
    }

    #[test]
    fn power_cut_during_append_is_ignored_and_journal_stays_usable() {
        let mut nor = RamNor::new(64 * 1024);
        let mut mgr = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();
        mgr.append_record(&mut nor, 0, SLOT_STATE_WRITTEN, 0, 0, v1(), 1, 0).unwrap();

        // Cut power part-way through programming the next record.
        nor.fail_after_bytes = Some(nor.bytes_written + 20);
        let res = mgr.append_record(&mut nor, 0, SLOT_STATE_PENDING, 0, 0, v1(), 1, 0);
        assert!(res.is_err());

        // "Reboot": power is back, the half-written record must be skipped.
        nor.fail_after_bytes = None;
        let mut re = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();
        assert_eq!(re.slot_a.state, SLOT_STATE_WRITTEN, "torn record must not take effect");
        assert_eq!(re.next_record_offset, 16 + 2 * RECORD_SIZE as u32, "torn slot is skipped, not reused");

        // And the journal keeps working afterwards.
        re.append_record(&mut nor, 0, SLOT_STATE_PENDING, 0, 0, v1(), 1, 0).unwrap();
        let again = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();
        assert_eq!(again.slot_a.state, SLOT_STATE_PENDING);
    }

    #[test]
    fn corrupted_record_crc_falls_back_to_previous_state() {
        let mut nor = RamNor::new(64 * 1024);
        let mut mgr = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();
        mgr.append_record(&mut nor, 0, SLOT_STATE_CONFIRMED, 0, 0, v1(), 1, 0).unwrap();
        mgr.append_record(&mut nor, 0, SLOT_STATE_TESTING, 1, 0, v1(), 1, 0).unwrap();

        // Flash can only clear bits: zero a CRC byte of the second record.
        let crc_byte = H750_NETLITE.journal[0].offset + 16 + RECORD_SIZE as u32 + 28;
        nor.program(crc_byte, &[0x00]).unwrap();

        let re = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();
        assert_eq!(re.slot_a.state, SLOT_STATE_CONFIRMED);
    }

    #[test]
    fn slots_are_tracked_independently() {
        let mut nor = RamNor::new(64 * 1024);
        let mut mgr = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();
        let v2 = ImageVersion { major: 2, minor: 0, revision: 0, build_num: 7 };
        mgr.append_record(&mut nor, 0, SLOT_STATE_CONFIRMED, 0, 0, v1(), 1, 0).unwrap();
        mgr.append_record(&mut nor, 1, SLOT_STATE_PENDING, 0, 0, v2, 2, 0x01).unwrap();

        let re = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();
        assert_eq!(re.slot_a.state, SLOT_STATE_CONFIRMED);
        assert_eq!(re.slot_a.img_ver, v1());
        assert_eq!(re.slot_b.state, SLOT_STATE_PENDING);
        assert_eq!(re.slot_b.img_ver, v2);
        assert_eq!(re.slot_b.min_version, 2);
        assert_eq!(re.slot_b.key_revoke_mask, 0x01);
    }

    #[test]
    fn invalid_slot_number_is_rejected() {
        let mut nor = RamNor::new(64 * 1024);
        let mut mgr = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();
        let res = mgr.append_record(&mut nor, 2, SLOT_STATE_PENDING, 0, 0, v1(), 1, 0);
        assert!(matches!(res, Err(JournalError::InvalidSlot)));
    }

    #[test]
    fn layout_with_non_4k_journal_regions_is_rejected() {
        let mut nor = RamNor::new(64 * 1024);
        let mut bad = H750_NETLITE;
        bad.journal[0].len = 8192;
        assert!(matches!(JournalManager::init(&mut nor, &bad), Err(JournalError::BadLayout)));
        let mut bad = H750_NETLITE;
        bad.journal[1].len = 2048;
        assert!(matches!(JournalManager::init(&mut nor, &bad), Err(JournalError::BadLayout)));
    }

    #[test]
    fn journal_follows_the_layouts_sector_addresses() {
        // Move the journal somewhere else: the data must land there and not at the H750 offsets.
        let mut nor = RamNor::new(64 * 1024);
        let mut moved = H750_NETLITE;
        moved.journal = [
            boot_layout::Region::new(0x4000, 4096),
            boot_layout::Region::new(0x5000, 4096),
        ];
        let mut mgr = JournalManager::init(&mut nor, &moved).unwrap();
        mgr.append_record(&mut nor, 0, SLOT_STATE_CONFIRMED, 0, 0, v1(), 1, 0).unwrap();
        assert_eq!(mgr.active_sector_addr, 0x4000);
        let mut at_h750 = [0u8; 16];
        nor.read(H750_NETLITE.journal[0].offset, &mut at_h750).unwrap();
        assert!(at_h750.iter().all(|&b| b == 0xFF), "nothing written at the old location");
        let re = JournalManager::init(&mut nor, &moved).unwrap();
        assert_eq!(re.slot_a.state, SLOT_STATE_CONFIRMED);
    }

    // --- Flash-trait coverage: the same journal on other devices and geometries ---------------

    use boot_flash::{Flash, RamFlash, F42X_DUAL_BANK_2MB, UNIFORM_4K_4MB};
    use boot_layout::Region;

    /// 300 appends with every parameter varying; crosses two rollovers on a 4 KiB journal.
    fn golden_sequence<F: Flash>(flash: &mut F) -> JournalManager {
        let mut mgr = JournalManager::init(flash, &H750_NETLITE).unwrap();
        for i in 0..300u32 {
            let v = ImageVersion {
                major: 1,
                minor: (i / 50) as u8,
                revision: (i % 7) as u16,
                build_num: i,
            };
            let (slot, state, trial, flags) =
                ((i % 2) as u8, (i % 6) as u8, (i % 4) as u8, (i % 3) as u8);
            mgr.append_record(flash, slot, state, trial, flags, v, i * 3, (i % 8) as u8)
                .unwrap();
        }
        mgr
    }

    /// CRC-32 values taken from the journal code as it was before it moved from `NorFlash` to
    /// `Flash` (commit 9dec531), over the two 4 KiB sectors after `golden_sequence`; computed
    /// outside this crate with Python `zlib.crc32` over a dump of the flash.
    const GOLDEN_BOTH_SECTORS_CRC: u32 = 0x8D11_EACB;
    const GOLDEN_SECTOR0_CRC: u32 = 0x6B95_A23E;
    const GOLDEN_SECTOR1_CRC: u32 = 0xF154_670A; // erased by the last rollover

    fn assert_golden(image: &[u8], mgr: &JournalManager) {
        assert_eq!(
            boot_crc::checksum(&image[..4096]),
            GOLDEN_SECTOR0_CRC,
            "sector 0"
        );
        assert_eq!(
            boot_crc::checksum(&image[4096..8192]),
            GOLDEN_SECTOR1_CRC,
            "sector 1"
        );
        assert_eq!(
            boot_crc::checksum(&image[..8192]),
            GOLDEN_BOTH_SECTORS_CRC,
            "both sectors"
        );
        assert_eq!(
            (
                mgr.generation,
                mgr.next_seq,
                mgr.next_record_offset,
                mgr.active_sector_addr
            ),
            (3, 305, 1616, 0)
        );
    }

    #[test]
    fn journal_writes_the_same_bytes_as_before_the_flash_trait() {
        let mut nor = RamNor::new(64 * 1024);
        let mgr = golden_sequence(&mut nor);
        assert_golden(&nor.mem, &mgr);
    }

    #[test]
    fn journal_writes_the_same_bytes_on_ramflash_with_any_program_word() {
        // byte-programmable, and 4-byte words as on the STM32F4: the format is the same
        let mut f1 = RamFlash::<1>::new(UNIFORM_4K_4MB);
        let m1 = golden_sequence(&mut f1);
        assert_golden(f1.bytes(), &m1);
        let mut f4 = RamFlash::<4>::new(UNIFORM_4K_4MB);
        let m4 = golden_sequence(&mut f4);
        assert_golden(f4.bytes(), &m4);
    }

    /// A layout whose journal sits in the 128 KiB sectors of an STM32F42x.
    fn f42x_journal_layout() -> Layout {
        let mut l = H750_NETLITE;
        l.journal = [
            Region::new(0x2_0000, 0x2_0000),
            Region::new(0x4_0000, 0x2_0000),
        ];
        l
    }

    #[test]
    fn journal_fills_a_128k_sector_then_rolls_over_and_keeps_its_state() {
        let layout = f42x_journal_layout();
        let mut flash = RamFlash::<4>::new(F42X_DUAL_BANK_2MB).dual_bank();
        let mut mgr = JournalManager::init(&mut flash, &layout).unwrap();
        assert_eq!((mgr.generation, mgr.active_sector_addr), (1, 0x2_0000));
        // (131072 - 16-byte header) / 32-byte records = 4095 records fit
        for i in 0..4095u32 {
            let v = ImageVersion {
                major: 1,
                minor: 0,
                revision: 0,
                build_num: i,
            };
            mgr.append_record(&mut flash, 0, SLOT_STATE_PENDING, 0, 0, v, 0, 0)
                .unwrap();
        }
        assert_eq!(mgr.next_record_offset, 16 + 4095 * 32);
        assert_eq!(mgr.generation, 1, "not rolled over yet");
        // slot B gets a distinct state so the snapshot has to carry both
        mgr.append_record(&mut flash, 1, SLOT_STATE_CONFIRMED, 0, 0, v1(), 7, 0)
            .unwrap();
        assert_eq!(mgr.generation, 2, "the 4096th record rolls over");
        assert_eq!(mgr.active_sector_addr, 0x4_0000);
        // header, two snapshots, then the record that triggered the rollover
        assert_eq!(mgr.next_record_offset, 16 + 3 * 32);
        // the old 128 KiB sector was erased as one unit
        assert!(flash.bytes()[0x2_0000..0x4_0000].iter().all(|&b| b == 0xFF));

        let re = JournalManager::init(&mut flash, &layout).unwrap();
        assert_eq!(re.generation, 2);
        assert_eq!(re.slot_a.state, SLOT_STATE_PENDING);
        assert_eq!(re.slot_a.img_ver.build_num, 4094);
        assert_eq!(re.slot_b.state, SLOT_STATE_CONFIRMED);
        assert_eq!(re.slot_b.min_version, 7);
        // nothing outside the two journal sectors was touched
        let touched = |a: usize| (0x2_0000..0x6_0000).contains(&a);
        assert!(flash
            .bytes()
            .iter()
            .enumerate()
            .all(|(a, &b)| touched(a) || b == 0xFF));
    }

    #[test]
    fn replay_reads_the_whole_sector_not_just_its_first_4k() {
        // 4 KiB holds 127 records; a 128 KiB sector holds thousands. After a reboot the state
        // has to come from the last record, wherever it is.
        let layout = f42x_journal_layout();
        let mut flash = RamFlash::<4>::new(F42X_DUAL_BANK_2MB).dual_bank();
        let mut mgr = JournalManager::init(&mut flash, &layout).unwrap();
        for i in 0..300u32 {
            let v = ImageVersion {
                major: 1,
                minor: 0,
                revision: 0,
                build_num: i,
            };
            let slot = (i % 2) as u8;
            mgr.append_record(
                &mut flash,
                slot,
                SLOT_STATE_TESTING,
                (i % 3) as u8,
                0,
                v,
                i,
                0,
            )
            .unwrap();
        }
        assert_eq!(mgr.generation, 1);
        let re = JournalManager::init(&mut flash, &layout).unwrap();
        assert_eq!(re.generation, 1);
        assert_eq!(re.next_record_offset, 16 + 300 * 32);
        assert_eq!(re.next_seq, 301);
        assert_eq!(
            (re.slot_a.img_ver.build_num, re.slot_a.min_version),
            (298, 298)
        );
        assert_eq!(
            (re.slot_b.img_ver.build_num, re.slot_b.min_version),
            (299, 299)
        );
        assert_eq!(
            (re.slot_a.trial, re.slot_b.trial),
            ((298 % 3) as u8, (299 % 3) as u8)
        );
    }

    #[test]
    fn layouts_that_are_not_whole_erase_units_of_the_device_are_rejected() {
        let bad = |regions: [Region; 2]| {
            let mut l = H750_NETLITE;
            l.journal = regions;
            l
        };
        // On the F42x (units: 16K x4, 64K, 128K x7 per bank).
        let mut f = RamFlash::<4>::new(F42X_DUAL_BANK_2MB);
        let cases = [
            (
                "64K region inside a 128K sector",
                [
                    Region::new(0x2_0000, 0x1_0000),
                    Region::new(0x4_0000, 0x1_0000),
                ],
            ),
            (
                "128K region not on a sector start",
                [
                    Region::new(0x2_4000, 0x2_0000),
                    Region::new(0x4_0000, 0x2_0000),
                ],
            ),
            (
                "the second one not on a sector start",
                [
                    Region::new(0x2_0000, 0x2_0000),
                    Region::new(0x4_4000, 0x2_0000),
                ],
            ),
            (
                "two sizes, the second not a unit",
                [
                    Region::new(0x2_0000, 0x2_0000),
                    Region::new(0x4_0000, 0x1_0000),
                ],
            ),
            // each one is a whole unit (the 64K sector, a 128K sector) but they differ in size
            (
                "two sizes, both whole units",
                [
                    Region::new(0x1_0000, 0x1_0000),
                    Region::new(0x2_0000, 0x2_0000),
                ],
            ),
            (
                "two regions that span two units",
                [Region::new(0x0, 0x8000), Region::new(0x8000, 0x8000)],
            ),
            (
                "the same region twice",
                [
                    Region::new(0x2_0000, 0x2_0000),
                    Region::new(0x2_0000, 0x2_0000),
                ],
            ),
            (
                "offset + len overflows",
                [
                    Region::new(0xFFFF_F000, 0x2000),
                    Region::new(0x4_0000, 0x2000),
                ],
            ),
            (
                "past the end of the device",
                [
                    Region::new(0x20_0000, 0x2_0000),
                    Region::new(0x22_0000, 0x2_0000),
                ],
            ),
            (
                "empty regions",
                [Region::new(0x2_0000, 0), Region::new(0x4_0000, 0)],
            ),
        ];
        for (what, regions) in cases {
            let r = JournalManager::init(&mut f, &bad(regions));
            assert!(
                matches!(r, Err(JournalError::BadLayout)),
                "{what}: {:?}",
                r.as_ref().err()
            );
        }
        // and a rejected layout writes nothing
        assert!(f.bytes().iter().all(|&b| b == 0xFF));
        // a 4 KiB region on a 4 KiB device is fine, on a 128 KiB-sector device it is not
        let l4k = bad([Region::new(0x4000, 4096), Region::new(0x5000, 4096)]);
        assert!(JournalManager::init(&mut RamFlash::<1>::new(UNIFORM_4K_4MB), &l4k).is_ok());
        assert!(matches!(
            JournalManager::init(&mut f, &l4k),
            Err(JournalError::BadLayout)
        ));
    }

    #[test]
    fn a_device_whose_program_word_does_not_divide_the_record_format_is_rejected() {
        // 16-byte header and 32-byte records: any word up to 16 bytes that divides 16 works,
        // a 32-byte word (some STM32H7 internal flash) does not.
        for align in [1u32, 2, 4, 8, 16] {
            let ok =
                match align {
                    1 => JournalManager::init(&mut RamFlash::<1>::new(&[(4096, 4)]), &moved_4k())
                        .is_ok(),
                    2 => JournalManager::init(&mut RamFlash::<2>::new(&[(4096, 4)]), &moved_4k())
                        .is_ok(),
                    4 => JournalManager::init(&mut RamFlash::<4>::new(&[(4096, 4)]), &moved_4k())
                        .is_ok(),
                    8 => JournalManager::init(&mut RamFlash::<8>::new(&[(4096, 4)]), &moved_4k())
                        .is_ok(),
                    _ => JournalManager::init(&mut RamFlash::<16>::new(&[(4096, 4)]), &moved_4k())
                        .is_ok(),
                };
            assert!(ok, "program word {align}");
        }
        let mut f32 = RamFlash::<32>::new(&[(4096, 4)]);
        assert!(matches!(
            JournalManager::init(&mut f32, &moved_4k()),
            Err(JournalError::BadLayout)
        ));
        assert!(
            f32.bytes().iter().all(|&b| b == 0xFF),
            "rejected before touching the device"
        );
        let mut f3 = RamFlash::<3>::new(&[(4098, 4)]); // 3 does not divide 16 either
        assert!(matches!(
            JournalManager::init(&mut f3, &moved_4k()),
            Err(JournalError::BadLayout)
        ));
    }

    fn moved_4k() -> Layout {
        let mut l = H750_NETLITE;
        l.journal = [Region::new(0, 4096), Region::new(4096, 4096)];
        l
    }

    #[test]
    fn flash_errors_surface_as_journal_flash_errors() {
        use boot_flash::{CutMode, FlashError};
        let mut f = RamFlash::<1>::new(UNIFORM_4K_4MB);
        f.arm_power_cut(0, CutMode::Clean);
        let r = JournalManager::init(&mut f, &H750_NETLITE);
        assert!(
            matches!(r, Err(JournalError::Flash(FlashError::PowerCut))),
            "{:?}",
            r.as_ref().err()
        );
    }

    // --- The real SPI driver: what the journal puts on the bus ----------------------------------

    extern crate std;
    use boot_abi::IoOps;
    use boot_nor::SpiNorIoOpsDriver;
    use core::cell::RefCell;
    use std::thread_local;
    use std::vec::Vec;

    /// A tiny SPI NOR: 8 KiB of memory behind page program (0x02, bits only clear), sector erase
    /// (0x20), fast read (0x0B) and the status register (never busy).
    struct Chip {
        mem: [u8; 8192],
        frames: Vec<(u8, u32, u32)>,
        first: bool,
        cmd: u8,
        addr: u32,
    }

    thread_local! {
        static CHIP: RefCell<Chip> = const {
            RefCell::new(Chip { mem: [0xFF; 8192], frames: Vec::new(), first: false, cmd: 0, addr: 0 })
        };
    }

    unsafe extern "C" fn bus_cs(assert: bool) {
        if assert {
            CHIP.with(|c| {
                let mut c = c.borrow_mut();
                c.frames.push((0, 0, 0));
                c.first = true;
            });
        }
    }

    unsafe extern "C" fn bus_spi(tx: *const u8, rx: *mut u8, len: u32) -> i32 {
        let rx = core::slice::from_raw_parts_mut(rx, len as usize);
        rx.fill(0);
        CHIP.with(|c| {
            let mut c = c.borrow_mut();
            let first = c.first;
            c.first = false;
            c.frames.last_mut().unwrap().2 += len;
            if first {
                let t = core::slice::from_raw_parts(tx, len as usize);
                let addr = if t.len() >= 4 {
                    u32::from(t[1]) << 16 | u32::from(t[2]) << 8 | u32::from(t[3])
                } else {
                    0
                };
                c.cmd = t[0];
                c.addr = addr;
                *c.frames.last_mut().unwrap() = (t[0], addr, len);
                if t[0] == 0x20 {
                    let s = (addr & !0xFFF) as usize;
                    c.mem[s..s + 4096].fill(0xFF);
                }
            } else if c.cmd == 0x02 {
                let t = core::slice::from_raw_parts(tx, len as usize);
                let a = c.addr as usize;
                for (i, &b) in t.iter().enumerate() {
                    c.mem[a + i] &= b;
                }
            } else if c.cmd == 0x0B {
                let a = c.addr as usize;
                rx.copy_from_slice(&c.mem[a..a + len as usize]);
            }
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

    /// Serialises the frames: command, 3 address bytes (big endian), clocked bytes (u32 LE).
    fn frames_as_bytes() -> Vec<u8> {
        CHIP.with(|c| {
            let mut out = Vec::new();
            for &(cmd, addr, len) in &c.borrow().frames {
                out.push(cmd);
                out.extend_from_slice(&addr.to_be_bytes()[1..]);
                out.extend_from_slice(&len.to_le_bytes());
            }
            out
        })
    }

    /// A blank chip: init (erase sector 0, write a header), 127 appends that fill the sector, one
    /// more that rolls over (erase the other sector, header, two snapshots, read back, erase the
    /// old one), and a final append.
    fn bus_scenario() -> (Vec<u8>, Vec<u8>) {
        CHIP.with(|c| {
            let mut c = c.borrow_mut();
            c.mem = [0xFF; 8192];
            c.frames.clear();
        });
        let mut driver = SpiNorIoOpsDriver::new(&BUS_IO);
        let mut mgr = JournalManager::init(&mut driver, &H750_NETLITE).unwrap();
        for i in 0..128u32 {
            let v = ImageVersion {
                major: 1,
                minor: 0,
                revision: 0,
                build_num: i,
            };
            mgr.append_record(
                &mut driver,
                (i % 2) as u8,
                SLOT_STATE_PENDING,
                0,
                0,
                v,
                i,
                0,
            )
            .unwrap();
        }
        mgr.append_record(&mut driver, 1, SLOT_STATE_CONFIRMED, 0, 0, v1(), 1, 0)
            .unwrap();
        assert_eq!(mgr.generation, 2, "the scenario must include a rollover");
        let mem = CHIP.with(|c| c.borrow().mem.to_vec());
        (frames_as_bytes(), mem)
    }

    // Recorded from the journal as it was before it moved to `Flash` (commit 9dec531) over the
    // same scenario; CRC-32s computed outside this crate with Python `zlib.crc32`. The chip
    // contents are the old ones unchanged. The frame CRC is the old frame sequence with the
    // rollover's three programs permuted from header, A, B to A, B, header (done in Python on
    // the recorded frames, not by running this code): same frames, header last.
    const GOLDEN_BUS_FRAMES: usize = 457;
    const GOLDEN_BUS_FRAMES_CRC: u32 = 0x005D_9A4E;
    const GOLDEN_BUS_CHIP_CRC: u32 = 0x861D_02C6;

    #[test]
    fn journal_puts_the_same_commands_on_the_spi_bus_as_before_the_flash_trait() {
        let (frames, chip) = bus_scenario();
        assert_eq!(frames.len() / 8, GOLDEN_BUS_FRAMES);
        assert_eq!(
            boot_crc::checksum(&frames),
            GOLDEN_BUS_FRAMES_CRC,
            "command/address/length sequence"
        );
        assert_eq!(
            boot_crc::checksum(&chip),
            GOLDEN_BUS_CHIP_CRC,
            "final chip contents"
        );
        // and, written out by hand: the sector erases are init (sector 0), then the rollover
        // erases the other sector first and the old one last
        let erases: Vec<u32> = frames
            .chunks(8)
            .filter(|f| f[0] == 0x20)
            .map(|f| u32::from_be_bytes([0, f[1], f[2], f[3]]))
            .collect();
        assert_eq!(erases, [0x0000, 0x1000, 0x0000]);
        // The rollover then programs the two snapshots and, last, the header that makes the new
        // sector valid.
        let after_erase = frames.chunks(8).skip_while(|f| f[0] != 0x20 || f[1..4] != [0, 0x10, 0]);
        let programs: Vec<u32> = after_erase
            .filter(|f| f[0] == 0x02)
            .take(3)
            .map(|f| u32::from_be_bytes([0, f[1], f[2], f[3]]))
            .collect();
        assert_eq!(programs, [0x1010, 0x1030, 0x1000]);
    }

    // --- Power-cut sweep ---------------------------------------------------------------------
    //
    // For every step an operation takes (each programmed word, each erase), cut the power there,
    // reboot, and check what the journal says. Whatever the cut point, the journal must open,
    // the slot states must be either exactly what they were before the operation or exactly what
    // they are after it (never a mix, never lost), and it must keep working.

    use boot_flash::CutMode;

    type States = (SlotJournalState, SlotJournalState);

    fn states(m: &JournalManager) -> States {
        (m.slot_a, m.slot_b)
    }

    /// Runs `op` on a copy of `base` with the power cut at every step it takes, in both cut modes,
    /// and calls `check(after_reboot, cut_step, mode)` each time. Returns the number of steps.
    fn sweep<const A: u32>(
        base: &RamFlash<A>,
        layout: &Layout,
        op: impl Fn(&mut RamFlash<A>, &mut JournalManager) -> Result<(), JournalError>,
        check: impl Fn(&mut RamFlash<A>, JournalManager, u64, CutMode),
    ) -> u64 {
        // How many steps does the operation take when nothing goes wrong?
        let mut probe = base.clone();
        let mut mgr = JournalManager::init(&mut probe, layout).unwrap();
        let before = probe.steps();
        op(&mut probe, &mut mgr).unwrap();
        let total = probe.steps() - before;
        assert!(total > 0);
        for mode in [CutMode::Clean, CutMode::Torn] {
            for k in 0..total {
                let mut f = base.clone();
                let mut mgr = JournalManager::init(&mut f, layout).unwrap();
                f.arm_power_cut(f.steps() + k, mode);
                assert!(
                    op(&mut f, &mut mgr).is_err(),
                    "a cut at step {k} must stop the operation"
                );
                f.power_on();
                let re = JournalManager::init(&mut f, layout).unwrap_or_else(|e| {
                    panic!("{mode:?} cut at step {k}/{total}: journal does not open: {e:?}")
                });
                check(&mut f, re, k, mode);
            }
        }
        total
    }

    /// The journal must be usable after a cut: one more record goes in and survives a reboot.
    fn assert_still_usable<const A: u32>(
        f: &mut RamFlash<A>,
        layout: &Layout,
        mut re: JournalManager,
        what: &str,
    ) {
        let v = ImageVersion {
            major: 9,
            minor: 9,
            revision: 9,
            build_num: 9,
        };
        re.append_record(f, 1, SLOT_STATE_INVALID, 0, 0, v, 99, 0)
            .unwrap_or_else(|e| panic!("{what}: append after reboot failed: {e:?}"));
        let again = JournalManager::init(f, layout).unwrap();
        assert_eq!(again.slot_b.state, SLOT_STATE_INVALID, "{what}");
        assert_eq!(again.slot_b.min_version, 99, "{what}");
    }

    /// A journal whose next append will roll over, with distinct, recognisable state in both slots.
    fn journal_about_to_roll_over<const A: u32>(
        f: &mut RamFlash<A>,
        layout: &Layout,
    ) -> JournalManager {
        let mut m = JournalManager::init(f, layout).unwrap();
        let capacity = (layout.journal[0].len - 16) / RECORD_SIZE as u32;
        let va = ImageVersion {
            major: 1,
            minor: 2,
            revision: 3,
            build_num: 4,
        };
        let vb = ImageVersion {
            major: 5,
            minor: 6,
            revision: 7,
            build_num: 8,
        };
        m.append_record(f, 0, SLOT_STATE_CONFIRMED, 0, 0, va, 11, 0)
            .unwrap();
        m.append_record(f, 1, SLOT_STATE_TESTING, 2, 0, vb, 22, 0)
            .unwrap();
        for _ in 2..capacity {
            m.append_record(f, 1, SLOT_STATE_TESTING, 2, 0, vb, 22, 0)
                .unwrap();
        }
        assert_eq!(
            m.next_record_offset + RECORD_SIZE as u32 > layout.journal[0].len,
            true,
            "full"
        );
        m
    }

    /// An append that changes slot A and, when the sector is full, rolls the journal over.
    fn rollover_op<const A: u32>(
        build: u32,
    ) -> impl Fn(&mut RamFlash<A>, &mut JournalManager) -> Result<(), JournalError> {
        move |f, m| {
            let v = ImageVersion {
                major: 1,
                minor: 2,
                revision: 3,
                build_num: build,
            };
            m.append_record(f, 0, SLOT_STATE_PENDING, 0, 0, v, 30 + build, 0)
        }
    }

    /// Appends (to slot B, leaving slot A alone) until the next append would roll over.
    fn fill_until_full<const A: u32>(f: &mut RamFlash<A>, layout: &Layout, m: &mut JournalManager) {
        let vb = ImageVersion {
            major: 5,
            minor: 6,
            revision: 7,
            build_num: 8,
        };
        while m.next_record_offset + RECORD_SIZE as u32 <= layout.journal[0].len {
            m.append_record(f, 1, SLOT_STATE_TESTING, 2, 0, vb, 22, 0)
                .unwrap();
        }
    }

    fn assert_atomic<const A: u32>(
        base: &RamFlash<A>,
        layout: &Layout,
        op: impl Fn(&mut RamFlash<A>, &mut JournalManager) -> Result<(), JournalError>,
        what: &str,
    ) -> u64 {
        // the two acceptable outcomes, from runs without any cut
        let mut f0 = base.clone();
        let m0 = JournalManager::init(&mut f0, layout).unwrap();
        let before = states(&m0);
        let mut f1 = base.clone();
        let mut m1 = JournalManager::init(&mut f1, layout).unwrap();
        op(&mut f1, &mut m1).unwrap();
        let after = states(&m1);
        assert_ne!(before, after);
        sweep(base, layout, &op, |f, re, k, mode| {
            let got = states(&re);
            assert!(
                got == before || got == after,
                "{what}: {mode:?} cut at step {k}: slot states are neither the old nor the new ones\n  got    {got:?}\n  before {before:?}\n  after  {after:?}"
            );
            assert_still_usable(
                f,
                layout,
                re,
                &std::format!("{what}: {mode:?} cut at step {k}"),
            );
        })
    }

    #[test]
    fn power_cut_at_any_step_of_an_append_loses_nothing_on_the_h750() {
        let mut base = RamFlash::<1>::new(UNIFORM_4K_4MB);
        let mut m = JournalManager::init(&mut base, &H750_NETLITE).unwrap();
        let v = ImageVersion {
            major: 1,
            minor: 0,
            revision: 0,
            build_num: 1,
        };
        m.append_record(&mut base, 0, SLOT_STATE_CONFIRMED, 0, 0, v, 1, 0)
            .unwrap();
        let n = assert_atomic(&base, &H750_NETLITE, rollover_op::<1>(5), "append, H750");
        assert_eq!(n, 32, "one 32-byte record, one step per byte");
    }

    #[test]
    fn power_cut_during_the_very_first_init_leaves_a_usable_journal() {
        // A blank device: init erases sector 0 and writes the header.
        let base = RamFlash::<1>::new(UNIFORM_4K_4MB);
        let mut probe = base.clone();
        JournalManager::init(&mut probe, &H750_NETLITE).unwrap();
        let total = probe.steps();
        assert_eq!(total, 1 + 16, "one erase, a 16-byte header");
        for mode in [CutMode::Clean, CutMode::Torn] {
            for k in 0..total {
                let mut f = base.clone();
                f.arm_power_cut(k, mode);
                assert!(JournalManager::init(&mut f, &H750_NETLITE).is_err());
                f.power_on();
                let re = JournalManager::init(&mut f, &H750_NETLITE)
                    .unwrap_or_else(|e| panic!("{mode:?} cut at step {k}: {e:?}"));
                assert_eq!(
                    states(&re),
                    (SlotJournalState::default(), SlotJournalState::default())
                );
                assert_still_usable(
                    &mut f,
                    &H750_NETLITE,
                    re,
                    &std::format!("first init, {mode:?} cut at step {k}"),
                );
            }
        }
    }

    #[test]
    fn power_cut_at_any_step_of_a_rollover_loses_nothing_on_the_h750() {
        let mut base = RamFlash::<1>::new(UNIFORM_4K_4MB);
        journal_about_to_roll_over(&mut base, &H750_NETLITE);
        let n = assert_atomic(&base, &H750_NETLITE, rollover_op::<1>(5), "rollover, H750");
        // erase, 16-byte header, two 32-byte snapshots, erase, one 32-byte record
        assert_eq!(n, 1 + 16 + 64 + 1 + 32);
    }

    #[test]
    fn power_cut_at_any_step_of_a_second_rollover_loses_nothing_on_the_h750() {
        // After one rollover the active and the spare sector have swapped roles.
        let mut base = RamFlash::<1>::new(UNIFORM_4K_4MB);
        let mut m = journal_about_to_roll_over(&mut base, &H750_NETLITE);
        rollover_op::<1>(5)(&mut base, &mut m).unwrap();
        assert_eq!((m.generation, m.active_sector_addr), (2, 0x1000));
        fill_until_full(&mut base, &H750_NETLITE, &mut m);
        let n = assert_atomic(
            &base,
            &H750_NETLITE,
            rollover_op::<1>(6),
            "second rollover, H750",
        );
        assert_eq!(n, 1 + 16 + 64 + 1 + 32);
        // the sweep itself ends on the other side: generation 3, back on sector 0
        let mut f = base.clone();
        let mut m = JournalManager::init(&mut f, &H750_NETLITE).unwrap();
        rollover_op::<1>(6)(&mut f, &mut m).unwrap();
        assert_eq!((m.generation, m.active_sector_addr), (3, 0));
    }

    #[test]
    fn with_two_valid_sectors_init_takes_the_higher_generation_in_either_position() {
        // A cut right after the new header is in and before the old sector is erased leaves two
        // valid sectors. The new one (higher generation) must win, whichever of the two it is.
        // Steps before that cut: erase 1, two 32-byte snapshots 64, the 16-byte header 16.
        let cut_before_the_old_sector_is_erased = 1 + 64 + 16;

        // Round 0: a full sector 0 (generation 1). Round 1: the same after one real rollover, so
        // the journal is in sector 1 (generation 2) and sector 0 is the spare.
        let mut first = RamFlash::<1>::new(UNIFORM_4K_4MB);
        journal_about_to_roll_over(&mut first, &H750_NETLITE);
        let mut second = first.clone();
        let mut m2 = JournalManager::init(&mut second, &H750_NETLITE).unwrap();
        rollover_op::<1>(5)(&mut second, &mut m2).unwrap();
        fill_until_full(&mut second, &H750_NETLITE, &mut m2);

        // (device, the append that rolls over, expected generation and sector after the cut)
        let rounds = [(first, 5, 2u32, 0x1000u32), (second, 6, 3, 0x0000)];
        for (round, (base, build, generation, active)) in rounds.into_iter().enumerate() {
            let mut f = base.clone();
            let before = states(&JournalManager::init(&mut f, &H750_NETLITE).unwrap());
            f.arm_power_cut(
                f.steps() + cut_before_the_old_sector_is_erased,
                CutMode::Clean,
            );
            let mut m = JournalManager::init(&mut f, &H750_NETLITE).unwrap();
            assert!(rollover_op::<1>(build)(&mut f, &mut m).is_err());
            f.power_on();
            // both sectors really are valid now
            for at in [0u32, 0x1000] {
                let mut b = [0u8; 16];
                f.read(at, &mut b).unwrap();
                let valid = JournalSectorHeader::ref_from_prefix(&b)
                    .unwrap()
                    .0
                    .is_valid();
                assert!(valid, "round {round}: header at {at:#x} should be valid");
            }
            let re = JournalManager::init(&mut f, &H750_NETLITE).unwrap();
            assert_eq!(
                (re.generation, re.active_sector_addr),
                (generation, active),
                "round {round}"
            );
            assert_eq!(
                states(&re),
                before,
                "round {round}: the snapshots carry the old state"
            );
        }
    }

    #[test]
    fn power_cut_at_any_step_of_a_rollover_loses_nothing_on_the_f42x() {
        let layout = f42x_journal_layout();
        let mut base = RamFlash::<4>::new(F42X_DUAL_BANK_2MB).dual_bank();
        journal_about_to_roll_over(&mut base, &layout);
        let n = assert_atomic(&base, &layout, rollover_op::<4>(5), "rollover, F42x");
        // erase, 4-word header, two 8-word snapshots, erase, one 8-word record
        assert_eq!(n, 1 + 4 + 16 + 1 + 8);
    }
}
