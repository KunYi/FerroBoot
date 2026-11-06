//! Cryptographic backend for STM32H750 Bootloader
//!
//! Provides SHA-256 hashing, ECDSA P-256 signature verification,
//! HKDF-SHA256 key derivation, XChaCha20 stream cipher seek/crypto,
//! and public key/secret table management.

#![no_std]

use boot_abi::{ERR_DECRYPT_FAILED, ERR_INVALID_PARAM, ERR_VERIFY_FAILED};
use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use chacha20::XChaCha20;
use hkdf::Hkdf;
use p256::ecdsa::signature::hazmat::PrehashVerifier;
use p256::ecdsa::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const KEYTABLE_MAGIC: u32 = 0x4B54_424C; // 'KTBL'
pub const SECRETS_MAGIC: u32 = 0x5345_4352; // 'SECR'

pub const MAX_PUBLIC_KEYS: usize = 3;
pub const MAX_PROJECT_KEYS: usize = 2;

/// A single public key slot in Flash.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicKeySlot {
    pub key_id: u8,
    pub reserved: [u8; 3],
    /// SEC1 encoded uncompressed public key (0x04 || X || Y = 65 bytes) or compressed (33 bytes).
    pub pubkey: [u8; 65],
    pub pubkey_len: u8,
    /// Pre-computed SHA-256 of the public key bytes for fast lookup against MCUboot KEYHASH TLV.
    pub keyhash: [u8; 32],
}

/// The public key table located in Flash at 0x0801F800 (2KB).
#[repr(C)]
pub struct KeyTable {
    pub magic: u32,
    pub version: u16,
    pub num_keys: u16,
    pub keys: [PublicKeySlot; MAX_PUBLIC_KEYS],
    pub reserved: [u8; 1024],
}

/// Project transit keys & global secret in Flash at 0x0801F000 (2KB).
#[repr(C)]
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretSlot {
    pub key_id: u8,
    pub active: u8,
    pub reserved: [u8; 2],
    pub key: [u8; 32],
}

#[repr(C)]
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretsTable {
    pub magic: u32,
    pub version: u16,
    pub reserved: [u8; 2],
    pub global_secret: [u8; 32],
    pub project_keys: [SecretSlot; MAX_PROJECT_KEYS],
}

/// Compute SHA-256 over a contiguous slice.
pub fn sha256_digest(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Incremental SHA-256 calculator.
#[derive(Clone)]
pub struct Sha256Context {
    hasher: Sha256,
}

impl Sha256Context {
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
        }
    }

    pub fn update(&mut self, chunk: &[u8]) {
        self.hasher.update(chunk);
    }

    pub fn finalize(self) -> [u8; 32] {
        self.hasher.finalize().into()
    }

    pub fn finalize_clone(&self) -> [u8; 32] {
        self.hasher.clone().finalize().into()
    }
}

/// Verifies an ECDSA P-256 signature against an uncompressed or compressed SEC1 public key.
///
/// `hash`: 32-byte SHA-256 digest
/// `pubkey_bytes`: SEC1 bytes (e.g. 65 bytes uncompressed: 0x04 || X || Y)
/// `sig_bytes`: 64 bytes (raw r || s) or ASN.1 DER format.
pub fn verify_p256_signature(
    hash: &[u8; 32],
    pubkey_bytes: &[u8],
    sig_bytes: &[u8],
) -> Result<(), i32> {
    let verifying_key = VerifyingKey::from_sec1_bytes(pubkey_bytes)
        .map_err(|_| ERR_INVALID_PARAM)?;

    let signature = if sig_bytes.len() == 64 {
        Signature::from_slice(sig_bytes).map_err(|_| ERR_INVALID_PARAM)?
    } else {
        Signature::from_der(sig_bytes).map_err(|_| ERR_INVALID_PARAM)?
    };

    verifying_key
        .verify_prehash(hash, &signature)
        .map_err(|_| ERR_VERIFY_FAILED)?;

    Ok(())
}

