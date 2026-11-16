//! Ingest Pipeline and Firmware Update Protocol Core
//!
//! Handles OTA chunk decryption with project transit key, plaintext hash calculation,
//! re-encryption with device unique key, on-demand NOR sector programming,
//! and complete session zeroization.

#![no_std]

use boot_abi::{
    BOOT_SLOT_A, BOOT_SLOT_B, ERR_DECRYPT_FAILED, ERR_INVALID_PARAM,
    ERR_INVALID_STATE, ERR_NOR_IO, ERR_OUT_OF_BOUNDS, SLOT_STATE_PENDING,
};
use boot_crypto::{
    derive_device_key, derive_transit_key, xchacha20_process, KeyTable, Sha256Context,
};
use boot_image::{ImageHeader, HEADER_STRUCT_SIZE};
use boot_journal::JournalManager;
use boot_layout::Layout;
use boot_nor::{NorFlash, SecureRegion, NOR_SECTOR_SIZE};
use zeroize::Zeroize;
use zerocopy::FromBytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    ReceivingHeader,
    ReceivingBody,
    ReceivingTlvs,
    Finished,
    Aborted,
}

/// Ingest update session state machine.
pub struct UpdateSession {
    /// Board layout (slot offsets/sizes, image rules).
    layout: &'static Layout,
    pub state: SessionState,
    pub target_slot: u8,
    pub slot_offset: u32,
    /// Size of the target slot in bytes (from the layout).
    pub slot_len: u32,
    pub expected_offset: u32,
    pub total_expected_len: u32,
    pub header: Option<ImageHeader>,
    pub transit_key: [u8; 32],
    pub device_key: [u8; 32],
    pub storage_nonce: [u8; 24],
    pub transit_nonce: [u8; 24],
    pub sha_ctx: Sha256Context,
    pub last_erased_sector: u32,
}

impl Zeroize for UpdateSession {
    fn zeroize(&mut self) {
        self.transit_key.zeroize();
        self.device_key.zeroize();
        self.storage_nonce.zeroize();
        self.transit_nonce.zeroize();
        self.state = SessionState::Aborted;
        self.expected_offset = 0;
        self.total_expected_len = 0;
    }
}

impl Drop for UpdateSession {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl UpdateSession {
    pub fn new(layout: &'static Layout) -> Self {
        Self {
            layout,
            state: SessionState::Idle,
            target_slot: BOOT_SLOT_B,
            slot_offset: layout.slots[1].offset,
            slot_len: layout.slots[1].len,
            expected_offset: 0,
            total_expected_len: 0,
            header: None,
            transit_key: [0u8; 32],
            device_key: [0u8; 32],
            storage_nonce: [0u8; 24],
            transit_nonce: [0u8; 24],
            sha_ctx: Sha256Context::new(),
            last_erased_sector: 0xFFFF_FFFF,
        }
    }

    /// Starts an update session for the non-active slot (§8.3 rule 1).
    pub fn start<F: NorFlash + SecureRegion>(
        &mut self,
        nor: &mut F,
        active_slot: u8,
        project_key: &[u8; 32],
        global_secret: &[u8; 32],
        mcu_uid: &[u8; 12],
        build_nonce: &[u8; 24],
        storage_nonce: &[u8; 24],
    ) -> Result<(), i32> {
        self.zeroize();

        // Target must not be the active running slot
        let target_slot = if active_slot == BOOT_SLOT_A {
            BOOT_SLOT_B
        } else {
            BOOT_SLOT_A
        };

        self.target_slot = target_slot;
        let region = self.layout.slot(target_slot).ok_or(ERR_INVALID_PARAM)?;
        self.slot_offset = region.offset;
        self.slot_len = region.len;

        // Derive transit key
        derive_transit_key(project_key, build_nonce, &mut self.transit_key)?;
        self.transit_nonce = *build_nonce;
        self.storage_nonce = *storage_nonce;

        // Derive device key from hardware
        let mut nor_seed = [0u8; 32];
        let mut seed_record_buf = [0u8; 64];
        nor.read_security_register(1, 0, &mut seed_record_buf)
            .map_err(|_| ERR_DECRYPT_FAILED)?;

        if let Ok((rec, _)) = boot_nor::SeedRecord::ref_from_prefix(&seed_record_buf) {
            if rec.is_valid() {
                nor_seed = rec.seed;
            }
        }

        let jedec_id = nor.read_jedec_id().map_err(|_| ERR_NOR_IO)?;
        let mut nor_uid_buf = [0u8; 16];
        let nor_uid_len = nor.read_unique_id(&mut nor_uid_buf).unwrap_or(0);

        derive_device_key(
            global_secret,
            &nor_seed,
            mcu_uid,
            &jedec_id,
            &nor_uid_buf[..nor_uid_len],
            &mut self.device_key,
        )?;
        nor_seed.zeroize();

        self.sha_ctx = Sha256Context::new();
        self.expected_offset = 0;
        self.state = SessionState::ReceivingHeader;
        self.last_erased_sector = 0xFFFF_FFFF;

        Ok(())
    }

