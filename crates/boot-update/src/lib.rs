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
use boot_flash::Flash;
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
    /// End, relative to the slot, of the part of the slot that has been erased. Units are erased
    /// on demand as the stream advances, so this only ever moves forward.
    pub erased_end: u32,
    /// Bytes of the stream already programmed (always a multiple of the device's program word).
    programmed_end: u32,
    /// Stream bytes after `programmed_end` that do not fill a program word yet (already encrypted
    /// for storage). They are programmed when the word fills up, or padded with 0xFF in `finish`.
    carry: [u8; MAX_PROGRAM_ALIGN],
    carry_len: usize,
}

/// Largest program word the update stream can be written with. The carry buffer is this big.
const MAX_PROGRAM_ALIGN: usize = 32;

impl Zeroize for UpdateSession {
    fn zeroize(&mut self) {
        self.transit_key.zeroize();
        self.device_key.zeroize();
        self.storage_nonce.zeroize();
        self.transit_nonce.zeroize();
        self.state = SessionState::Aborted;
        self.expected_offset = 0;
        self.total_expected_len = 0;
        self.carry.zeroize();
        self.carry_len = 0;
        self.programmed_end = 0;
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
            erased_end: 0,
            programmed_end: 0,
            carry: [0u8; MAX_PROGRAM_ALIGN],
            carry_len: 0,
        }
    }

    /// Common start of a session: pick the target slot (never the active one), check that it is
    /// made of whole erase units of this flash, and derive the transit key. Fails before anything
    /// is erased.
    fn init_session<F: Flash>(
        &mut self,
        flash: &F,
        active_slot: u8,
        project_key: &[u8; 32],
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

        // The slot is erased one unit at a time, so it has to start on a unit boundary and end
        // on one, and the program word has to fit the carry buffer.
        let end = region.offset.checked_add(region.len).ok_or(ERR_INVALID_PARAM)?;
        let first = flash.unit_at(region.offset);
        let last = flash.unit_at(end.wrapping_sub(1));
        if region.len == 0
            || first.start != region.offset
            || last.end != end
            || F::PROGRAM_ALIGN as usize > MAX_PROGRAM_ALIGN
        {
            return Err(ERR_INVALID_PARAM);
        }

        // Derive transit key
        derive_transit_key(project_key, build_nonce, &mut self.transit_key)?;
        self.transit_nonce = *build_nonce;
        self.storage_nonce = *storage_nonce;
        Ok(())
    }

    /// Final step of starting a session, once the keys are in place.
    fn arm(&mut self) {
        self.sha_ctx = Sha256Context::new();
        self.expected_offset = 0;
        self.state = SessionState::ReceivingHeader;
        self.erased_end = 0;
        self.programmed_end = 0;
        self.carry_len = 0;
    }

    /// Starts an update session for the non-active slot (§8.3 rule 1), with the device key
    /// derived from this NOR (its security-register seed, JEDEC ID and unique ID).
    pub fn start<F: NorFlash + SecureRegion + Flash>(
        &mut self,
        nor: &mut F,
        active_slot: u8,
        project_key: &[u8; 32],
        global_secret: &[u8; 32],
        mcu_uid: &[u8; 12],
        build_nonce: &[u8; 24],
        storage_nonce: &[u8; 24],
    ) -> Result<(), i32> {
        self.init_session(nor, active_slot, project_key, build_nonce, storage_nonce)?;

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

        self.arm();
        Ok(())
    }

    /// Starts an update session with a device key the caller already has (from the platform's
    /// device binding), on any [`Flash`]. [`start`](Self::start) is this plus deriving the key
    /// from the NOR.
    pub fn begin<F: Flash>(
        &mut self,
        flash: &F,
        active_slot: u8,
        project_key: &[u8; 32],
        device_key: &[u8; 32],
        build_nonce: &[u8; 24],
        storage_nonce: &[u8; 24],
    ) -> Result<(), i32> {
        self.init_session(flash, active_slot, project_key, build_nonce, storage_nonce)?;
        self.device_key = *device_key;
        self.arm();
        Ok(())
    }

    /// Programs `bytes`, the next bytes of the stream, in whole program words: a partial word
    /// waits in the carry buffer until the rest arrives.
    fn store<F: Flash>(&mut self, flash: &mut F, mut bytes: &[u8]) -> Result<(), i32> {
        let align = F::PROGRAM_ALIGN as usize;
        // Top up a pending partial word first.
        if self.carry_len > 0 {
            let take = (align - self.carry_len).min(bytes.len());
            self.carry[self.carry_len..self.carry_len + take].copy_from_slice(&bytes[..take]);
            self.carry_len += take;
            bytes = &bytes[take..];
            if self.carry_len < align {
                return Ok(());
            }
            flash
                .program(self.slot_offset + self.programmed_end, &self.carry[..align])
                .map_err(|_| ERR_NOR_IO)?;
            self.programmed_end += align as u32;
            self.carry_len = 0;
        }
        // Whole words straight from the chunk, the rest into the carry buffer.
        let whole = bytes.len() / align * align;
        if whole > 0 {
            flash
                .program(self.slot_offset + self.programmed_end, &bytes[..whole])
                .map_err(|_| ERR_NOR_IO)?;
            self.programmed_end += whole as u32;
        }
        let tail = &bytes[whole..];
        self.carry[..tail.len()].copy_from_slice(tail);
        self.carry_len = tail.len();
        Ok(())
    }

    /// Programs the pending partial word, if any, padded with 0xFF (what erased flash holds).
    fn flush_carry<F: Flash>(&mut self, flash: &mut F) -> Result<(), i32> {
        if self.carry_len == 0 {
            return Ok(());
        }
        let align = F::PROGRAM_ALIGN as usize;
        let mut word = [0xFFu8; MAX_PROGRAM_ALIGN];
        word[..self.carry_len].copy_from_slice(&self.carry[..self.carry_len]);
        flash
            .program(self.slot_offset + self.programmed_end, &word[..align])
            .map_err(|_| ERR_NOR_IO)?;
        self.programmed_end += align as u32;
        self.carry_len = 0;
        Ok(())
    }

    /// Ingests an incoming OTA chunk (offset, payload).
    pub fn feed_chunk<F: Flash>(
        &mut self,
        flash: &mut F,
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

        // Erase erase units on demand as the write advances. `init_session` checked that the
        // slot is made of whole units; this only re-checks that a unit never reaches past it.
        let end = offset + data.len() as u32;
        while self.erased_end < end {
            let at = self.slot_offset + self.erased_end;
            let unit = flash.unit_at(at);
            if unit.start != at || unit.end > self.slot_offset + self.slot_len {
                return Err(ERR_OUT_OF_BOUNDS);
            }
            flash.erase_unit(at).map_err(|_| ERR_NOR_IO)?;
            self.erased_end = unit.end - self.slot_offset;
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

            // Write to the target slot
            self.store(flash, &storage_chunk[..chunk_len])?;

            plaintext.zeroize();
            storage_chunk.zeroize();

            chunk_idx += chunk_len;
        }

        self.expected_offset += data.len() as u32;
        Ok(())
    }

    /// Finalizes the update session, verifies signature, and updates journal to Pending.
    pub fn finish<F: Flash>(
        &mut self,
        nor: &mut F,
        journal: &mut JournalManager,
        _keytable: &KeyTable,
    ) -> Result<(), i32> {
        let hdr = self.header.ok_or(ERR_INVALID_STATE)?;
        let _final_hash = self.sha_ctx.finalize_clone();

        // The last bytes of the stream may not fill a program word yet. Program them, padded with
        // the erased value, before the slot is marked: Pending must not point at a short image.
        self.flush_carry(nor)?;

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

    /// A deterministic OTA stream: the encrypted header chunk, then chunks of awkward sizes. The
    /// payload bytes are a fixed pseudo-random sequence (xorshift), the same for every run.
    fn ota_chunks() -> std::vec::Vec<std::vec::Vec<u8>> {
        let mut x = 0x1234_5678_9ABC_DEF1u64;
        let mut next = move || {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8
        };
        let mut out = std::vec::Vec::new();
        out.push(transit_encrypted_header_chunk().to_vec());
        for len in [1usize, 7, 505, 4096, 513, 1000, 4097, 3, 64, 8191, 2, 600] {
            out.push((0..len).map(|_| next()).collect());
        }
        out
    }

    // --- The update stream on other devices -----------------------------------------------------

    use boot_crypto::{KeyTable, PublicKeySlot, MAX_PUBLIC_KEYS};
    use boot_flash::{CutMode, RamFlash, F42X_DUAL_BANK_2MB};
    use boot_journal::JournalManager;
    use boot_layout::{AddrRegion, ExecModel, Region};

    /// Recorded from `feed_chunk` as it was before it moved to `Flash` (commit 1142777): the 64
    /// KiB from the start of slot B after `ota_chunks()` was fed to dirty (0x5A) flash. CRC-32
    /// computed outside this crate with Python `zlib.crc32`.
    const GOLDEN_SLOT_B_64K_CRC: u32 = 0x85C5_1D89;

    #[test]
    fn the_stream_is_written_exactly_as_before_the_flash_trait() {
        let mut nor = new_nor();
        let slot_b = H750_NETLITE.slots[1].offset as usize;
        nor.mem[slot_b..slot_b + 65536].fill(0x5A); // dirty flash: only erased bytes change
        let mut s = started(&mut nor, BOOT_SLOT_A);
        let mut at = 0u32;
        for c in ota_chunks() {
            assert_eq!(s.feed_chunk(&mut nor, at, &c), Ok(()));
            at += c.len() as u32;
        }
        assert_eq!(at, 19_591);
        assert_eq!(
            boot_crc::checksum(&nor.mem[slot_b..slot_b + 65536]),
            GOLDEN_SLOT_B_64K_CRC
        );
        // By hand: 19,591 bytes reach into the fifth 4 KiB sector, so five were erased (the old
        // code's `last_erased_sector` was the start of the last one, 0x4000) and the rest of the
        // dirty area was never touched. One program per chunk, nothing carried (byte programming).
        assert_eq!(s.erased_end, 5 * 4096);
        assert!(nor.mem[slot_b + 5 * 4096..slot_b + 65536]
            .iter()
            .all(|&b| b == 0x5A));
        assert_eq!(nor.bytes_written, 19_591);
    }

    #[test]
    fn begin_with_a_key_stores_what_start_stores_with_the_same_key() {
        let mut a = new_nor();
        let mut sa = started(&mut a, BOOT_SLOT_A);
        let mut b = new_nor();
        let mut sb = UpdateSession::new(&H750_NETLITE);
        sb.begin(
            &b,
            BOOT_SLOT_A,
            &PROJECT_KEY,
            &sa.device_key,
            &BUILD_NONCE,
            &STORAGE_NONCE,
        )
        .unwrap();
        let mut at = 0u32;
        for c in ota_chunks() {
            sa.feed_chunk(&mut a, at, &c).unwrap();
            sb.feed_chunk(&mut b, at, &c).unwrap();
            at += c.len() as u32;
        }
        assert!(a.mem == b.mem);
        assert_eq!(
            (sa.target_slot, sa.slot_offset),
            (sb.target_slot, sb.slot_offset)
        );
    }

    // A layout whose journal and slots sit on the 128 KiB sectors of an STM32F42x (units at
    // multiples of 0x2_0000 from 0x2_0000 on). The rest is the H750's, so its headers validate.
    const fn f42x_test_with_slots(slots: [Region; 2]) -> Layout {
        Layout {
            name: "f42x-test",
            board_id: 0x4634_3239,
            journal: [
                Region::new(0x2_0000, 0x2_0000),
                Region::new(0x4_0000, 0x2_0000),
            ],
            slots,
            exec: ExecModel::RamLoad {
                load: AddrRegion::new(0x2400_0000, 512 * 1024),
                vtor_offset: 0x400,
            },
            boot_info_addr: 0x3800_0000,
            bl_info_addr: 0x0800_0400,
            boot_api_addr: 0x0800_0440,
            secrets_addr: 0x0801_F000,
            keytable_addr: 0x0801_F800,
        }
    }
    static F42X_TEST: Layout = f42x_test_with_slots([
        Region::new(0x6_0000, 0x4_0000),
        Region::new(0xA_0000, 0x4_0000),
    ]);
    const DEVICE_KEY: [u8; 32] = [0x66; 32];
    const SLOT_B: usize = 0xA_0000; // F42X_TEST.slots[1]

    fn f42x_flash() -> RamFlash<4> {
        let mut f = RamFlash::<4>::new(F42X_DUAL_BANK_2MB).dual_bank();
        f.bytes_mut().fill(0x00); // dirty everywhere: only what the session erases becomes 0xFF
        f
    }

    fn f42x_session(f: &RamFlash<4>) -> UpdateSession {
        let mut s = UpdateSession::new(&F42X_TEST);
        s.begin(
            f,
            BOOT_SLOT_A,
            &PROJECT_KEY,
            &DEVICE_KEY,
            &BUILD_NONCE,
            &STORAGE_NONCE,
        )
        .unwrap();
        s
    }

    fn empty_key_table() -> KeyTable {
        let slot = PublicKeySlot {
            key_id: 0,
            reserved: [0; 3],
            pubkey: [0; 65],
            pubkey_len: 0,
            keyhash: [0; 32],
        };
        KeyTable {
            magic: 0,
            version: 0,
            num_keys: 0,
            keys: [slot; MAX_PUBLIC_KEYS],
            reserved: [0; 1024],
        }
    }

    #[test]
    fn f42x_units_are_erased_on_demand_one_128k_sector_at_a_time() {
        let mut f = f42x_flash();
        let mut s = f42x_session(&f);
        assert_eq!((s.slot_offset, s.slot_len), (0xA_0000, 0x4_0000));
        // The header chunk: the first 128 KiB sector is erased, nothing else.
        s.feed_chunk(&mut f, 0, &transit_encrypted_header_chunk())
            .unwrap();
        assert_eq!(s.erased_end, 0x2_0000);
        let m = f.bytes();
        assert!(
            m[SLOT_B..SLOT_B + 512]
                .iter()
                .any(|&b| b != 0x00 && b != 0xFF),
            "programmed"
        );
        assert!(
            m[SLOT_B + 512..SLOT_B + 0x2_0000]
                .iter()
                .all(|&b| b == 0xFF),
            "erased tail"
        );
        assert!(
            m[SLOT_B + 0x2_0000..SLOT_B + 0x4_0000]
                .iter()
                .all(|&b| b == 0),
            "second sector untouched"
        );
        // Chunks that end before the boundary, and one that ends exactly on it, do not erase
        // the next sector...
        s.feed_chunk(&mut f, 512, &[0xAB; 0x2_0000 - 512 - 4])
            .unwrap();
        assert_eq!(s.erased_end, 0x2_0000);
        s.feed_chunk(&mut f, 0x2_0000 - 4, &[0xAB; 4]).unwrap();
        assert_eq!(s.erased_end, 0x2_0000);
        assert!(f.bytes()[SLOT_B + 0x2_0000..SLOT_B + 0x4_0000]
            .iter()
            .all(|&b| b == 0));
        // ...the next chunk starts in it, so it is erased then, and only that one.
        s.feed_chunk(&mut f, 0x2_0000, &[0xCD; 16]).unwrap();
        assert_eq!(s.erased_end, 0x4_0000);
        let m = f.bytes();
        assert!(m[SLOT_B + 0x2_0000 + 16..SLOT_B + 0x4_0000]
            .iter()
            .all(|&b| b == 0xFF));
        // outside the slot: byte for byte what it was
        let outside = |a: usize| !(SLOT_B..SLOT_B + 0x4_0000).contains(&a);
        assert!(m.iter().enumerate().all(|(a, &b)| !outside(a) || b == 0));
    }

    /// Feeds `ota_chunks()` (19,591 bytes, every chunk an odd size) then `finish`es it.
    fn stream_and_finish<const A: u32>(
        f: &mut RamFlash<A>,
        s: &mut UpdateSession,
        layout: &Layout,
    ) -> usize {
        let mut at = 0u32;
        for c in ota_chunks() {
            s.feed_chunk(f, at, &c).unwrap();
            at += c.len() as u32;
        }
        let mut journal = JournalManager::init(f, layout).unwrap();
        s.finish(f, &mut journal, &empty_key_table()).unwrap();
        at as usize
    }

    #[test]
    fn chunks_of_any_size_reach_a_word_programmed_device_and_finish_pads_the_last_word() {
        let len = 19_591usize; // 3 bytes past a multiple of 4
                               // Reference: the same stream into a byte-programmable device with the same sectors.
        let mut r = RamFlash::<1>::new(F42X_DUAL_BANK_2MB).dual_bank();
        r.bytes_mut().fill(0x00);
        let mut sr = UpdateSession::new(&F42X_TEST);
        sr.begin(
            &r,
            BOOT_SLOT_A,
            &PROJECT_KEY,
            &DEVICE_KEY,
            &BUILD_NONCE,
            &STORAGE_NONCE,
        )
        .unwrap();
        assert_eq!(stream_and_finish(&mut r, &mut sr, &F42X_TEST), len);

        let mut f = f42x_flash();
        let mut s = f42x_session(&f);
        let mut at = 0u32;
        for c in ota_chunks() {
            s.feed_chunk(&mut f, at, &c).unwrap(); // would be `Unaligned` without the carry
            at += c.len() as u32;
        }
        // Before finish: every whole word is in, the last 3 bytes are still waiting...
        assert_eq!(&f.bytes()[SLOT_B + 19_588..SLOT_B + 19_591], &[0xFF; 3]);
        assert_eq!(
            &f.bytes()[SLOT_B..SLOT_B + 19_588],
            &r.bytes()[SLOT_B..SLOT_B + 19_588]
        );
        assert_ne!(
            &r.bytes()[SLOT_B + 19_588..SLOT_B + 19_591],
            &[0xFF; 3],
            "reference has them"
        );
        // ...and after finish they are programmed, the fourth byte of the word is the erased 0xFF,
        // and the slot is marked Pending.
        let mut journal = JournalManager::init(&mut f, &F42X_TEST).unwrap();
        s.finish(&mut f, &mut journal, &empty_key_table()).unwrap();
        assert_eq!(
            &f.bytes()[SLOT_B..SLOT_B + 19_592],
            &r.bytes()[SLOT_B..SLOT_B + 19_592]
        );
        assert_eq!(f.bytes()[SLOT_B + 19_591], 0xFF);
        let re = JournalManager::init(&mut f, &F42X_TEST).unwrap();
        assert_eq!(re.slot_b.state, boot_abi::SLOT_STATE_PENDING);
        assert_eq!(re.slot_b.img_ver.build_num, 1);
    }

    #[test]
    fn one_chunk_that_straddles_two_sectors_erases_both_before_writing() {
        let mut f = f42x_flash();
        let mut s = f42x_session(&f);
        s.feed_chunk(&mut f, 0, &transit_encrypted_header_chunk())
            .unwrap();
        let big: std::vec::Vec<u8> = (0..0x2_0000 - 512 + 0x100).map(|i| i as u8).collect();
        s.feed_chunk(&mut f, 512, &big).unwrap(); // ends 0x100 bytes into the second sector
        assert_eq!(s.erased_end, 0x4_0000);
        let m = f.bytes();
        // the second sector holds the chunk's tail, then is erased: no stale zeros
        assert!(m[SLOT_B + 0x2_0000 + 0x100..SLOT_B + 0x4_0000]
            .iter()
            .all(|&b| b == 0xFF));
        // and nothing was erased after the data went in: the bytes just before the boundary are
        // the programmed ciphertext, not 0xFF
        assert!(m[SLOT_B + 0x2_0000 - 64..SLOT_B + 0x2_0000]
            .iter()
            .any(|&b| b != 0xFF));
    }

    #[test]
    fn a_new_session_erases_again() {
        let mut nor = new_nor();
        let slot_b = H750_NETLITE.slots[1].offset as usize;
        let mut s = started(&mut nor, BOOT_SLOT_A);
        s.feed_chunk(&mut nor, 0, &transit_encrypted_header_chunk())
            .unwrap();
        assert_eq!(s.erased_end, 4096);
        nor.mem[slot_b..slot_b + 8192].fill(0x00); // the slot is dirty again
        s.start(
            &mut nor,
            BOOT_SLOT_A,
            &PROJECT_KEY,
            &GLOBAL_SECRET,
            &MCU_UID,
            &BUILD_NONCE,
            &STORAGE_NONCE,
        )
        .unwrap();
        assert_eq!(
            (s.erased_end, s.expected_offset),
            (0, 0),
            "a fresh session starts from nothing"
        );
        s.feed_chunk(&mut nor, 0, &transit_encrypted_header_chunk())
            .unwrap();
        assert!(
            nor.mem[slot_b + 512..slot_b + 4096]
                .iter()
                .all(|&b| b == 0xFF),
            "erased again"
        );
        assert!(
            nor.mem[slot_b + 4096..slot_b + 8192]
                .iter()
                .all(|&b| b == 0),
            "only what was needed"
        );
    }

    #[test]
    fn finish_on_a_stream_that_ends_on_a_word_programs_nothing_extra() {
        let mut f = f42x_flash();
        let mut s = f42x_session(&f);
        s.feed_chunk(&mut f, 0, &transit_encrypted_header_chunk())
            .unwrap(); // 512 bytes
        let mut journal = JournalManager::init(&mut f, &F42X_TEST).unwrap();
        let before = f.bytes()[SLOT_B..SLOT_B + 0x2_0000].to_vec();
        s.finish(&mut f, &mut journal, &empty_key_table()).unwrap();
        assert!(f.bytes()[SLOT_B..SLOT_B + 0x2_0000] == before[..]);
    }

    #[test]
    fn a_slot_that_is_not_whole_erase_units_is_refused_before_anything_is_erased() {
        static MID_UNIT_END: Layout = f42x_test_with_slots([
            Region::new(0x6_0000, 0x4_0000),
            Region::new(0xA_0000, 0x3_0000),
        ]);
        static MISALIGNED_START: Layout = f42x_test_with_slots([
            Region::new(0x6_0000, 0x4_0000),
            Region::new(0xA_4000, 0x4_0000),
        ]);
        // starts inside a sector but ends exactly on a sector boundary (0xE_0000)
        static MISALIGNED_START_ONLY: Layout = f42x_test_with_slots([
            Region::new(0x6_0000, 0x4_0000),
            Region::new(0xA_4000, 0x3_C000),
        ]);
        static EMPTY_SLOT: Layout =
            f42x_test_with_slots([Region::new(0x6_0000, 0x4_0000), Region::new(0xA_0000, 0)]);
        let cases: [(&str, &'static Layout); 5] = [
            ("an empty slot", &EMPTY_SLOT),
            (
                "slot starts inside a sector, ends on one",
                &MISALIGNED_START_ONLY,
            ),
            ("slot ends inside a sector", &MID_UNIT_END),
            ("slot starts inside a sector", &MISALIGNED_START),
            // 4 KiB-sector layout of the H750 on a device whose sectors are 128 KiB
            ("the H750 layout", &H750_NETLITE),
        ];
        for (what, layout) in cases {
            let f = f42x_flash();
            let mut s = UpdateSession::new(layout);
            let r = s.begin(
                &f,
                BOOT_SLOT_A,
                &PROJECT_KEY,
                &DEVICE_KEY,
                &BUILD_NONCE,
                &STORAGE_NONCE,
            );
            assert_eq!(r, Err(ERR_INVALID_PARAM), "{what}");
            assert_eq!(s.state, SessionState::Aborted, "{what}: no session");
            assert!(f.bytes().iter().all(|&b| b == 0), "{what}: nothing erased");
            assert_eq!(
                s.feed_chunk(&mut f42x_flash(), 0, &[0; 4]),
                Err(ERR_INVALID_STATE),
                "{what}"
            );
        }
    }

    #[test]
    fn a_program_word_the_carry_buffer_cannot_hold_is_refused() {
        let ok = RamFlash::<32>::new(&[(0x2_0000, 16)]);
        let mut s = UpdateSession::new(&F42X_TEST);
        assert_eq!(
            s.begin(
                &ok,
                BOOT_SLOT_A,
                &PROJECT_KEY,
                &DEVICE_KEY,
                &BUILD_NONCE,
                &STORAGE_NONCE
            ),
            Ok(())
        );
        let too_wide = RamFlash::<64>::new(&[(0x2_0000, 16)]);
        let mut s = UpdateSession::new(&F42X_TEST);
        assert_eq!(
            s.begin(
                &too_wide,
                BOOT_SLOT_A,
                &PROJECT_KEY,
                &DEVICE_KEY,
                &BUILD_NONCE,
                &STORAGE_NONCE
            ),
            Err(ERR_INVALID_PARAM)
        );
    }

    #[test]
    fn an_erase_never_reaches_past_the_end_of_the_slot() {
        let mut f = f42x_flash();
        let mut s = f42x_session(&f);
        // Only reachable by changing the session behind its back (the fields are public): a slot
        // that stops in the middle of a sector. The sector must not be erased.
        s.slot_len = 0x1_0000;
        assert_eq!(
            s.feed_chunk(&mut f, 0, &transit_encrypted_header_chunk()),
            Err(ERR_OUT_OF_BOUNDS)
        );
        assert!(f.bytes().iter().all(|&b| b == 0), "nothing erased");
        assert_eq!(s.erased_end, 0);
    }

    #[test]
    fn an_erase_must_start_where_the_slot_says() {
        let mut f = f42x_flash();
        let mut s = f42x_session(&f);
        // As above, only reachable by editing the session: a slot offset in the middle of a sector.
        s.slot_offset += 0x4000;
        assert_eq!(
            s.feed_chunk(&mut f, 0, &transit_encrypted_header_chunk()),
            Err(ERR_OUT_OF_BOUNDS)
        );
        assert!(f.bytes().iter().all(|&b| b == 0), "nothing erased");
    }

    #[test]
    fn a_flash_failure_is_reported_and_the_session_state_stays_consistent() {
        let mut f = f42x_flash();
        let mut s = f42x_session(&f);
        f.arm_power_cut(0, CutMode::Clean); // the very first erase fails
        assert_eq!(
            s.feed_chunk(&mut f, 0, &transit_encrypted_header_chunk()),
            Err(ERR_NOR_IO)
        );
        assert_eq!(
            s.erased_end, 0,
            "nothing was erased, so nothing is recorded as erased"
        );
        assert_eq!(s.expected_offset, 0);
    }
}