/// Constant-time comparison for digests and keys.
pub fn ct_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Derives the device-unique storage encryption key (D-15).
///
/// `DeviceKey = HKDF-SHA256(ikm = global_secret || nor_otp_seed, salt = mcu_uid || jedec_id || nor_uid_len || nor_uid, info = "dev-enc-v1")`
pub fn derive_device_key(
    global_secret: &[u8; 32],
    nor_otp_seed: &[u8; 32],
    mcu_uid: &[u8; 12],
    jedec_id: &[u8; 3],
    nor_uid: &[u8], // 8 or 16 bytes
    out_key: &mut [u8; 32],
) -> Result<(), i32> {
    let mut ikm = [0u8; 64];
    ikm[..32].copy_from_slice(global_secret);
    ikm[32..].copy_from_slice(nor_otp_seed);

    let mut salt = [0u8; 32];
    // salt = mcu_uid (12B) || jedec_id (3B) || nor_uid_len (1B) || nor_uid (up to 16B)
    salt[..12].copy_from_slice(mcu_uid);
    salt[12..15].copy_from_slice(jedec_id);
    salt[15] = nor_uid.len() as u8;
    let uid_len = nor_uid.len().min(16);
    salt[16..16 + uid_len].copy_from_slice(&nor_uid[..uid_len]);

    let hk = Hkdf::<Sha256>::new(Some(&salt[..16 + uid_len]), &ikm);
    let info = b"dev-enc-v1";

    let res = hk
        .expand(info, out_key)
        .map_err(|_| ERR_DECRYPT_FAILED);

    ikm.zeroize();
    res
}

/// Derives the in-transit OTA decryption key (D-14).
///
/// `TransitKey = HKDF-SHA256(ikm = project_key, salt = build_nonce, info = "ota-v1")`
pub fn derive_transit_key(
    project_key: &[u8; 32],
    build_nonce: &[u8; 24],
    out_key: &mut [u8; 32],
) -> Result<(), i32> {
    let hk = Hkdf::<Sha256>::new(Some(build_nonce), project_key);
    let info = b"ota-v1";
    hk.expand(info, out_key).map_err(|_| ERR_DECRYPT_FAILED)
}

/// In-place XChaCha20 stream encryption/decryption at an arbitrary block offset.
///
/// Since XChaCha20 is a stream cipher (XOR with keystream), encryption and decryption are identical.
pub fn xchacha20_process(
    key: &[u8; 32],
    nonce: &[u8; 24],
    offset: u32,
    buffer: &mut [u8],
) -> Result<(), i32> {
    let mut cipher = XChaCha20::new(key.into(), nonce.into());
    cipher.seek(offset as u64);
    cipher.apply_keystream(buffer);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_incremental() {
        let data = b"Hello, STM32H750 Bootloader!";
        let single = sha256_digest(data);

        let mut ctx = Sha256Context::new();
        ctx.update(&data[..10]);
        ctx.update(&data[10..]);
        let inc = ctx.finalize();

        assert_eq!(single, inc);
    }

    #[test]
    fn test_xchacha20_roundtrip_with_seek() {
        let key = [0x42u8; 32];
        let nonce = [0x17u8; 24];

        let plaintext = [0x55u8; 128];
        let mut cipher_buf = plaintext;

        // Encrypt first 64 bytes at offset 0
        xchacha20_process(&key, &nonce, 0, &mut cipher_buf[..64]).unwrap();
        // Encrypt next 64 bytes at offset 64
        xchacha20_process(&key, &nonce, 64, &mut cipher_buf[64..]).unwrap();

        assert_ne!(cipher_buf, plaintext);

        // Decrypt entire 128 bytes starting at offset 0
        let mut decrypted = cipher_buf;
        xchacha20_process(&key, &nonce, 0, &mut decrypted).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_key_derivation_deterministic() {
        let gs = [1u8; 32];
        let seed = [2u8; 32];
        let uid = [3u8; 12];
        let jedec = [0x68, 0x40, 0x16];
        let nor_uid = [0xAA; 16];

        let mut k1 = [0u8; 32];
        let mut k2 = [0u8; 32];

        derive_device_key(&gs, &seed, &uid, &jedec, &nor_uid, &mut k1).unwrap();
        derive_device_key(&gs, &seed, &uid, &jedec, &nor_uid, &mut k2).unwrap();

        assert_eq!(k1, k2);
        assert_ne!(k1, [0u8; 32]);
    }
}
