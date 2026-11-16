//! MCUboot-compatible Image Header and TLV Parsing
//!
//! Provides zero-copy, bounds-checked parsing for firmware images stored in NOR flash.

#![no_std]

use boot_abi::ImageVersion;
use boot_layout::{ExecModel, Layout};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub const IMAGE_MAGIC: u32 = 0x96f3_b83d;
pub const EXPECTED_HDR_SIZE: u16 = 0x0400; // 1024 bytes (0x400)
pub const HEADER_STRUCT_SIZE: usize = 32;

// Header flags
pub const IMAGE_F_RAM_LOAD: u32 = 0x0000_0020;
pub const IMAGE_F_ENCRYPTED: u32 = 0x0001_0000; // Private custom flag for encrypted payloads

// TLV info magics
pub const IMAGE_TLV_INFO_MAGIC: u16 = 0x6907; // Standard unprotected TLV
pub const IMAGE_TLV_PROT_INFO_MAGIC: u16 = 0x6908; // Protected TLV

// Standard MCUboot TLV types
pub const IMAGE_TLV_KEYHASH: u16 = 0x0001; // SHA-256 of public key (32 bytes)
pub const IMAGE_TLV_SHA256: u16 = 0x0010; // Image SHA-256 (32 bytes)
pub const IMAGE_TLV_ECDSA256: u16 = 0x0022; // ECDSA P-256 signature (64 bytes)
pub const IMAGE_TLV_ED25519: u16 = 0x0024; // Ed25519 signature (64 bytes)

// Custom Protected TLV types
pub const IMAGE_TLV_IMAGE_TYPE: u16 = 0x0040; // 4 bytes u32 (0 = App, 1 = Bootloader)
pub const IMAGE_TLV_BOARD_ID: u16 = 0x0041; // 4 bytes u32 (Board Identifier)
pub const IMAGE_TLV_MIN_BL_VERSION: u16 = 0x0042; // 4 bytes u32 (Min BL version required)

// Custom Unprotected TLV types
pub const IMAGE_TLV_ENC_INFO: u16 = 0x0050; // Scheme (1B), KeyID (1B), KDFVer (1B), Rsvd (1B), Nonce (24B)

pub const ENC_SCHEME_PROJECT: u8 = 1; // OTA in-transit encryption
pub const ENC_SCHEME_DEVICE: u8 = 2; // Device storage NOR encryption

pub const IMAGE_TYPE_APP: u32 = 0;
pub const IMAGE_TYPE_BOOTLOADER: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageParseError {
    BufferTooShort,
    BadMagic,
    InvalidHeaderSize,
    InvalidLoadAddress,
    MissingRamLoadFlag,
    ImageTooLarge,
    BadTlvMagic,
    TlvLengthOverflow,
    CorruptedTlvEntry,
    TlvLimitExceeded,
    UnsupportedImageType,
    BoardIdMismatch,
    MinBlVersionNotMet,
    MissingRequiredTlv,
    UnencryptedImageNotAllowed,
    EncryptionSchemeMismatch,
}

/// MCUboot 32-byte header.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct ImageHeader {
    pub ih_magic: u32,
    pub ih_load_addr: u32,
    pub ih_hdr_size: u16,
    pub ih_protect_tlv_size: u16,
    pub ih_img_size: u32,
    pub ih_flags: u32,
    pub ih_ver: ImageVersion,
    pub _pad1: u32,
}

/// TLV section header (4 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct ImageTlvInfo {
    pub it_magic: u16,
    pub it_tlv_tot: u16,
}

/// TLV entry header (4 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct ImageTlvEntry {
    pub it_type: u16,
    pub it_len: u16,
}

/// Custom encryption info payload (28 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct EncInfoTlv {
    pub scheme: u8,
    pub key_id: u8,
    pub kdf_ver: u8,
    pub reserved: u8,
    pub nonce: [u8; 24],
}

