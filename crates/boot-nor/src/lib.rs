//! SPI NOR Flash Driver and Host-Simulated Mock
//!
//! Supports Boya BY25Q32ES and Winbond W25Q32JV (3-byte address, 4MB).
//! Handles Page Program (256B), Sector Erase (4KB), Block Erase (64KB),
//! JEDEC ID, Status Registers, and Security Register OTP operations.

#![no_std]

#[cfg(feature = "std")]
extern crate std;

use boot_abi::IoOps;
use boot_crc::Hasher;
// `Flash` itself is deliberately not imported: the drivers implement both `NorFlash` and
// `Flash`, which share method names (`read`), so having both traits in scope makes
// `nor.read(..)` ambiguous. The impls below name it by its full path.
use boot_flash::FlashError;
use core::ops::Range;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// SPI NOR geometry (BY25Q32ES and compatible). These describe the *chip*, not the board.
pub const NOR_PAGE_SIZE: u32 = 256;
/// 4 KiB sector erase unit.
pub const NOR_SECTOR_SIZE: u32 = 4096;
/// 64 KiB block erase unit.
pub const NOR_BLOCK_SIZE: u32 = 65536;
/// Capacity of the supported chips (BY25Q32ES, W25Q32JV: 32 Mbit = 4 MiB).
pub const NOR_CAPACITY: u32 = 4 * 1024 * 1024;

pub const CMD_WRITE_ENABLE: u8 = 0x06;
pub const CMD_WRITE_DISABLE: u8 = 0x04;
pub const CMD_READ_STATUS_1: u8 = 0x05;
pub const CMD_READ_STATUS_2: u8 = 0x35;
pub const CMD_WRITE_STATUS_2: u8 = 0x31;
pub const CMD_PAGE_PROGRAM: u8 = 0x02;
pub const CMD_SECTOR_ERASE_4K: u8 = 0x20;
pub const CMD_BLOCK_ERASE_64K: u8 = 0xD8;
pub const CMD_READ_DATA: u8 = 0x03;
pub const CMD_FAST_READ: u8 = 0x0B;
pub const CMD_READ_JEDEC_ID: u8 = 0x9F;
pub const CMD_RESET_ENABLE: u8 = 0x66;
pub const CMD_RESET: u8 = 0x99;
pub const CMD_READ_UNIQUE_ID: u8 = 0x4B;
pub const CMD_READ_SECURITY_REG: u8 = 0x48;
pub const CMD_PROGRAM_SECURITY_REG: u8 = 0x42;
pub const CMD_ERASE_SECURITY_REG: u8 = 0x44;

pub const SR1_WIP: u8 = 0x01; // Write in Progress
pub const SR1_WEL: u8 = 0x02; // Write Enable Latch
pub const SR1_BP_MASK: u8 = 0x1C; // Block Protect bits
pub const SR2_QE: u8 = 0x02; // Quad Enable
pub const SR2_LB1: u8 = 0x08; // Security Register 1 Lock bit (bit 3)
pub const SR2_LB2: u8 = 0x10; // Security Register 2 Lock bit (bit 4)
pub const SR2_LB3: u8 = 0x20; // Security Register 3 Lock bit (bit 5)
pub const SR2_CMP: u8 = 0x40; // Complement Protect

pub const JEDEC_ID_BOYA_BY25Q32ES: [u8; 3] = [0x68, 0x40, 0x16];
pub const JEDEC_ID_WINBOND_W25Q32JV: [u8; 3] = [0xEF, 0x40, 0x16];

pub const SEED_RECORD_MAGIC: u32 = 0x5345_4544; // 'SEED'

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NorError {
    IoError,
    /// The `IoOps` provides no SPI transport (the platform's bootloader owns the flash).
    NoTransport,
    Timeout,
    Protected,
    InvalidAddress,
    Unaligned,
    Locked,
    Corrupted,
    PowerCutSimulated,
}

/// Seed record stored in NOR Security Register #1 (64 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct SeedRecord {
    pub magic: u32,
    pub fmt_ver: u8,
    pub reserved1: [u8; 3],
    pub seed: [u8; 32],
    pub reserved2: [u8; 20],
    pub crc32: u32,
}

impl SeedRecord {
    pub fn new(seed: [u8; 32]) -> Self {
        let mut rec = Self {
            magic: SEED_RECORD_MAGIC,
            fmt_ver: 1,
            reserved1: [0xFF; 3],
            seed,
            reserved2: [0xFF; 20],
            crc32: 0,
        };
        let mut hasher = Hasher::new();
        hasher.update(&rec.magic.to_le_bytes());
        hasher.update(&[rec.fmt_ver]);
        hasher.update(&rec.reserved1);
        hasher.update(&rec.seed);
        hasher.update(&rec.reserved2);
        rec.crc32 = hasher.finalize();
        rec
    }

    pub fn is_valid(&self) -> bool {
        if self.magic != SEED_RECORD_MAGIC || self.fmt_ver != 1 {
            return false;
        }
        let mut hasher = Hasher::new();
        hasher.update(&self.magic.to_le_bytes());
        hasher.update(&[self.fmt_ver]);
        hasher.update(&self.reserved1);
        hasher.update(&self.seed);
        hasher.update(&self.reserved2);
        self.crc32 == hasher.finalize()
    }
}

/// Abstract NOR flash operations.
pub trait NorFlash {
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), NorError>;
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), NorError>;
    fn erase_sector_4k(&mut self, addr: u32) -> Result<(), NorError>;
    fn erase_block_64k(&mut self, addr: u32) -> Result<(), NorError>;
    fn read_jedec_id(&mut self) -> Result<[u8; 3], NorError>;
    fn read_status1(&mut self) -> Result<u8, NorError>;
    fn read_status2(&mut self) -> Result<u8, NorError>;
    fn wait_ready(&mut self, timeout_ms: u32) -> Result<(), NorError>;
}

/// Abstract Security Register and hardware UID operations.
pub trait SecureRegion {
    fn read_unique_id(&mut self, buf: &mut [u8]) -> Result<usize, NorError>;
    fn read_security_register(&mut self, reg_num: u8, offset: u16, buf: &mut [u8]) -> Result<(), NorError>;
    fn program_security_register(&mut self, reg_num: u8, offset: u16, data: &[u8]) -> Result<(), NorError>;
    fn lock_security_register(&mut self, reg_num: u8) -> Result<(), NorError>;
    fn is_security_register_locked(&mut self, reg_num: u8) -> Result<bool, NorError>;
}

/// Driver operating directly on `IoOps` pointer provided via ABI or local hardware.
pub struct SpiNorIoOpsDriver<'a> {
    io: &'a IoOps,
}

impl<'a> SpiNorIoOpsDriver<'a> {
    pub fn new(io: &'a IoOps) -> Self {
        Self { io }
    }

    /// Drives chip select if the platform provides one (no-op otherwise; the transfer itself then
    /// fails cleanly with `NoTransport`).
    fn cs_set(&self, assert: bool) {
        if let Some(cs) = self.io.cs {
            unsafe { cs(assert) };
        }
    }

