//! Core Bootloader Decision & Loader Engine
//!
//! Contains the pure `decide()` slot prioritization function, invariant tests,
//! streaming RAM copy & verification pipeline, and anti-rollback checks.

#![no_std]

use boot_abi::{
    BOOT_SLOT_A, BOOT_SLOT_B, ERR_DECRYPT_FAILED, ERR_INVALID_PARAM, ERR_VERIFY_FAILED,
    NOR_SLOT_A_OFFSET, NOR_SLOT_B_OFFSET, SLOT_STATE_CONFIRMED, SLOT_STATE_EMPTY,
    SLOT_STATE_INVALID, SLOT_STATE_PENDING, SLOT_STATE_TESTING,
};
use boot_crypto::{
    ct_equal, derive_device_key, verify_p256_signature, xchacha20_process, KeyTable,
    Sha256Context,
};
use boot_image::{parse_image, ImageHeader, ParsedImage, ENC_SCHEME_DEVICE, HEADER_STRUCT_SIZE};
use boot_journal::SlotJournalState;
use boot_nor::{NorFlash, SecureRegion};
use zeroize::Zeroize;
use zerocopy::FromBytes;

pub const DEFAULT_MAX_TRIALS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootDecision {
    /// Boot candidates in priority order (first tried first)
    TryCandidates {
        primary: u8,
        fallback: Option<u8>,
    },
    /// No bootable slot available; enter rescue mode
    EnterRescue,
}

/// Pure decision function prioritizing firmware slots (§7.2, T3.2).
pub fn decide(
    slot_a: &SlotJournalState,
    slot_b: &SlotJournalState,
    max_trials: u8,
) -> BootDecision {
    let a_bootable = is_slot_bootable(slot_a, max_trials);
    let b_bootable = is_slot_bootable(slot_b, max_trials);

    match (a_bootable, b_bootable) {
        (true, true) => {
            // Prioritize Pending or Testing (trial in progress)
            let a_priority = slot_priority(slot_a);
            let b_priority = slot_priority(slot_b);

            if a_priority > b_priority {
                BootDecision::TryCandidates {
                    primary: BOOT_SLOT_A,
                    fallback: Some(BOOT_SLOT_B),
                }
            } else if b_priority > a_priority {
                BootDecision::TryCandidates {
                    primary: BOOT_SLOT_B,
                    fallback: Some(BOOT_SLOT_A),
                }
            } else {
                // If same state (e.g. both confirmed), pick higher version or slot A
                if is_version_newer(&slot_b.img_ver, &slot_a.img_ver) {
                    BootDecision::TryCandidates {
                        primary: BOOT_SLOT_B,
                        fallback: Some(BOOT_SLOT_A),
                    }
                } else {
                    BootDecision::TryCandidates {
                        primary: BOOT_SLOT_A,
                        fallback: Some(BOOT_SLOT_B),
                    }
                }
            }
        }
        (true, false) => BootDecision::TryCandidates {
            primary: BOOT_SLOT_A,
            fallback: None,
        },
        (false, true) => BootDecision::TryCandidates {
            primary: BOOT_SLOT_B,
            fallback: None,
        },
        (false, false) => BootDecision::EnterRescue,
    }
}

fn is_slot_bootable(s: &SlotJournalState, max_trials: u8) -> bool {
    match s.state {
        SLOT_STATE_PENDING => true,
        SLOT_STATE_TESTING => s.trial < max_trials,
        SLOT_STATE_CONFIRMED => true,
        SLOT_STATE_EMPTY | SLOT_STATE_INVALID | _ => false,
    }
}

fn slot_priority(s: &SlotJournalState) -> u8 {
    match s.state {
        SLOT_STATE_TESTING => 3, // In progress trial takes immediate priority
        SLOT_STATE_PENDING => 2,
        SLOT_STATE_CONFIRMED => 1,
        _ => 0,
    }
}

fn is_version_newer(v_new: &boot_abi::ImageVersion, v_old: &boot_abi::ImageVersion) -> bool {
    if v_new.major != v_old.major {
        return v_new.major > v_old.major;
    }
    if v_new.minor != v_old.minor {
        return v_new.minor > v_old.minor;
    }
    if v_new.revision != v_old.revision {
        return v_new.revision > v_old.revision;
    }
    v_new.build_num > v_old.build_num
}

