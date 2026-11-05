//! Application Template executed from AXI SRAM (0x24000400)
//!
//! Connects to the Bootloader resident service ABI, reads BootInfo from SRAM4,
//! confirms test boot if needed, and operates user I/O.

#![no_std]
#![no_main]

use boot_abi::IoOps;
use boot_api_client::BootApiClient;
use cortex_m_rt::entry;
use panic_halt as _;

static APP_IO_OPS: IoOps = IoOps {
    spi_transfer: app_spi_transfer,
    cs: app_cs,
    millis: app_millis,
    yield_: app_yield,
    reserved: [0; 4],
};

unsafe extern "C" fn app_spi_transfer(tx: *const u8, rx: *mut u8, len: u32) -> i32 {
    boot_stm32h7::spi1_transfer(tx, rx, len)
}

unsafe extern "C" fn app_cs(assert: bool) {
    boot_stm32h7::set_nor_cs(assert);
}

static mut APP_TICKS: u32 = 0;

unsafe extern "C" fn app_millis() -> u32 {
    APP_TICKS
}

unsafe extern "C" fn app_yield() {
    boot_stm32h7::Iwdg::kick();
}

#[entry]
fn main() -> ! {
    // Connect to Bootloader ABI at 0x08000440
    if let Some(client) = BootApiClient::new() {
        if let Ok(info) = client.get_boot_info() {
            // If in Trial boot, confirm the image to cancel automatic rollback
            if info.trial_count > 0 {
                let _ = client.confirm(&APP_IO_OPS);
            }
        }
    }

    // Application main loop
    loop {
        boot_stm32h7::Iwdg::kick();
        for _ in 0..2_000_000 {
            cortex_m::asm::nop();
        }
    }
}
