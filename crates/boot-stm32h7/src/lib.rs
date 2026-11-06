//! STM32H750 Hardware Abstraction & Board Bring-up
//!
//! Provides clock init (400MHz/VOS1), SRAM4 ECC zeroing, SPI1 NOR driver,
//! USART1 driver, RTC backup registers, IWDG watchdog, and the §6.1 jump routine.

#![no_std]

pub mod board;

use board::*;
use boot_abi::{SRAM4_BASE, SRAM4_SIZE, TAMP_CMD_ENTER_RESCUE};
use stm32_metapac as pac;

/// Reads the unique 96-bit MCU UID from STM32H7 factory information block.
pub fn read_mcu_uid() -> [u8; 12] {
    let uid_base = 0x1FF0_F420 as *const u32;
    let mut uid = [0u8; 12];
    unsafe {
        let w0 = core::ptr::read_volatile(uid_base);
        let w1 = core::ptr::read_volatile(uid_base.add(1));
        let w2 = core::ptr::read_volatile(uid_base.add(2));
        uid[0..4].copy_from_slice(&w0.to_le_bytes());
        uid[4..8].copy_from_slice(&w1.to_le_bytes());
        uid[8..12].copy_from_slice(&w2.to_le_bytes());
    }
    uid
}

/// Enables SRAM4 clock and zero-initializes with 64-bit aligned writes for RAM ECC.
pub fn init_sram4_ecc() {
    pac::RCC.ahb4enr().modify(|w| {
        w.0 |= RCC_AHB4ENR_RAMD3EN;
    });

    // Zero 64-bit aligned for ECC initialization (U-16, E2)
    let sram_ptr = SRAM4_BASE as *mut u64;
    let count = (SRAM4_SIZE / 8) as usize;
    for i in 0..count {
        unsafe {
            core::ptr::write_volatile(sram_ptr.add(i), 0);
        }
    }
}

/// Initializes MCU system clocks to 400MHz (VOS1) using 25MHz HSE crystal.
pub fn init_clocks() {
    // 1. Configure power supply (LDO) and Voltage Output Scale 1 (VOS1)
    pac::PWR.cr3().modify(|w| {
        w.set_ldoen(true);
    });

    pac::PWR.d3cr().modify(|w| {
        w.set_vos(pac::pwr::vals::Vos::SCALE1);
    });

    while !pac::PWR.d3cr().read().vosrdy() {
        // Wait for voltage scaling ready
    }

    // 2. Configure Flash latency to 4 wait states (for 400MHz operation)
    pac::FLASH.acr().modify(|w| {
        w.set_latency(4);
    });

    // Configure bus prescalers (HCLK max 200MHz, APB1/2/3/4 max 100MHz):
    pac::RCC.d1cfgr().modify(|w| {
        w.set_d1cpre(pac::rcc::vals::Hpre::DIV1);
        w.set_hpre(pac::rcc::vals::Hpre::DIV2);
        w.set_d1ppre(pac::rcc::vals::Ppre::DIV2);
    });

    pac::RCC.d2cfgr().modify(|w| {
        w.set_d2ppre1(pac::rcc::vals::Ppre::DIV2);
        w.set_d2ppre2(pac::rcc::vals::Ppre::DIV2);
    });

    pac::RCC.d3cfgr().modify(|w| {
        w.set_d3ppre(pac::rcc::vals::Ppre::DIV2);
    });

    // 3. Enable HSE (25MHz)
    pac::RCC.cr().modify(|w| {
        w.set_hseon(true);
    });

    while !pac::RCC.cr().read().hserdy() {
        // Wait for HSE ready
    }

    // 4. Configure PLL1:
    // HSE = 25MHz. DIVM1 = 5 -> PLL input = 5MHz.
    // DIVN1 = 160 -> VCO = 800MHz.
    // DIVP1 = 2 -> PLL1_P (SYSCLK) = 400MHz.
    pac::RCC.pllckselr().modify(|w| {
        w.set_pllsrc(pac::rcc::vals::Pllsrc::HSE);
        w.set_divm(0, pac::rcc::vals::Pllm::DIV5);
    });

    pac::RCC.plldivr(0).modify(|w| {
        w.set_plln(pac::rcc::vals::Plln::from_bits(160 - 1));
        w.set_pllp(pac::rcc::vals::Plldiv::DIV2);
    });

    pac::RCC.pllcfgr().modify(|w| {
        w.set_divpen(0, true);
        w.set_pllrge(0, pac::rcc::vals::Pllrge::RANGE4);
        w.set_pllvcosel(0, pac::rcc::vals::Pllvcosel::WIDE_VCO);
    });

    pac::RCC.cr().modify(|w| {
        w.set_pllon(0, true);
    });

    while !pac::RCC.cr().read().pllrdy(0) {
        // Wait for PLL1 ready
    }

    // 5. Switch System Clock to PLL1
    pac::RCC.cfgr().modify(|w| {
        w.set_sw(pac::rcc::vals::Sw::PLL1_P);
    });

    while pac::RCC.cfgr().read().sws() != pac::rcc::vals::Sw::PLL1_P {
        // Wait for switch to PLL1
    }

    // Enable GPIO clocks
    pac::RCC.ahb4enr().modify(|w| {
        w.set_gpioaen(true);
        w.set_gpioben(true);
        w.set_gpiocen(true);
    });
}