/// Parsed image metadata containing header and validated TLV references.
#[derive(Debug, Clone)]
pub struct ParsedImage<'a> {
    pub header: ImageHeader,
    pub sha256: Option<&'a [u8; 32]>,
    pub keyhash: Option<&'a [u8; 32]>,
    pub signature: Option<&'a [u8]>,
    pub sig_type: u16,
    pub image_type: u32,
    pub board_id: u32,
    pub min_bl_version: u32,
    pub enc_info: Option<EncInfoTlv>,
    pub total_len: usize,
}

impl ImageHeader {
    /// Validates the header fields against bootloader safety rules for this board's `layout`.
    pub fn validate(&self, layout: &Layout) -> Result<(), ImageParseError> {
        if self.ih_magic != IMAGE_MAGIC {
            return Err(ImageParseError::BadMagic);
        }
        if self.ih_hdr_size != EXPECTED_HDR_SIZE {
            return Err(ImageParseError::InvalidHeaderSize);
        }
        // Execution-model specific rules. Unknown models are refused rather than guessed.
        let load = match layout.exec {
            ExecModel::RamLoad { load, .. } => load,
            #[allow(unreachable_patterns)]
            _ => return Err(ImageParseError::InvalidLoadAddress),
        };
        if self.ih_load_addr != load.base {
            return Err(ImageParseError::InvalidLoadAddress);
        }
        if (self.ih_flags & IMAGE_F_RAM_LOAD) == 0 {
            return Err(ImageParseError::MissingRamLoadFlag);
        }

        let total_size = (self.ih_hdr_size as u64)
            .checked_add(self.ih_img_size as u64)
            .and_then(|s| s.checked_add(self.ih_protect_tlv_size as u64))
            .ok_or(ImageParseError::ImageTooLarge)?;

        if total_size > layout.max_image_size() as u64 {
            return Err(ImageParseError::ImageTooLarge);
        }

        // The image must fit entirely inside the board's RAM load window.
        let ram_end = (self.ih_load_addr as u64)
            .checked_add(self.ih_hdr_size as u64)
            .and_then(|s| s.checked_add(self.ih_img_size as u64))
            .ok_or(ImageParseError::ImageTooLarge)?;

        if ram_end > load.end() {
            return Err(ImageParseError::ImageTooLarge);
        }

        Ok(())
    }

    pub fn is_encrypted(&self) -> bool {
        (self.ih_flags & IMAGE_F_ENCRYPTED) != 0
    }
}

