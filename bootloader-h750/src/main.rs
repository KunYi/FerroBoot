//! STM32H750 Rust Bootloader Main Binary
//!
//! Handles early chip bring-up, TAMP/KEY2 rescue detection, SPI NOR probe,
//! ping-pong journal replay, slot decryption/verification, and jump to App in AXI SRAM.

#![no_std]
#![no_main]

use boot_abi::*;
use boot_core::{decide, BootDecision, SlotLoader, DEFAULT_MAX_TRIALS};
use boot_crypto::{KeyTable, SecretsTable, KEYTABLE_MAGIC, SECRETS_MAGIC};
use boot_image::DEFAULT_BOARD_ID;
use boot_journal::JournalManager;
use boot_nor::{NorFlash, SpiNorIoOpsDriver};
use boot_stm32h7::{
    init_clocks, init_gpio_ui, init_nor_cs, init_spi1, init_sram4_ecc, init_usart1,
    is_key2_pressed, jump_to_app, read_mcu_uid, set_led, set_nor_cs, spi1_transfer,
    uart_read_byte_blocking, uart_write_byte, uart_write_hex, uart_write_str, Iwdg,
    TampControl,
};
use cortex_m_rt::entry;
use panic_halt as _;

// ---------------------------------------------------------------------------
// Fixed Flash Data Sections (§4.1, §9.3)
// ---------------------------------------------------------------------------

#[link_section = ".bl_info"]
#[used]
pub static BOOTLOADER_INFO: BlInfo = BlInfo {
    magic: BL_INFO_MAGIC,
    bl_version: 1,
    bl_size: 96 * 1024,
    build_hash: [0x5A; 32],
    flags: 0,
    reserved: [0; 4],
};

#[link_section = ".boot_api"]
#[used]
pub static BOOT_API_TABLE: BootApi = BootApi {
    magic: BOOT_API_MAGIC,
    abi_version: BOOT_ABI_VERSION,
    table_len: 12,
    ctx_size: 1024,
    min_stack: 2048,
    get_boot_info: abi_get_boot_info,
    slot_status: abi_slot_status,
    slot_erase: abi_slot_erase,
    slot_write: abi_slot_write,
    verify_slot: abi_verify_slot,
    mark_pending: abi_mark_pending,
    confirm: abi_confirm,
    request_rescue: abi_request_rescue,
    update_init: abi_update_init,
    update_feed: abi_update_feed,
    update_abort: abi_update_abort,
    system_reset: abi_system_reset,
    reserved: [0; 8],
};

#[link_section = ".secrets"]
#[used]
pub static SECRETS_STORAGE: SecretsTable = SecretsTable {
    magic: SECRETS_MAGIC,
    version: 1,
    reserved: [0; 2],
    global_secret: [0x33; 32], // Default development global secret
    project_keys: [
        boot_crypto::SecretSlot {
            key_id: 1,
            active: 1,
            reserved: [0; 2],
            key: [0x44; 32],
        },
        boot_crypto::SecretSlot {
            key_id: 2,
            active: 0,
            reserved: [0; 2],
            key: [0; 32],
        },
    ],
};

