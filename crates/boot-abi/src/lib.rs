//! Bootloader ABI definitions
//!
//! Fixed memory layout, C-compatible structs, and error codes
//! shared between the STM32H750 Bootloader and resident Applications.

#![no_std]

/// Magic constant for `BootApi`: ASCII 'BAPI' in little-endian.
pub const BOOT_API_MAGIC: u32 = 0x4950_4142;

/// Current ABI specification version.
/// Version 2: `IoOps::spi_transfer`/`cs` optional, `BootApi::caps`, `BlInfo` platform fields,
/// no board addresses in this crate. (Version 1 was never released.)
pub const BOOT_ABI_VERSION: u16 = 2;

/// Magic constant for `BootInfo`: ASCII 'INFO' in little-endian.
pub const BOOT_INFO_MAGIC: u32 = 0x4F46_4E49;

/// Magic constant for `BlInfo`: ASCII '.BLI' in little-endian.
pub const BL_INFO_MAGIC: u32 = 0x494C_422E;

// Board-specific addresses and flash partitions live in `boot-layout` (per-board `Layout`),
// not here: this crate only describes the ABI (structures, codes, capability bits).

// Capability bits reported in `BootApi::caps` (what this bootloader/platform provides).
/// Flash services need `IoOps` from the application (SPI transport lent to the bootloader).
pub const CAP_IO_FROM_APP: u32 = 1 << 0;
/// The bootloader owns the flash controller; the application needs to provide no transport.
pub const CAP_BL_OWNS_FLASH: u32 = 1 << 1;
/// Flash-modifying services mask interrupts while they run.
pub const CAP_SERVICE_MASKS_IRQ: u32 = 1 << 2;
/// Flash-modifying services execute from a reserved RAM region (see `BlInfo::ramfunc_*`).
pub const CAP_SERVICE_RAM_RESIDENT: u32 = 1 << 3;
/// The bootloader maps the selected slot to a fixed execution address (bank swap / remap).
pub const CAP_REMAP_SLOT_MAPPING: u32 = 1 << 4;
/// Images are stored encrypted on flash (device-bound storage encryption).
pub const CAP_STORAGE_ENCRYPTION: u32 = 1 << 5;

// Execution models reported in `BlInfo::exec_model`.
/// The verified image is copied into RAM and executed there.
pub const EXEC_MODEL_RAM_LOAD: u32 = 0;

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

/// Bootloader information section (`.bl_info`); its address comes from the board `Layout`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct BlInfo {
    pub magic: u32,
    pub bl_version: u32,
    pub bl_size: u32,
    pub build_hash: [u8; 32],
    pub flags: u32,
    /// Board id of the layout this bootloader was built for (same value as the image `BOARD_ID`).
    pub platform_id: u32,
    /// One of the `EXEC_MODEL_*` values.
    pub exec_model: u32,
    /// Reserved RAM region for RAM-resident flash routines (0/0 when unused).
    pub ramfunc_base: u32,
    pub ramfunc_size: u32,
}

/// Hardware bit-transfer operations provided by the Application.
#[repr(C)]
pub struct IoOps {
    /// SPI transport lent to the bootloader. `None` on platforms where the bootloader owns the
    /// flash controller (see `CAP_BL_OWNS_FLASH`). `Option<fn>` has the same size as `fn`.
    pub spi_transfer: Option<unsafe extern "C" fn(tx: *const u8, rx: *mut u8, len: u32) -> i32>,
    /// Chip-select control matching `spi_transfer`.
    pub cs: Option<unsafe extern "C" fn(assert: bool)>,
    pub millis: unsafe extern "C" fn() -> u32,
    pub yield_: unsafe extern "C" fn(),
    pub reserved: [usize; 4],
}

/// Bootloader resident service ABI function table (`.boot_api`); its address comes from the
/// board `Layout` (`boot_api_addr`).
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
    /// `CAP_*` bits describing the platform (formerly reserved; 0 on a v1 table).
    pub caps: u32,
    pub reserved: [u32; 7],
}

// Ensure struct sizes match design specification:
// BlInfo is exactly 64 bytes (0x40).
const _: () = assert!(core::mem::size_of::<BlInfo>() == 64);

// Pointer-sized fields make these layouts target-dependent; check them where they matter (32-bit).
#[cfg(target_pointer_width = "32")]
const _: () = assert!(core::mem::size_of::<IoOps>() == 32);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(core::mem::size_of::<BootApi>() == 96);