/// Slot loader that streams NOR flash encrypted content into RAM, decrypts on-the-fly,
/// computes SHA-256 over plaintext, and verifies the image signature.
pub struct SlotLoader<'a, F: NorFlash + SecureRegion> {
    nor: &'a mut F,
    keytable: &'a KeyTable,
    global_secret: &'a [u8; 32],
    mcu_uid: [u8; 12],
    expected_board_id: u32,
    current_bl_version: u32,
}

impl<'a, F: NorFlash + SecureRegion> SlotLoader<'a, F> {
    pub fn new(
        nor: &'a mut F,
        keytable: &'a KeyTable,
        global_secret: &'a [u8; 32],
        mcu_uid: [u8; 12],
        expected_board_id: u32,
        current_bl_version: u32,
    ) -> Self {
        Self {
            nor,
            keytable,
            global_secret,
            mcu_uid,
            expected_board_id,
            current_bl_version,
        }
    }

    /// Loads, decrypts, and validates the slot into RAM target buffer.
    ///
    /// If verification fails, `ram_buffer` is zeroed out before returning an error.
    pub fn load_and_verify(
        &mut self,
        slot_id: u8,
        ram_buffer: &mut [u8],
        require_encrypted: bool,
    ) -> Result<ParsedImage<'static>, i32> {
        let slot_offset = match slot_id {
            BOOT_SLOT_A => NOR_SLOT_A_OFFSET,
            BOOT_SLOT_B => NOR_SLOT_B_OFFSET,
            _ => return Err(ERR_INVALID_PARAM),
        };

        // 1. Read first 2KB from NOR (header and initial chunk)
        let mut header_buf = [0u8; 2048];
        self.nor
            .read(slot_offset, &mut header_buf)
            .map_err(|_| ERR_VERIFY_FAILED)?;

        // 2. Derive device key from hardware OTP seed, MCU UID, and NOR UID
        let mut nor_seed = [0u8; 32];
        let mut seed_record_buf = [0u8; 64];
        self.nor
            .read_security_register(1, 0, &mut seed_record_buf)
            .map_err(|_| ERR_DECRYPT_FAILED)?;

        if let Ok((rec, _)) = boot_nor::SeedRecord::ref_from_prefix(&seed_record_buf) {
            if rec.is_valid() {
                nor_seed = rec.seed;
            }
        }

        let jedec_id = self.nor.read_jedec_id().map_err(|_| ERR_VERIFY_FAILED)?;
        let mut nor_uid_buf = [0u8; 16];
        let nor_uid_len = self
            .nor
            .read_unique_id(&mut nor_uid_buf)
            .unwrap_or(0);

        let mut device_key = [0u8; 32];
        derive_device_key(
            self.global_secret,
            &nor_seed,
            &self.mcu_uid,
            &jedec_id,
            &nor_uid_buf[..nor_uid_len],
            &mut device_key,
        )?;

        // 3. Preliminary parse of the image header (first 32 bytes)
        let (hdr_ref, _) = ImageHeader::ref_from_prefix(&header_buf[..HEADER_STRUCT_SIZE])
            .map_err(|_| {
                device_key.zeroize();
                nor_seed.zeroize();
                ERR_VERIFY_FAILED
            })?;
        let hdr = *hdr_ref;
        if hdr.validate(boot_abi::NOR_SLOT_SIZE).is_err() {
            device_key.zeroize();
            nor_seed.zeroize();
            return Err(ERR_VERIFY_FAILED);
        }
        if require_encrypted && !hdr.is_encrypted() {
            device_key.zeroize();
            nor_seed.zeroize();
            return Err(ERR_VERIFY_FAILED);
        }

        let hdr_size = hdr.ih_hdr_size as usize;
        let img_size = hdr.ih_img_size as usize;
        let total_load_len = hdr_size + img_size;

        if ram_buffer.len() < total_load_len {
            device_key.zeroize();
            return Err(ERR_INVALID_PARAM);
        }

        // Copy entire image (Header + encrypted body + TLVs) from NOR into RAM buffer
        let mut curr_nor = slot_offset;
        let mut curr_ram = 0;
        let mut sha_ctx = Sha256Context::new();