#[link_section = ".keytable"]
#[used]
pub static PUBLIC_KEY_TABLE: KeyTable = KeyTable {
    magic: KEYTABLE_MAGIC,
    version: 1,
    num_keys: 1,
    keys: [
        boot_crypto::PublicKeySlot {
            key_id: 1,
            reserved: [0; 3],
            // Default dev P-256 public key (65 bytes SEC1 uncompressed)
            pubkey: [
                0x04, 0x6f, 0xf0, 0x3b, 0x94, 0x92, 0x41, 0xce, 0x1d, 0xad, 0xd4, 0x35, 0x19,
                0xe6, 0x96, 0x0e, 0x0a, 0x85, 0xb4, 0x1a, 0x69, 0xa0, 0x5c, 0x32, 0x81, 0x03,
                0xaa, 0x2b, 0xce, 0x15, 0x94, 0xca, 0x16, 0x3c, 0x4f, 0x75, 0x3a, 0x55, 0xbf,
                0x01, 0xdc, 0x53, 0xf6, 0xc0, 0xb0, 0xc7, 0xee, 0xe7, 0x8b, 0x40, 0xc6, 0xff,
                0x7d, 0x25, 0xa9, 0x6e, 0x22, 0x82, 0xb9, 0x89, 0xce, 0xf7, 0x1c, 0x14, 0x4a,
            ],
            pubkey_len: 65,
            keyhash: [
                0x25, 0x6b, 0x1b, 0xe3, 0x27, 0x45, 0xd1, 0xd1, 0xc9, 0xd4, 0x91, 0x21, 0xcc,
                0x87, 0xbc, 0x45, 0x1c, 0x4f, 0x2d, 0xfa, 0x75, 0x6d, 0x89, 0xb4, 0xc9, 0x10,
                0xaf, 0xae, 0xb6, 0x25, 0x8a, 0xe0,
            ],
        },
        boot_crypto::PublicKeySlot {
            key_id: 0,
            reserved: [0; 3],
            pubkey: [0; 65],
            pubkey_len: 0,
            keyhash: [0; 32],
        },
        boot_crypto::PublicKeySlot {
            key_id: 0,
            reserved: [0; 3],
            pubkey: [0; 65],
            pubkey_len: 0,
            keyhash: [0; 32],
        },
    ],
    reserved: [0; 1024],
};

// ---------------------------------------------------------------------------
// Hardware Callbacks for Bootloader Resident ABI
// ---------------------------------------------------------------------------

static LOCAL_IO_OPS: IoOps = IoOps {
    spi_transfer: local_spi_transfer,
    cs: local_cs,
    millis: local_millis,
    yield_: local_yield,
    reserved: [0; 4],
};

unsafe extern "C" fn local_spi_transfer(tx: *const u8, rx: *mut u8, len: u32) -> i32 {
    spi1_transfer(tx, rx, len)
}

unsafe extern "C" fn local_cs(assert: bool) {
    set_nor_cs(assert);
}

static mut SYSTEM_TICKS: u32 = 0;

unsafe extern "C" fn local_millis() -> u32 {
    SYSTEM_TICKS
}

unsafe extern "C" fn local_yield() {
    Iwdg::kick();
}

// ---------------------------------------------------------------------------
// ABI Export Functions
// ---------------------------------------------------------------------------

unsafe extern "C" fn abi_get_boot_info(out: *mut BootInfo) -> i32 {
    if out.is_null() {
        return ERR_INVALID_PARAM;
    }
    let src = SRAM4_BOOT_INFO_ADDR as *const BootInfo;
    core::ptr::copy_nonoverlapping(src, out, 1);
    BOOT_OK
}

unsafe extern "C" fn abi_slot_status(slot: u8, out: *mut SlotInfo) -> i32 {
    if out.is_null() || slot > 1 {
        return ERR_INVALID_PARAM;
    }
    // Replay journal with temporary driver
    let mut driver = SpiNorIoOpsDriver::new(&LOCAL_IO_OPS);
    if let Ok(journal) = JournalManager::init(&mut driver) {
        let state = if slot == 0 { journal.slot_a } else { journal.slot_b };
        (*out).slot_id = slot;
        (*out).state = state.state;
        (*out).trial_count = state.trial;
        (*out).img_version = state.img_ver;
        (*out).min_version = state.min_version;
        BOOT_OK
    } else {
        ERR_NOR_IO
    }
}