    fn spi_call(&self, tx: *const u8, rx: *mut u8, len: usize) -> Result<(), NorError> {
        let Some(transfer) = self.io.spi_transfer else {
            return Err(NorError::NoTransport);
        };
        let ret = unsafe { transfer(tx, rx, len as u32) };
        if ret == 0 {
            Ok(())
        } else {
            Err(NorError::IoError)
        }
    }

    fn spi_xfer(&self, tx: &[u8], rx: &mut [u8]) -> Result<(), NorError> {
        self.spi_call(tx.as_ptr(), rx.as_mut_ptr(), tx.len())
    }

    fn write_cmd(&self, cmd: &[u8]) -> Result<(), NorError> {
        self.cs_set(true);
        let mut rx = [0u8; 8];
        let res = self.spi_xfer(cmd, &mut rx[..cmd.len()]);
        self.cs_set(false);
        res
    }

    pub fn write_enable(&self) -> Result<(), NorError> {
        self.write_cmd(&[CMD_WRITE_ENABLE])
    }

    pub fn software_reset(&self) -> Result<(), NorError> {
        self.write_cmd(&[CMD_RESET_ENABLE])?;
        self.write_cmd(&[CMD_RESET])?;
        // Wait at least 0.5ms after reset
        let start = unsafe { (self.io.millis)() };
        while unsafe { (self.io.millis)() }.wrapping_sub(start) < 1 {
            unsafe { (self.io.yield_)() };
        }
        Ok(())
    }
}

impl<'a> NorFlash for SpiNorIoOpsDriver<'a> {
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), NorError> {
        let cmd = [
            CMD_FAST_READ,
            (addr >> 16) as u8,
            (addr >> 8) as u8,
            addr as u8,
            0x00, // dummy byte
        ];
        self.cs_set(true);
        let mut dummy = [0u8; 5];
        let res1 = self.spi_xfer(&cmd, &mut dummy);
        let res2 = if res1.is_ok() {
            // Read incoming data by clocking out zeros
            self.spi_call(core::ptr::null(), buf.as_mut_ptr(), buf.len())
        } else {
            res1
        };
        self.cs_set(false);
        res2
    }

    fn write(&mut self, mut addr: u32, mut data: &[u8]) -> Result<(), NorError> {
        while !data.is_empty() {
            self.write_enable()?;
            let page_offset = (addr % NOR_PAGE_SIZE) as usize;
            let chunk_len = (NOR_PAGE_SIZE as usize - page_offset).min(data.len());

            let cmd = [
                CMD_PAGE_PROGRAM,
                (addr >> 16) as u8,
                (addr >> 8) as u8,
                addr as u8,
            ];
            self.cs_set(true);
            let mut dummy = [0u8; 4];
            let mut rx_dummy = [0u8; 256];
            let res = self.spi_xfer(&cmd, &mut dummy).and_then(|_| {
                self.spi_xfer(&data[..chunk_len], &mut rx_dummy[..chunk_len])
            });
            // Always release chip select, also when a transfer failed.
            self.cs_set(false);
            res?;

            self.wait_ready(100)?;
            addr += chunk_len as u32;
            data = &data[chunk_len..];
        }
        Ok(())
    }

    fn erase_sector_4k(&mut self, addr: u32) -> Result<(), NorError> {
        self.write_enable()?;
        let cmd = [
            CMD_SECTOR_ERASE_4K,
            (addr >> 16) as u8,
            (addr >> 8) as u8,
            addr as u8,
        ];
        self.write_cmd(&cmd)?;
        self.wait_ready(1000)
    }

    fn erase_block_64k(&mut self, addr: u32) -> Result<(), NorError> {
        self.write_enable()?;
        let cmd = [
            CMD_BLOCK_ERASE_64K,
            (addr >> 16) as u8,
            (addr >> 8) as u8,
            addr as u8,
        ];
        self.write_cmd(&cmd)?;
        self.wait_ready(3000)
    }

    fn read_jedec_id(&mut self) -> Result<[u8; 3], NorError> {
        self.cs_set(true);
        let cmd = [CMD_READ_JEDEC_ID, 0, 0, 0];
        let mut rx = [0u8; 4];
        let res = self.spi_xfer(&cmd, &mut rx);
        self.cs_set(false);
        res.map(|_| [rx[1], rx[2], rx[3]])
    }

    fn read_status1(&mut self) -> Result<u8, NorError> {
        self.cs_set(true);
        let cmd = [CMD_READ_STATUS_1, 0];
        let mut rx = [0u8; 2];
        let res = self.spi_xfer(&cmd, &mut rx);
        self.cs_set(false);
        res.map(|_| rx[1])
    }

    fn read_status2(&mut self) -> Result<u8, NorError> {
        self.cs_set(true);
        let cmd = [CMD_READ_STATUS_2, 0];
        let mut rx = [0u8; 2];
        let res = self.spi_xfer(&cmd, &mut rx);
        self.cs_set(false);
        res.map(|_| rx[1])
    }

    fn wait_ready(&mut self, timeout_ms: u32) -> Result<(), NorError> {
        let start = unsafe { (self.io.millis)() };
        loop {
            let s1 = self.read_status1()?;
            if (s1 & SR1_WIP) == 0 {
                return Ok(());
            }
            if unsafe { (self.io.millis)() }.wrapping_sub(start) > timeout_ms {
                return Err(NorError::Timeout);
            }
            unsafe { (self.io.yield_)() };
        }
    }
}

impl<'a> SecureRegion for SpiNorIoOpsDriver<'a> {
    fn read_unique_id(&mut self, buf: &mut [u8]) -> Result<usize, NorError> {
        // 4Bh + 4 dummy bytes + 8 or 16 UID bytes
        self.cs_set(true);
        let cmd = [CMD_READ_UNIQUE_ID, 0, 0, 0, 0];
        let mut dummy = [0u8; 5];
        let read_len = buf.len().min(16);
        let zero = [0u8; 16];
        let res = self
            .spi_xfer(&cmd, &mut dummy)
            .and_then(|_| self.spi_call(zero.as_ptr(), buf.as_mut_ptr(), read_len));
        self.cs_set(false);
        res.map(|_| read_len)
    }

    fn read_security_register(&mut self, reg_num: u8, offset: u16, buf: &mut [u8]) -> Result<(), NorError> {
        let addr = ((reg_num as u32) << 12) | (offset as u32 & 0x0FFF);
        let cmd = [
            CMD_READ_SECURITY_REG,
            (addr >> 16) as u8,
            (addr >> 8) as u8,
            addr as u8,
            0x00, // 1 dummy byte
        ];
        self.cs_set(true);
        let mut dummy = [0u8; 5];
        let res = self
            .spi_xfer(&cmd, &mut dummy)
            .and_then(|_| self.spi_call(core::ptr::null(), buf.as_mut_ptr(), buf.len()));
        self.cs_set(false);
        res
    }

