//! Board-specific hardware definitions for STM32H750 NetLite (§4.4, T0.6)
//!
//! Pin assignments, clock frequencies, power configurations, and polarities.

pub const HSE_FREQ_HZ: u32 = 25_000_000; // 25MHz external crystal
pub const CORE_CLOCK_HZ: u32 = 400_000_000; // 400MHz VOS1
pub const UART_BAUD_RATE: u32 = 115_200;

// SPI NOR Pins (SPI1)
pub const PIN_NOR_CS_PORT: char = 'A';
pub const PIN_NOR_CS_PIN: u8 = 4; // PA4 (Software CS, default HIGH)

pub const PIN_NOR_SCK_PORT: char = 'A';
pub const PIN_NOR_SCK_PIN: u8 = 5; // PA5 (AF5 SPI1_SCK)

pub const PIN_NOR_MISO_PORT: char = 'A';
pub const PIN_NOR_MISO_PIN: u8 = 6; // PA6 (AF5 SPI1_MISO)

pub const PIN_NOR_MOSI_PORT: char = 'B';
pub const PIN_NOR_MOSI_PIN: u8 = 5; // PB5 (AF5 SPI1_MOSI)

// Debug & Rescue UART (USART1)
pub const PIN_UART_TX_PORT: char = 'A';
pub const PIN_UART_TX_PIN: u8 = 9; // PA9 (AF7 USART1_TX)

pub const PIN_UART_RX_PORT: char = 'A';
pub const PIN_UART_RX_PIN: u8 = 10; // PA10 (AF7 USART1_RX)

// User Interface
pub const PIN_LED_PORT: char = 'C';
pub const PIN_LED_PIN: u8 = 3; // PC3 (Status LED)
pub const LED_ACTIVE_LOW: bool = true;

pub const PIN_KEY2_PORT: char = 'C';
pub const PIN_KEY2_PIN: u8 = 13; // PC13 (Rescue button)
pub const KEY2_ACTIVE_LOW: bool = true;

// SRAM Clocks required
pub const RCC_AHB4ENR_RAMD3EN: u32 = 1 << 24; // SRAM4 clock enable (AHB4)
