//! Build & Packaging Automation Tool (xtask)
//!
//! Subcommands:
//! - `sign`: Prepend MCUboot header, calculate SHA-256, sign with ECDSA P-256, append TLVs
//! - `pad`: Pad a raw binary to a given size
//! - `corrupt`: Flip one body bit in a signed image (test input)
//! - `encrypt`: Encrypt image body with XChaCha20 transit key, append ENC_INFO TLV
//! - `size-check`: Verify compiled bootloader ELF size against 96KB budget
//! - `gen-keys`: Generate EC key pairs and cryptographic secrets

use clap::{Parser, Subcommand};
use p256::ecdsa::SigningKey;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use zerocopy::{FromBytes, IntoBytes};

use boot_abi::ImageVersion;
use boot_image::{
    EncInfoTlv, ImageHeader, ImageTlvEntry, ImageTlvInfo, ENC_SCHEME_PROJECT,
    EXPECTED_HDR_SIZE, IMAGE_F_ENCRYPTED, IMAGE_F_RAM_LOAD, IMAGE_MAGIC, IMAGE_TLV_BOARD_ID,
    IMAGE_TLV_ECDSA256, IMAGE_TLV_ENC_INFO, IMAGE_TLV_INFO_MAGIC, IMAGE_TLV_KEYHASH,
    IMAGE_TLV_PROT_INFO_MAGIC, IMAGE_TLV_SHA256,
};
use boot_layout::Layout;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Target board (selects the memory layout, board id and load address).
    #[arg(long, global = true, default_value = "h750-netlite")]
    board: String,
    #[command(subcommand)]
    command: Commands,
}

fn parse_hex_byte(s: &str) -> Result<u8, String> {
    u8::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| format!("not a hex byte: {e}"))
}

/// `data` extended to `size` bytes with `fill`; an error if it is already longer.
fn pad_to(mut data: Vec<u8>, size: usize, fill: u8) -> Result<Vec<u8>, String> {
    if data.len() > size {
        return Err(format!("already {} bytes, larger than {}", data.len(), size));
    }
    data.resize(size, fill);
    Ok(data)
}

/// A copy of a signed image with one bit of its body flipped, so the hash no longer matches, and the
/// body offset that was changed (default: the middle of the body). The body is found from the
/// header (`ih_hdr_size`, `ih_img_size`), so this works for any version and any image size.
fn corrupt_image(data: &[u8], offset: Option<usize>) -> Result<(Vec<u8>, usize), String> {
    let (hdr, _) = ImageHeader::read_from_prefix(data)
        .map_err(|_| "file too small to be a signed image".to_string())?;
    if hdr.ih_magic != IMAGE_MAGIC {
        return Err("not a signed image (bad header magic)".into());
    }
    let (hdr_size, img_size) = (hdr.ih_hdr_size as usize, hdr.ih_img_size as usize);
    if hdr_size.checked_add(img_size).map_or(true, |end| end > data.len()) {
        return Err(format!(
            "the header announces a {hdr_size}-byte header and a {img_size}-byte body, the file has {} bytes",
            data.len()
        ));
    }
    let off = offset.unwrap_or(img_size / 2);
    if off >= img_size {
        return Err(format!("offset {off} is outside the image body ({img_size} bytes)"));
    }
    let mut out = data.to_vec();
    out[hdr_size + off] ^= 0x01;
    Ok((out, off))
}