    fn program_security_register(&mut self, reg_num: u8, offset: u16, data: &[u8]) -> Result<(), NorError> {
        // A security register page is at most 256 bytes; reject before touching the bus.
        if data.len() > 256 {
            return Err(NorError::InvalidAddress);
        }
        self.write_enable()?;
        let addr = ((reg_num as u32) << 12) | (offset as u32 & 0x0FFF);
        let cmd = [
            CMD_PROGRAM_SECURITY_REG,
            (addr >> 16) as u8,
            (addr >> 8) as u8,
            addr as u8,
        ];
        let mut rx_dummy = [0u8; 256];
        self.cs_set(true);
        let mut dummy = [0u8; 4];
        let res = self
            .spi_xfer(&cmd, &mut dummy)
            .and_then(|_| self.spi_xfer(data, &mut rx_dummy[..data.len()]));
        self.cs_set(false);
        res?;
        self.wait_ready(100)
    }

    fn lock_security_register(&mut self, reg_num: u8) -> Result<(), NorError> {
        let mut s2 = self.read_status2()?;
        let lock_bit = match reg_num {
            1 => SR2_LB1,
            2 => SR2_LB2,
            3 => SR2_LB3,
            _ => return Err(NorError::InvalidAddress),
        };
        s2 |= lock_bit;
        self.write_enable()?;
        let cmd = [CMD_WRITE_STATUS_2, s2];
        self.write_cmd(&cmd)?;
        self.wait_ready(200)?;
        let verify_s2 = self.read_status2()?;
        if (verify_s2 & lock_bit) != 0 {
            Ok(())
        } else {
            Err(NorError::Protected)
        }
    }

    fn is_security_register_locked(&mut self, reg_num: u8) -> Result<bool, NorError> {
        let s2 = self.read_status2()?;
        let lock_bit = match reg_num {
            1 => SR2_LB1,
            2 => SR2_LB2,
            3 => SR2_LB3,
            _ => return Err(NorError::InvalidAddress),
        };
        Ok((s2 & lock_bit) != 0)
    }
}

/// In-memory simulated NOR flash (4MB) for host testing with hardware physics.
#[cfg(feature = "std")]
pub struct RamNor {
    pub mem: std::vec::Vec<u8>,
    pub security_regs: [std::vec::Vec<u8>; 3],
    pub sr2_lb: [bool; 3],
    pub jedec_id: [u8; 3],
    pub unique_id: [u8; 16],
    pub fail_after_bytes: Option<usize>,
    pub bytes_written: usize,
}

#[cfg(feature = "std")]
impl RamNor {
    pub fn new(size: usize) -> Self {
        Self {
            mem: std::vec![0xFF; size],
            security_regs: [
                std::vec![0xFF; 1024],
                std::vec![0xFF; 1024],
                std::vec![0xFF; 1024],
            ],
            sr2_lb: [false; 3],
            jedec_id: JEDEC_ID_BOYA_BY25Q32ES,
            unique_id: [0x5A; 16],
            fail_after_bytes: None,
            bytes_written: 0,
        }
    }
}

#[cfg(feature = "std")]
impl NorFlash for RamNor {
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), NorError> {
        let start = addr as usize;
        let end = start + buf.len();
        if end > self.mem.len() {
            return Err(NorError::InvalidAddress);
        }
        buf.copy_from_slice(&self.mem[start..end]);
        Ok(())
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), NorError> {
        let mut curr_addr = addr as usize;
        for &byte in data {
            if let Some(limit) = self.fail_after_bytes {
                if self.bytes_written >= limit {
                    return Err(NorError::PowerCutSimulated);
                }
            }
            if curr_addr >= self.mem.len() {
                return Err(NorError::InvalidAddress);
            }
            // Flash physics: can only clear bits 1 -> 0
            self.mem[curr_addr] &= byte;
            curr_addr += 1;
            self.bytes_written += 1;
        }
        Ok(())
    }

    fn erase_sector_4k(&mut self, addr: u32) -> Result<(), NorError> {
        let start = (addr & !0x0FFF) as usize;
        let end = start + 4096;
        if end > self.mem.len() {
            return Err(NorError::InvalidAddress);
        }
        self.mem[start..end].fill(0xFF);
        Ok(())
    }

    fn erase_block_64k(&mut self, addr: u32) -> Result<(), NorError> {
        let start = (addr & !0xFFFF) as usize;
        let end = start + 65536;
        if end > self.mem.len() {
            return Err(NorError::InvalidAddress);
        }
        self.mem[start..end].fill(0xFF);
        Ok(())
    }

    fn read_jedec_id(&mut self) -> Result<[u8; 3], NorError> {
        Ok(self.jedec_id)
    }

    fn read_status1(&mut self) -> Result<u8, NorError> {
        Ok(0)
    }

    fn read_status2(&mut self) -> Result<u8, NorError> {
        let mut s2 = 0;
        if self.sr2_lb[0] { s2 |= SR2_LB1; }
        if self.sr2_lb[1] { s2 |= SR2_LB2; }
        if self.sr2_lb[2] { s2 |= SR2_LB3; }
        Ok(s2)
    }

    fn wait_ready(&mut self, _timeout_ms: u32) -> Result<(), NorError> {
        Ok(())
    }
}

#[cfg(feature = "std")]
impl SecureRegion for RamNor {
    fn read_unique_id(&mut self, buf: &mut [u8]) -> Result<usize, NorError> {
        let n = buf.len().min(self.unique_id.len());
        buf[..n].copy_from_slice(&self.unique_id[..n]);
        Ok(n)
    }

    fn read_security_register(&mut self, reg_num: u8, offset: u16, buf: &mut [u8]) -> Result<(), NorError> {
        if reg_num < 1 || reg_num > 3 {
            return Err(NorError::InvalidAddress);
        }
        let reg = &self.security_regs[(reg_num - 1) as usize];
        let start = offset as usize;
        let end = start + buf.len();
        if end > reg.len() {
            return Err(NorError::InvalidAddress);
        }
        buf.copy_from_slice(&reg[start..end]);
        Ok(())
    }

    fn program_security_register(&mut self, reg_num: u8, offset: u16, data: &[u8]) -> Result<(), NorError> {
        if reg_num < 1 || reg_num > 3 {
            return Err(NorError::InvalidAddress);
        }
        if self.sr2_lb[(reg_num - 1) as usize] {
            return Err(NorError::Locked);
        }
        let reg = &mut self.security_regs[(reg_num - 1) as usize];
        let mut curr = offset as usize;
        for &byte in data {
            if curr >= reg.len() {
                return Err(NorError::InvalidAddress);
            }
            reg[curr] &= byte;
            curr += 1;
        }
        Ok(())
    }

    fn lock_security_register(&mut self, reg_num: u8) -> Result<(), NorError> {
        if reg_num < 1 || reg_num > 3 {
            return Err(NorError::InvalidAddress);
        }
        self.sr2_lb[(reg_num - 1) as usize] = true;
        Ok(())
    }

    fn is_security_register_locked(&mut self, reg_num: u8) -> Result<bool, NorError> {
        if reg_num < 1 || reg_num > 3 {
            return Err(NorError::InvalidAddress);
        }
        Ok(self.sr2_lb[(reg_num - 1) as usize])
    }
}

