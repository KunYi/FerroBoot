//! Board layout as **pure data**.
//!
//! Everything that differs between boards in terms of *where things live* (flash slots, journal
//! sectors, execution/load addresses, fixed bootloader tables) is described by one [`Layout`]
//! value. The bootloader, the host tools (`xtask`, `host-update`) and the tests all read the same
//! value, so the linker script, the Rust code and the signing tool cannot drift apart.
//!
//! This crate deliberately has **no dependency on any HAL**: host tools can use it, and every
//! board's layout is compiled everywhere (a few hundred bytes of const data).
//!
//! Adding a board = adding a `pub static` here (or in the board's own crate) and selecting it
//! from the board's `main`; nothing in `boot-core`, `boot-journal`, `boot-image` or
//! `boot-update` changes.

#![no_std]
#![forbid(unsafe_code)]

/// A byte range inside the **flash backend's address space** (for H750 NetLite: offsets in the
/// external SPI NOR, starting at 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub offset: u32,
    pub len: u32,
}

impl Region {
    pub const fn new(offset: u32, len: u32) -> Self {
        Self { offset, len }
    }

    /// First byte after the region.
    pub const fn end(&self) -> u32 {
        self.offset + self.len
    }
}

/// A byte range in the **CPU address space** (RAM or memory-mapped flash).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddrRegion {
    pub base: u32,
    pub len: u32,
}

impl AddrRegion {
    pub const fn new(base: u32, len: u32) -> Self {
        Self { base, len }
    }

    pub const fn end(&self) -> u64 {
        self.base as u64 + self.len as u64
    }
}

/// How a verified image is made executable.
///
/// `#[non_exhaustive]`: execute-in-place models are added in a later step; matching code needs a
/// wildcard arm and keeps compiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExecModel {
    /// Copy the image into RAM and run it there (H750 NetLite: AXI SRAM).
    RamLoad {
        /// RAM window the image (header + body) must fit in; `load.base` is the load address.
        load: AddrRegion,
        /// Offset of the vector table from the load address (the image header size).
        vtor_offset: u32,
    },
}

/// Everything board-specific about *where things live*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Short stable name, used by `xtask --board`.
    pub name: &'static str,
    /// Value stored in the image's `BOARD_ID` TLV; images for another board are rejected.
    pub board_id: u32,
    /// Journal sectors (ping-pong), in flash-backend offsets. Each region is one erase unit.
    pub journal: [Region; 2],
    /// Application slots A and B, in flash-backend offsets.
    pub slots: [Region; 2],
    /// How verified images are executed.
    pub exec: ExecModel,
    /// RAM address where `BootInfo` is handed to the application.
    pub boot_info_addr: u32,
    /// Fixed bootloader tables in the bootloader's own (memory-mapped) flash. These addresses
    /// must match the bootloader linker script; a host test checks that for every board.
    pub bl_info_addr: u32,
    /// Bootloader resident service table (`BootApi`); applications look it up here.
    pub boot_api_addr: u32,
    /// Project keys and global secret (`SecretsTable`).
    pub secrets_addr: u32,
    /// Public key table (`KeyTable`).
    pub keytable_addr: u32,
}

impl Layout {
    /// Slot region by logical slot id (0 = A, 1 = B).
    pub const fn slot(&self, id: u8) -> Option<Region> {
        match id {
            0 => Some(self.slots[0]),
            1 => Some(self.slots[1]),
            _ => None,
        }
    }

    /// Largest image (header + body + protected TLVs) that fits in every slot.
    pub const fn max_image_size(&self) -> u32 {
        if self.slots[0].len < self.slots[1].len {
            self.slots[0].len
        } else {
            self.slots[1].len
        }
    }

    /// RAM window for RAM-load boards.
    pub const fn load_region(&self) -> Option<AddrRegion> {
        match self.exec {
            ExecModel::RamLoad { load, .. } => Some(load),
        }
    }

    /// Load address for RAM-load boards.
    pub const fn load_addr(&self) -> Option<u32> {
        match self.exec {
            ExecModel::RamLoad { load, .. } => Some(load.base),
        }
    }

    /// Address of the application's vector table once it is ready to run.
    pub const fn entry_vtor_addr(&self) -> u32 {
        match self.exec {
            ExecModel::RamLoad { load, vtor_offset } => load.base + vtor_offset,
        }
    }
}

/// `BOARD_ID` of the H750 NetLite (ASCII 'H750'); a plain const so statics can use it.
pub const H750_NETLITE_BOARD_ID: u32 = 0x4837_3530;

/// H750 NetLite: STM32H750 + 4 MB external SPI NOR, application loaded into AXI SRAM.
///
/// Numbers are the historical values of the former `boot-abi` constants (plan §4.1–§4.3).
pub static H750_NETLITE: Layout = Layout {
    name: "h750-netlite",
    board_id: H750_NETLITE_BOARD_ID,
    journal: [Region::new(0x0000_0000, 4096), Region::new(0x0000_1000, 4096)],
    slots: [
        Region::new(0x0001_0000, 640 * 1024),
        Region::new(0x000B_0000, 640 * 1024),
    ],
    exec: ExecModel::RamLoad {
        load: AddrRegion::new(0x2400_0000, 512 * 1024),
        vtor_offset: 0x400,
    },
    boot_info_addr: 0x3800_0000,
    bl_info_addr: 0x0800_0400,
    boot_api_addr: 0x0800_0440,
    secrets_addr: 0x0801_F000,
    keytable_addr: 0x0801_F800,
};

/// Every known board, for tools that select one by name.
pub static ALL: &[&Layout] = &[&H750_NETLITE];

