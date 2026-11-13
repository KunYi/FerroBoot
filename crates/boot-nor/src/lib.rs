//! SPI NOR Flash Driver and Host-Simulated Mock
//!
//! Supports Boya BY25Q32ES and Winbond W25Q32JV (3-byte address, 4MB).
//! Handles Page Program (256B), Sector Erase (4KB), Block Erase (64KB),
//! JEDEC ID, Status Registers, and Security Register OTP operations.

#![no_std]

#[cfg(feature = "std")]
extern crate std;

use boot_abi::{IoOps, NOR_PAGE_SIZE};
use boot_crc::Hasher;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

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

    fn spi_xfer(&self, tx: &[u8], rx: &mut [u8]) -> Result<(), NorError> {
        let ret = unsafe { (self.io.spi_transfer)(tx.as_ptr(), rx.as_mut_ptr(), tx.len() as u32) };
        if ret == 0 {
            Ok(())
        } else {
            Err(NorError::IoError)
        }
    }

    fn write_cmd(&self, cmd: &[u8]) -> Result<(), NorError> {
        unsafe { (self.io.cs)(true) };
        let mut rx = [0u8; 8];
        let res = self.spi_xfer(cmd, &mut rx[..cmd.len()]);
        unsafe { (self.io.cs)(false) };
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
        while unsafe { (self.io.millis)() } - start < 1 {
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
        unsafe { (self.io.cs)(true) };
        let mut dummy = [0u8; 5];
        let res1 = self.spi_xfer(&cmd, &mut dummy);
        let res2 = if res1.is_ok() {
            // Read incoming data by clocking out zeros
            let ret = unsafe { (self.io.spi_transfer)(core::ptr::null(), buf.as_mut_ptr(), buf.len() as u32) };
            if ret == 0 { Ok(()) } else { Err(NorError::IoError) }
        } else {
            res1
        };
        unsafe { (self.io.cs)(false) };
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
            unsafe { (self.io.cs)(true) };
            let mut dummy = [0u8; 4];
            self.spi_xfer(&cmd, &mut dummy)?;
            let mut rx_dummy = [0u8; 256];
            self.spi_xfer(&data[..chunk_len], &mut rx_dummy[..chunk_len])?;
            unsafe { (self.io.cs)(false) };

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
        unsafe { (self.io.cs)(true) };
        let cmd = [CMD_READ_JEDEC_ID, 0, 0, 0];
        let mut rx = [0u8; 4];
        let res = self.spi_xfer(&cmd, &mut rx);
        unsafe { (self.io.cs)(false) };
        res.map(|_| [rx[1], rx[2], rx[3]])
    }

    fn read_status1(&mut self) -> Result<u8, NorError> {
        unsafe { (self.io.cs)(true) };
        let cmd = [CMD_READ_STATUS_1, 0];
        let mut rx = [0u8; 2];
        let res = self.spi_xfer(&cmd, &mut rx);
        unsafe { (self.io.cs)(false) };
        res.map(|_| rx[1])
    }

    fn read_status2(&mut self) -> Result<u8, NorError> {
        unsafe { (self.io.cs)(true) };
        let cmd = [CMD_READ_STATUS_2, 0];
        let mut rx = [0u8; 2];
        let res = self.spi_xfer(&cmd, &mut rx);
        unsafe { (self.io.cs)(false) };
        res.map(|_| rx[1])
    }

    fn wait_ready(&mut self, timeout_ms: u32) -> Result<(), NorError> {
        let start = unsafe { (self.io.millis)() };
        loop {
            let s1 = self.read_status1()?;
            if (s1 & SR1_WIP) == 0 {
                return Ok(());
            }
            if unsafe { (self.io.millis)() } - start > timeout_ms {
                return Err(NorError::Timeout);
            }
            unsafe { (self.io.yield_)() };
        }
    }
}

impl<'a> SecureRegion for SpiNorIoOpsDriver<'a> {
    fn read_unique_id(&mut self, buf: &mut [u8]) -> Result<usize, NorError> {
        // 4Bh + 4 dummy bytes + 8 or 16 UID bytes
        unsafe { (self.io.cs)(true) };
        let cmd = [CMD_READ_UNIQUE_ID, 0, 0, 0, 0];
        let mut dummy = [0u8; 5];
        self.spi_xfer(&cmd, &mut dummy)?;
        let read_len = buf.len().min(16);
        let zero = [0u8; 16];
        let ret = unsafe { (self.io.spi_transfer)(zero.as_ptr(), buf.as_mut_ptr(), read_len as u32) };
        unsafe { (self.io.cs)(false) };
        if ret == 0 { Ok(read_len) } else { Err(NorError::IoError) }
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
        unsafe { (self.io.cs)(true) };
        let mut dummy = [0u8; 5];
        self.spi_xfer(&cmd, &mut dummy)?;
        let ret = unsafe { (self.io.spi_transfer)(core::ptr::null(), buf.as_mut_ptr(), buf.len() as u32) };
        unsafe { (self.io.cs)(false) };
        if ret == 0 { Ok(()) } else { Err(NorError::IoError) }
    }

    fn program_security_register(&mut self, reg_num: u8, offset: u16, data: &[u8]) -> Result<(), NorError> {
        self.write_enable()?;
        let addr = ((reg_num as u32) << 12) | (offset as u32 & 0x0FFF);
        let cmd = [
            CMD_PROGRAM_SECURITY_REG,
            (addr >> 16) as u8,
            (addr >> 8) as u8,
            addr as u8,
        ];
        unsafe { (self.io.cs)(true) };
        let mut dummy = [0u8; 4];
        self.spi_xfer(&cmd, &mut dummy)?;
        let mut rx_dummy = [0u8; 256];
        self.spi_xfer(data, &mut rx_dummy[..data.len()])?;
        unsafe { (self.io.cs)(false) };
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
}