/// Initializes PA4 (NOR CS) as GPIO output driven HIGH immediately.
pub fn init_nor_cs() {
    let gpioa = pac::GPIOA;
    gpioa.moder().modify(|w| {
        w.set_moder(PIN_NOR_CS_PIN as usize, pac::gpio::vals::Moder::OUTPUT);
    });
    gpioa.ospeedr().modify(|w| {
        w.set_ospeedr(PIN_NOR_CS_PIN as usize, pac::gpio::vals::Ospeedr::VERY_HIGH_SPEED);
    });
    gpioa.bsrr().write(|w| {
        w.set_bs(PIN_NOR_CS_PIN as usize, true);
    });
}

/// Sets PA4 CS state.
pub fn set_nor_cs(assert: bool) {
    let gpioa = pac::GPIOA;
    if assert {
        gpioa.bsrr().write(|w| {
            w.set_br(PIN_NOR_CS_PIN as usize, true);
        });
    } else {
        gpioa.bsrr().write(|w| {
            w.set_bs(PIN_NOR_CS_PIN as usize, true);
        });
    }
}

/// Initializes PC3 as LED status output and PC13 as rescue button input.
pub fn init_gpio_ui() {
    let gpioc = pac::GPIOC;

    // PC3 Output
    gpioc.moder().modify(|w| {
        w.set_moder(PIN_LED_PIN as usize, pac::gpio::vals::Moder::OUTPUT);
    });
    set_led(false);

    // PC13 Input: pull resistor chosen from board polarity
    gpioc.moder().modify(|w| {
        w.set_moder(PIN_KEY2_PIN as usize, pac::gpio::vals::Moder::INPUT);
    });
    gpioc.pupdr().modify(|w| {
        w.set_pupdr(
            PIN_KEY2_PIN as usize,
        if KEY2_ACTIVE_LOW {
                pac::gpio::vals::Pupdr::PULL_UP
            } else {
                pac::gpio::vals::Pupdr::PULL_DOWN
            },
        );
    });
    cortex_m::asm::delay(100_000);
}

pub fn set_led(on: bool) {
    let gpioc = pac::GPIOC;
    let pin = PIN_LED_PIN as usize;
    if on ^ LED_ACTIVE_LOW {
        gpioc.bsrr().write(|w| w.set_bs(pin, true));
    } else {
        gpioc.bsrr().write(|w| w.set_br(pin, true));
    }
}

pub fn is_key2_pressed() -> bool {
    let gpioc = pac::GPIOC;
    let is_high = gpioc.idr().read().idr(PIN_KEY2_PIN as usize) == pac::gpio::vals::Idr::HIGH;
    if KEY2_ACTIVE_LOW { !is_high } else { is_high }
}

/// Access to STM32 RTC backup registers for boot handshake flags (§7.3).
pub struct TampControl;

impl TampControl {
    pub fn enable_backup_domain() {
        pac::PWR.cr1().modify(|w| {
            w.set_dbp(true);
        });
    }

    pub fn read_bkp0() -> u32 {
        pac::RTC.bkpr(0).read().bkp()
    }

    pub fn write_bkp0(val: u32) {
        pac::RTC.bkpr(0).write(|w| w.set_bkp(val));
    }

    pub fn read_bkp1() -> u32 {
        pac::RTC.bkpr(1).read().bkp()
    }

    pub fn write_bkp1(val: u32) {
        pac::RTC.bkpr(1).write(|w| w.set_bkp(val));
    }

    pub fn check_and_clear_rescue_request() -> bool {
        Self::enable_backup_domain();
        let cmd = Self::read_bkp0();
        if cmd == TAMP_CMD_ENTER_RESCUE {
            Self::write_bkp0(0);
            true
        } else {
            false
        }
    }
}

/// Independent Watchdog (IWDG) control during Trial boots (§6, T3.3).
pub struct Iwdg;