/// Resolve `--board` to its layout, listing the known boards on a typo.
fn resolve_board(name: &str) -> Result<&'static Layout, Box<dyn std::error::Error>> {
    boot_layout::by_name(name).ok_or_else(|| {
        let known: Vec<&str> = boot_layout::ALL.iter().map(|l| l.name).collect();
        format!("unknown board '{}' (known: {})", name, known.join(", ")).into()
    })
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
    /// Pad a raw binary with a fill byte (default 0xFF) up to `size` bytes (to make an image larger than a threshold)
    Pad {
        #[arg(short, long)]
        bin: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// Target size in bytes
        #[arg(long)]
        size: usize,
        /// Fill byte, in hex (default FF, the erased value of a NOR flash)
        #[arg(long, default_value = "ff", value_parser = parse_hex_byte)]
        fill: u8,
    },
    /// Flip one bit in the body of a signed image so its hash no longer matches
    Corrupt {
        #[arg(short, long)]
        input: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// Offset into the image body (after the header); default is the middle
        #[arg(long)]
        offset: Option<usize>,
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
    let layout = resolve_board(&cli.board)?;

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
            println!("[xtask] Version: {}.{}.{}+{}", major, minor, rev, build);
            let load_addr = layout
                .load_addr()
                .ok_or("selected board does not use a RAM-load execution model")?;
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
                ih_load_addr: load_addr,
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
            output.extend_from_slice(&layout.board_id.to_le_bytes());

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
        Commands::Pad { bin, out, size, fill } => {
            let data = pad_to(fs::read(&bin)?, size, fill).map_err(|e| format!("{:?}: {}", bin, e))?;
            fs::write(&out, &data)?;
            println!("[xtask] Padded {:?} to {} bytes with 0x{:02X} -> {:?}", bin, size, fill, out);
        }
        Commands::Corrupt { input, out, offset } => {
            let (data, off) = corrupt_image(&fs::read(&input)?, offset)
                .map_err(|e| format!("{:?}: {}", input, e))?;
            fs::write(&out, &data)?;
            println!("[xtask] Flipped one bit at body offset {} -> {:?}", off, out);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The 32 header bytes `xtask sign --major 4 --minor 2 --rev 3 --build 7` wrote for a
    /// 4096-byte binary (taken from the tool's own output): hdr_size 0x400, protect_tlv_size 12,
    /// img_size 0x1000, flags 0x20, version 4.2.3+7. The version is deliberately not 1.0.0+1.
    const HDR: [u8; 32] = [
        0x3d, 0xb8, 0xf3, 0x96, 0x00, 0x00, 0x00, 0x24, 0x00, 0x04, 0x0c, 0x00, 0x00, 0x10, 0x00, 0x00,
        0x20, 0x00, 0x00, 0x00, 0x04, 0x02, 0x03, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    /// A signed-image-shaped file: header, zeros up to 1024, `body` bytes, 100 trailer bytes.
    fn image(hdr: [u8; 32], body: usize) -> Vec<u8> {
        let mut v = hdr.to_vec();
        v[12..16].copy_from_slice(&(body as u32).to_le_bytes());
        v.resize(1024, 0);
        v.extend((0..body).map(|i| (i % 251) as u8));
        v.extend([0xEE; 100]);
        v
    }

    fn differing(a: &[u8], b: &[u8]) -> Vec<usize> {
        a.iter().zip(b).enumerate().filter(|(_, (x, y))| x != y).map(|(i, _)| i).collect()
    }

    #[test]
    fn corrupt_flips_one_bit_in_the_middle_of_the_body() {
        let img = image(HDR, 4096);
        let (out, off) = corrupt_image(&img, None).unwrap();
        assert_eq!(off, 2048);
        assert_eq!(differing(&img, &out), vec![1024 + 2048]);
        assert_eq!((img[1024 + 2048] ^ out[1024 + 2048]).count_ones(), 1);
        assert_eq!(out.len(), img.len());
    }

    #[test]
    fn the_body_is_found_from_the_header_whatever_the_version_is() {
        // The first version of this read the u32 at byte 20 as the body size. That is the start of
        // the version: 1.0.0 read as 1 (so the first body byte was flipped, which happened to work)
        // and 4.2.3 as 197124 (so the tool refused the image).
        for ver in [[1u8, 0, 0, 0, 1, 0, 0, 0], [4, 2, 3, 0, 7, 0, 0, 0], [255, 255, 255, 255, 255, 255, 255, 255]] {
            let mut hdr = HDR;
            hdr[20..28].copy_from_slice(&ver);
            let img = image(hdr, 3480);
            let (out, off) = corrupt_image(&img, None).unwrap();
            assert_eq!(off, 1740, "version {ver:?}");
            assert_eq!(differing(&img, &out), vec![1024 + 1740], "version {ver:?}");
        }
    }

    #[test]
    fn corrupt_changes_the_body_only_for_any_image_size() {
        for body in [1usize, 2, 255, 4096, 327_680] {
            let img = image(HDR, body);
            let (out, off) = corrupt_image(&img, None).unwrap();
            let d = differing(&img, &out);
            assert_eq!(d, vec![1024 + body / 2], "body {body}");
            assert_eq!(off, body / 2);
            assert!(d[0] >= 1024 && d[0] < 1024 + body, "inside the body, not the header or the trailer");
        }
    }

    #[test]
    fn an_explicit_offset_is_used_and_checked_against_the_body() {
        let img = image(HDR, 4096);
        assert_eq!(differing(&img, &corrupt_image(&img, Some(0)).unwrap().0), vec![1024]);
        assert_eq!(differing(&img, &corrupt_image(&img, Some(4095)).unwrap().0), vec![1024 + 4095]);
        assert!(corrupt_image(&img, Some(4096)).is_err(), "one past the body");
        assert!(corrupt_image(&img, Some(usize::MAX)).is_err());
    }

    #[test]
    fn corrupt_refuses_what_is_not_a_signed_image() {
        // too short for a header
        assert!(corrupt_image(&[0u8; 20], None).is_err());
        // wrong magic
        let mut bad = image(HDR, 4096);
        bad[0] ^= 0xFF;
        assert!(corrupt_image(&bad, None).unwrap_err().contains("magic"));
        // the body it announces is not all there
        let short = &image(HDR, 4096)[..1024 + 4000];
        assert!(corrupt_image(short, None).is_err());
        // a body size of 0, and one that would overflow the sum
        let mut zero = image(HDR, 4096);
        zero[12..16].copy_from_slice(&0u32.to_le_bytes());
        assert!(corrupt_image(&zero, None).is_err());
        let mut huge = image(HDR, 4096);
        huge[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(corrupt_image(&huge, None).is_err());
    }

    #[test]
    fn pad_fills_with_the_byte_and_keeps_the_start() {
        assert_eq!(pad_to(vec![1, 2, 3], 6, 0xA5).unwrap(), vec![1, 2, 3, 0xA5, 0xA5, 0xA5]);
        assert_eq!(pad_to(vec![1, 2, 3], 3, 0x00).unwrap(), vec![1, 2, 3]);
        assert_eq!(pad_to(vec![], 2, 0xFF).unwrap(), vec![0xFF, 0xFF]);
        assert!(pad_to(vec![1, 2, 3, 4], 3, 0xFF).unwrap_err().contains("larger than 3"));
    }

    #[test]
    fn the_two_big_image_fills_cannot_leave_a_stale_block_looking_fine() {
        // 0xA5 and 0x5A are what `make test-img-big` pads with: flash programming ANDs, so any
        // byte of a stale first image under the second is 0x00, never either fill.
        for (a, b) in (0u8..=255).map(|_| (0xA5u8, 0x5Au8)).take(1) {
            assert_eq!(a & b, 0x00);
            assert_ne!(a, 0x00);
            assert_ne!(b, 0x00);
        }
        // an 0xFF fill could not show a stale block: erased flash already reads 0xFF
        assert_eq!(0xFFu8 & 0xA5, 0xA5);
    }
}