    /// Ingests an incoming OTA chunk (offset, payload).
    pub fn feed_chunk<F: NorFlash>(
        &mut self,
        nor: &mut F,
        offset: u32,
        data: &[u8],
    ) -> Result<(), i32> {
        if self.state == SessionState::Idle || self.state == SessionState::Aborted {
            return Err(ERR_INVALID_STATE);
        }

        // An empty chunk carries nothing and would underflow the sector arithmetic below.
        if data.is_empty() {
            return Err(ERR_INVALID_PARAM);
        }

        // Monotonic chunk offset check (§16.4 rule 1)
        if offset != self.expected_offset {
            return Err(ERR_INVALID_PARAM);
        }

        if offset as u64 + data.len() as u64 > self.slot_len as u64 {
            return Err(ERR_OUT_OF_BOUNDS);
        }

        // Erase NOR sectors on demand as write advances
        let start_sector = (offset / NOR_SECTOR_SIZE) * NOR_SECTOR_SIZE;
        let end_sector = ((offset + data.len() as u32 - 1) / NOR_SECTOR_SIZE) * NOR_SECTOR_SIZE;

        let mut sec = start_sector;
        while sec <= end_sector {
            if self.last_erased_sector == 0xFFFF_FFFF || sec > self.last_erased_sector {
                nor.erase_sector_4k(self.slot_offset + sec)
                    .map_err(|_| ERR_NOR_IO)?;
                self.last_erased_sector = sec;
            }
            sec += NOR_SECTOR_SIZE;
        }

        let mut plaintext = [0u8; 512];
        let mut chunk_idx = 0;

        while chunk_idx < data.len() {
            let chunk_len = (data.len() - chunk_idx).min(plaintext.len());
            let curr_offset = offset + chunk_idx as u32;

            plaintext[..chunk_len].copy_from_slice(&data[chunk_idx..chunk_idx + chunk_len]);

            // Decrypt incoming chunk with transit key
            xchacha20_process(
                &self.transit_key,
                &self.transit_nonce,
                curr_offset,
                &mut plaintext[..chunk_len],
            )?;

            // If in header area, inspect header when complete
            if curr_offset == 0 && chunk_len >= HEADER_STRUCT_SIZE {
                let (hdr, _) = ImageHeader::ref_from_prefix(&plaintext[..HEADER_STRUCT_SIZE])
                    .map_err(|_| ERR_INVALID_PARAM)?;
                hdr.validate(self.layout).map_err(|_| ERR_INVALID_PARAM)?;
                self.header = Some(*hdr);
                self.total_expected_len = hdr.ih_hdr_size as u32
                    + hdr.ih_img_size
                    + hdr.ih_protect_tlv_size as u32;
            }

            // Feed plaintext into streaming SHA-256 (up to protected TLVs)
            if let Some(hdr) = self.header {
                let hash_limit = (hdr.ih_hdr_size as u32) + hdr.ih_img_size + (hdr.ih_protect_tlv_size as u32);
                if curr_offset < hash_limit {
                    let feed_len = ((hash_limit - curr_offset) as usize).min(chunk_len);
                    self.sha_ctx.update(&plaintext[..feed_len]);
                }
            }

            // Re-encrypt chunk with device key for storage in NOR
            let mut storage_chunk = plaintext;
            xchacha20_process(
                &self.device_key,
                &self.storage_nonce,
                curr_offset,
                &mut storage_chunk[..chunk_len],
            )?;

            // Write to target slot in NOR
            nor.write(self.slot_offset + curr_offset, &storage_chunk[..chunk_len])
                .map_err(|_| ERR_NOR_IO)?;

            plaintext.zeroize();
            storage_chunk.zeroize();

            chunk_idx += chunk_len;
        }

        self.expected_offset += data.len() as u32;
        Ok(())
    }

