//! Build & Packaging Automation Tool (xtask)
//!
//! Subcommands:
//! - `sign`: Prepend MCUboot header, calculate SHA-256, sign with ECDSA P-256, append TLVs
//! - `encrypt`: Encrypt image body with XChaCha20 transit key, append ENC_INFO TLV
//! - `size-check`: Verify compiled bootloader ELF size against 96KB budget
//! - `gen-keys`: Generate EC key pairs and cryptographic secrets

use clap::{Parser, Subcommand};
use p256::ecdsa::SigningKey;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use zerocopy::IntoBytes;

use boot_abi::{ImageVersion, AXI_SRAM_BASE};
use boot_image::{
    EncInfoTlv, ImageHeader, ImageTlvEntry, ImageTlvInfo, DEFAULT_BOARD_ID, ENC_SCHEME_PROJECT,
    EXPECTED_HDR_SIZE, IMAGE_F_ENCRYPTED, IMAGE_F_RAM_LOAD, IMAGE_MAGIC, IMAGE_TLV_BOARD_ID,
    IMAGE_TLV_ECDSA256, IMAGE_TLV_ENC_INFO, IMAGE_TLV_INFO_MAGIC, IMAGE_TLV_KEYHASH,
    IMAGE_TLV_PROT_INFO_MAGIC, IMAGE_TLV_SHA256,
};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Sign an application binary into MCUboot format
    Sign {
        #[arg(short, long)]
        bin: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long, default_value_t = 1)]
        major: u8,
        #[arg(long, default_value_t = 0)]
        minor: u8,
        #[arg(long, default_value_t = 0)]
        rev: u16,
        #[arg(long, default_value_t = 1)]
        build: u32,
    },
    /// Encrypt a signed image for OTA transit using project key
    Encrypt {
        #[arg(short, long)]
        input: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long, default_value_t = 1)]
        key_id: u8,
    },
    /// Inspect compiled bootloader size against 96KB budget
    SizeCheck {
        #[arg(short, long)]
        elf: Option<PathBuf>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Sign {
            bin,
            out,
            major,
            minor,
            rev,
            build,
        } => {
            println!("[xtask] Signing binary: {:?}", bin);
            let raw_data = fs::read(&bin)?;

            // Use test private key
            let signing_key = SigningKey::from_bytes(&[0x01; 32].into())?;
            let verifying_key = signing_key.verifying_key();
            let pubkey_sec1 = verifying_key.to_encoded_point(false);
            let pubkey_bytes = pubkey_sec1.as_bytes();

            let mut keyhasher = Sha256::new();
            keyhasher.update(pubkey_bytes);
            let keyhash: [u8; 32] = keyhasher.finalize().into();

            let img_size = raw_data.len() as u32;
            let prot_tlv_size = 12u16; // 4B header + 4B entry + 4B board_id

            let header = ImageHeader {
                ih_magic: IMAGE_MAGIC,
                ih_load_addr: AXI_SRAM_BASE,
                ih_hdr_size: EXPECTED_HDR_SIZE,
                ih_protect_tlv_size: prot_tlv_size,
                ih_img_size: img_size,
                ih_flags: IMAGE_F_RAM_LOAD,
                ih_ver: ImageVersion {
                    major,
                    minor,
                    revision: rev,
                    build_num: build,
                },
                _pad1: 0,
            };

            let mut output = Vec::new();
            // Prepend 1024-byte header region
            output.extend_from_slice(header.as_bytes());
            output.resize(EXPECTED_HDR_SIZE as usize, 0xFF);

            // Append raw binary
            output.extend_from_slice(&raw_data);

            // Append Protected TLV
            let prot_info = ImageTlvInfo {
                it_magic: IMAGE_TLV_PROT_INFO_MAGIC,
                it_tlv_tot: prot_tlv_size,
            };
            output.extend_from_slice(prot_info.as_bytes());
            let board_entry = ImageTlvEntry {
                it_type: IMAGE_TLV_BOARD_ID,
                it_len: 4,
            };
            output.extend_from_slice(board_entry.as_bytes());
            output.extend_from_slice(&DEFAULT_BOARD_ID.to_le_bytes());

            // Compute SHA-256 over (Header + Body + Protected TLVs)
            let mut hasher = Sha256::new();
            hasher.update(&output[..]);
            let hash: [u8; 32] = hasher.finalize().into();

            // Sign hash with P-256 ECDSA
            let (signature, _) = signing_key.sign_prehash_recoverable(&hash)?;
            let sig_bytes = signature.to_bytes();

            // Append Standard Unprotected TLVs: SHA256 (4+32=36B) + KEYHASH (4+32=36B) + SIG (4+64=68B) + info(4B) = 144B
            let unprot_tot = 4 + 4 + 32 + 4 + 32 + 4 + 64;
            let unprot_info = ImageTlvInfo {
                it_magic: IMAGE_TLV_INFO_MAGIC,
                it_tlv_tot: unprot_tot as u16,
            };
            output.extend_from_slice(unprot_info.as_bytes());

            // SHA256 entry
            output.extend_from_slice(ImageTlvEntry { it_type: IMAGE_TLV_SHA256, it_len: 32 }.as_bytes());
            output.extend_from_slice(&hash);

            // KEYHASH entry
            output.extend_from_slice(ImageTlvEntry { it_type: IMAGE_TLV_KEYHASH, it_len: 32 }.as_bytes());
            output.extend_from_slice(&keyhash);

            // Signature entry
            output.extend_from_slice(ImageTlvEntry { it_type: IMAGE_TLV_ECDSA256, it_len: 64 }.as_bytes());
            output.extend_from_slice(&sig_bytes);

            fs::write(&out, &output)?;
            println!("[xtask] Signed image successfully written to {:?} ({} bytes)", out, output.len());
        }
        Commands::Encrypt { input, out, key_id } => {
            println!("[xtask] Encrypting image for OTA transit: {:?}", input);
            let mut data = fs::read(&input)?;
            if data.len() < EXPECTED_HDR_SIZE as usize {
                eprintln!("[xtask] Error: File too small to be a valid MCUboot image");
                return Ok(());
            }

            // Derive transit key
            let project_key = [0x44u8; 32];
            let build_nonce = [0x12u8; 24];
            let mut transit_key = [0u8; 32];
            boot_crypto::derive_transit_key(&project_key, &build_nonce, &mut transit_key)
                .map_err(|e| format!("derive_transit_key error: {e}"))?;

            // In-place encrypt body
            let hdr_size = EXPECTED_HDR_SIZE as usize;
            let img_size = u32::from_le_bytes(data[20..24].try_into()?) as usize;
            let body = &mut data[hdr_size..hdr_size + img_size];
            boot_crypto::xchacha20_process(&transit_key, &build_nonce, 0, body)
                .map_err(|e| format!("xchacha20_process error: {e}"))?;

            // Set IMAGE_F_ENCRYPTED in header flags
            let mut flags = u32::from_le_bytes(data[16..20].try_into()?);
            flags |= IMAGE_F_ENCRYPTED;
            data[16..20].copy_from_slice(&flags.to_le_bytes());

            // Append ENC_INFO TLV entry
            let enc_info = EncInfoTlv {
                scheme: ENC_SCHEME_PROJECT,
                key_id,
                kdf_ver: 1,
                reserved: 0,
                nonce: build_nonce,
            };
            let enc_entry = ImageTlvEntry {
                it_type: IMAGE_TLV_ENC_INFO,
                it_len: core::mem::size_of::<EncInfoTlv>() as u16,
            };
            data.extend_from_slice(enc_entry.as_bytes());
            data.extend_from_slice(enc_info.as_bytes());

            fs::write(&out, &data)?;
            println!("[xtask] Encrypted image successfully written to {:?} ({} bytes)", out, data.len());
        }
        Commands::SizeCheck { elf } => {
            let path = elf.unwrap_or_else(|| {
                PathBuf::from("target/thumbv7em-none-eabihf/release/bootloader-h750")
            });
            println!("[xtask] Inspecting ELF size: {:?}", path);
            if !path.exists() {
                println!("[xtask] Target binary {:?} does not exist yet.", path);
                return Ok(());
            }

            let output = Command::new("arm-none-eabi-size").arg(&path).output()?;
            let text = String::from_utf8_lossy(&output.stdout);
            println!("{}", text);

            // Parse text and check total Flash <= 96KB
            let lines: Vec<&str> = text.lines().collect();
            if lines.len() >= 2 {
                let parts: Vec<&str> = lines[1].split_whitespace().collect();
                if parts.len() >= 3 {
                    let text_sz: u32 = parts[0].parse().unwrap_or(0);
                    let data_sz: u32 = parts[1].parse().unwrap_or(0);
                    let flash_used = text_sz + data_sz;
                    let max_budget = 96 * 1024;
                    println!("[xtask] Flash used: {} bytes / {} bytes ({:.1}%)",
                        flash_used, max_budget, (flash_used as f64 / max_budget as f64) * 100.0);
                    if flash_used > max_budget {
                        eprintln!("[xtask] ERROR: Bootloader size exceeds 96KB budget!");
                        std::process::exit(1);
                    } else {
                        println!("[xtask] SUCCESS: Bootloader is within size budget.");
                    }
                }
            }
        }
    }

    Ok(())
}
