//! End-to-End Simulation and Invariant Integration Tests

use boot_abi::*;
use boot_core::{decide, BootDecision, SlotLoader};
use boot_crypto::{KeyTable, PublicKeySlot, KEYTABLE_MAGIC};
use boot_image::{
    DEFAULT_BOARD_ID, EXPECTED_HDR_SIZE, IMAGE_F_RAM_LOAD, IMAGE_MAGIC, IMAGE_TLV_BOARD_ID,
    IMAGE_TLV_ECDSA256, IMAGE_TLV_INFO_MAGIC, IMAGE_TLV_KEYHASH, IMAGE_TLV_PROT_INFO_MAGIC,
    IMAGE_TLV_SHA256,
};
use boot_journal::SlotJournalState;
use boot_nor::{NorFlash, RamNor, SecureRegion, SeedRecord};
use p256::ecdsa::SigningKey;
use proptest::prelude::*;
use sha2::{Digest, Sha256};
use zerocopy::IntoBytes;

fn create_test_signed_image(
    body: &[u8],
    signing_key: &SigningKey,
) -> (Vec<u8>, [u8; 32], [u8; 32], [u8; 65]) {
    let verifying_key = signing_key.verifying_key();
    let pubkey_point = verifying_key.to_encoded_point(false);
    let mut pubkey_bytes = [0u8; 65];
    pubkey_bytes.copy_from_slice(pubkey_point.as_bytes());

    let mut keyhasher = Sha256::new();
    keyhasher.update(&pubkey_bytes);
    let keyhash: [u8; 32] = keyhasher.finalize().into();

    let img_size = body.len() as u32;
    let prot_tlv_size = 12u16;

    let header = boot_image::ImageHeader {
        ih_magic: IMAGE_MAGIC,
        ih_load_addr: AXI_SRAM_BASE,
        ih_hdr_size: EXPECTED_HDR_SIZE,
        ih_protect_tlv_size: prot_tlv_size,
        ih_img_size: img_size,
        ih_flags: IMAGE_F_RAM_LOAD,
        ih_ver: ImageVersion {
            major: 1,
            minor: 0,
            revision: 0,
            build_num: 1,
        },
        _pad1: 0,
    };

    let mut output = Vec::new();
    output.extend_from_slice(header.as_bytes());
    output.resize(EXPECTED_HDR_SIZE as usize, 0xFF);
    output.extend_from_slice(body);

    // Protected TLV: Board ID
    let prot_info = boot_image::ImageTlvInfo {
        it_magic: IMAGE_TLV_PROT_INFO_MAGIC,
        it_tlv_tot: prot_tlv_size,
    };
    output.extend_from_slice(prot_info.as_bytes());
    let board_entry = boot_image::ImageTlvEntry {
        it_type: IMAGE_TLV_BOARD_ID,
        it_len: 4,
    };
    output.extend_from_slice(board_entry.as_bytes());
    output.extend_from_slice(&DEFAULT_BOARD_ID.to_le_bytes());

    // Digest
    let mut hasher = Sha256::new();
    hasher.update(&output[..]);
    let hash: [u8; 32] = hasher.finalize().into();

    // Signature
    let (signature, _) = signing_key.sign_prehash_recoverable(&hash).unwrap();
    let sig_bytes = signature.to_bytes();

    let unprot_tot = 4 + 4 + 32 + 4 + 32 + 4 + 64;
    let unprot_info = boot_image::ImageTlvInfo {
        it_magic: IMAGE_TLV_INFO_MAGIC,
        it_tlv_tot: unprot_tot as u16,
    };
    output.extend_from_slice(unprot_info.as_bytes());
    output.extend_from_slice(boot_image::ImageTlvEntry { it_type: IMAGE_TLV_SHA256, it_len: 32 }.as_bytes());
    output.extend_from_slice(&hash);
    output.extend_from_slice(boot_image::ImageTlvEntry { it_type: IMAGE_TLV_KEYHASH, it_len: 32 }.as_bytes());
    output.extend_from_slice(&keyhash);
    output.extend_from_slice(boot_image::ImageTlvEntry { it_type: IMAGE_TLV_ECDSA256, it_len: 64 }.as_bytes());
    output.extend_from_slice(&sig_bytes);

    (output, hash, keyhash, pubkey_bytes)
}