unsafe extern "C" fn abi_slot_erase(io: *const IoOps, slot: u8) -> i32 {
    if io.is_null() || slot > 1 {
        return ERR_INVALID_PARAM;
    }
    // Protect active slot (§8.3 rule 1)
    let boot_info = &*(SRAM4_BOOT_INFO_ADDR as *const BootInfo);
    if slot == boot_info.boot_slot {
        return ERR_ACTIVE_SLOT;
    }

    let mut driver = SpiNorIoOpsDriver::new(&*io);
    let offset = if slot == 0 { NOR_SLOT_A_OFFSET } else { NOR_SLOT_B_OFFSET };

    for sec in 0..(NOR_SLOT_SIZE / NOR_SECTOR_SIZE) {
        if driver.erase_sector_4k(offset + sec * NOR_SECTOR_SIZE).is_err() {
            return ERR_NOR_IO;
        }
    }
    BOOT_OK
}

unsafe extern "C" fn abi_slot_write(
    io: *const IoOps,
    slot: u8,
    off: u32,
    data: *const u8,
    len: u32,
) -> i32 {
    if io.is_null() || data.is_null() || slot > 1 {
        return ERR_INVALID_PARAM;
    }
    let boot_info = &*(SRAM4_BOOT_INFO_ADDR as *const BootInfo);
    if slot == boot_info.boot_slot {
        return ERR_ACTIVE_SLOT;
    }
    if off + len > NOR_SLOT_SIZE {
        return ERR_OUT_OF_BOUNDS;
    }

    let mut driver = SpiNorIoOpsDriver::new(&*io);
    let slot_base = if slot == 0 { NOR_SLOT_A_OFFSET } else { NOR_SLOT_B_OFFSET };
    let slice = core::slice::from_raw_parts(data, len as usize);

    if driver.write(slot_base + off, slice).is_ok() {
        BOOT_OK
    } else {
        ERR_NOR_IO
    }
}

unsafe extern "C" fn abi_verify_slot(
    io: *const IoOps,
    slot: u8,
    work: *mut u8,
    work_len: u32,
) -> i32 {
    if io.is_null() || work.is_null() || slot > 1 {
        return ERR_INVALID_PARAM;
    }
    let mut driver = SpiNorIoOpsDriver::new(&*io);
    let mcu_uid = read_mcu_uid();
    let mut loader = SlotLoader::new(
        &mut driver,
        &PUBLIC_KEY_TABLE,
        &SECRETS_STORAGE.global_secret,
        mcu_uid,
        DEFAULT_BOARD_ID,
        BOOTLOADER_INFO.bl_version,
    );

    let ram_slice = core::slice::from_raw_parts_mut(work, work_len as usize);
    if loader.load_and_verify(slot, ram_slice, false).is_ok() {
        BOOT_OK
    } else {
        ERR_VERIFY_FAILED
    }
}

unsafe extern "C" fn abi_mark_pending(io: *const IoOps, slot: u8) -> i32 {
    if io.is_null() || slot > 1 {
        return ERR_INVALID_PARAM;
    }
    let mut driver = SpiNorIoOpsDriver::new(&*io);
    let mut journal = match JournalManager::init(&mut driver) {
        Ok(j) => j,
        Err(_) => return ERR_NOR_IO,
    };

    let target_state = if slot == 0 { journal.slot_a } else { journal.slot_b };
    let res = journal.append_record(
        &mut driver,
        slot,
        SLOT_STATE_PENDING,
        0,
        0,
        target_state.img_ver,
        target_state.min_version,
        target_state.key_revoke_mask,
    );

    if res.is_ok() { BOOT_OK } else { ERR_NOR_IO }
}