        // 4. Stream copy from NOR to RAM and decrypt body in-place
        while curr_ram < total_load_len {
            let chunk_len = (total_load_len - curr_ram).min(512);
            self.nor
                .read(curr_nor, &mut ram_buffer[curr_ram..curr_ram + chunk_len])
                .map_err(|_| {
                    ram_buffer.zeroize();
                    ERR_VERIFY_FAILED
                })?;

            curr_nor += chunk_len as u32;
            curr_ram += chunk_len;
        }

        // Read trailing TLVs directly into ram_buffer after body for metadata parsing
        let tlv_start = slot_offset + (hdr_size + img_size) as u32;
        let tlv_len = 1024.min(ram_buffer.len().saturating_sub(hdr_size + img_size));
        self.nor
            .read(tlv_start, &mut ram_buffer[hdr_size + img_size..hdr_size + img_size + tlv_len])
            .map_err(|_| ERR_VERIFY_FAILED)?;

        // Decrypt image body in RAM if encrypted
        if hdr.is_encrypted() {
            let parsed_with_tlv = parse_image(
                &ram_buffer[..hdr_size + img_size + tlv_len],
                self.expected_board_id,
                self.current_bl_version,
                require_encrypted,
            );

            // Fetch storage nonce from ENC_INFO TLV
            let nonce = if let Ok(ref p) = parsed_with_tlv {
                if let Some(enc) = p.enc_info {
                    if enc.scheme != ENC_SCHEME_DEVICE {
                        ram_buffer.zeroize();
                        device_key.zeroize();
                        return Err(ERR_DECRYPT_FAILED);
                    }
                    enc.nonce
                } else {
                    ram_buffer.zeroize();
                    device_key.zeroize();
                    return Err(ERR_DECRYPT_FAILED);
                }
            } else {
                ram_buffer.zeroize();
                device_key.zeroize();
                return Err(ERR_DECRYPT_FAILED);
            };

            // Stream decrypt plaintext body in RAM
            let body = &mut ram_buffer[hdr_size..hdr_size + img_size];
            xchacha20_process(&device_key, &nonce, 0, body)?;
        }

        device_key.zeroize();
        nor_seed.zeroize();

        // 5. Compute SHA-256 over Plaintext (Header + Plaintext Image Body + Protected TLV)
        sha_ctx.update(&ram_buffer[..hdr_size + img_size]);
        if hdr.ih_protect_tlv_size > 0 {
            let prot_len = hdr.ih_protect_tlv_size as usize;
            sha_ctx.update(&ram_buffer[hdr_size + img_size..hdr_size + img_size + prot_len]);
        }
        let calculated_hash = sha_ctx.finalize();

        // 6. Verify Signature against keytable
        let parse_res = parse_image(
            &ram_buffer[..hdr_size + img_size + tlv_len],
            self.expected_board_id,
            self.current_bl_version,
            require_encrypted,
        );
        let parsed = match parse_res {
            Ok(p) => p,
            Err(_) => {
                ram_buffer.zeroize();
                return Err(ERR_VERIFY_FAILED);
            }
        };

        // Extract required metadata to release borrow on ram_buffer
        let expected_hash = match parsed.sha256 {
            Some(h) => *h,
            None => {
                ram_buffer.zeroize();
                return Err(ERR_VERIFY_FAILED);
            }
        };
        let expected_keyhash = match parsed.keyhash {
            Some(k) => *k,
            None => {
                ram_buffer.zeroize();
                return Err(ERR_VERIFY_FAILED);
            }
        };
        let mut sig_buf = [0u8; 64];
        let sig_len = match parsed.signature {
            Some(s) if s.len() <= 64 => {
                sig_buf[..s.len()].copy_from_slice(s);
                s.len()
            }
            _ => {
                ram_buffer.zeroize();
                return Err(ERR_VERIFY_FAILED);
            }
        };

        let header = parsed.header;
        let sig_type = parsed.sig_type;
        let image_type = parsed.image_type;
        let board_id = parsed.board_id;
        let min_bl_version = parsed.min_bl_version;
        let enc_info = parsed.enc_info;
        drop(parsed);