impl Iwdg {
    pub fn start(timeout_ms: u32) {
        let iwdg = pac::IWDG1;
        iwdg.kr().write(|w| w.set_key(0x5555.into()));
        iwdg.pr().write(|w| w.set_pr(pac::iwdg::vals::Pr::DIVIDE_BY128));
        let reload = ((timeout_ms * 250) / 1000).min(0xFFF);
        iwdg.rlr().write(|w| w.set_rl(reload as u16));
        iwdg.kr().write(|w| w.set_key(0xAAAA.into()));
        iwdg.kr().write(|w| w.set_key(0xCCCC.into()));
    }

    pub fn kick() {
        pac::IWDG1.kr().write(|w| w.set_key(0xAAAA.into()));
    }
}

/// Polling SPI1 driver for NOR Flash.
pub fn init_spi1() {
    let gpioa = pac::GPIOA;
    let gpiob = pac::GPIOB;

    // Configure PA5 (SCK), PA6 (MISO) as AF5 (in AFRL, index 0)
    gpioa.moder().modify(|w| {
        w.set_moder(5, pac::gpio::vals::Moder::ALTERNATE);
        w.set_moder(6, pac::gpio::vals::Moder::ALTERNATE);
    });
    gpioa.afr(0).modify(|w| {
        w.set_afr(5, 5); // AF5
        w.set_afr(6, 5); // AF5
    });

    // Configure PB5 (MOSI) as AF5 (in AFRL, index 0)
    gpiob.moder().modify(|w| {
        w.set_moder(5, pac::gpio::vals::Moder::ALTERNATE);
    });
    gpiob.afr(0).modify(|w| {
        w.set_afr(5, 5); // AF5
    });

    // Enable SPI1 clock
    pac::RCC.apb2enr().modify(|w| {
        w.set_spi1en(true);
    });

    // Configure SPI1 in Master Mode, Mode 0 (CPOL=0, CPHA=0)
    let spi = pac::SPI1;
    spi.cr1().modify(|w| {
        w.set_spe(false);
    });

    // Set SSI high while SPE is 0 to prevent MODF when enabling Master mode
    spi.cr1().modify(|w| {
        w.set_ssi(true);
    });

    spi.cfg1().modify(|w| {
        w.set_mbr(pac::spi::vals::Mbr::DIV16);
        w.set_dsize(7); // 8-bit data frame
    });

    spi.cfg2().modify(|w| {
        w.set_master(pac::spi::vals::Master::MASTER);
        w.set_ssm(true);
        w.set_ssoe(false);
        w.set_cpol(pac::spi::vals::Cpol::IDLE_LOW);
        w.set_cpha(pac::spi::vals::Cpha::FIRST_EDGE);
    });

    // Clear any pending MODF or error flags
    spi.ifcr().write(|w| {
        w.set_modfc(true);
        w.set_ovrc(true);
    });

    // Enable SPI peripheral
    spi.cr1().modify(|w| {
        w.set_ssi(true);
        w.set_spe(true);
    });
}

/// Blocking SPI transfer for STM32H7 SPI1 (SPI V5).
pub fn spi1_transfer(tx: *const u8, rx: *mut u8, len: u32) -> i32 {
    if len == 0 {
        return 0;
    }
    let spi = pac::SPI1;

    // Clear any previous error flags
    spi.ifcr().write(|w| {
        w.set_eotc(true);
        w.set_txtfc(true);
        w.set_modfc(true);
        w.set_ovrc(true);
        w.set_udrc(true);
    });

    // Ensure Master is active, SSI is high and SPE is enabled
    spi.cfg2().modify(|w| {
        w.set_master(pac::spi::vals::Master::MASTER);
        w.set_ssm(true);
    });
    spi.cr1().modify(|w| {
        w.set_ssi(true);
        w.set_spe(true);
    });

    // Set packet transfer size
    spi.cr2().modify(|w| {
        w.set_tsize(len as u16);
    });

    // Start transfer
    spi.cr1().modify(|w| {
        w.set_cstart(true);
    });

    for i in 0..len as usize {
        let byte_out = if !tx.is_null() {
            unsafe { *tx.add(i) }
        } else {
            0x00
        };

        // Write to TXDR with timeout
        let mut timeout = 200_000;
        while !spi.sr().read().txp() && timeout > 0 {
            timeout -= 1;
        }
        if timeout == 0 {
            return -1;
        }
        unsafe {
            core::ptr::write_volatile(spi.txdr8().as_ptr(), byte_out);
        }

        // Read from RXDR with timeout
        timeout = 200_000;
        while !spi.sr().read().rxp() && timeout > 0 {
            timeout -= 1;
        }
        if timeout == 0 {
            return -2;
        }
        let byte_in = unsafe { core::ptr::read_volatile(spi.rxdr8().as_ptr()) };

        if !rx.is_null() {
            unsafe {
                *rx.add(i) = byte_in;
            }
        }
    }

    // Wait for End Of Transfer with timeout
    let mut timeout = 200_000;
    while !spi.sr().read().eot() && timeout > 0 {
        timeout -= 1;
    }

    // Clear EOT and TXTF flags
    spi.ifcr().write(|w| {
        w.set_eotc(true);
        w.set_txtfc(true);
    });

    if timeout == 0 { -3 } else { 0 }
}

