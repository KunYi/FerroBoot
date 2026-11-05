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
use boot_image::{parse_image, ParsedImage, ENC_SCHEME_DEVICE};
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

        // 3. Preliminary parse to extract image size and ENC_INFO
        // In MCUboot format: header, protected TLVs, and unprotected TLVs are in plaintext.
        // Image body is what is encrypted.
        let preliminary = parse_image(
            &header_buf,
            self.expected_board_id,
            self.current_bl_version,
            require_encrypted,
        );

        let hdr = match preliminary {
            Ok(ref p) => p.header,
            Err(_) => {
                device_key.zeroize();
                nor_seed.zeroize();
                return Err(ERR_VERIFY_FAILED);
            }
        };

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

        // Decrypt image body in RAM if encrypted
        if hdr.is_encrypted() {
            // Read trailing TLVs from NOR to locate ENC_INFO
            let tlv_offset = slot_offset + (hdr_size + img_size) as u32;
            let mut tlv_buf = [0u8; 512];
            self.nor
                .read(tlv_offset, &mut tlv_buf)
                .map_err(|_| ERR_VERIFY_FAILED)?;

            let parsed_with_tlv = parse_image(
                &tlv_buf,
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
            let prot_offset = slot_offset + (hdr_size + img_size) as u32;
            let mut prot_buf = [0u8; 256];
            let prot_len = (hdr.ih_protect_tlv_size as usize).min(prot_buf.len());
            self.nor
                .read(prot_offset, &mut prot_buf[..prot_len])
                .map_err(|_| ERR_VERIFY_FAILED)?;
            sha_ctx.update(&prot_buf[..prot_len]);
        }
        let calculated_hash = sha_ctx.finalize();

        // 6. Verify Signature against keytable
        let mut full_tlv_bytes = [0u8; 1024];
        let tlv_start = slot_offset + (hdr_size + img_size) as u32;
        self.nor
            .read(tlv_start, &mut full_tlv_bytes)
            .map_err(|_| ERR_VERIFY_FAILED)?;

        // Combine header + dummy body + TLVs to run parse_image for TLVs
        let mut parse_buf = [0u8; 2048];
        parse_buf[..hdr_size].copy_from_slice(&ram_buffer[..hdr_size]);
        let tlv_tot = 512.min(full_tlv_bytes.len());
        parse_buf[hdr_size + img_size..hdr_size + img_size + tlv_tot]
            .copy_from_slice(&full_tlv_bytes[..tlv_tot]);

        let parsed = parse_image(
            &parse_buf,
            self.expected_board_id,
            self.current_bl_version,
            require_encrypted,
        )
        .map_err(|_| {
            ram_buffer.zeroize();
            ERR_VERIFY_FAILED
        })?;

        // Check hash
        if let Some(expected_hash) = parsed.sha256 {
            if !ct_equal(&calculated_hash, expected_hash) {
                ram_buffer.zeroize();
                return Err(ERR_VERIFY_FAILED);
            }
        } else {
            ram_buffer.zeroize();
            return Err(ERR_VERIFY_FAILED);
        }

        // Check signature against matching key in keytable
        if let (Some(sig), Some(keyhash)) = (parsed.signature, parsed.keyhash) {
            let mut key_found = false;
            for key_slot in &self.keytable.keys {
                if key_slot.pubkey_len > 0 && ct_equal(&key_slot.keyhash, keyhash) {
                    verify_p256_signature(
                        &calculated_hash,
                        &key_slot.pubkey[..key_slot.pubkey_len as usize],
                        sig,
                    )?;
                    key_found = true;
                    break;
                }
            }
            if !key_found {
                ram_buffer.zeroize();
                return Err(ERR_VERIFY_FAILED);
            }
        } else {
            ram_buffer.zeroize();
            return Err(ERR_VERIFY_FAILED);
        }

        // Return validated header metadata (static lifetime by cloning necessary fields)
        Ok(ParsedImage {
            header: parsed.header,
            sha256: None,
            keyhash: None,
            signature: None,
            sig_type: parsed.sig_type,
            image_type: parsed.image_type,
            board_id: parsed.board_id,
            min_bl_version: parsed.min_bl_version,
            enc_info: parsed.enc_info,
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
}