// ---------------------------------------------------------------------------------------
// `boot_flash::Flash` for the SPI NOR drivers.
//
// `NorFlash` stays as the chip-level interface (JEDEC ID, status registers, 4 KiB / 64 KiB
// erase commands); `Flash` is the view the journal, updater and rescue code use. The
// drivers keep their behaviour; what `Flash` adds is the range checking the raw drivers
// never had (an address past 4 MiB silently wraps on the chip), and rejected calls touch
// nothing, not even the SPI bus.
// ---------------------------------------------------------------------------------------

impl From<NorError> for FlashError {
    /// `Locked` (a security register) and `Corrupted` (a seed record) have no `Flash`
    /// counterpart: `Flash` never touches security registers. They map to the nearest
    /// meaning so a stray one is not mistaken for success.
    fn from(e: NorError) -> Self {
        match e {
            NorError::IoError => FlashError::IoError,
            NorError::NoTransport => FlashError::NoTransport,
            NorError::Timeout => FlashError::Timeout,
            NorError::Protected => FlashError::Protected,
            NorError::InvalidAddress => FlashError::OutOfRange,
            NorError::Unaligned => FlashError::Unaligned,
            NorError::Locked => FlashError::Protected,
            NorError::Corrupted => FlashError::IoError,
            NorError::PowerCutSimulated => FlashError::PowerCut,
        }
    }
}

/// The 4 KiB sector containing `off` on a chip of `cap` bytes; empty past the end.
fn nor_unit_at(off: u32, cap: u32) -> Range<u32> {
    if off >= cap {
        return cap..cap;
    }
    let start = off & !(NOR_SECTOR_SIZE - 1);
    start..(start + NOR_SECTOR_SIZE).min(cap)
}

/// `Err(OutOfRange)` unless `off..off + len` fits in `cap` bytes.
fn nor_check_range(off: u32, len: usize, cap: u32) -> Result<(), FlashError> {
    if u64::from(off) + len as u64 > u64::from(cap) {
        Err(FlashError::OutOfRange)
    } else {
        Ok(())
    }
}

/// `Flash::erase_span` for a chip with 4 KiB sectors and 64 KiB blocks: a whole block when
/// `off` starts one and `limit` leaves room for all of it, otherwise one sector.
fn nor_erase_span<N: NorFlash>(
    nor: &mut N,
    off: u32,
    limit: u32,
    cap: u32,
) -> Result<Range<u32>, FlashError> {
    let unit = nor_unit_at(off, cap);
    if unit.is_empty() {
        return Err(FlashError::OutOfRange);
    }
    if unit.start != off || unit.end > limit {
        return Err(FlashError::Unaligned);
    }
    // `limit >= unit.end > off`, so the subtraction cannot underflow.
    if off % NOR_BLOCK_SIZE == 0 && limit - off >= NOR_BLOCK_SIZE && off + NOR_BLOCK_SIZE <= cap {
        nor.erase_block_64k(off)?;
        return Ok(off..off + NOR_BLOCK_SIZE);
    }
    nor.erase_sector_4k(off)?;
    Ok(unit)
}

impl<'a> boot_flash::Flash for SpiNorIoOpsDriver<'a> {
    const PROGRAM_ALIGN: u32 = 1;

    fn capacity(&self) -> u32 {
        NOR_CAPACITY
    }

    fn read(&mut self, off: u32, buf: &mut [u8]) -> Result<(), FlashError> {
        nor_check_range(off, buf.len(), NOR_CAPACITY)?;
        if buf.is_empty() {
            return Ok(());
        }
        Ok(NorFlash::read(self, off, buf)?)
    }

    fn program(&mut self, off: u32, data: &[u8]) -> Result<(), FlashError> {
        nor_check_range(off, data.len(), NOR_CAPACITY)?;
        Ok(NorFlash::write(self, off, data)?)
    }

    fn unit_at(&self, off: u32) -> Range<u32> {
        nor_unit_at(off, NOR_CAPACITY)
    }

    fn erase_unit(&mut self, off: u32) -> Result<(), FlashError> {
        let unit = nor_unit_at(off, NOR_CAPACITY);
        if unit.is_empty() {
            return Err(FlashError::OutOfRange);
        }
        Ok(NorFlash::erase_sector_4k(self, unit.start)?)
    }

    fn erase_span(&mut self, off: u32, limit: u32) -> Result<Range<u32>, FlashError> {
        nor_erase_span(self, off, limit, NOR_CAPACITY)
    }
}

#[cfg(feature = "std")]
impl boot_flash::Flash for RamNor {
    const PROGRAM_ALIGN: u32 = 1;

    fn capacity(&self) -> u32 {
        self.mem.len() as u32
    }

    fn read(&mut self, off: u32, buf: &mut [u8]) -> Result<(), FlashError> {
        nor_check_range(off, buf.len(), self.mem.len() as u32)?;
        Ok(NorFlash::read(self, off, buf)?)
    }

    fn program(&mut self, off: u32, data: &[u8]) -> Result<(), FlashError> {
        // `RamNor::write` checks the bound one byte at a time, so a write that runs off the
        // end would already have changed the bytes before it. Reject it up front.
        nor_check_range(off, data.len(), self.mem.len() as u32)?;
        Ok(NorFlash::write(self, off, data)?)
    }

    fn unit_at(&self, off: u32) -> Range<u32> {
        nor_unit_at(off, self.mem.len() as u32)
    }

    fn erase_unit(&mut self, off: u32) -> Result<(), FlashError> {
        let unit = nor_unit_at(off, self.mem.len() as u32);
        if unit.is_empty() {
            return Err(FlashError::OutOfRange);
        }
        Ok(NorFlash::erase_sector_4k(self, unit.start)?)
    }