#[test]
fn test_end_to_end_nor_load_and_verification() {
    let mut nor = RamNor::new(4 * 1024 * 1024);

    // Program seed record in NOR Security Register #1
    let otp_seed = [0x55u8; 32];
    let seed_rec = SeedRecord::new(otp_seed);
    nor.program_security_register(1, 0, seed_rec.as_bytes()).unwrap();

    let signing_key = SigningKey::from_bytes(&[0x02; 32].into()).unwrap();
    let app_code = [0xEEu8; 1024];
    let (signed_image, _hash, keyhash, pubkey) = create_test_signed_image(&app_code, &signing_key);

    // Write signed image to Slot A
    nor.erase_block_64k(NOR_SLOT_A_OFFSET).unwrap();
    nor.write(NOR_SLOT_A_OFFSET, &signed_image).unwrap();

    // Key table with corresponding public key
    let keytable = KeyTable {
        magic: KEYTABLE_MAGIC,
        version: 1,
        num_keys: 1,
        keys: [
            PublicKeySlot {
                key_id: 1,
                reserved: [0; 3],
                pubkey,
                pubkey_len: 65,
                keyhash,
            },
            PublicKeySlot { key_id: 0, reserved: [0; 3], pubkey: [0; 65], pubkey_len: 0, keyhash: [0; 32] },
            PublicKeySlot { key_id: 0, reserved: [0; 3], pubkey: [0; 65], pubkey_len: 0, keyhash: [0; 32] },
        ],
        reserved: [0; 1024],
    };

    let global_secret = [0x33u8; 32];
    let mcu_uid = [0x11u8; 12];
    let mut ram_buf = vec![0u8; 512 * 1024];

    {
        let mut loader = SlotLoader::new(
            &mut nor,
            &keytable,
            &global_secret,
            mcu_uid,
            DEFAULT_BOARD_ID,
            1,
        );
        let res = loader.load_and_verify(BOOT_SLOT_A, &mut ram_buf, false);
        assert!(res.is_ok(), "Slot A should verify successfully: {:?}", res);
    }

    // Negative test: Corrupt 1 byte in NOR and verify rejection
    nor.mem[(NOR_SLOT_A_OFFSET as usize) + 1024 + 10] ^= 0xFF;

    {
        let mut loader2 = SlotLoader::new(
            &mut nor,
            &keytable,
            &global_secret,
            mcu_uid,
            DEFAULT_BOARD_ID,
            1,
        );
        let corrupt_res = loader2.load_and_verify(BOOT_SLOT_A, &mut ram_buf, false);
        assert!(corrupt_res.is_err(), "Corrupted image must be rejected");
        assert_eq!(ram_buf[1024 + 10], 0, "RAM must be zeroized on verification failure");
    }
}

proptest! {
    #[test]
    fn prop_decision_invariants(
        state_a in 0u8..6u8,
        trial_a in 0u8..5u8,
        state_b in 0u8..6u8,
        trial_b in 0u8..5u8,
        max_trials in 1u8..4u8,
    ) {
        let slot_a = SlotJournalState {
            state: state_a,
            trial: trial_a,
            flags: 0,
            img_ver: ImageVersion { major: 1, minor: 0, revision: 0, build_num: 1 },
            min_version: 0,
            key_revoke_mask: 0,
        };

        let slot_b = SlotJournalState {
            state: state_b,
            trial: trial_b,
            flags: 0,
            img_ver: ImageVersion { major: 2, minor: 0, revision: 0, build_num: 1 },
            min_version: 0,
            key_revoke_mask: 0,
        };

        let decision = decide(&slot_a, &slot_b, max_trials);

        match decision {
            BootDecision::TryCandidates { primary, fallback } => {
                let s_pri = if primary == 0 { &slot_a } else { &slot_b };
                prop_assert_ne!(s_pri.state, SLOT_STATE_INVALID);
                prop_assert_ne!(s_pri.state, SLOT_STATE_EMPTY);

                if s_pri.state == SLOT_STATE_TESTING {
                    prop_assert!(s_pri.trial < max_trials);
                }

                if let Some(fb) = fallback {
                    let s_fb = if fb == 0 { &slot_a } else { &slot_b };
                    prop_assert_ne!(s_fb.state, SLOT_STATE_INVALID);
                    prop_assert_ne!(s_fb.state, SLOT_STATE_EMPTY);
                }
            }
            BootDecision::EnterRescue => {
                let a_ok = slot_a.state == SLOT_STATE_CONFIRMED || slot_a.state == SLOT_STATE_PENDING
                    || (slot_a.state == SLOT_STATE_TESTING && slot_a.trial < max_trials);
                let b_ok = slot_b.state == SLOT_STATE_CONFIRMED || slot_b.state == SLOT_STATE_PENDING
                    || (slot_b.state == SLOT_STATE_TESTING && slot_b.trial < max_trials);
                prop_assert!(!a_ok && !b_ok);
            }
        }
    }
}