unsafe extern "C" fn abi_confirm(io: *const IoOps) -> i32 {
    if io.is_null() {
        return ERR_INVALID_PARAM;
    }
    let boot_info = &*(SRAM4_BOOT_INFO_ADDR as *const BootInfo);
    if boot_info.boot_slot > 1 {
        return ERR_INVALID_PARAM;
    }

    let mut driver = SpiNorIoOpsDriver::new(&*io);
    let mut journal = match JournalManager::init(&mut driver) {
        Ok(j) => j,
        Err(_) => return ERR_NOR_IO,
    };

    let slot = boot_info.boot_slot;
    let target = if slot == 0 { journal.slot_a } else { journal.slot_b };

    if target.state != SLOT_STATE_TESTING {
        return ERR_INVALID_STATE;
    }

    let res = journal.append_record(
        &mut driver,
        slot,
        SLOT_STATE_CONFIRMED,
        target.trial,
        0,
        target.img_ver,
        target.min_version,
        target.key_revoke_mask,
    );

    if res.is_ok() { BOOT_OK } else { ERR_NOR_IO }
}

unsafe extern "C" fn abi_request_rescue() -> i32 {
    TampControl::enable_backup_domain();
    TampControl::write_bkp0(TAMP_CMD_ENTER_RESCUE);
    BOOT_OK
}

unsafe extern "C" fn abi_update_init(_ctx: *mut u8, _io: *const IoOps) -> i32 {
    BOOT_OK
}

unsafe extern "C" fn abi_update_feed(
    _ctx: *mut u8,
    _rx: *const u8,
    _rx_len: u32,
    _tx: *mut u8,
    _tx_cap: u32,
) -> i32 {
    BOOT_OK
}

unsafe extern "C" fn abi_update_abort(_ctx: *mut u8) -> i32 {
    BOOT_OK
}

unsafe extern "C" fn abi_system_reset() -> ! {
    cortex_m::peripheral::SCB::sys_reset();
}

// ---------------------------------------------------------------------------
// Entry Point
// ---------------------------------------------------------------------------

