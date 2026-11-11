//! Application Template executed from AXI SRAM (0x24000400)
//!
//! Connects to the Bootloader resident service ABI, reads BootInfo from SRAM4,
//! confirms test boot if needed, and operates user I/O.

#![no_std]
#![no_main]

use boot_abi::{IoOps, ERR_INVALID_STATE, SLOT_STATE_TESTING};
use boot_api_client::BootApiClient;
use cortex_m_rt::{entry, exception};
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

unsafe extern "C" fn app_millis() -> u32 {
    boot_stm32h7::millis()
}

/// 1 kHz tick that drives `app_millis()`; the bootloader's NOR timeouts rely on it.
#[exception]
fn SysTick() {
    boot_stm32h7::systick_tick();
}

unsafe extern "C" fn app_yield() {
    boot_stm32h7::Iwdg::kick();
}

#[entry]
fn main() -> ! {
    // 1. Initialize Clocks, SPI1, and USART1 for Application
    boot_stm32h7::init_clocks();
    boot_stm32h7::init_millis(); // also re-enables interrupts, which the bootloader left masked
    boot_stm32h7::init_gpio_ui(); // do not rely on GPIO state left behind by the bootloader
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

            // Confirm only while this slot is on trial (state Testing). Decide from the slot
            // state, not from trial_count: a confirmed image must not try to confirm again.
            let testing = matches!(
                client.slot_status(info.boot_slot),
                Ok(s) if s.state == SLOT_STATE_TESTING
            );
            if testing {
                boot_stm32h7::uart_write_str("[APP] Confirming firmware update via ABI...\r\n");
                match client.confirm(&APP_IO_OPS) {
                    Ok(()) => boot_stm32h7::uart_write_str(
                        "[APP] Firmware successfully CONFIRMED! Boot is permanent.\r\n",
                    ),
                    Err(ERR_INVALID_STATE) => {
                        boot_stm32h7::uart_write_str("[APP] Firmware already confirmed.\r\n")
                    }
                    Err(_) => boot_stm32h7::uart_write_str("[APP] Firmware confirm failed!\r\n"),
                }
            } else {
                boot_stm32h7::uart_write_str("[APP] Firmware already confirmed, normal boot.\r\n");
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
        for _ in 0..50_000_000 {
            cortex_m::asm::nop();
        }
    }
}