    fn erase_span(&mut self, off: u32, limit: u32) -> Result<Range<u32>, FlashError> {
        let cap = self.mem.len() as u32;
        nor_erase_span(self, off, limit, cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seed_record_validation() {
        let seed = [0x77u8; 32];
        let rec = SeedRecord::new(seed);
        assert!(rec.is_valid());

        let mut bad_rec = rec;
        bad_rec.seed[0] ^= 0x01;
        assert!(!bad_rec.is_valid());
    }

    // -----------------------------------------------------------------------
    // Real driver (SpiNorIoOpsDriver) busy-wait / timeout behaviour.
    // The RamNor mock never reports "busy", so these use a fake IoOps instead.
    // -----------------------------------------------------------------------

    extern crate std;
    use core::cell::RefCell;
    use std::thread_local;

    #[derive(Default)]
    struct Fake {
        millis: u32,
        millis_step: u32,
        busy_polls: u32,
        yields: u32,
    }

    thread_local! {
        static FAKE: RefCell<Fake> = RefCell::new(Fake::default());
    }

    fn setup(millis: u32, millis_step: u32, busy_polls: u32) {
        FAKE.with(|f| {
            *f.borrow_mut() = Fake { millis, millis_step, busy_polls, yields: 0 };
        });
    }

    unsafe extern "C" fn fake_spi(tx: *const u8, rx: *mut u8, len: u32) -> i32 {
        // The driver passes tx = null when it only wants to clock data in.
        let first_tx_byte = if tx.is_null() { 0 } else { *tx };
        let rx = core::slice::from_raw_parts_mut(rx, len as usize);
        rx.fill(0);
        if first_tx_byte == CMD_READ_STATUS_1 && len >= 2 {
            FAKE.with(|f| {
                let mut f = f.borrow_mut();
                if f.busy_polls > 0 {
                    f.busy_polls -= 1;
                    rx[1] = SR1_WIP;
                }
            });
        }
        0
    }

    unsafe extern "C" fn fake_cs(_assert: bool) {}

    unsafe extern "C" fn fake_millis() -> u32 {
        FAKE.with(|f| {
            let mut f = f.borrow_mut();
            let now = f.millis;
            f.millis = f.millis.wrapping_add(f.millis_step);
            now
        })
    }

    unsafe extern "C" fn fake_yield() {
        FAKE.with(|f| f.borrow_mut().yields += 1);
    }

    static FAKE_IO: IoOps = IoOps {
        spi_transfer: Some(fake_spi),
        cs: Some(fake_cs),
        millis: fake_millis,
        yield_: fake_yield,
        reserved: [0; 4],
    };

    #[test]
    fn wait_ready_returns_ok_once_device_is_idle() {
        setup(1_000, 1, 5);
        let mut nor = SpiNorIoOpsDriver::new(&FAKE_IO);
        assert_eq!(nor.wait_ready(100), Ok(()));
        assert!(FAKE.with(|f| f.borrow().yields) >= 5, "yields while busy");
    }

    #[test]
    fn wait_ready_times_out_when_device_stays_busy() {
        setup(1_000, 1, u32::MAX);
        let mut nor = SpiNorIoOpsDriver::new(&FAKE_IO);
        assert_eq!(nor.wait_ready(10), Err(NorError::Timeout));
        // It must give up after roughly the timeout, not spin forever.
        let elapsed = FAKE.with(|f| f.borrow().millis) - 1_000;
        assert!((11..=14).contains(&elapsed), "elapsed {elapsed} ms");
    }

    #[test]
    fn wait_ready_times_out_across_millis_wraparound() {
        // Regression: `millis() - start` panicked (overflow-checks) when the
        // millisecond counter wrapped between start and now.
        setup(u32::MAX - 3, 1, u32::MAX);
        let mut nor = SpiNorIoOpsDriver::new(&FAKE_IO);
        assert_eq!(nor.wait_ready(20), Err(NorError::Timeout));
    }

    #[test]
    fn wait_ready_succeeds_across_millis_wraparound() {
        setup(u32::MAX - 3, 1, 8);
        let mut nor = SpiNorIoOpsDriver::new(&FAKE_IO);
        assert_eq!(nor.wait_ready(100), Ok(()));
    }

    #[test]
    fn software_reset_returns_once_the_clock_has_advanced() {
        // Regression: with a clock stuck at 0 the wait loop could never end.
        setup(500, 1, 0);
        let nor = SpiNorIoOpsDriver::new(&FAKE_IO);
        assert_eq!(nor.software_reset(), Ok(()));
        assert!(FAKE.with(|f| f.borrow().millis) >= 501, "clock was consulted");
    }

    #[test]
    fn software_reset_survives_millis_wraparound() {
        setup(u32::MAX, 1, 0);
        let nor = SpiNorIoOpsDriver::new(&FAKE_IO);
        assert_eq!(nor.software_reset(), Ok(()));
    }

    /// SeedRecord::new([0x77; 32]); CRC computed independently with Python's zlib.
    const GOLDEN_SEED_RECORD: [u8; 64] = [
        0x44, 0x45, 0x45, 0x53, 0x01, 0xFF, 0xFF, 0xFF, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77,
        0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77,
        0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0x4C, 0xD5, 0x19, 0xC9,
    ];

    #[test]
    fn golden_seed_record_bytes_are_stable() {
        let rec = SeedRecord::new([0x77u8; 32]);
        assert_eq!(rec.as_bytes(), &GOLDEN_SEED_RECORD[..]);
        assert!(rec.is_valid());
    }

    #[test]
    fn any_single_bit_flip_invalidates_a_seed_record() {
        for byte in 0..GOLDEN_SEED_RECORD.len() {
            for bit in 0..8 {
                let mut raw = GOLDEN_SEED_RECORD;
                raw[byte] ^= 1 << bit;
                let rec = SeedRecord::read_from_bytes(&raw).unwrap();
                assert!(!rec.is_valid(), "byte {byte} bit {bit} went unnoticed");
            }
        }
    }

    // --- Optional transport and chip-select hygiene ---------------------------------------------

    thread_local! {
        static CS_LOG: RefCell<(u32, u32)> = const { RefCell::new((0, 0)) }; // (asserts, releases)
        /// Number of SPI transfers that still succeed; the next one fails (`None` = never fail).
        static SPI_BUDGET: RefCell<Option<u32>> = const { RefCell::new(None) };
    }

    unsafe extern "C" fn counting_cs(assert: bool) {
        CS_LOG.with(|c| {
            let mut c = c.borrow_mut();
            if assert {
                c.0 += 1
            } else {
                c.1 += 1
            }
        });
    }

    unsafe extern "C" fn maybe_failing_spi(tx: *const u8, rx: *mut u8, len: u32) -> i32 {
        let allowed = SPI_BUDGET.with(|b| {
            let mut b = b.borrow_mut();
            match *b {
                None => true,
                Some(0) => false,
                Some(n) => {
                    *b = Some(n - 1);
                    true
                }
            }
        });
        if !allowed {
            return -1;
        }
        fake_spi(tx, rx, len)
    }

    static COUNTING_IO: IoOps = IoOps {
        spi_transfer: Some(maybe_failing_spi),
        cs: Some(counting_cs),
        millis: fake_millis,
        yield_: fake_yield,
        reserved: [0; 4],
    };

    static NO_TRANSPORT_IO: IoOps = IoOps {
        spi_transfer: None,
        cs: None,
        millis: fake_millis,
        yield_: fake_yield,
        reserved: [0; 4],
    };

    fn reset_counters(spi_budget: Option<u32>) {
        setup(1_000, 1, 0);
        CS_LOG.with(|c| *c.borrow_mut() = (0, 0));
        SPI_BUDGET.with(|b| *b.borrow_mut() = spi_budget);
    }

    #[test]
    fn without_a_transport_every_operation_fails_cleanly() {
        setup(1_000, 1, 0);
        let mut nor = SpiNorIoOpsDriver::new(&NO_TRANSPORT_IO);
        let mut buf = [0u8; 4];
        assert_eq!(nor.read_jedec_id(), Err(NorError::NoTransport));
        assert_eq!(nor.read(0, &mut buf), Err(NorError::NoTransport));
        assert_eq!(nor.write(0, &[1, 2, 3]), Err(NorError::NoTransport));
        assert_eq!(nor.erase_sector_4k(0), Err(NorError::NoTransport));
        assert_eq!(nor.read_security_register(1, 0, &mut buf), Err(NorError::NoTransport));
        assert_eq!(nor.read_unique_id(&mut buf), Err(NorError::NoTransport));
    }

    #[test]
    fn chip_select_is_released_at_every_failure_point() {
        type Op = fn(&mut SpiNorIoOpsDriver, &mut [u8]) -> Result<(), NorError>;
        let ops: [(&str, Op); 7] = [
            ("read", |n, b| n.read(0, b)),
            ("write", |n, _| n.write(0, &[1, 2, 3, 4])),
            ("erase_sector_4k", |n, _| n.erase_sector_4k(0)),
            ("read_jedec_id", |n, _| n.read_jedec_id().map(|_| ())),
            ("read_security_register", |n, b| n.read_security_register(1, 0, b)),
            ("program_security_register", |n, _| n.program_security_register(1, 0, &[1, 2])),
            ("read_unique_id", |n, b| n.read_unique_id(b).map(|_| ())),
        ];
        let mut buf = [0u8; 8];
        for (name, op) in ops {
            // Let exactly `budget` transfers succeed, then fail: this walks the failure through
            // every early-return point of the operation.
            for budget in 0..=6u32 {
                reset_counters(Some(budget));
                let mut nor = SpiNorIoOpsDriver::new(&COUNTING_IO);
                let _ = op(&mut nor, &mut buf);
                let (asserts, releases) = CS_LOG.with(|c| *c.borrow());
                assert_eq!(
                    asserts, releases,
                    "{name}: chip select left asserted when transfer #{budget} failed"
                );
            }
        }
    }

    #[test]
    fn program_security_register_rejects_oversized_data_without_touching_the_bus() {
        reset_counters(None);
        let mut nor = SpiNorIoOpsDriver::new(&COUNTING_IO);
        let big = [0u8; 257];
        assert_eq!(nor.program_security_register(1, 0, &big), Err(NorError::InvalidAddress));
        assert_eq!(CS_LOG.with(|c| c.borrow().0), 0);
    }
}

/// Tests for the `boot_flash::Flash` impls. Kept out of `mod tests` on purpose: this module
/// imports `Flash` but not `NorFlash` (see the note at the top of the file), so `nor.read()`
/// means `Flash::read` here.
#[cfg(test)]
mod flash_impl_tests {
    extern crate std;

    use super::{
        NorError, RamNor, SpiNorIoOpsDriver, CMD_BLOCK_ERASE_64K, CMD_FAST_READ, CMD_PAGE_PROGRAM,
        CMD_SECTOR_ERASE_4K, NOR_CAPACITY,
    };
    use boot_abi::IoOps;
    use boot_flash::{Flash, FlashError, RamFlash, UNIFORM_4K_4MB};
    use core::cell::RefCell;
    use core::ops::Range;
    use std::thread_local;
    use std::vec;
    use std::vec::Vec;

    // --- A SPI transport that records every chip-select frame --------------------------

    /// One chip-select frame: the first byte sent, the 3-byte address that follows it (if
    /// any), and the total bytes clocked.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Frame {
        cmd: u8,
        addr: u32,
        bytes: usize,
    }

    #[derive(Default)]
    struct Bus {
        frames: Vec<Frame>,
        first: bool,
        asserts: u32,
    }

    thread_local! {
        static BUS: RefCell<Bus> = RefCell::new(Bus::default());
    }

    unsafe extern "C" fn rec_cs(assert: bool) {
        BUS.with(|b| {
            let mut b = b.borrow_mut();
            if assert {
                b.asserts += 1;
                b.first = true;
                b.frames.push(Frame {
                    cmd: 0,
                    addr: 0,
                    bytes: 0,
                });
            }
        });
    }

    unsafe extern "C" fn rec_spi(tx: *const u8, rx: *mut u8, len: u32) -> i32 {
        // all reads return zeros, so the status register never says "busy"
        core::slice::from_raw_parts_mut(rx, len as usize).fill(0);
        BUS.with(|b| {
            let mut b = b.borrow_mut();
            let first = b.first;
            let f = b
                .frames
                .last_mut()
                .expect("transfer outside a chip-select frame");
            f.bytes += len as usize;
            if first && !tx.is_null() {
                let t = core::slice::from_raw_parts(tx, len as usize);
                f.cmd = t[0];
                if t.len() >= 4 {
                    f.addr = u32::from(t[1]) << 16 | u32::from(t[2]) << 8 | u32::from(t[3]);
                }
            }
            b.first = false;
        });
        0
    }

    unsafe extern "C" fn rec_millis() -> u32 {
        0
    }

    unsafe extern "C" fn rec_yield() {}

    static REC_IO: IoOps = IoOps {
        spi_transfer: Some(rec_spi),
        cs: Some(rec_cs),
        millis: rec_millis,
        yield_: rec_yield,
        reserved: [0; 4],
    };

    static NO_TRANSPORT_IO: IoOps = IoOps {
        spi_transfer: None,
        cs: None,
        millis: rec_millis,
        yield_: rec_yield,
        reserved: [0; 4],
    };

    fn fresh_bus() {
        BUS.with(|b| *b.borrow_mut() = Bus::default());
    }

    /// The data-moving frames, without the status polls and write-enables around them.
    fn ops() -> Vec<Frame> {
        const READ: u8 = CMD_FAST_READ;
        BUS.with(|b| {
            b.borrow()
                .frames
                .iter()
                .copied()
                .filter(|f| {
                    matches!(
                        f.cmd,
                        CMD_PAGE_PROGRAM | CMD_SECTOR_ERASE_4K | CMD_BLOCK_ERASE_64K | READ
                    )
                })
                .collect()
        })
    }

    fn bus_untouched() -> bool {
        BUS.with(|b| b.borrow().asserts == 0)
    }

    // --- SpiNorIoOpsDriver ------------------------------------------------------------

    #[test]
    fn spi_capacity_and_units() {
        let nor = SpiNorIoOpsDriver::new(&REC_IO);
        assert_eq!(nor.capacity(), 4 * 1024 * 1024);
        assert_eq!(nor.capacity(), NOR_CAPACITY);
        assert_eq!(<SpiNorIoOpsDriver as Flash>::PROGRAM_ALIGN, 1);
        assert_eq!(nor.unit_at(0), 0..0x1000);
        assert_eq!(nor.unit_at(0xFFF), 0..0x1000);
        assert_eq!(nor.unit_at(0x1000), 0x1000..0x2000);
        assert_eq!(nor.unit_at(0x3F_FFFF), 0x3F_F000..0x40_0000);
        assert!(nor.unit_at(0x40_0000).is_empty());
        assert!(nor.unit_at(u32::MAX).is_empty());
    }

    #[test]
    fn spi_program_is_split_at_256_byte_pages() {
        fresh_bus();
        let mut nor = SpiNorIoOpsDriver::new(&REC_IO);
        // 300 bytes from 0x1F0: 16 bytes to the end of that page, one whole page, then 28.
        nor.program(0x1F0, &[0xAB; 300]).unwrap();
        assert_eq!(
            ops(),
            vec![
                Frame {
                    cmd: CMD_PAGE_PROGRAM,
                    addr: 0x1F0,
                    bytes: 4 + 16
                },
                Frame {
                    cmd: CMD_PAGE_PROGRAM,
                    addr: 0x200,
                    bytes: 4 + 256
                },
                Frame {
                    cmd: CMD_PAGE_PROGRAM,
                    addr: 0x300,
                    bytes: 4 + 28
                },
            ]
        );
    }

    #[test]
    fn spi_read_sends_fast_read_with_the_address() {
        fresh_bus();
        let mut nor = SpiNorIoOpsDriver::new(&REC_IO);
        let mut buf = [0u8; 32];
        nor.read(0x12_3456, &mut buf).unwrap();
        assert_eq!(
            ops(),
            vec![Frame {
                cmd: CMD_FAST_READ,
                addr: 0x12_3456,
                bytes: 5 + 32
            }]
        );
    }

    #[test]
    fn spi_erase_unit_erases_the_sector_that_contains_the_offset() {
        fresh_bus();
        let mut nor = SpiNorIoOpsDriver::new(&REC_IO);
        nor.erase_unit(0x3456).unwrap();
        nor.erase_unit(0x3000).unwrap();
        nor.erase_unit(0x3FFF).unwrap();
        let sector = Frame {
            cmd: CMD_SECTOR_ERASE_4K,
            addr: 0x3000,
            bytes: 4,
        };
        assert_eq!(ops(), vec![sector, sector, sector]);
    }

    #[test]
    fn spi_rejected_calls_do_not_touch_the_bus() {
        fresh_bus();
        let mut nor = SpiNorIoOpsDriver::new(&REC_IO);
        let cap = nor.capacity();
        let mut b8 = [0u8; 8];
        assert_eq!(nor.read(cap - 4, &mut b8), Err(FlashError::OutOfRange));
        assert_eq!(nor.read(cap, &mut b8), Err(FlashError::OutOfRange));
        assert_eq!(nor.read(u32::MAX, &mut b8), Err(FlashError::OutOfRange));
        assert_eq!(nor.program(cap - 4, &[0; 8]), Err(FlashError::OutOfRange));
        assert_eq!(nor.program(cap, &[0]), Err(FlashError::OutOfRange));
        assert_eq!(nor.erase_unit(cap), Err(FlashError::OutOfRange));
        assert_eq!(nor.erase_span(cap, cap), Err(FlashError::OutOfRange));
        assert_eq!(nor.erase_span(0x1001, 0x4_0000), Err(FlashError::Unaligned));
        assert_eq!(nor.erase_span(0x1000, 0x1FFF), Err(FlashError::Unaligned));
        assert!(bus_untouched(), "a rejected call must not select the chip");
    }

    #[test]
    fn spi_empty_read_and_program_are_free() {
        fresh_bus();
        let mut nor = SpiNorIoOpsDriver::new(&REC_IO);
        nor.read(0x100, &mut []).unwrap();
        nor.program(0x100, &[]).unwrap();
        nor.read(nor.capacity(), &mut []).unwrap(); // empty, ends exactly at the end
        assert!(bus_untouched());
        assert_eq!(
            nor.read(nor.capacity() + 1, &mut []),
            Err(FlashError::OutOfRange)
        );
    }

    #[test]
    fn spi_the_last_bytes_of_the_chip_are_reachable() {
        fresh_bus();
        let mut nor = SpiNorIoOpsDriver::new(&REC_IO);
        let cap = nor.capacity();
        nor.program(cap - 16, &[0; 16]).unwrap();
        assert_eq!(
            ops(),
            vec![Frame {
                cmd: CMD_PAGE_PROGRAM,
                addr: cap - 16,
                bytes: 4 + 16
            }]
        );
    }

    #[test]
    fn spi_span_uses_a_block_only_when_aligned_and_there_is_room() {
        let sector = |addr| Frame {
            cmd: CMD_SECTOR_ERASE_4K,
            addr,
            bytes: 4,
        };
        let block = |addr| Frame {
            cmd: CMD_BLOCK_ERASE_64K,
            addr,
            bytes: 4,
        };
        // (off, limit, expected frame, expected range)
        let cases: [(u32, u32, Frame, Range<u32>); 5] = [
            (0x2_0000, 0x4_0000, block(0x2_0000), 0x2_0000..0x3_0000),
            (0x2_0000, 0x3_0000, block(0x2_0000), 0x2_0000..0x3_0000), // limit == block end
            (0x2_0000, 0x2_F000, sector(0x2_0000), 0x2_0000..0x2_1000), // 4 KiB short
            (0x2_1000, 0x4_0000, sector(0x2_1000), 0x2_1000..0x2_2000), // sector, not block, aligned
            (0x3F_0000, 0x40_0000, block(0x3F_0000), 0x3F_0000..0x40_0000), // last block
        ];
        for (off, limit, frame, range) in cases {
            fresh_bus();
            let mut nor = SpiNorIoOpsDriver::new(&REC_IO);
            assert_eq!(
                nor.erase_span(off, limit),
                Ok(range),
                "span {off:#x}..{limit:#x}"
            );
            assert_eq!(ops(), vec![frame], "span {off:#x}..{limit:#x}");
        }
    }

    /// Clears `start..end` by looping on `erase_span` and returns the frames sent.
    fn clear_spi(start: u32, end: u32) -> Vec<Frame> {
        fresh_bus();
        let mut nor = SpiNorIoOpsDriver::new(&REC_IO);
        let mut at = start;
        while at < end {
            at = nor.erase_span(at, end).unwrap().end;
        }
        assert_eq!(at, end);
        ops()
    }

    #[test]
    fn spi_clearing_a_640k_slot_takes_ten_block_erases_like_the_rescue_e_command() {
        let frames = clear_spi(0x10_0000, 0x10_0000 + 640 * 1024);
        let expected: Vec<Frame> = (0..10)
            .map(|i| Frame {
                cmd: CMD_BLOCK_ERASE_64K,
                addr: 0x10_0000 + i * 0x1_0000,
                bytes: 4,
            })
            .collect();
        assert_eq!(frames, expected);
    }

    #[test]
    fn spi_clearing_a_range_that_starts_off_a_block_edge_uses_sectors_then_blocks() {
        // 0x1_1000..0x3_0000: fifteen sectors up to the 0x2_0000 block edge, then one block.
        let frames = clear_spi(0x1_1000, 0x3_0000);
        let mut expected: Vec<Frame> = (0..15)
            .map(|i| Frame {
                cmd: CMD_SECTOR_ERASE_4K,
                addr: 0x1_1000 + i * 0x1000,
                bytes: 4,
            })
            .collect();
        expected.push(Frame {
            cmd: CMD_BLOCK_ERASE_64K,
            addr: 0x2_0000,
            bytes: 4,
        });
        assert_eq!(frames.len(), 16);
        assert_eq!(frames, expected);
        // and a range ending mid-block goes back to sectors for the tail
        let tail = clear_spi(0x2_0000, 0x3_3000);
        assert_eq!(
            tail,
            vec![
                Frame {
                    cmd: CMD_BLOCK_ERASE_64K,
                    addr: 0x2_0000,
                    bytes: 4
                },
                Frame {
                    cmd: CMD_SECTOR_ERASE_4K,
                    addr: 0x3_0000,
                    bytes: 4
                },
                Frame {
                    cmd: CMD_SECTOR_ERASE_4K,
                    addr: 0x3_1000,
                    bytes: 4
                },
                Frame {
                    cmd: CMD_SECTOR_ERASE_4K,
                    addr: 0x3_2000,
                    bytes: 4
                },
            ]
        );
    }

    #[test]
    fn spi_without_a_transport_the_errors_are_flash_errors() {
        let mut nor = SpiNorIoOpsDriver::new(&NO_TRANSPORT_IO);
        let mut b = [0u8; 4];
        assert_eq!(nor.read(0, &mut b), Err(FlashError::NoTransport));
        assert_eq!(nor.program(0, &[1]), Err(FlashError::NoTransport));
        assert_eq!(nor.erase_unit(0), Err(FlashError::NoTransport));
        assert_eq!(nor.erase_span(0, 0x1_0000), Err(FlashError::NoTransport));
    }

    #[test]
    fn nor_errors_map_to_flash_errors() {
        // Written out by hand: a new NorError variant also breaks the exhaustive `match`
        // in `From`, so this table cannot silently fall behind.
        let table = [
            (NorError::IoError, FlashError::IoError),
            (NorError::NoTransport, FlashError::NoTransport),
            (NorError::Timeout, FlashError::Timeout),
            (NorError::Protected, FlashError::Protected),
            (NorError::InvalidAddress, FlashError::OutOfRange),
            (NorError::Unaligned, FlashError::Unaligned),
            (NorError::Locked, FlashError::Protected),
            (NorError::Corrupted, FlashError::IoError),
            (NorError::PowerCutSimulated, FlashError::PowerCut),
        ];
        for (n, f) in table {
            assert_eq!(FlashError::from(n), f, "{n:?}");
        }
    }

    // --- RamNor against RamFlash: two independent simulators as each other's oracle ---------

    /// Small deterministic PRNG (xorshift64*), so the sequences are reproducible.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u32) -> u32 {
            (self.next() >> 33) as u32 % n
        }
    }

    fn clear_range<F: Flash>(f: &mut F, start: u32, end: u32) -> Vec<Range<u32>> {
        let mut spans = Vec::new();
        let mut at = start;
        while at < end {
            let r = f.erase_span(at, end).unwrap();
            at = r.end;
            spans.push(r);
        }
        spans
    }

    #[test]
    fn ramnor_and_ramflash_agree_on_every_operation() {
        let mut nor = RamNor::new(4 * 1024 * 1024);
        let mut reference = RamFlash::<1>::new(UNIFORM_4K_4MB);
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        // Work inside 512 KiB so programs and erases keep colliding, plus a few probes at the
        // far end of the device.
        for step in 0..3000u32 {
            let near_end = rng.below(20) == 0;
            let base = if near_end { 4 * 1024 * 1024 - 8192 } else { 0 };
            let span = if near_end { 8192 } else { 512 * 1024 };
            let off = base + rng.below(span);
            match rng.below(4) {
                0 | 1 => {
                    let len = rng.below(700) as usize;
                    let data: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
                    assert_eq!(
                        nor.program(off, &data),
                        reference.program(off, &data),
                        "program #{step} at {off:#x} len {len}"
                    );
                }
                2 => assert_eq!(
                    nor.erase_unit(off),
                    reference.erase_unit(off),
                    "erase #{step} at {off:#x}"
                ),
                _ => {
                    let len = rng.below(300) as usize;
                    let (mut a, mut b) = (vec![0u8; len], vec![0u8; len]);
                    assert_eq!(nor.read(off, &mut a), reference.read(off, &mut b));
                    assert_eq!(a, b, "read #{step}");
                }
            }
        }
        assert!(nor.mem == reference.bytes(), "final contents differ");
    }

    #[test]
    fn ramnor_and_ramflash_agree_on_what_clearing_a_range_does() {
        for (start, end) in [
            (0x1_0000, 0x1_0000 + 640 * 1024),
            (0x1_1000, 0x3_0000),
            (0x2_0000, 0x3_3000),
            (0, 4 * 1024 * 1024),
        ] {
            let mut nor = RamNor::new(4 * 1024 * 1024);
            let mut reference = RamFlash::<1>::new(UNIFORM_4K_4MB);
            nor.mem.fill(0x5A);
            reference.bytes_mut().fill(0x5A);
            let nor_spans = clear_range(&mut nor, start, end);
            let ref_spans = clear_range(&mut reference, start, end);
            assert!(nor.mem == reference.bytes(), "{start:#x}..{end:#x}");
            for (i, &b) in nor.mem.iter().enumerate() {
                let inside = (start as usize..end as usize).contains(&i);
                assert_eq!(b, if inside { 0xFF } else { 0x5A }, "byte {i:#x}");
            }
            // The reference (default span) always erases one 4 KiB unit; RamNor coalesces
            // into blocks where it can, so it needs fewer calls and never more.
            assert!(ref_spans.iter().all(|r| r.end - r.start == 4096));
            assert!(nor_spans.len() <= ref_spans.len());
            assert!(nor_spans.iter().any(|r| r.end - r.start == 65536));
        }
    }

    #[test]
    fn ramnor_power_cut_matches_ramflash_clean_cut() {
        for cut in 0..48u64 {
            let mut nor = RamNor::new(4 * 1024 * 1024);
            nor.fail_after_bytes = Some(cut as usize);
            let mut reference = RamFlash::<1>::new(UNIFORM_4K_4MB);
            reference.arm_power_cut(cut, boot_flash::CutMode::Clean);
            let data: Vec<u8> = (0..40u8).map(|i| i.wrapping_mul(7)).collect();
            let a = nor.program(0x100, &data);
            let b = reference.program(0x100, &data);
            assert_eq!(a, b, "cut after {cut} bytes");
            assert!(nor.mem == reference.bytes(), "cut after {cut} bytes");
            assert_eq!(a.is_err(), cut < 40);
        }
    }

    #[test]
    fn ramnor_program_past_the_end_changes_nothing() {
        let mut nor = RamNor::new(8192);
        // `RamNor::write` alone would program the first 2 bytes before failing on the 3rd.
        assert_eq!(
            nor.program(8190, &[0, 0, 0, 0]),
            Err(FlashError::OutOfRange)
        );
        assert!(nor.mem.iter().all(|&b| b == 0xFF));
        let mut b = [0u8; 4];
        assert_eq!(nor.read(8190, &mut b), Err(FlashError::OutOfRange));
        assert_eq!(nor.erase_unit(8192), Err(FlashError::OutOfRange));
        assert_eq!(nor.erase_span(8192, 8192), Err(FlashError::OutOfRange));
        assert_eq!(nor.capacity(), 8192);
    }

    #[test]
    fn ramnor_span_never_runs_a_block_past_the_end() {
        // 72 KiB: the block at 0x1_0000 would end at 0x2_0000, beyond the device, even though
        // the caller's limit leaves room for it. It must fall back to a sector.
        let mut nor = RamNor::new(0x1_2000);
        nor.mem.fill(0);
        assert_eq!(nor.erase_span(0x1_0000, 0x2_0000), Ok(0x1_0000..0x1_1000));
        assert!(nor.mem[0x1_0000..0x1_1000].iter().all(|&b| b == 0xFF));
        assert!(nor.mem[0x1_1000..].iter().all(|&b| b == 0));
        // the 64 KiB block that does fit is still used
        assert_eq!(nor.erase_span(0, 0x1_0000), Ok(0..0x1_0000));
    }
}