#[entry]
fn main() -> ! {
    // 1. Clock, SRAM4 ECC init, PA4 NOR CS High, UI, UART, SPI1
    init_clocks();
    init_sram4_ecc();
    init_nor_cs();
    init_gpio_ui();
    init_usart1();
    init_spi1();

    uart_write_str("\r\n========================================\r\n");
    uart_write_str("[BL] STM32H750 Rust Bootloader v1.0\r\n");
    uart_write_str("[BL] 400MHz / VOS1, AXI SRAM Loader Mode\r\n");
    uart_write_str("========================================\r\n");

    // 2. Check TAMP rescue request or KEY2 press
    let tamp_rescue = TampControl::check_and_clear_rescue_request();
    let key2_rescue = is_key2_pressed();

    if tamp_rescue || key2_rescue {
        uart_write_str("[BL] Rescue mode requested via TAMP / KEY2.\r\n");
        enter_rescue_mode(if key2_rescue {
            BOOT_REASON_RESCUE_KEY
        } else {
            BOOT_REASON_RESCUE_TAMP
        });
    }

    // 3. Probe SPI NOR
    let mut nor_driver = SpiNorIoOpsDriver::new(&LOCAL_IO_OPS);
    match nor_driver.read_jedec_id() {
        Ok(jedec) => {
            uart_write_str("[BL] NOR Flash JEDEC ID: 0x");
            uart_write_hex(jedec[0]);
            uart_write_str(" 0x");
            uart_write_hex(jedec[1]);
            uart_write_str(" 0x");
            uart_write_hex(jedec[2]);
            uart_write_str("\r\n");
        }
        Err(err) => {
            uart_write_str("[BL] NOR Flash communication failed! Error: ");
            match err {
                boot_nor::NorError::IoError => uart_write_str("IoError"),
                boot_nor::NorError::Timeout => uart_write_str("Timeout"),
                _ => uart_write_str("Other"),
            }
            uart_write_str("\r\n");
            enter_rescue_mode(BOOT_REASON_ALL_INVALID);
        }
    }

    // 4. Replay Journal
    let mut journal = match JournalManager::init(&mut nor_driver) {
        Ok(j) => j,
        Err(_) => {
            uart_write_str("[BL] Journal init failed, entering rescue.\r\n");
            enter_rescue_mode(BOOT_REASON_ALL_INVALID);
        }
    };

    // 5. Run Decision Engine
    let decision = decide(&journal.slot_a, &journal.slot_b, DEFAULT_MAX_TRIALS);

    let (cand1, cand2) = match decision {
        BootDecision::TryCandidates { primary, fallback } => (primary, fallback),
        BootDecision::EnterRescue => {
            uart_write_str("[BL] No valid bootable slot found. Entering rescue.\r\n");
            enter_rescue_mode(BOOT_REASON_ALL_INVALID);
        }
    };

    // 6. Attempt loading Candidate 1
    let mcu_uid = read_mcu_uid();
    let mut loader = SlotLoader::new(
        &mut nor_driver,
        &PUBLIC_KEY_TABLE,
        &SECRETS_STORAGE.global_secret,
        mcu_uid,
        DEFAULT_BOARD_ID,
        BOOTLOADER_INFO.bl_version,
    );

    let ram_target = unsafe {
        core::slice::from_raw_parts_mut(AXI_SRAM_BASE as *mut u8, AXI_SRAM_SIZE as usize)
    };

    let mut boot_slot = cand1;
    let mut verified_res = loader.load_and_verify(cand1, ram_target, false);

    if verified_res.is_err() && cand2.is_some() {
        let fallback_slot = cand2.unwrap();
        uart_write_str("[BL] Primary candidate failed. Trying fallback slot.\r\n");
        boot_slot = fallback_slot;
        verified_res = loader.load_and_verify(fallback_slot, ram_target, false);
    }

    let parsed_img = match verified_res {
        Ok(p) => p,
        Err(_) => {
            uart_write_str("[BL] All candidates failed verification. Entering rescue.\r\n");
            enter_rescue_mode(BOOT_REASON_ALL_INVALID);
        }
    };

    uart_write_str("[BL] Slot verified successfully. Preparing jump.\r\n");

    // 7. Update Journal if Pending -> Testing
    let target_state = if boot_slot == 0 { journal.slot_a } else { journal.slot_b };
    let is_trial = target_state.state == SLOT_STATE_PENDING;
    if is_trial {
        let _ = journal.append_record(
            &mut nor_driver,
            boot_slot,
            SLOT_STATE_TESTING,
            target_state.trial + 1,
            0,
            parsed_img.header.ih_ver,
            0,
            0,
        );
        // Start watchdog for test boot
        Iwdg::start(3000);
    }

    // 8. Write Boot Info to SRAM4 (0x38000000)
    let boot_info = BootInfo {
        magic: BOOT_INFO_MAGIC,
        struct_version: 1,
        size: core::mem::size_of::<BootInfo>() as u16,
        boot_slot,
        boot_reason: BOOT_REASON_NORMAL,
        trial_count: target_state.trial + (if is_trial { 1 } else { 0 }),
        reserved0: 0,
        img_version: parsed_img.header.ih_ver,
        bl_version: BOOTLOADER_INFO.bl_version,
        abi_version: BOOT_ABI_VERSION,
        flags: 0,
        crc32: 0,
    };
    unsafe {
        core::ptr::write_volatile(SRAM4_BOOT_INFO_ADDR as *mut BootInfo, boot_info);
    }

    // 9. Jump to Application VTOR (0x24000400)
    unsafe {
        jump_to_app(APP_VTOR_ADDR);
    }
}

fn read_delimited_hex() -> (Option<u32>, u8) {
    let mut val = 0u32;
    let mut digits = 0;
    loop {
        let b = uart_read_byte_blocking();
        if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' {
            if digits > 0 {
                return (Some(val), b);
            } else {
                continue;
            }
        }
        let d = match b {
            b'0'..=b'9' => (b - b'0') as u32,
            b'a'..=b'f' => (b - b'a' + 10) as u32,
            b'A'..=b'F' => (b - b'A' + 10) as u32,
            _ => return (None, b),
        };
        val = (val << 4) | d;
        digits += 1;
    }
}

