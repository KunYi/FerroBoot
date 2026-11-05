//! NOR Flash Ping-Pong State Journal
//!
//! Stores firmware slot state transitions across two 4KB ping-pong sectors
//! with power-cut tolerance and snapshot rollover.

#![no_std]

use boot_abi::{
    ImageVersion, NOR_JOURNAL_SECTOR_0, NOR_JOURNAL_SECTOR_1, NOR_SECTOR_SIZE, SLOT_STATE_EMPTY,
};
use boot_nor::{NorError, NorFlash};
use crc32fast::Hasher;
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
    pub active_sector_addr: u32,
    pub generation: u32,
    pub next_seq: u32,
    pub next_record_offset: u32,
    pub slot_a: SlotJournalState,
    pub slot_b: SlotJournalState,
}

impl JournalManager {
    /// Loads or initializes the journal across the ping-pong sectors.
    pub fn init<F: NorFlash>(nor: &mut F) -> Result<Self, JournalError> {
        let mut hdr0_buf = [0u8; 16];
        let mut hdr1_buf = [0u8; 16];

        nor.read(NOR_JOURNAL_SECTOR_0, &mut hdr0_buf)?;
        nor.read(NOR_JOURNAL_SECTOR_1, &mut hdr1_buf)?;

        let hdr0 = JournalSectorHeader::ref_from_prefix(&hdr0_buf).ok().map(|(h, _)| *h);
        let hdr1 = JournalSectorHeader::ref_from_prefix(&hdr1_buf).ok().map(|(h, _)| *h);

        let v0 = hdr0.map_or(false, |h| h.is_valid());
        let v1 = hdr1.map_or(false, |h| h.is_valid());

        let (active_addr, generation) = match (v0, v1) {
            (true, true) => {
                let g0 = hdr0.unwrap().generation;
                let g1 = hdr1.unwrap().generation;
                if g0 >= g1 {
                    (NOR_JOURNAL_SECTOR_0, g0)
                } else {
                    (NOR_JOURNAL_SECTOR_1, g1)
                }
            }
            (true, false) => (NOR_JOURNAL_SECTOR_0, hdr0.unwrap().generation),
            (false, true) => (NOR_JOURNAL_SECTOR_1, hdr1.unwrap().generation),
            (false, false) => {
                // Initialize clean journal on Sector 0
                nor.erase_sector_4k(NOR_JOURNAL_SECTOR_0)?;
                let new_hdr = JournalSectorHeader::new(1);
                nor.write(NOR_JOURNAL_SECTOR_0, new_hdr.as_bytes())?;
                (NOR_JOURNAL_SECTOR_0, 1)
            }
        };

        let mut mgr = Self {
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

        while offset + (RECORD_SIZE as u32) <= NOR_SECTOR_SIZE {
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
        if self.next_record_offset + (RECORD_SIZE as u32) > NOR_SECTOR_SIZE {
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
        let other_addr = if self.active_sector_addr == NOR_JOURNAL_SECTOR_0 {
            NOR_JOURNAL_SECTOR_1
        } else {
            NOR_JOURNAL_SECTOR_0
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
    use boot_nor::RamNor;

    #[test]
    fn test_journal_init_and_state_transitions() {
        let mut nor = RamNor::new(64 * 1024);
        let mut mgr = JournalManager::init(&mut nor).unwrap();

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
        let reloaded = JournalManager::init(&mut nor).unwrap();
        assert_eq!(reloaded.slot_a.state, SLOT_STATE_CONFIRMED);
        assert_eq!(reloaded.slot_a.trial, 1);
        assert_eq!(reloaded.slot_a.img_ver.major, 1);
    }

    #[test]
    fn test_journal_rollover() {
        let mut nor = RamNor::new(64 * 1024);
        let mut mgr = JournalManager::init(&mut nor).unwrap();

        let v = ImageVersion::default();
        // Append records until rollover occurs
        for i in 0..150 {
            mgr.append_record(&mut nor, 0, (i % 5) as u8, 0, 0, v, 0, 0)
                .unwrap();
        }

        assert!(mgr.generation > 1);

        // Verify that after reboot, the state is accurately recovered from the new active sector
        let reloaded = JournalManager::init(&mut nor).unwrap();
        assert_eq!(reloaded.generation, mgr.generation);
        assert_eq!(reloaded.slot_a.state, mgr.slot_a.state);
    }
}
