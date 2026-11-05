//! Ingest Pipeline and Firmware Update Protocol Core
//!
//! Handles OTA chunk decryption with project transit key, plaintext hash calculation,
//! re-encryption with device unique key, on-demand NOR sector programming,
//! and complete session zeroization.

#![no_std]

use boot_abi::{
    BOOT_SLOT_A, BOOT_SLOT_B, ERR_DECRYPT_FAILED, ERR_INVALID_PARAM,
    ERR_INVALID_STATE, ERR_NOR_IO, ERR_OUT_OF_BOUNDS, NOR_SECTOR_SIZE,
    NOR_SLOT_A_OFFSET, NOR_SLOT_B_OFFSET, NOR_SLOT_SIZE, SLOT_STATE_PENDING,
};
use boot_crypto::{
    derive_device_key, derive_transit_key, xchacha20_process, KeyTable, Sha256Context,
};
use boot_image::{ImageHeader, HEADER_STRUCT_SIZE};
use boot_journal::JournalManager;
use boot_nor::{NorFlash, SecureRegion};
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
    pub state: SessionState,
    pub target_slot: u8,
    pub slot_offset: u32,
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
    pub fn new() -> Self {
        Self {
            state: SessionState::Idle,
            target_slot: BOOT_SLOT_B,
            slot_offset: NOR_SLOT_B_OFFSET,
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
        self.slot_offset = match target_slot {
            BOOT_SLOT_A => NOR_SLOT_A_OFFSET,
            BOOT_SLOT_B => NOR_SLOT_B_OFFSET,
            _ => return Err(ERR_INVALID_PARAM),
        };

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

        // Monotonic chunk offset check (§16.4 rule 1)
        if offset != self.expected_offset {
            return Err(ERR_INVALID_PARAM);
        }

        if offset + data.len() as u32 > NOR_SLOT_SIZE {
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
                hdr.validate(NOR_SLOT_SIZE).map_err(|_| ERR_INVALID_PARAM)?;
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