fn enter_rescue_mode(_reason: u8) -> ! {
    set_led(true);
    uart_write_str("[RESCUE] Entering UART Rescue Mode. Ready.\r\n");

    let mut nor = SpiNorIoOpsDriver::new(&LOCAL_IO_OPS);

    loop {
        Iwdg::kick();
        let cmd = uart_read_byte_blocking();
        match cmd {
            b'p' | b'P' => {
                // Ping
                uart_write_str("PONG\r\n");
            }
            b'i' | b'I' => {
                // Info
                uart_write_str("[RESCUE] BL v1.0, STM32H750, NOR BY25Q32ES\r\n");
            }
            b'e' | b'E' => {
                // Erase slot block 0: e <slot>\n
                let mut slot = uart_read_byte_blocking();
                while slot == b' ' || slot == b'\t' {
                    slot = uart_read_byte_blocking();
                }
                let base = if slot == b'0' {
                    NOR_SLOT_A_OFFSET
                } else {
                    NOR_SLOT_B_OFFSET
                };
                if nor.erase_block_64k(base).is_ok() {
                    uart_write_str("OK\r\n");
                } else {
                    uart_write_str("ERR\r\n");
                }
            }
            b'w' | b'W' => {
                // Write chunk: w <slot> <offset_hex> <len_hex>\n<bytes>
                let mut slot = uart_read_byte_blocking();
                while slot == b' ' || slot == b'\t' {
                    slot = uart_read_byte_blocking();
                }
                let slot_base = if slot == b'0' {
                    NOR_SLOT_A_OFFSET
                } else {
                    NOR_SLOT_B_OFFSET
                };
                let (offset, _) = match read_delimited_hex() {
                    (Some(o), d) => (o, d),
                    (None, _) => {
                        uart_write_str("ERR_OFF\r\n");
                        continue;
                    }
                };
                let (len, mut delim) = match read_delimited_hex() {
                    (Some(l), d) => (l, d),
                    (None, _) => {
                        uart_write_str("ERR_LEN\r\n");
                        continue;
                    }
                };
                while delim != b'\n' {
                    delim = uart_read_byte_blocking();
                }

                let mut buf = [0u8; 512];
                let to_read = len.min(512) as usize;
                for i in 0..to_read {
                    buf[i] = uart_read_byte_blocking();
                }
                if nor.write(slot_base + offset, &buf[..to_read]).is_ok() {
                    uart_write_str("OK\r\n");
                } else {
                    uart_write_str("ERR_WR\r\n");
                }
            }
            b'm' | b'M' => {
                // Mark slot as pending in journal: m <slot>\n
                let mut slot_char = uart_read_byte_blocking();
                while slot_char == b' ' || slot_char == b'\t' {
                    slot_char = uart_read_byte_blocking();
                }
                let slot_id = if slot_char == b'0' { 0 } else { 1 };
                if let Ok(mut journal) = JournalManager::init(&mut nor) {
                    let res = journal.append_record(
                        &mut nor,
                        slot_id,
                        SLOT_STATE_PENDING,
                        0,
                        0,
                        ImageVersion {
                            major: 1,
                            minor: 0,
                            revision: 0,
                            build_num: 1,
                        },
                        0,
                        0,
                    );
                    if res.is_ok() {
                        uart_write_str("OK\r\n");
                    } else {
                        uart_write_str("ERR_JRN\r\n");
                    }
                } else {
                    uart_write_str("ERR_INIT\r\n");
                }
            }
            b'r' | b'R' => {
                // Reboot
                uart_write_str("REBOOT\r\n");
                cortex_m::peripheral::SCB::sys_reset();
            }
            _ => {
                // Ignore unexpected characters (whitespace, etc.)
            }
        }
    }
}