        // Check hash
        if !ct_equal(&calculated_hash, &expected_hash) {
            ram_buffer.zeroize();
            return Err(ERR_VERIFY_FAILED);
        }

        // Check signature against matching key in keytable
        let mut key_found = false;
        for key_slot in &self.keytable.keys {
            if key_slot.pubkey_len > 0 && ct_equal(&key_slot.keyhash, &expected_keyhash) {
                if verify_p256_signature(
                    &calculated_hash,
                    &key_slot.pubkey[..key_slot.pubkey_len as usize],
                    &sig_buf[..sig_len],
                ).is_ok() {
                    key_found = true;
                    break;
                }
            }
        }
        if !key_found {
            ram_buffer.zeroize();
            return Err(ERR_VERIFY_FAILED);
        }

        // Clear any trailing TLVs in RAM above loaded image
        ram_buffer[hdr_size + img_size..].fill(0);

        // Return validated header metadata (static lifetime by cloning necessary fields)
        Ok(ParsedImage {
            header,
            sha256: None,
            keyhash: None,
            signature: None,
            sig_type,
            image_type,
            board_id,
            min_bl_version,
            enc_info,
            total_len: total_load_len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boot_abi::{SLOT_STATE_CONFIRMED, SLOT_STATE_PENDING, SLOT_STATE_TESTING};

    #[test]
    fn test_decide_pending_over_confirmed() {
        let mut slot_a = SlotJournalState::default();
        slot_a.state = SLOT_STATE_CONFIRMED;

        let mut slot_b = SlotJournalState::default();
        slot_b.state = SLOT_STATE_PENDING;

        // Slot B is Pending update, so it should be prioritized as primary candidate
        let decision = decide(&slot_a, &slot_b, DEFAULT_MAX_TRIALS);
        assert_eq!(
            decision,
            BootDecision::TryCandidates {
                primary: BOOT_SLOT_B,
                fallback: Some(BOOT_SLOT_A),
            }
        );
    }

    #[test]
    fn test_decide_exhausted_trials_reverts_to_confirmed() {
        let mut slot_a = SlotJournalState::default();
        slot_a.state = SLOT_STATE_CONFIRMED;

        let mut slot_b = SlotJournalState::default();
        slot_b.state = SLOT_STATE_TESTING;
        slot_b.trial = 3; // Max trials reached

        // Slot B trials exhausted, fall back to Slot A
        let decision = decide(&slot_a, &slot_b, 3);
        assert_eq!(
            decision,
            BootDecision::TryCandidates {
                primary: BOOT_SLOT_A,
                fallback: None,
            }
        );
    }

    #[test]
    fn test_decide_both_empty_enters_rescue() {
        let slot_a = SlotJournalState::default();
        let slot_b = SlotJournalState::default();

        let decision = decide(&slot_a, &slot_b, 3);
        assert_eq!(decision, BootDecision::EnterRescue);
    }

    // -----------------------------------------------------------------------
    // Characterization tests (P0): the full decide() behaviour table.
    // -----------------------------------------------------------------------

    use boot_abi::{
        ImageVersion, SLOT_STATE_EMPTY, SLOT_STATE_INVALID, SLOT_STATE_WRITTEN,
    };

    fn slot(state: u8, trial: u8, major: u8, minor: u8, revision: u16, build: u32) -> SlotJournalState {
        SlotJournalState {
            state,
            trial,
            img_ver: ImageVersion { major, minor, revision, build_num: build },
            ..SlotJournalState::default()
        }
    }

    fn plain(state: u8) -> SlotJournalState {
        slot(state, 0, 1, 0, 0, 1)
    }

    fn try_a(fallback: Option<u8>) -> BootDecision {
        BootDecision::TryCandidates { primary: BOOT_SLOT_A, fallback }
    }

    fn try_b(fallback: Option<u8>) -> BootDecision {
        BootDecision::TryCandidates { primary: BOOT_SLOT_B, fallback }
    }

    #[test]
    fn both_confirmed_equal_versions_prefers_slot_a() {
        let d = decide(&plain(SLOT_STATE_CONFIRMED), &plain(SLOT_STATE_CONFIRMED), 3);
        assert_eq!(d, try_a(Some(BOOT_SLOT_B)));
    }

    #[test]
    fn both_confirmed_newer_version_wins_at_each_field() {
        let base = slot(SLOT_STATE_CONFIRMED, 0, 2, 5, 7, 9);
        // (field that is bigger in B)
        let newer_b = [
            slot(SLOT_STATE_CONFIRMED, 0, 3, 0, 0, 0), // major dominates minor/revision/build
            slot(SLOT_STATE_CONFIRMED, 0, 2, 6, 0, 0), // minor dominates revision/build
            slot(SLOT_STATE_CONFIRMED, 0, 2, 5, 8, 0), // revision dominates build
            slot(SLOT_STATE_CONFIRMED, 0, 2, 5, 7, 10), // build number
        ];
        for b in newer_b {
            assert_eq!(decide(&base, &b, 3), try_b(Some(BOOT_SLOT_A)), "B {:?}", b.img_ver);
            // Swapped: A newer -> A.
            assert_eq!(decide(&b, &base, 3), try_a(Some(BOOT_SLOT_B)), "A {:?}", b.img_ver);
        }
    }

    #[test]
    fn state_priority_is_testing_then_pending_then_confirmed() {
        let testing = slot(SLOT_STATE_TESTING, 1, 1, 0, 0, 1);
        let pending = plain(SLOT_STATE_PENDING);
        let confirmed = plain(SLOT_STATE_CONFIRMED);
        assert_eq!(decide(&testing, &pending, 3), try_a(Some(BOOT_SLOT_B)));
        assert_eq!(decide(&pending, &testing, 3), try_b(Some(BOOT_SLOT_A)));
        assert_eq!(decide(&pending, &confirmed, 3), try_a(Some(BOOT_SLOT_B)));
        assert_eq!(decide(&confirmed, &pending, 3), try_b(Some(BOOT_SLOT_A)));
        assert_eq!(decide(&testing, &confirmed, 3), try_a(Some(BOOT_SLOT_B)));
        assert_eq!(decide(&confirmed, &testing, 3), try_b(Some(BOOT_SLOT_A)));
    }

    #[test]
    fn state_priority_beats_a_newer_version() {
        // A confirmed v9 must not outrank a Pending v1: an update in progress wins.
        let confirmed_new = slot(SLOT_STATE_CONFIRMED, 0, 9, 0, 0, 0);
        let pending_old = slot(SLOT_STATE_PENDING, 0, 1, 0, 0, 0);
        assert_eq!(decide(&confirmed_new, &pending_old, 3), try_b(Some(BOOT_SLOT_A)));
    }

    #[test]
    fn testing_is_bootable_only_while_trials_remain() {
        for max in [1u8, 3, 5] {
            let last_chance = slot(SLOT_STATE_TESTING, max - 1, 1, 0, 0, 1);
            let exhausted = slot(SLOT_STATE_TESTING, max, 1, 0, 0, 1);
            let confirmed = plain(SLOT_STATE_CONFIRMED);
            assert_eq!(decide(&last_chance, &confirmed, max), try_a(Some(BOOT_SLOT_B)), "max {max}");
            assert_eq!(
                decide(&exhausted, &confirmed, max),
                try_b(None),
                "max {max}: exhausted trial reverts to the confirmed slot"
            );
        }
    }

    #[test]
    fn empty_written_and_invalid_slots_are_never_bootable() {
        let confirmed = plain(SLOT_STATE_CONFIRMED);
        for state in [SLOT_STATE_EMPTY, SLOT_STATE_WRITTEN, SLOT_STATE_INVALID, 0xEE] {
            let other = plain(state);
            assert_eq!(decide(&confirmed, &other, 3), try_a(None), "B state {state}");
            assert_eq!(decide(&other, &confirmed, 3), try_b(None), "A state {state}");
        }
    }

    #[test]
    fn nothing_bootable_enters_rescue() {
        let dead = [
            plain(SLOT_STATE_EMPTY),
            plain(SLOT_STATE_WRITTEN),
            plain(SLOT_STATE_INVALID),
            slot(SLOT_STATE_TESTING, 3, 1, 0, 0, 1),
        ];
        for a in &dead {
            for b in &dead {
                assert_eq!(decide(a, b, 3), BootDecision::EnterRescue);
            }
        }
    }
}