/// Look a board up by its [`Layout::name`].
pub fn by_name(name: &str) -> Option<&'static Layout> {
    ALL.iter().copied().find(|l| l.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden values written out by hand from plan §4.1–§4.3 (independent of boot-abi).
    #[test]
    fn h750_netlite_golden_values() {
        let l = &H750_NETLITE;
        assert_eq!(l.name, "h750-netlite");
        assert_eq!(l.board_id, u32::from_be_bytes(*b"H750"));
        assert_eq!(l.journal[0], Region::new(0, 0x1000));
        assert_eq!(l.journal[1], Region::new(0x1000, 0x1000));
        assert_eq!(l.slots[0], Region::new(0x1_0000, 0xA_0000));
        assert_eq!(l.slots[1], Region::new(0xB_0000, 0xA_0000));
        assert_eq!(l.max_image_size(), 640 * 1024);
        assert_eq!(l.load_addr(), Some(0x2400_0000));
        assert_eq!(l.load_region(), Some(AddrRegion::new(0x2400_0000, 512 * 1024)));
        assert_eq!(l.entry_vtor_addr(), 0x2400_0400);
        assert_eq!(l.boot_info_addr, 0x3800_0000);
    }

    #[test]
    fn regions_do_not_overlap_and_are_ordered() {
        for l in ALL {
            let mut all = [l.journal[0], l.journal[1], l.slots[0], l.slots[1]];
            all.sort_by_key(|r| r.offset);
            for w in all.windows(2) {
                assert!(w[0].end() <= w[1].offset, "{}: {:?} overlaps {:?}", l.name, w[0], w[1]);
            }
        }
    }

    #[test]
    fn slots_are_symmetric_and_erase_unit_aligned() {
        for l in ALL {
            assert_eq!(l.slots[0].len, l.slots[1].len, "{}", l.name);
            for r in l.slots.iter().chain(l.journal.iter()) {
                assert_eq!(r.offset % 4096, 0, "{}: {:?}", l.name, r);
                assert_eq!(r.len % 4096, 0, "{}: {:?}", l.name, r);
            }
        }
    }

    #[test]
    fn board_ids_and_names_are_unique() {
        for (i, a) in ALL.iter().enumerate() {
            for b in ALL.iter().skip(i + 1) {
                assert_ne!(a.board_id, b.board_id);
                assert_ne!(a.name, b.name);
            }
        }
    }

    #[test]
    fn slot_lookup_and_by_name() {
        assert_eq!(H750_NETLITE.slot(0), Some(H750_NETLITE.slots[0]));
        assert_eq!(H750_NETLITE.slot(1), Some(H750_NETLITE.slots[1]));
        assert_eq!(H750_NETLITE.slot(2), None);
        assert_eq!(by_name("h750-netlite"), Some(&H750_NETLITE));
        assert_eq!(by_name("nope"), None);
    }

    // --- Drift guards: the linker scripts and the Layout must agree ---------------------------

    const BOOTLOADER_MEMORY_X: &str = include_str!("../../../bootloader-h750/memory.x");
    const APP_MEMORY_X: &str = include_str!("../../../app-template/memory.x");

    fn parse_hex(s: &str) -> u32 {
        u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).unwrap()
    }

    /// Address given to an output section: `.name 0xADDR :`.
    fn section_addr(script: &str, name: &str) -> u32 {
        let line = script
            .lines()
            .find(|l| l.trim_start().starts_with(name) && l.contains("0x"))
            .unwrap_or_else(|| panic!("section {name} not found in linker script"));
        let tok = line.split_whitespace().nth(1).unwrap();
        parse_hex(tok)
    }

    /// `ORIGIN = 0x...` of a MEMORY region line starting with `name`.
    fn region_origin(script: &str, name: &str) -> u32 {
        let line = script
            .lines()
            .find(|l| l.trim_start().starts_with(name))
            .unwrap_or_else(|| panic!("region {name} not found"));
        let after = line.split("ORIGIN").nth(1).unwrap();
        let tok = after.split('=').nth(1).unwrap().split(',').next().unwrap();
        parse_hex(tok.trim())
    }

    #[test]
    fn h750_fixed_tables_match_the_bootloader_linker_script() {
        let l = &H750_NETLITE;
        assert_eq!(section_addr(BOOTLOADER_MEMORY_X, ".bl_info"), l.bl_info_addr);
        assert_eq!(section_addr(BOOTLOADER_MEMORY_X, ".boot_api"), l.boot_api_addr);
        assert_eq!(section_addr(BOOTLOADER_MEMORY_X, ".secrets"), l.secrets_addr);
        assert_eq!(section_addr(BOOTLOADER_MEMORY_X, ".keytable"), l.keytable_addr);
    }

    #[test]
    fn h750_bootinfo_sits_in_the_reserved_head_of_the_bootloader_ram() {
        let l = &H750_NETLITE;
        let ram = region_origin(BOOTLOADER_MEMORY_X, "RAM");
        // BootInfo lives in the first 0x100 bytes of SRAM4; the bootloader's own RAM starts after.
        assert_eq!(ram, l.boot_info_addr + 0x100);
        assert!(core::mem::size_of::<boot_abi::BootInfo>() <= 0x100);
    }

    #[test]
    fn h750_app_is_linked_where_the_layout_says_it_runs() {
        let l = &H750_NETLITE;
        // The application's code starts at the vector table, right after the 1 KiB header.
        assert_eq!(region_origin(APP_MEMORY_X, "FLASH"), l.entry_vtor_addr());
    }

    #[test]
    fn fixed_tables_are_ordered_and_fit_the_bootloader_flash() {
        for l in ALL {
            assert!(l.bl_info_addr < l.boot_api_addr);
            assert!(l.boot_api_addr + 0x100 <= l.secrets_addr);
            assert!(l.secrets_addr < l.keytable_addr);
        }
    }
}