/// Parses and validates an entire image in slice (Header + Body + TLVs).
pub fn parse_image<'a>(
    data: &'a [u8],
    layout: &Layout,
    current_bl_version: u32,
    require_encrypted: bool,
) -> Result<ParsedImage<'a>, ImageParseError> {
    if data.len() < HEADER_STRUCT_SIZE {
        return Err(ImageParseError::BufferTooShort);
    }

    let (hdr_ref, _) = ImageHeader::ref_from_prefix(data)
        .map_err(|_| ImageParseError::BufferTooShort)?;
    let header = *hdr_ref;
    header.validate(layout)?;

    if require_encrypted && !header.is_encrypted() {
        return Err(ImageParseError::UnencryptedImageNotAllowed);
    }

    let hdr_size = header.ih_hdr_size as usize;
    let img_size = header.ih_img_size as usize;
    let prot_tlv_size = header.ih_protect_tlv_size as usize;

    let body_end = hdr_size
        .checked_add(img_size)
        .ok_or(ImageParseError::ImageTooLarge)?;

    if data.len() < body_end {
        return Err(ImageParseError::BufferTooShort);
    }

    let mut cursor = body_end;
    let mut image_type = IMAGE_TYPE_APP;
    let mut board_id = layout.board_id;
    let mut min_bl_version = 0u32;
    let mut sha256: Option<&'a [u8; 32]> = None;
    let mut keyhash: Option<&'a [u8; 32]> = None;
    let mut signature: Option<&'a [u8]> = None;
    let mut sig_type = 0u16;
    let mut enc_info: Option<EncInfoTlv> = None;

    // 1. Process Protected TLVs if present
    if prot_tlv_size > 0 {
        if prot_tlv_size < 4 {
            return Err(ImageParseError::BadTlvMagic);
        }
        let prot_end = cursor
            .checked_add(prot_tlv_size)
            .ok_or(ImageParseError::TlvLengthOverflow)?;

        if data.len() < prot_end {
            return Err(ImageParseError::BufferTooShort);
        }

        let prot_bytes = &data[cursor..prot_end];
        let (prot_info, _) = ImageTlvInfo::ref_from_prefix(prot_bytes)
            .map_err(|_| ImageParseError::CorruptedTlvEntry)?;

        if prot_info.it_magic != IMAGE_TLV_PROT_INFO_MAGIC {
            return Err(ImageParseError::BadTlvMagic);
        }
        if prot_info.it_tlv_tot as usize != prot_tlv_size {
            return Err(ImageParseError::TlvLengthOverflow);
        }

        let mut offset = 4;
        let mut loop_count = 0;
        while offset < prot_tlv_size {
            loop_count += 1;
            if loop_count > 64 {
                return Err(ImageParseError::TlvLimitExceeded);
            }
            if offset + 4 > prot_tlv_size {
                return Err(ImageParseError::CorruptedTlvEntry);
            }
            let (entry, _) = ImageTlvEntry::ref_from_prefix(&prot_bytes[offset..])
                .map_err(|_| ImageParseError::CorruptedTlvEntry)?;
            let t_len = entry.it_len as usize;
            let val_start = offset + 4;
            let val_end = val_start
                .checked_add(t_len)
                .ok_or(ImageParseError::TlvLengthOverflow)?;

            if val_end > prot_tlv_size {
                return Err(ImageParseError::CorruptedTlvEntry);
            }
            let val = &prot_bytes[val_start..val_end];

            match entry.it_type {
                IMAGE_TLV_IMAGE_TYPE => {
                    if val.len() == 4 {
                        image_type = u32::from_le_bytes([val[0], val[1], val[2], val[3]]);
                    }
                }
                IMAGE_TLV_BOARD_ID if val.len() == 4 => {
                    board_id = u32::from_le_bytes([val[0], val[1], val[2], val[3]]);
                }
                IMAGE_TLV_MIN_BL_VERSION if val.len() == 4 => {
                    min_bl_version = u32::from_le_bytes([val[0], val[1], val[2], val[3]]);
                }
                _ => {
                    // Unknown protected TLV is ignored unless critical range
                }
            }
            offset = val_end;
        }

        cursor = prot_end;
    }

    // 2. Process Standard (Unprotected) TLVs
    if data.len() >= cursor + 4 {
        let rem_bytes = &data[cursor..];
        let (tlv_info, _) = ImageTlvInfo::ref_from_prefix(rem_bytes)
            .map_err(|_| ImageParseError::CorruptedTlvEntry)?;

        if tlv_info.it_magic == IMAGE_TLV_INFO_MAGIC {
            let tlv_tot = tlv_info.it_tlv_tot as usize;
            let tlv_end = cursor
                .checked_add(tlv_tot)
                .ok_or(ImageParseError::TlvLengthOverflow)?;

            if data.len() < tlv_end {
                return Err(ImageParseError::BufferTooShort);
            }

            let tlv_bytes = &data[cursor..tlv_end];
            let mut offset = 4;
            let mut loop_count = 0;

            while offset < tlv_tot {
                loop_count += 1;
                if loop_count > 64 {
                    return Err(ImageParseError::TlvLimitExceeded);
                }
                if offset + 4 > tlv_tot {
                    return Err(ImageParseError::CorruptedTlvEntry);
                }
                let (entry, _) = ImageTlvEntry::ref_from_prefix(&tlv_bytes[offset..])
                    .map_err(|_| ImageParseError::CorruptedTlvEntry)?;
                let t_len = entry.it_len as usize;
                let val_start = offset + 4;
                let val_end = val_start
                    .checked_add(t_len)
                    .ok_or(ImageParseError::TlvLengthOverflow)?;

                if val_end > tlv_tot {
                    return Err(ImageParseError::CorruptedTlvEntry);
                }
                let val = &tlv_bytes[val_start..val_end];

                match entry.it_type {
                    IMAGE_TLV_SHA256 => {
                        if val.len() == 32 {
                            let (arr_ref, _) = <[u8; 32]>::ref_from_prefix(val)
                                .map_err(|_| ImageParseError::CorruptedTlvEntry)?;
                            sha256 = Some(arr_ref);
                        }
                    }
                    IMAGE_TLV_KEYHASH => {
                        if val.len() == 32 {
                            let (arr_ref, _) = <[u8; 32]>::ref_from_prefix(val)
                                .map_err(|_| ImageParseError::CorruptedTlvEntry)?;
                            keyhash = Some(arr_ref);
                        }
                    }
                    IMAGE_TLV_ECDSA256 | IMAGE_TLV_ED25519 => {
                        sig_type = entry.it_type;
                        signature = Some(val);
                    }
                    IMAGE_TLV_ENC_INFO if val.len() >= core::mem::size_of::<EncInfoTlv>() => {
                        let (enc_ref, _) = EncInfoTlv::ref_from_prefix(val)
                            .map_err(|_| ImageParseError::CorruptedTlvEntry)?;
                        enc_info = Some(*enc_ref);
                    }
                    _ => {}
                }
                offset = val_end;
            }

            cursor = tlv_end;
        }
    }

    // Safety checks:
    if image_type != IMAGE_TYPE_APP {
        return Err(ImageParseError::UnsupportedImageType);
    }
    if board_id != layout.board_id {
        return Err(ImageParseError::BoardIdMismatch);
    }
    if min_bl_version > current_bl_version {
        return Err(ImageParseError::MinBlVersionNotMet);
    }

    Ok(ParsedImage {
        header,
        sha256,
        keyhash,
        signature,
        sig_type,
        image_type,
        board_id,
        min_bl_version,
        enc_info,
        total_len: cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use boot_layout::H750_NETLITE;

    const LOAD_BASE: u32 = 0x2400_0000;

    /// A minimal well-formed image (header, 512 B body, protected TLV with BOARD_ID, SHA-256 TLV).
    fn build_image(board_id: u32) -> [u8; 2048] {
        let mut buf = [0u8; 2048];
        let hdr = ImageHeader {
            ih_magic: IMAGE_MAGIC,
            ih_load_addr: LOAD_BASE,
            ih_hdr_size: EXPECTED_HDR_SIZE,
            ih_protect_tlv_size: 16,
            ih_img_size: 512,
            ih_flags: IMAGE_F_RAM_LOAD,
            ih_ver: ImageVersion {
                major: 1,
                minor: 0,
                revision: 0,
                build_num: 1,
            },
            _pad1: 0,
        };
        buf[..32].copy_from_slice(hdr.as_bytes());

        // Protected TLV: magic 0x6908, tot 16 bytes
        // 4B header + 4B entry(BOARD_ID) + 4B val + 4B entry(dummy)
        let prot_tlv = ImageTlvInfo {
            it_magic: IMAGE_TLV_PROT_INFO_MAGIC,
            it_tlv_tot: 16,
        };
        let prot_start = 1024 + 512;
        buf[prot_start..prot_start + 4].copy_from_slice(prot_tlv.as_bytes());

        let entry = ImageTlvEntry {
            it_type: IMAGE_TLV_BOARD_ID,
            it_len: 4,
        };
        buf[prot_start + 4..prot_start + 8].copy_from_slice(entry.as_bytes());
        buf[prot_start + 8..prot_start + 12].copy_from_slice(&board_id.to_le_bytes());

        // Trailing 4 bytes pad
        let entry_pad = ImageTlvEntry {
            it_type: 0xFFFF,
            it_len: 0,
        };
        buf[prot_start + 12..prot_start + 16].copy_from_slice(entry_pad.as_bytes());

        // Unprotected TLV: magic 0x6907, tot 4 + 4 + 32 = 40 bytes
        let unprot_start = prot_start + 16;
        let unprot_tlv = ImageTlvInfo {
            it_magic: IMAGE_TLV_INFO_MAGIC,
            it_tlv_tot: 40,
        };
        buf[unprot_start..unprot_start + 4].copy_from_slice(unprot_tlv.as_bytes());
        let hash_entry = ImageTlvEntry {
            it_type: IMAGE_TLV_SHA256,
            it_len: 32,
        };
        buf[unprot_start + 4..unprot_start + 8].copy_from_slice(hash_entry.as_bytes());
        buf[unprot_start + 8..unprot_start + 40].fill(0xAA);
        buf
    }

    #[test]
    fn test_valid_image_parsing() {
        let buf = build_image(H750_NETLITE.board_id);
        let parsed = parse_image(&buf, &H750_NETLITE, 1, false).unwrap();
        assert_eq!(parsed.header.ih_magic, IMAGE_MAGIC);
        assert_eq!(parsed.board_id, H750_NETLITE.board_id);
        assert!(parsed.sha256.is_some());
        assert_eq!(parsed.sha256.unwrap()[0], 0xAA);
    }

    // --- ImageHeader::validate: every rejection path, per Layout -----------------------------

    fn good_header() -> ImageHeader {
        ImageHeader {
            ih_magic: IMAGE_MAGIC,
            ih_load_addr: LOAD_BASE,
            ih_hdr_size: EXPECTED_HDR_SIZE,
            ih_protect_tlv_size: 16,
            ih_img_size: 4096,
            ih_flags: IMAGE_F_RAM_LOAD,
            ih_ver: ImageVersion { major: 1, minor: 0, revision: 0, build_num: 1 },
            _pad1: 0,
        }
    }

    #[test]
    fn validate_accepts_a_well_formed_header() {
        assert_eq!(good_header().validate(&H750_NETLITE), Ok(()));
    }

    #[test]
    fn validate_rejects_bad_magic_and_header_size() {
        let mut h = good_header();
        h.ih_magic ^= 1;
        assert_eq!(h.validate(&H750_NETLITE), Err(ImageParseError::BadMagic));
        let mut h = good_header();
        h.ih_hdr_size = 0x200;
        assert_eq!(h.validate(&H750_NETLITE), Err(ImageParseError::InvalidHeaderSize));
    }

    #[test]
    fn validate_rejects_wrong_load_address_and_missing_ram_flag() {
        let mut h = good_header();
        h.ih_load_addr = LOAD_BASE + 0x400;
        assert_eq!(h.validate(&H750_NETLITE), Err(ImageParseError::InvalidLoadAddress));
        let mut h = good_header();
        h.ih_flags = 0;
        assert_eq!(h.validate(&H750_NETLITE), Err(ImageParseError::MissingRamLoadFlag));
    }

    #[test]
    fn validate_enforces_the_slot_size_limit_exactly() {
        // Give the board a RAM window larger than its slots so the slot limit is the binding one.
        let mut roomy = H750_NETLITE;
        roomy.exec = ExecModel::RamLoad {
            load: boot_layout::AddrRegion::new(LOAD_BASE, 2 * 1024 * 1024),
            vtor_offset: 0x400,
        };
        let slot = roomy.max_image_size();
        let mut h = good_header();
        h.ih_protect_tlv_size = 0;
        // header + body == slot size: allowed.
        h.ih_img_size = slot - EXPECTED_HDR_SIZE as u32;
        assert_eq!(h.validate(&roomy), Ok(()));
        // One byte more: rejected.
        h.ih_img_size += 1;
        assert_eq!(h.validate(&roomy), Err(ImageParseError::ImageTooLarge));
        // Protected TLVs count towards the slot limit too.
        let mut h = good_header();
        h.ih_img_size = slot - EXPECTED_HDR_SIZE as u32 - 16;
        h.ih_protect_tlv_size = 16;
        assert_eq!(h.validate(&roomy), Ok(()));
        h.ih_protect_tlv_size = 17;
        assert_eq!(h.validate(&roomy), Err(ImageParseError::ImageTooLarge));
    }

    #[test]
    fn h750_effective_image_limit_is_the_512k_ram_window_not_the_640k_slot() {
        // Characterization: slots are 640 KiB but AXI SRAM is 512 KiB, so RAM is what limits images.
        let mut h = good_header();
        h.ih_protect_tlv_size = 0;
        h.ih_img_size = 512 * 1024 - EXPECTED_HDR_SIZE as u32;
        assert_eq!(h.validate(&H750_NETLITE), Ok(()));
        h.ih_img_size += 1;
        assert_eq!(h.validate(&H750_NETLITE), Err(ImageParseError::ImageTooLarge));
    }

    #[test]
    fn validate_rejects_arithmetic_overflow() {
        let mut h = good_header();
        h.ih_img_size = u32::MAX;
        h.ih_protect_tlv_size = u16::MAX;
        assert_eq!(h.validate(&H750_NETLITE), Err(ImageParseError::ImageTooLarge));
    }

    #[test]
    fn validate_uses_the_layouts_ram_window_and_slot_size() {
        // A board with a small RAM window rejects an image that fits in its slot but not in RAM.
        let mut small = H750_NETLITE;
        small.exec = ExecModel::RamLoad {
            load: boot_layout::AddrRegion::new(LOAD_BASE, 8 * 1024),
            vtor_offset: 0x400,
        };
        let mut h = good_header();
        h.ih_protect_tlv_size = 0;
        h.ih_img_size = 7 * 1024; // 1 KiB header + 7 KiB body = 8 KiB: fits exactly
        assert_eq!(h.validate(&small), Ok(()));
        h.ih_img_size = 7 * 1024 + 1;
        assert_eq!(h.validate(&small), Err(ImageParseError::ImageTooLarge));
        // Same header is fine on the full-size board.
        assert_eq!(h.validate(&H750_NETLITE), Ok(()));
        // A board whose load address differs rejects images linked for H750.
        let mut elsewhere = H750_NETLITE;
        elsewhere.exec = ExecModel::RamLoad {
            load: boot_layout::AddrRegion::new(0x3000_0000, 512 * 1024),
            vtor_offset: 0x400,
        };
        assert_eq!(good_header().validate(&elsewhere), Err(ImageParseError::InvalidLoadAddress));
    }

    #[test]
    fn parse_rejects_an_image_built_for_another_board() {
        let buf = build_image(u32::from_be_bytes(*b"F429"));
        assert_eq!(
            parse_image(&buf, &H750_NETLITE, 1, false).unwrap_err(),
            ImageParseError::BoardIdMismatch
        );
        // The same image is accepted by a layout whose board id matches.
        let mut other = H750_NETLITE;
        other.board_id = u32::from_be_bytes(*b"F429");
        assert!(parse_image(&buf, &other, 1, false).is_ok());
    }

    #[test]
    fn parse_requires_encryption_only_when_asked() {
        let buf = build_image(H750_NETLITE.board_id);
        assert_eq!(
            parse_image(&buf, &H750_NETLITE, 1, true).unwrap_err(),
            ImageParseError::UnencryptedImageNotAllowed
        );
        assert!(parse_image(&buf, &H750_NETLITE, 1, false).is_ok());
    }

    #[test]
    fn parse_rejects_truncated_buffers() {
        let buf = build_image(H750_NETLITE.board_id);
        assert_eq!(
            parse_image(&buf[..16], &H750_NETLITE, 1, false).unwrap_err(),
            ImageParseError::BufferTooShort
        );
        assert_eq!(
            parse_image(&buf[..1024 + 100], &H750_NETLITE, 1, false).unwrap_err(),
            ImageParseError::BufferTooShort
        );
    }
}
