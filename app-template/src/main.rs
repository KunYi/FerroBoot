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
    // 1. Initialize Clocks, SPI1, and USART1 for Application
    boot_stm32h7::init_clocks();
    boot_stm32h7::init_nor_cs();
    boot_stm32h7::init_spi1();
    boot_stm32h7::init_usart1();

    boot_stm32h7::uart_write_str("\r\n========================================\r\n");
    boot_stm32h7::uart_write_str("[APP] Hello from Application running in AXI SRAM!\r\n");
    boot_stm32h7::uart_write_str("[APP] VTOR: 0x24000400, Executing from RAM.\r\n");

    // Connect to Bootloader ABI at 0x08000440
    if let Some(client) = BootApiClient::new() {
        boot_stm32h7::uart_write_str("[APP] Connected to Bootloader ABI successfully.\r\n");
        if let Ok(info) = client.get_boot_info() {
            boot_stm32h7::uart_write_str("[APP] Boot Slot: ");
            boot_stm32h7::uart_write_hex(info.boot_slot);
            boot_stm32h7::uart_write_str(", Trial Count: ");
            boot_stm32h7::uart_write_hex(info.trial_count);
            boot_stm32h7::uart_write_str("\r\n");

            // If in Trial boot, confirm the image to cancel automatic rollback
            if info.trial_count > 0 {
                boot_stm32h7::uart_write_str("[APP] Confirming firmware update via ABI...\r\n");
                let res = client.confirm(&APP_IO_OPS);
                if res.is_ok() {
                    boot_stm32h7::uart_write_str("[APP] Firmware successfully CONFIRMED! Boot is permanent.\r\n");
                } else {
                    boot_stm32h7::uart_write_str("[APP] Firmware confirm failed!\r\n");
                }
            }
        }
    } else {
        boot_stm32h7::uart_write_str("[APP] ERROR: Failed to locate Bootloader ABI at 0x08000440!\r\n");
    }

    boot_stm32h7::uart_write_str("========================================\r\n");

    // Application main loop
    let mut led_state = false;
    loop {
        boot_stm32h7::Iwdg::kick();
        led_state = !led_state;
        boot_stm32h7::set_led(led_state);
        for _ in 0..2_000_000 {
            cortex_m::asm::nop();
        }
    }
}