/// Polling USART1 driver for debug log and UART rescue.
pub fn init_usart1() {
    let gpioa = pac::GPIOA;
    // PA9 (TX), PA10 (RX) as AF7 (in AFRH, index 1)
    gpioa.moder().modify(|w| {
        w.set_moder(9, pac::gpio::vals::Moder::ALTERNATE);
        w.set_moder(10, pac::gpio::vals::Moder::ALTERNATE);
    });
    gpioa.afr(1).modify(|w| {
        w.set_afr(9 - 8, 7);
        w.set_afr(10 - 8, 7);
    });

    pac::RCC.apb2enr().modify(|w| {
        w.set_usart1en(true);
    });

    let usart = pac::USART1;
    usart.cr1().modify(|w| {
        w.set_ue(false);
    });

    // 115200 baud at 100MHz APB2
    usart.brr().write(|w| {
        w.set_brr((100_000_000 / UART_BAUD_RATE) as u16);
    });

    usart.cr1().modify(|w| {
        w.set_te(true);
        w.set_re(true);
        w.set_ue(true);
    });
}

pub fn uart_write_byte(b: u8) {
    let usart = pac::USART1;
    while !usart.isr().read().txe() {}
    usart.tdr().write(|w| w.set_dr(b as u16));
}

pub fn uart_write_str(s: &str) {
    for b in s.bytes() {
        uart_write_byte(b);
    }
}

pub fn uart_write_hex(b: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    uart_write_byte(HEX[(b >> 4) as usize]);
    uart_write_byte(HEX[(b & 0x0F) as usize]);
}

pub fn uart_read_byte() -> Option<u8> {
    let usart = pac::USART1;
    if usart.isr().read().rxne() {
        Some((usart.rdr().read().dr() & 0xFF) as u8)
    } else {
        None
    }
}

pub fn uart_read_byte_blocking() -> u8 {
    let usart = pac::USART1;
    while !usart.isr().read().rxne() {
        Iwdg::kick();
    }
    (usart.rdr().read().dr() & 0xFF) as u8
}

/// Executes jump to Application strictly following §6.1 checklist.
///
/// # Safety
/// This function changes processor state, disables interrupts, changes VTOR, and resets SP.
pub unsafe fn jump_to_app(vtor_addr: u32) -> ! {
    // 1. Disable all interrupts globally
    cortex_m::interrupt::disable();

    // 2. Clear NVIC enable and pending registers
    let nvic = cortex_m::peripheral::NVIC::PTR;
    for i in 0..8 {
        (*nvic).icer[i].write(0xFFFF_FFFF);
        (*nvic).icpr[i].write(0xFFFF_FFFF);
    }

    let syst = cortex_m::peripheral::SYST::PTR;
    (*syst).csr.write(0);
    (*syst).rvr.write(0);
    (*syst).cvr.write(0);

    // 3. Deinit SPI, GPIO, UART
    pac::SPI1.cr1().modify(|w| w.set_spe(false));
    pac::USART1.cr1().modify(|w| w.set_ue(false));

    // 4. Restore RCC to HSI (64MHz), wait HSI ready, keep SRAM4 clock enabled
    pac::RCC.cr().modify(|w| w.set_hsion(true));
    while !pac::RCC.cr().read().hsirdy() {}
    pac::RCC.cfgr().modify(|w| w.set_sw(pac::rcc::vals::Sw::HSI));
    while pac::RCC.cfgr().read().sws() != pac::rcc::vals::Sw::HSI {}

    // Ensure SRAM4 clock remains enabled for BootInfo
    pac::RCC.ahb4enr().modify(|w| {
        w.0 |= RCC_AHB4ENR_RAMD3EN;
    });

    // 5. Memory barriers
    cortex_m::asm::dsb();
    cortex_m::asm::isb();

    // 6. Set VTOR = vtor_addr (0x24000400)
    let scb = cortex_m::peripheral::SCB::PTR;
    (*scb).vtor.write(vtor_addr);

    // 7. Read MSP and Reset Vector from VTOR table
    let msp = *(vtor_addr as *const u32);
    let reset_handler = *((vtor_addr + 4) as *const u32);

    // 8. Reset MSP and branch to reset handler
    core::arch::asm!(
        "msr msp, {msp}",
        "bx {reset}",
        msp = in(reg) msp,
        reset = in(reg) reset_handler,
        options(noreturn)
    );
}