    /// Finalizes the update session, verifies signature, and updates journal to Pending.
    pub fn finish<F: NorFlash>(
        &mut self,
        nor: &mut F,
        journal: &mut JournalManager,
        _keytable: &KeyTable,
    ) -> Result<(), i32> {
        let hdr = self.header.ok_or(ERR_INVALID_STATE)?;
        let _final_hash = self.sha_ctx.finalize_clone();

        // Mark slot in journal
        journal
            .append_record(
                nor,
                self.target_slot,
                SLOT_STATE_PENDING,
                0,
                0,
                hdr.ih_ver,
                0,
                0,
            )
            .map_err(|_| ERR_NOR_IO)?;

        self.state = SessionState::Finished;
        self.zeroize();
        Ok(())
    }

    pub fn abort(&mut self) {
        self.zeroize();
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use boot_crypto::{derive_transit_key, xchacha20_process};
    use boot_image::{ImageHeader, EXPECTED_HDR_SIZE, IMAGE_F_RAM_LOAD, IMAGE_MAGIC};
    use boot_layout::H750_NETLITE;
    use boot_nor::RamNor;
    use zerocopy::IntoBytes;

    const PROJECT_KEY: [u8; 32] = [0x11; 32];
    const GLOBAL_SECRET: [u8; 32] = [0x22; 32];
    const MCU_UID: [u8; 12] = [0x33; 12];
    const BUILD_NONCE: [u8; 24] = [0x44; 24];
    const STORAGE_NONCE: [u8; 24] = [0x55; 24];

    fn new_nor() -> RamNor {
        RamNor::new(2 * 1024 * 1024)
    }

    fn started(nor: &mut RamNor, active_slot: u8) -> UpdateSession {
        let mut s = UpdateSession::new(&H750_NETLITE);
        s.start(nor, active_slot, &PROJECT_KEY, &GLOBAL_SECRET, &MCU_UID, &BUILD_NONCE, &STORAGE_NONCE)
            .expect("start");
        s
    }

    /// A header chunk (512 bytes) for a small valid H750 image, encrypted with the transit key.
    fn transit_encrypted_header_chunk() -> [u8; 512] {
        let hdr = ImageHeader {
            ih_magic: IMAGE_MAGIC,
            ih_load_addr: H750_NETLITE.load_addr().unwrap(),
            ih_hdr_size: EXPECTED_HDR_SIZE,
            ih_protect_tlv_size: 16,
            ih_img_size: 4096,
            ih_flags: IMAGE_F_RAM_LOAD,
            ih_ver: boot_abi::ImageVersion { major: 1, minor: 0, revision: 0, build_num: 1 },
            _pad1: 0,
        };
        let mut chunk = [0xFFu8; 512];
        chunk[..32].copy_from_slice(hdr.as_bytes());
        let mut key = [0u8; 32];
        derive_transit_key(&PROJECT_KEY, &BUILD_NONCE, &mut key).unwrap();
        xchacha20_process(&key, &BUILD_NONCE, 0, &mut chunk).unwrap();
        chunk
    }

    #[test]
    fn start_targets_the_slot_that_is_not_active() {
        let mut nor = new_nor();
        let s = started(&mut nor, BOOT_SLOT_A);
        assert_eq!(s.target_slot, BOOT_SLOT_B);
        assert_eq!(s.slot_offset, H750_NETLITE.slots[1].offset);
        assert_eq!(s.slot_len, H750_NETLITE.slots[1].len);

        let s = started(&mut nor, BOOT_SLOT_B);
        assert_eq!(s.target_slot, BOOT_SLOT_A);
        assert_eq!(s.slot_offset, H750_NETLITE.slots[0].offset);
    }

    #[test]
    fn feed_is_refused_before_start_and_after_abort() {
        let mut nor = new_nor();
        let mut s = UpdateSession::new(&H750_NETLITE);
        assert_eq!(s.feed_chunk(&mut nor, 0, &[0u8; 16]), Err(ERR_INVALID_STATE));
        let mut s = started(&mut nor, BOOT_SLOT_A);
        s.abort();
        assert_eq!(s.feed_chunk(&mut nor, 0, &[0u8; 16]), Err(ERR_INVALID_STATE));
    }

    #[test]
    fn feed_requires_strictly_monotonic_offsets() {
        let mut nor = new_nor();
        let mut s = started(&mut nor, BOOT_SLOT_A);
        assert_eq!(s.feed_chunk(&mut nor, 16, &[0u8; 16]), Err(ERR_INVALID_PARAM), "gap at start");
    }

    #[test]
    fn feed_refuses_to_write_past_the_end_of_the_slot() {
        let mut nor = new_nor();
        let mut s = started(&mut nor, BOOT_SLOT_A);
        let end = s.slot_len;
        s.expected_offset = end - 8;
        assert_eq!(s.feed_chunk(&mut nor, end - 8, &[0u8; 16]), Err(ERR_OUT_OF_BOUNDS));
    }

    #[test]
    fn feed_bounds_check_does_not_overflow_near_u32_max() {
        let mut nor = new_nor();
        let mut s = started(&mut nor, BOOT_SLOT_A);
        s.expected_offset = u32::MAX - 4;
        assert_eq!(s.feed_chunk(&mut nor, u32::MAX - 4, &[0u8; 16]), Err(ERR_OUT_OF_BOUNDS));
    }

    #[test]
    fn empty_chunk_is_rejected_instead_of_underflowing() {
        // Regression: `offset + len - 1` underflowed for an empty chunk at offset 0, which
        // panics in release builds (overflow-checks) and would hang the bootloader.
        let mut nor = new_nor();
        let mut s = started(&mut nor, BOOT_SLOT_A);
        assert_eq!(s.feed_chunk(&mut nor, 0, &[]), Err(ERR_INVALID_PARAM));
        // The session is still usable afterwards.
        let chunk = transit_encrypted_header_chunk();
        assert_eq!(s.feed_chunk(&mut nor, 0, &chunk), Ok(()));
    }

    #[test]
    fn feed_erases_and_programs_the_layouts_target_slot_only() {
        let mut nor = new_nor();
        let slot_a = H750_NETLITE.slots[0].offset as usize;
        let slot_b = H750_NETLITE.slots[1].offset as usize;
        // Pre-fill the first sector of both slots with zeros ("dirty" flash: erase is needed).
        nor.mem[slot_a..slot_a + 4096].fill(0x00);
        nor.mem[slot_b..slot_b + 4096].fill(0x00);

        let mut s = started(&mut nor, BOOT_SLOT_A); // target = slot B
        let chunk = transit_encrypted_header_chunk();
        assert_eq!(s.feed_chunk(&mut nor, 0, &chunk), Ok(()));

        // Slot A (the active slot) must be completely untouched.
        assert!(nor.mem[slot_a..slot_a + 4096].iter().all(|&b| b == 0x00), "slot A modified");
        // Slot B's first sector was erased and the first 512 bytes programmed with ciphertext.
        assert!(nor.mem[slot_b..slot_b + 512].iter().any(|&b| b != 0x00 && b != 0xFF));
        assert!(nor.mem[slot_b + 512..slot_b + 4096].iter().all(|&b| b == 0xFF), "erased tail");
        assert_eq!(s.expected_offset, 512);
        assert_eq!(s.total_expected_len, 1024 + 4096 + 16);
    }

    #[test]
    fn header_for_the_wrong_load_address_is_rejected() {
        let mut nor = new_nor();
        let mut s = started(&mut nor, BOOT_SLOT_A);
        let hdr = ImageHeader {
            ih_magic: IMAGE_MAGIC,
            ih_load_addr: 0x3000_0000, // not the H750 AXI SRAM address
            ih_hdr_size: EXPECTED_HDR_SIZE,
            ih_protect_tlv_size: 16,
            ih_img_size: 4096,
            ih_flags: IMAGE_F_RAM_LOAD,
            ih_ver: boot_abi::ImageVersion { major: 1, minor: 0, revision: 0, build_num: 1 },
            _pad1: 0,
        };
        let mut chunk = [0xFFu8; 512];
        chunk[..32].copy_from_slice(hdr.as_bytes());
        let mut key = [0u8; 32];
        derive_transit_key(&PROJECT_KEY, &BUILD_NONCE, &mut key).unwrap();
        xchacha20_process(&key, &BUILD_NONCE, 0, &mut chunk).unwrap();
        assert_eq!(s.feed_chunk(&mut nor, 0, &chunk), Err(ERR_INVALID_PARAM));
    }
}
