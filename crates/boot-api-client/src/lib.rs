//! Safe Client Wrapper for Bootloader ABI Services
//!
//! Used by Applications to inspect boot state, manage firmware slots,
//! confirm test images, and feed OTA update chunks.

#![no_std]

use boot_abi::{
    BootApi, BootInfo, IoOps, SlotInfo, BOOT_API_ADDR, BOOT_API_MAGIC, BOOT_OK,
};

pub struct BootApiClient {
    api: &'static BootApi,
}

impl BootApiClient {
    /// Connects to the Bootloader ABI table at fixed address 0x08000440.
    ///
    /// Returns `None` if the magic number is invalid or ABI table is unpopulated.
    pub fn new() -> Option<Self> {
        let api_ptr = BOOT_API_ADDR as *const BootApi;
        let api = unsafe { &*api_ptr };
        if api.magic != BOOT_API_MAGIC {
            return None;
        }
        Some(Self { api })
    }

    /// Reads BootInfo from Bootloader.
    pub fn get_boot_info(&self) -> Result<BootInfo, i32> {
        let mut info = core::mem::MaybeUninit::<BootInfo>::uninit();
        let ret = unsafe { (self.api.get_boot_info)(info.as_mut_ptr()) };
        if ret == BOOT_OK {
            Ok(unsafe { info.assume_init() })
        } else {
            Err(ret)
        }
    }

    /// Reads status of a specified slot (0 = Slot A, 1 = Slot B).
    pub fn slot_status(&self, slot: u8) -> Result<SlotInfo, i32> {
        let mut info = core::mem::MaybeUninit::<SlotInfo>::uninit();
        let ret = unsafe { (self.api.slot_status)(slot, info.as_mut_ptr()) };
        if ret == BOOT_OK {
            Ok(unsafe { info.assume_init() })
        } else {
            Err(ret)
        }
    }

    /// Erases an entire inactive slot in NOR flash.
    pub fn slot_erase(&self, io: &IoOps, slot: u8) -> Result<(), i32> {
        let ret = unsafe { (self.api.slot_erase)(io as *const _, slot) };
        if ret == BOOT_OK { Ok(()) } else { Err(ret) }
    }

    /// Writes data to an inactive slot at a given byte offset.
    pub fn slot_write(&self, io: &IoOps, slot: u8, offset: u32, data: &[u8]) -> Result<(), i32> {
        let ret = unsafe {
            (self.api.slot_write)(
                io as *const _,
                slot,
                offset,
                data.as_ptr(),
                data.len() as u32,
            )
        };
        if ret == BOOT_OK { Ok(()) } else { Err(ret) }
    }

    /// Verifies the signature and hash of an inactive slot.
    pub fn verify_slot(&self, io: &IoOps, slot: u8, work: &mut [u8]) -> Result<(), i32> {
        let ret = unsafe {
            (self.api.verify_slot)(
                io as *const _,
                slot,
                work.as_mut_ptr(),
                work.len() as u32,
            )
        };
        if ret == BOOT_OK { Ok(()) } else { Err(ret) }
    }

    /// Marks a verified slot as Pending for test boot on the next reboot.
    pub fn mark_pending(&self, io: &IoOps, slot: u8) -> Result<(), i32> {
        let ret = unsafe { (self.api.mark_pending)(io as *const _, slot) };
        if ret == BOOT_OK { Ok(()) } else { Err(ret) }
    }

    /// Confirms the currently running image, promoting it to Confirmed and cancelling rollback.
    pub fn confirm(&self, io: &IoOps) -> Result<(), i32> {
        let ret = unsafe { (self.api.confirm)(io as *const _) };
        if ret == BOOT_OK { Ok(()) } else { Err(ret) }
    }

    /// Requests entry into UART rescue mode on next reset.
    pub fn request_rescue(&self) -> Result<(), i32> {
        let ret = unsafe { (self.api.request_rescue)() };
        if ret == BOOT_OK { Ok(()) } else { Err(ret) }
    }

    /// Initiates a software reset via SCB AIRCR.
    pub fn system_reset(&self) -> ! {
        unsafe { (self.api.system_reset)() }
    }
}
