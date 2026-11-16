//! NOR Flash Ping-Pong State Journal
//!
//! Stores firmware slot state transitions across two 4KB ping-pong sectors
//! with power-cut tolerance and snapshot rollover.

#![no_std]

use boot_abi::{ImageVersion, SLOT_STATE_EMPTY};
use boot_layout::Layout;
use boot_nor::{NorError, NorFlash, NOR_SECTOR_SIZE};
use boot_crc::Hasher;
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
    Nor(NorError),
    Corrupted,
    NoValidSector,
    InvalidSlot,
    /// The layout's journal regions are not 4 KiB erase units (the only unit supported today).
    BadLayout,
}

impl From<NorError> for JournalError {
    fn from(e: NorError) -> Self {
        JournalError::Nor(e)
    }
}

/// Sector header stored at offset 0 of each 4KB journal sector.
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
    /// The two ping-pong sector addresses, from `Layout::journal`.
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

impl JournalManager {
    /// Loads or initializes the journal across the ping-pong sectors.
    pub fn init<F: NorFlash>(nor: &mut F, layout: &Layout) -> Result<Self, JournalError> {
        let sectors = [layout.journal[0].offset, layout.journal[1].offset];
        let sector_len = layout.journal[0].len;
        // Erase/rollover use 4 KiB sector erases; reject any layout that disagrees.
        if layout.journal[1].len != sector_len || sector_len != NOR_SECTOR_SIZE {
            return Err(JournalError::BadLayout);
        }

        let mut hdr0_buf = [0u8; 16];
        let mut hdr1_buf = [0u8; 16];

        nor.read(sectors[0], &mut hdr0_buf)?;
        nor.read(sectors[1], &mut hdr1_buf)?;

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
                nor.erase_sector_4k(sectors[0])?;
                let new_hdr = JournalSectorHeader::new(1);
                nor.write(sectors[0], new_hdr.as_bytes())?;
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

        mgr.replay_active_sector(nor)?;
        Ok(mgr)
    }

    fn replay_active_sector<F: NorFlash>(&mut self, nor: &mut F) -> Result<(), JournalError> {
        let mut buf = [0u8; RECORD_SIZE];
        let mut offset = 16u32;

        while offset + (RECORD_SIZE as u32) <= self.sector_len {
            nor.read(self.active_sector_addr + offset, &mut buf)?;
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
    pub fn append_record<F: NorFlash>(
        &mut self,
        nor: &mut F,
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
            self.rollover(nor)?;
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

        nor.write(
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
    fn rollover<F: NorFlash>(&mut self, nor: &mut F) -> Result<(), JournalError> {
        let other_addr = if self.active_sector_addr == self.sectors[0] {
            self.sectors[1]
        } else {
            self.sectors[0]
        };

        nor.erase_sector_4k(other_addr)?;

        let new_gen = self.generation + 1;
        let new_hdr = JournalSectorHeader::new(new_gen);
        nor.write(other_addr, new_hdr.as_bytes())?;

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
        nor.write(other_addr + 16, rec_a.as_bytes())?;

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
        nor.write(other_addr + 16 + RECORD_SIZE as u32, rec_b.as_bytes())?;

        // Verify written snapshot header before erasing old sector
        let mut verify_buf = [0u8; 16];
        nor.read(other_addr, &mut verify_buf)?;
        let (v_hdr, _) = JournalSectorHeader::ref_from_prefix(&verify_buf)
            .map_err(|_| JournalError::Corrupted)?;
        if !v_hdr.is_valid() {
            return Err(JournalError::Corrupted);
        }

        // Erase old sector
        nor.erase_sector_4k(self.active_sector_addr)?;

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
        nor.write(crc_byte, &[0x00]).unwrap();

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
}
