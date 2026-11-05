//! Bootloader ABI definitions
//!
//! Fixed memory layout, C-compatible structs, and error codes
//! shared between the STM32H750 Bootloader and resident Applications.

#![no_std]

/// Magic constant for `BootApi`: ASCII 'BAPI' in little-endian.
pub const BOOT_API_MAGIC: u32 = 0x4950_4142;

/// Current ABI specification version.
pub const BOOT_ABI_VERSION: u16 = 1;

/// Magic constant for `BootInfo`: ASCII 'INFO' in little-endian.
pub const BOOT_INFO_MAGIC: u32 = 0x4F46_4E49;

/// Magic constant for `BlInfo`: ASCII '.BLI' in little-endian.
pub const BL_INFO_MAGIC: u32 = 0x494C_422E;

// Fixed addresses in STM32H750 Flash (0x08000000, 128KB single sector)
pub const FLASH_BASE_ADDR: u32 = 0x0800_0000;
pub const VECTOR_TABLE_SIZE: u32 = 0x400; // 1KB reserved
pub const BL_INFO_ADDR: u32 = 0x0800_0400;
pub const BOOT_API_ADDR: u32 = 0x0800_0440;
pub const SECRETS_ADDR: u32 = 0x0801_F000; // 2KB project keys & global secret
pub const KEYTABLE_ADDR: u32 = 0x0801_F800; // 2KB public keys

// Fixed addresses in RAM
pub const AXI_SRAM_BASE: u32 = 0x2400_0000;
pub const AXI_SRAM_SIZE: u32 = 512 * 1024;
pub const APP_LOAD_ADDR: u32 = 0x2400_0000;
pub const APP_VTOR_OFFSET: u32 = 0x400; // Header size is 1KB
pub const APP_VTOR_ADDR: u32 = APP_LOAD_ADDR + APP_VTOR_OFFSET; // 0x24000400

pub const SRAM4_BASE: u32 = 0x3800_0000;
pub const SRAM4_SIZE: u32 = 64 * 1024;
pub const SRAM4_BOOT_INFO_ADDR: u32 = 0x3800_0000;

// NOR flash partition layout
pub const NOR_JOURNAL_SECTOR_0: u32 = 0x0000_0000;
pub const NOR_JOURNAL_SECTOR_1: u32 = 0x0000_1000;
pub const NOR_RESERVED_OFFSET: u32 = 0x0000_2000;
pub const NOR_SLOT_A_OFFSET: u32 = 0x0001_0000;
pub const NOR_SLOT_B_OFFSET: u32 = 0x000B_0000;
pub const NOR_SLOT_SIZE: u32 = 640 * 1024;
pub const NOR_PAGE_SIZE: u32 = 256;
pub const NOR_SECTOR_SIZE: u32 = 4096;
pub const NOR_BLOCK_SIZE: u32 = 65536;

// Error codes
pub const BOOT_OK: i32 = 0;
pub const ERR_INVALID_PARAM: i32 = -1;
pub const ERR_ACTIVE_SLOT: i32 = -2;
pub const ERR_OUT_OF_BOUNDS: i32 = -3;
pub const ERR_VERIFY_FAILED: i32 = -4;
pub const ERR_NOR_IO: i32 = -5;
pub const ERR_INVALID_STATE: i32 = -6;
pub const ERR_BUSY: i32 = -7;
pub const ERR_UNSUPPORTED: i32 = -8;
pub const ERR_DECRYPT_FAILED: i32 = -9;
pub const ERR_BUFFER_TOO_SMALL: i32 = -10;

// Slot states
pub const SLOT_STATE_EMPTY: u8 = 0;
pub const SLOT_STATE_WRITTEN: u8 = 1;
pub const SLOT_STATE_PENDING: u8 = 2;
pub const SLOT_STATE_TESTING: u8 = 3;
pub const SLOT_STATE_CONFIRMED: u8 = 4;
pub const SLOT_STATE_INVALID: u8 = 5;

// Boot slot identifiers
pub const BOOT_SLOT_A: u8 = 0;
pub const BOOT_SLOT_B: u8 = 1;
pub const BOOT_SLOT_NONE: u8 = 0xFF;

// Boot reasons
pub const BOOT_REASON_NORMAL: u8 = 0;
pub const BOOT_REASON_RESCUE_KEY: u8 = 1;
pub const BOOT_REASON_RESCUE_TAMP: u8 = 2;
pub const BOOT_REASON_ROLLBACK: u8 = 3;
pub const BOOT_REASON_ALL_INVALID: u8 = 4;

// TAMP commands
pub const TAMP_MAGIC_MASK: u32 = 0xFFFF_0000;
pub const TAMP_CMD_MASK: u32 = 0x0000_FFFF;
pub const TAMP_CMD_NORMAL: u32 = 0x0000_0000;
pub const TAMP_CMD_ENTER_RESCUE: u32 = 0x5245_5343; // 'RESC'
pub const TAMP_CMD_REQUEST_BL_UPDATE: u32 = 0x424C_5550; // 'BLUP'

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Version representation matching MCUboot format.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct ImageVersion {
    pub major: u8,
    pub minor: u8,
    pub revision: u16,
    pub build_num: u32,
}

/// Boot information passed to application via SRAM4.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct BootInfo {
    pub magic: u32,
    pub struct_version: u16,
    pub size: u16,
    pub boot_slot: u8,
    pub boot_reason: u8,
    pub trial_count: u8,
    pub reserved0: u8,
    pub img_version: ImageVersion,
    pub bl_version: u32,
    pub abi_version: u16,
    pub flags: u16,
    pub crc32: u32,
}

/// Status of a firmware slot.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct SlotInfo {
    pub slot_id: u8,
    pub state: u8,
    pub trial_count: u8,
    pub reserved0: u8,
    pub img_version: ImageVersion,
    pub img_size: u32,
    pub flags: u32,
    pub min_version: u32,
}

/// Bootloader information section (.bl_info) located at 0x08000400.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct BlInfo {
    pub magic: u32,
    pub bl_version: u32,
    pub bl_size: u32,
    pub build_hash: [u8; 32],
    pub flags: u32,
    pub reserved: [u32; 4],
}

/// Hardware bit-transfer operations provided by the Application.
#[repr(C)]
pub struct IoOps {
    pub spi_transfer: unsafe extern "C" fn(tx: *const u8, rx: *mut u8, len: u32) -> i32,
    pub cs: unsafe extern "C" fn(assert: bool),
    pub millis: unsafe extern "C" fn() -> u32,
    pub yield_: unsafe extern "C" fn(),
    pub reserved: [usize; 4],
}

/// Bootloader resident service ABI function table (.boot_api) located at 0x08000440.
#[repr(C)]
pub struct BootApi {
    pub magic: u32,
    pub abi_version: u16,
    pub table_len: u16,
    pub ctx_size: u32,
    pub min_stack: u32,
    pub get_boot_info: unsafe extern "C" fn(out: *mut BootInfo) -> i32,
    pub slot_status: unsafe extern "C" fn(slot: u8, out: *mut SlotInfo) -> i32,
    pub slot_erase: unsafe extern "C" fn(io: *const IoOps, slot: u8) -> i32,
    pub slot_write: unsafe extern "C" fn(io: *const IoOps, slot: u8, off: u32, data: *const u8, len: u32) -> i32,
    pub verify_slot: unsafe extern "C" fn(io: *const IoOps, slot: u8, work: *mut u8, work_len: u32) -> i32,
    pub mark_pending: unsafe extern "C" fn(io: *const IoOps, slot: u8) -> i32,
    pub confirm: unsafe extern "C" fn(io: *const IoOps) -> i32,
    pub request_rescue: unsafe extern "C" fn() -> i32,
    pub update_init: unsafe extern "C" fn(ctx: *mut u8, io: *const IoOps) -> i32,
    pub update_feed: unsafe extern "C" fn(ctx: *mut u8, rx: *const u8, rx_len: u32, tx: *mut u8, tx_cap: u32) -> i32,
    pub update_abort: unsafe extern "C" fn(ctx: *mut u8) -> i32,
    pub system_reset: unsafe extern "C" fn() -> !,
    pub reserved: [usize; 8],
}

// Ensure struct sizes match design specification:
// BlInfo is exactly 64 bytes (0x40).
const _: () = assert!(core::mem::size_of::<BlInfo>() == 64);
