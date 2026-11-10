# FerroBoot Application Boot API Guide

*ABI version 1 · Target: STM32H750VB · Written from a source review of the FerroBoot repository. The examples have not been compiled or run on hardware.*

## 1. Overview

The FerroBoot bootloader stays resident in internal flash and exports a **function table** (the Boot API) that the application can call at run time. Through it, the application can:

- read the boot context (`BootInfo`) left by the bootloader,
- query the state of the two firmware slots in external SPI NOR,
- confirm a trial image so that automatic rollback is cancelled,
- stage a new image into the inactive slot (erase, write, verify, mark pending),
- request UART rescue mode and trigger a system reset.

The application never links against the bootloader. It finds the table at a fixed address and calls through function pointers using the C ABI (`extern "C"`, AAPCS). The same table can be used from C; only the structures in `boot-abi` need to be mirrored.

The Rust wrapper `boot-api-client` (`BootApiClient`) is a thin safe layer over this table and is used throughout this guide.

## 2. Memory map and prerequisites

| Item | Address / value | Notes |
| --- | --- | --- |
| Bootloader flash | `0x0800_0000` | 128 KB, single sector |
| `BlInfo` | `0x0800_0400` | 64 bytes, magic `.BLI` |
| **Boot API table** | **`0x0800_0440`** | magic `BAPI` (`0x4950_4142`) |
| App load address (AXI SRAM) | `0x2400_0000` | 1 KB image header, then the app |
| **App vector table (VTOR)** | **`0x2400_0400`** | `FLASH` region in the app's `memory.x` |
| `BootInfo` | `0x3800_0000` (SRAM4) | needs the SRAM4 clock enabled to read directly |
| NOR slot A / slot B | `0x0001_0000` / `0x000B_0000` | 640 KB each |
| NOR journal sectors | `0x0000_0000`, `0x0000_1000` | managed by the bootloader |

The application must:

1. Be linked with its vector table at `0x2400_0400` and be packaged with the 1 KB signed header (`xtask sign`).
2. Provide at least **2048 bytes of free stack** when calling any API function. The table's `min_stack` field is 2048, and the bootloader code runs on the caller's stack.
3. Provide a working `IoOps` implementation (Section 5), because the bootloader code has no access to the application's drivers and the bootloader's SPI state is not shared with the app.

The bootloader leaves the CPU with **interrupts disabled (PRIMASK set)** before jumping. If your application uses interrupts, such as SysTick for `millis`, enable them explicitly with `unsafe { cortex_m::interrupt::enable() }`.

## 3. Connecting to the Boot API

```rust
use boot_abi::{BOOT_ABI_VERSION, BOOT_API_ADDR, BootApi};
use boot_api_client::BootApiClient;

let client = match BootApiClient::new() {
    Some(c) => c,
    None => { /* magic mismatch: no bootloader table at 0x0800_0440 */ loop {} }
};
```

`BootApiClient::new()` checks only the magic number. For forward compatibility, also check the ABI version yourself before relying on newer behavior:

```rust
let api = unsafe { &*(BOOT_API_ADDR as *const BootApi) };
assert!(api.abi_version == BOOT_ABI_VERSION);
```

`table_len` and the trailing `reserved` entries are provided so that future ABI versions can append functions without breaking existing applications.

## 4. Data structures

### 4.1 `BootInfo`

Returned by `get_boot_info()`. Written by the bootloader into SRAM4 just before the jump.

| Field | Meaning |
| --- | --- |
| `magic` | `BOOT_INFO_MAGIC` (`INFO`); verify it before using the rest |
| `struct_version`, `size` | layout version and structure size |
| `boot_slot` | `0` = A, `1` = B: the slot this image was loaded from |
| `boot_reason` | `BOOT_REASON_*`; currently `NORMAL` on every normal boot |
| `trial_count` | trial counter taken from the journal (see Section 7 and Section 10) |
| `img_version` | `ImageVersion { major, minor, revision, build_num }` of the running image |
| `bl_version`, `abi_version` | bootloader version and ABI version |
| `flags`, `crc32` | reserved; **currently written as 0**, so do not validate the CRC |

### 4.2 `SlotInfo`

Returned by `slot_status(slot)`.

| Field | Meaning |
| --- | --- |
| `slot_id` | `0` or `1` |
| `state` | one of the `SLOT_STATE_*` values (Section 7) |
| `trial_count` | trial counter from the journal |
| `img_version`, `min_version` | versions recorded in the journal |
| `img_size`, `flags` | reserved in the current implementation |

### 4.3 `IoOps`

Hardware callbacks that the **application** provides to the bootloader code:

```rust
#[repr(C)]
pub struct IoOps {
    pub spi_transfer: unsafe extern "C" fn(tx: *const u8, rx: *mut u8, len: u32) -> i32,
    pub cs:           unsafe extern "C" fn(assert: bool),
    pub millis:       unsafe extern "C" fn() -> u32,
    pub yield_:       unsafe extern "C" fn(),
    pub reserved:     [usize; 4],
}
```

The `IoOps` instance must stay valid for the duration of the call. Declare it `static` and pass a reference.

## 5. Implementing `IoOps`

### 5.1 `spi_transfer`

Full-duplex transfer of `len` bytes. Return `0` on success and a negative value on error.

- The NOR driver issues **reads with `tx == NULL`**. Your implementation must clock out dummy bytes in that case. The `spi1_transfer` helper in `boot-stm32h7` is the reference implementation.
- Be defensive: accept `rx == NULL` as well (discard received data).

### 5.2 `cs`

`cs(true)` must drive the chip-select pin **low** (select the device) and `cs(false)` must drive it **high**. On the reference board this is PA4.

### 5.3 `millis`

Must return a **monotonic millisecond counter**. The NOR driver uses it for busy-wait timeouts (erase and program completion). If it never advances, those timeouts can never fire.

The template's `APP_TICKS` is never incremented, so provide a real implementation, for example with SysTick:

```rust
use core::sync::atomic::{AtomicU32, Ordering};
use cortex_m::peripheral::syst::SystClkSource;

static TICKS: AtomicU32 = AtomicU32::new(0);

#[cortex_m_rt::exception]
fn SysTick() { TICKS.fetch_add(1, Ordering::Relaxed); }

unsafe extern "C" fn app_millis() -> u32 { TICKS.load(Ordering::Relaxed) }

fn init_millis() {
    let mut syst = unsafe { cortex_m::Peripherals::steal() }.SYST;
    syst.set_clock_source(SystClkSource::Core);
    syst.set_reload(400_000 - 1);     // 1 ms at a 400 MHz core clock
    syst.clear_current();
    syst.enable_interrupt();
    syst.enable_counter();
    unsafe { cortex_m::interrupt::enable() };   // PRIMASK is left set by the bootloader
}
```

Call `init_millis()` after `init_clocks()`, since the reload value assumes the 400 MHz core clock.

### 5.3.1 `yield_`

Called by the bootloader code while it waits on long operations such as sector erase (tens to hundreds of milliseconds). Use it to **kick the watchdog**:

```rust
unsafe extern "C" fn app_yield() { boot_stm32h7::Iwdg::kick(); }
```

During a trial boot the bootloader starts the independent watchdog with a 3 s timeout, and it cannot be stopped once started.

## 6. API reference

All functions return `0` (`BOOT_OK`) on success and a negative error code otherwise. `BootApiClient` maps this to `Result<_, i32>`.

| Wrapper method | Purpose | Needs `IoOps` |
| --- | --- | --- |
| `get_boot_info()` | Copy `BootInfo` from SRAM4 | no |
| `slot_status(slot)` | Replay the journal and return state of one slot | no (uses the bootloader's own I/O) |
| `slot_erase(io, slot)` | Erase the whole inactive slot | yes |
| `slot_write(io, slot, off, data)` | Write bytes into the inactive slot | yes |
| `verify_slot(io, slot, work)` | Load, hash and verify the signature of a slot | yes |
| `mark_pending(io, slot)` | Journal the slot as Pending for the next boot | yes |
| `confirm(io)` | Promote the running (Testing) image to Confirmed | yes |
| `request_rescue()` | Set the TAMP flag that selects rescue mode at next reset | no |
| `system_reset()` | Software reset through `SCB::sys_reset`; never returns | no |

Notes for each function:

- **`slot_erase`**: erases all 160 sectors (640 KB) of a slot, one 4 KB sector at a time. It takes seconds, so `yield_` must keep the watchdog alive. It returns `ERR_ACTIVE_SLOT` if `slot` is the running slot.
- **`slot_write`**: `off + len` must not exceed 640 KB (`ERR_OUT_OF_BOUNDS`). The slot must have been erased first, because NOR can only clear bits. Offset `0` must contain the signed image **header**, exactly as produced by `xtask sign`. Writing to the active slot returns `ERR_ACTIVE_SLOT`.
- **`verify_slot`**: loads the slot into `work` and verifies header, signature and hash. The buffer must be at least header plus image size (Section 9). Any verification failure is reported as `ERR_VERIFY_FAILED`.
- **`mark_pending`**: does **not** verify the slot itself. Always call `verify_slot` first and mark only a slot that verified successfully.
- **`confirm`**: only valid while the running slot is in the **Testing** state. Otherwise it returns `ERR_INVALID_STATE`.
- **`request_rescue`**: only sets the flag. Call `system_reset()` afterwards to actually enter rescue mode.

The update-streaming entries `update_init`, `update_feed` and `update_abort` exist in the table, but in the current bootloader they are **stubs that return `BOOT_OK` without doing anything**. Do not rely on them yet.

## 7. Boot lifecycle and slot states

| State | Value | Meaning |
| --- | --- | --- |
| Empty | 0 | nothing usable in the slot |
| Written | 1 | data written, not yet marked for boot |
| Pending | 2 | marked for a test boot on the next reset |
| Testing | 3 | currently on trial; must call `confirm()` |
| Confirmed | 4 | accepted; boots normally |
| Invalid | 5 | failed verification or exhausted its trials |

Typical lifecycle of an update:

```
slot_erase -> slot_write (chunks) -> verify_slot -> mark_pending -> system_reset
   bootloader: verify again -> Pending becomes Testing (trial+1), IWDG started (3 s) -> jump
   new app:    self-test -> confirm() -> Confirmed
   no confirm: IWDG reset -> trials exhausted -> slot Invalid -> previous slot boots (rollback)
```

The two slots are symmetric. The **inactive slot** is `1 - BootInfo.boot_slot`, and it is the only slot you may erase or write.

## 8. Recipes

### 8.1 Startup: read context and confirm

Decide whether to confirm from the **slot state**, not from `trial_count` alone (see Section 10):

```rust
use boot_abi::*;

static APP_IO_OPS: IoOps = IoOps {
    spi_transfer: app_spi_transfer,
    cs: app_cs,
    millis: app_millis,
    yield_: app_yield,
    reserved: [0; 4],
};

fn boot_handshake(client: &BootApiClient) {
    let info = match client.get_boot_info() {
        Ok(i) if i.magic == BOOT_INFO_MAGIC => i,
        _ => return,                       // no valid boot context
    };

    let testing = matches!(
        client.slot_status(info.boot_slot),
        Ok(s) if s.state == SLOT_STATE_TESTING
    );
    if !testing {
        return;                            // Confirmed, or started without a trial
    }

    if run_self_test() {                   // your own health checks
        match client.confirm(&APP_IO_OPS) {
            Ok(()) => { /* confirmed */ }
            Err(ERR_INVALID_STATE) => { /* already confirmed: benign */ }
            Err(_e) => { /* log; the watchdog will roll back */ }
        }
    } else {
        // Do NOT confirm. Let the watchdog reset the board so the bootloader rolls back.
    }
}
```

Important: from the first instruction of a trial boot you must keep calling `Iwdg::kick()` at an interval well below 3 s. Run the self-test with that in mind and keep it short, or kick inside it.

### 8.2 Staging an update into the inactive slot

```rust
fn stage_update(
    client: &BootApiClient,
    info: &BootInfo,
    mut next_chunk: impl FnMut(&mut [u8]) -> usize,   // fills buf, returns 0 at the end
    work: &mut [u8],                                 // see Section 9
) -> Result<(), i32> {
    let inactive = 1 - info.boot_slot;
    let mut buf = [0u8; 256];
    let mut off = 0u32;

    client.slot_erase(&APP_IO_OPS, inactive)?;

    loop {
        let n = next_chunk(&mut buf);
        if n == 0 { break; }
        client.slot_write(&APP_IO_OPS, inactive, off, &buf[..n])?;
        off += n as u32;
    }

    client.verify_slot(&APP_IO_OPS, inactive, work)?;
    client.mark_pending(&APP_IO_OPS, inactive)?;
    client.system_reset()                                // never returns
}
```

The image that you stream must be the **signed image** with the 1 KB header at offset 0. Encrypted images are not supported by `verify_slot` at the moment, because it calls the loader with `require_encrypted = false`.

### 8.3 Request rescue mode

```rust
client.request_rescue()?;   // sets the TAMP flag
client.system_reset();      // bootloader sees the flag and enters UART rescue
```

### 8.4 Plain reset

```rust
client.system_reset();
```

## 9. Constraints and best practices

**Work buffer for `verify_slot`.** The loader copies the whole image (header plus payload) into `work`. Choose a buffer that:

- is at least `1 KB + image size` long,
- **does not overlap the running application**. The app executes from AXI SRAM starting at `0x2400_0000`, and the `FLASH` region in the template's `memory.x` spans the whole 512 KB, so use only memory beyond the end of your own image, or a different RAM,
- is in clocked memory. On STM32H7 the D2 SRAM1/2/3 at `0x3000_0000` are clock-gated at reset. Enable their clocks in `RCC_AHB2ENR` before use.

I did not verify how the loader behaves with a buffer that is too small. Assume that the result is `ERR_VERIFY_FAILED`.

**Watchdog.** Once a trial boot has started the watchdog, kick it from the first line of `main`, from `yield_`, and anywhere a loop may run for more than about a second.

**Active slot protection.** Erase and write are rejected for the running slot. Do not try to overwrite the image you are running. Always stage into `1 - boot_slot`.

**Power loss.** Mark a slot Pending only after `verify_slot` succeeded. A power cut during `slot_write` leaves a slot that is not marked, so the running slot keeps booting.

**Interrupts and reentrancy.** The API functions are not reentrant and use the SPI peripheral directly through your `IoOps`. Do not call them from interrupt handlers, and do not use the same SPI instance concurrently from other tasks.

**Stack.** Reserve at least 2 KB of free stack for each call. Update code that runs deep in the call tree needs correspondingly more.

## 10. Error codes

| Code | Name | Typical cause and handling |
| --- | --- | --- |
| 0 | `BOOT_OK` | success |
| -1 | `ERR_INVALID_PARAM` | null pointer or slot > 1; fix the call |
| -2 | `ERR_ACTIVE_SLOT` | tried to erase or write the running slot; use `1 - boot_slot` |
| -3 | `ERR_OUT_OF_BOUNDS` | `off + len` beyond the 640 KB slot |
| -4 | `ERR_VERIFY_FAILED` | signature, hash or header invalid, or the work buffer too small |
| -5 | `ERR_NOR_IO` | SPI or NOR failure; check wiring, `IoOps` and timeouts |
| -6 | `ERR_INVALID_STATE` | e.g. `confirm()` while the slot is not Testing |
| -7 | `ERR_BUSY` | reserved for the update stream API |
| -8 | `ERR_UNSUPPORTED` | reserved |
| -9 | `ERR_DECRYPT_FAILED` | reserved for encrypted images |
| -10 | `ERR_BUFFER_TOO_SMALL` | reserved |

## 11. Known limitations of the current implementation

These come from reading the sources and should be treated as open issues, not as guarantees.

1. **`trial_count` stays at 1 after a confirm.** `abi_confirm` writes Confirmed with the old trial value, so later boots report `trial_count = 1`. An application that confirms whenever `trial_count > 0` gets `ERR_INVALID_STATE` on every later boot. Fix in the bootloader by writing trial `0` on confirm, and use `slot_status().state == TESTING` in the application, as in Section 8.1.
2. **Rollback may not trigger for a Testing slot that reboots.** The bootloader increments the trial counter and starts the watchdog only on the Pending to Testing transition. A slot that is already Testing and reboots without confirming is neither counted nor watchdog-protected. With `DEFAULT_MAX_TRIALS = 3` (the design document says 1) it may keep being selected.
3. **`update_init`, `update_feed`, `update_abort` are stubs.** Use the erase, write, verify and mark sequence from Section 8.2.
4. **Version fields in the journal.** `mark_pending` copies `img_ver` from the existing journal record, and `slot_write` does not update the journal, so a freshly written slot may report version 0.0.0 in `slot_status()` until the bootloader loads it.
5. **`BootInfo.crc32` and `flags` are not populated.**
6. **`millis` in the template never advances**, which disables the NOR timeouts. See Section 5.3.
7. **Encrypted images** are not accepted by `verify_slot` or by the boot path (`require_encrypted = false`).

## 12. Integration checklist

- [ ] App linked at `0x2400_0400`, signed with `xtask sign`, and uploaded to a slot
- [ ] `init_clocks()` runs first, and `IoOps` callbacks are implemented, including a working `millis`
- [ ] Interrupts enabled if SysTick or other interrupts are used
- [ ] Watchdog kicked from the first instruction and from `yield_`
- [ ] `BootApiClient::new()` succeeds and `abi_version == 1`
- [ ] Confirmation is decided from `slot_status(boot_slot).state == SLOT_STATE_TESTING`
- [ ] Updates stage into `1 - boot_slot`, call `verify_slot` before `mark_pending`, and use a work buffer that does not overlap the running image
- [ ] A test that deliberately does not confirm, to check rollback on your board

## Appendix A. C header (`boot_abi.h`)

This header mirrors the `boot-abi` crate for a **32-bit Cortex-M7 target** (GCC/Clang/IAR/Keil, C11; C99 works if you replace `_Static_assert` with a typedef trick). All structures are plain C layout with natural alignment, which is what `#[repr(C)]` produces, so no packing pragmas are needed. The `_Static_assert` lines guard against drift between the header and the bootloader.

```c
#ifndef BOOT_ABI_H
#define BOOT_ABI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- Magic numbers, version, fixed addresses ---- */
#define BOOT_API_MAGIC     0x49504142u   /* 'BAPI' */
#define BOOT_INFO_MAGIC    0x4F464E49u   /* 'INFO' */
#define BOOT_ABI_VERSION   1u

#define BOOT_API_ADDR      0x08000440u   /* function table in bootloader flash */
#define BOOT_INFO_ADDR     0x38000000u   /* SRAM4 (clock must be enabled) */
#define APP_VTOR_ADDR      0x24000400u   /* app vector table in AXI SRAM */
#define BOOT_SLOT_SIZE     (640u * 1024u)

/* ---- Error codes ---- */
#define BOOT_OK                0
#define ERR_INVALID_PARAM     (-1)
#define ERR_ACTIVE_SLOT       (-2)
#define ERR_OUT_OF_BOUNDS     (-3)
#define ERR_VERIFY_FAILED     (-4)
#define ERR_NOR_IO            (-5)
#define ERR_INVALID_STATE     (-6)
#define ERR_BUSY              (-7)
#define ERR_UNSUPPORTED       (-8)
#define ERR_DECRYPT_FAILED    (-9)
#define ERR_BUFFER_TOO_SMALL  (-10)

/* ---- Slot states, slot ids, boot reasons ---- */
#define SLOT_STATE_EMPTY       0u
#define SLOT_STATE_WRITTEN     1u
#define SLOT_STATE_PENDING     2u
#define SLOT_STATE_TESTING     3u
#define SLOT_STATE_CONFIRMED   4u
#define SLOT_STATE_INVALID     5u

#define BOOT_SLOT_A            0u
#define BOOT_SLOT_B            1u
#define BOOT_SLOT_NONE         0xFFu

#define BOOT_REASON_NORMAL        0u
#define BOOT_REASON_RESCUE_KEY    1u
#define BOOT_REASON_RESCUE_TAMP   2u
#define BOOT_REASON_ROLLBACK      3u
#define BOOT_REASON_ALL_INVALID   4u

/* ---- Data structures ---- */
typedef struct {
    uint8_t  major;
    uint8_t  minor;
    uint16_t revision;
    uint32_t build_num;
} boot_image_version_t;

typedef struct {
    uint32_t magic;            /* BOOT_INFO_MAGIC */
    uint16_t struct_version;
    uint16_t size;
    uint8_t  boot_slot;        /* 0 = A, 1 = B */
    uint8_t  boot_reason;
    uint8_t  trial_count;
    uint8_t  reserved0;
    boot_image_version_t img_version;
    uint32_t bl_version;
    uint16_t abi_version;
    uint16_t flags;            /* currently 0 */
    uint32_t crc32;            /* currently 0, do not validate */
} boot_info_t;

typedef struct {
    uint8_t  slot_id;
    uint8_t  state;            /* SLOT_STATE_* */
    uint8_t  trial_count;
    uint8_t  reserved0;
    boot_image_version_t img_version;
    uint32_t img_size;         /* reserved */
    uint32_t flags;            /* reserved */
    uint32_t min_version;
} boot_slot_info_t;

/* Hardware callbacks provided BY THE APPLICATION to the bootloader code.
 * tx may be NULL (read: send dummy bytes). cs(true) drives CS low. */
typedef struct {
    int32_t  (*spi_transfer)(const uint8_t *tx, uint8_t *rx, uint32_t len);
    void     (*cs)(bool select);
    uint32_t (*millis)(void);
    void     (*yield)(void);        /* kick the watchdog here */
    uintptr_t reserved[4];
} boot_io_ops_t;

/* Function table exported by the bootloader at BOOT_API_ADDR. */
typedef struct {
    uint32_t magic;
    uint16_t abi_version;
    uint16_t table_len;
    uint32_t ctx_size;         /* update context size (update_* are stubs now) */
    uint32_t min_stack;        /* free stack required per call, bytes */

    int32_t (*get_boot_info)(boot_info_t *out);
    int32_t (*slot_status)(uint8_t slot, boot_slot_info_t *out);
    int32_t (*slot_erase)(const boot_io_ops_t *io, uint8_t slot);
    int32_t (*slot_write)(const boot_io_ops_t *io, uint8_t slot, uint32_t off,
                          const uint8_t *data, uint32_t len);
    int32_t (*verify_slot)(const boot_io_ops_t *io, uint8_t slot,
                           uint8_t *work, uint32_t work_len);
    int32_t (*mark_pending)(const boot_io_ops_t *io, uint8_t slot);
    int32_t (*confirm)(const boot_io_ops_t *io);
    int32_t (*request_rescue)(void);
    int32_t (*update_init)(uint8_t *ctx, const boot_io_ops_t *io);
    int32_t (*update_feed)(uint8_t *ctx, const uint8_t *rx, uint32_t rx_len,
                           uint8_t *tx, uint32_t tx_cap);
    int32_t (*update_abort)(uint8_t *ctx);
    void    (*system_reset)(void);  /* never returns; see note below */
    uintptr_t reserved[8];
} boot_api_t;

#ifndef __cplusplus
_Static_assert(sizeof(boot_image_version_t) == 8,  "boot_image_version_t size");
_Static_assert(sizeof(boot_info_t)          == 32, "boot_info_t size");
_Static_assert(sizeof(boot_slot_info_t)     == 24, "boot_slot_info_t size");
_Static_assert(sizeof(boot_io_ops_t)        == 32, "boot_io_ops_t size (32-bit target)");
_Static_assert(sizeof(boot_api_t)           == 96, "boot_api_t size (32-bit target)");
#endif

/* Returns the table, or NULL if the magic or ABI version does not match. */
static inline const boot_api_t *boot_api_get(void)
{
    const boot_api_t *api = (const boot_api_t *)(uintptr_t)BOOT_API_ADDR;
    if (api->magic != BOOT_API_MAGIC || api->abi_version != BOOT_ABI_VERSION) {
        return NULL;
    }
    return api;
}

#ifdef __cplusplus
}
#endif
#endif /* BOOT_ABI_H */
```

Notes on the header:

- **`system_reset` never returns.** The Rust side declares it as `-> !`. The pointer is declared as a plain `void (*)(void)` here for portability, so follow every call with `for (;;) {}`.
- **`bool` compatibility.** Rust `bool` and C `_Bool` are both one byte holding 0 or 1, so the `cs(bool)` callback is ABI-compatible.
- **`table_len` is not checked** by this header. If `boot_api_get()` returns NULL although the magic matches, print `api->abi_version` to see which version the bootloader reports.
- **Layout assumption.** The size asserts assume 4-byte pointers and `uintptr_t`, which holds on Cortex-M. They are derived from the Rust structure definitions, so keep them in sync if `boot-abi` changes.

## Appendix B. C examples

The examples use CMSIS names for STM32H750 (`stm32h7xx.h`). Board-specific functions (`board_spi_xfer_byte`, `board_nor_cs`, `clock_init`, `run_self_test`) are placeholders that you provide. Check register and macro names against your own CMSIS header.

### B.1 Project setup for a C application

**Linker script.** The application executes from AXI SRAM, directly behind the 1 KB signed header, and keeps data in DTCM:

```ld
MEMORY
{
  FLASH (rx)  : ORIGIN = 0x24000400, LENGTH = 512K - 0x400   /* vector table first */
  RAM   (rwx) : ORIGIN = 0x20000000, LENGTH = 128K
}
```

Place `.isr_vector` first in `FLASH`, and set the initial stack pointer to the top of `RAM` (`0x20020000`). The bootloader reads the initial SP and reset vector from `0x24000400` and sets `SCB->VTOR` to that address before jumping.

**Do not let `SystemInit()` reset VTOR to flash.** The ST template `system_stm32h7xx.c` sets `SCB->VTOR` to the internal flash base by default. That would point the vector table at the bootloader and break SysTick and all other interrupts. Either define the user vector table address (typically `USER_VECT_TAB_ADDRESS` with `VECT_TAB_BASE_ADDRESS 0x24000000` and `VECT_TAB_OFFSET 0x400`) or remove the assignment.

**Build, sign, upload.**

```sh
arm-none-eabi-objcopy -O binary app.elf app.bin
cargo run --release -p xtask -- sign --bin app.bin --out app_signed.bin
python3 rescue_upload.py --port /dev/ttyUSB0 --image app_signed.bin --slot 0
```

### B.2 `IoOps` implementation

```c
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <stm32h7xx.h>
#include <boot_abi.h>

/* Provided by your board code */
extern uint8_t board_spi_xfer_byte(uint8_t tx);  /* blocking 1-byte exchange on SPI1 */
extern void    board_nor_cs(bool select);        /* true = drive PA4 low */

static int32_t app_spi_transfer(const uint8_t *tx, uint8_t *rx, uint32_t len)
{
    for (uint32_t i = 0; i < len; i++) {
        uint8_t r = board_spi_xfer_byte((tx != NULL) ? tx[i] : 0xFFu);
        if (rx != NULL) {
            rx[i] = r;
        }
    }
    return 0;                                    /* negative on error */
}

/* 1 ms tick: the NOR driver uses millis() for erase/program timeouts. */
static volatile uint32_t g_ms;

void SysTick_Handler(void)
{
    g_ms++;
}

static uint32_t app_millis(void)
{
    return g_ms;
}

static void app_yield(void)
{
    IWDG1->KR = 0xAAAAu;                         /* kick the independent watchdog */
}

static void init_millis(void)
{
    SysTick_Config(SystemCoreClock / 1000u);     /* SystemCoreClock must be 400 MHz */
    __enable_irq();                              /* the bootloader leaves PRIMASK set */
}

static const boot_io_ops_t g_io = {
    .spi_transfer = app_spi_transfer,
    .cs           = board_nor_cs,
    .millis       = app_millis,
    .yield        = app_yield,
    .reserved     = { 0u, 0u, 0u, 0u },
};
```

The byte-wise SPI loop is correct but slow. For NOR erase and program speed, replace it with a DMA or FIFO-based transfer, like the `spi1_transfer` helper in `boot-stm32h7`.

### B.3 Startup handshake and confirm

Decide from the slot **state**, not from `trial_count` (see Section 11):

```c
extern bool run_self_test(void);   /* your own health checks; keep them short */

static void boot_handshake(const boot_api_t *api)
{
    boot_info_t info;
    boot_slot_info_t slot;

    if (api->get_boot_info(&info) != BOOT_OK || info.magic != BOOT_INFO_MAGIC) {
        return;                                  /* no valid boot context */
    }
    if (api->slot_status(info.boot_slot, &slot) != BOOT_OK) {
        return;
    }
    if (slot.state != SLOT_STATE_TESTING) {
        return;                                  /* Confirmed or no trial */
    }
    if (!run_self_test()) {
        return;                                  /* do NOT confirm: watchdog rolls back */
    }

    int32_t rc = api->confirm(&g_io);
    if (rc == BOOT_OK || rc == ERR_INVALID_STATE) {
        /* confirmed (or already confirmed: benign) */
    } else {
        /* log rc; leave unconfirmed so the bootloader rolls back */
    }
}
```

From the first instruction of a trial boot you must kick the watchdog (`app_yield()`) at an interval well below 3 s.

### B.4 Staging an update into the inactive slot

```c
#define WORK_BASE  0x30000000u                   /* D2 SRAM1 + SRAM2, contiguous */
#define WORK_SIZE  (256u * 1024u)                /* image + 1 KB header must fit */

typedef size_t (*chunk_source_t)(uint8_t *buf, size_t cap, void *ctx);  /* 0 = end */

int32_t stage_update(const boot_api_t *api, const boot_info_t *info,
                     chunk_source_t next_chunk, void *ctx)
{
    uint8_t  buf[256];
    uint32_t off = 0u;
    size_t   n;
    uint8_t  inactive = (uint8_t)(1u - info->boot_slot);   /* never the running slot */
    int32_t  rc;

    rc = api->slot_erase(&g_io, inactive);                 /* takes seconds; yield kicks the IWDG */
    if (rc != BOOT_OK) { return rc; }

    while ((n = next_chunk(buf, sizeof buf, ctx)) != 0u) {
        rc = api->slot_write(&g_io, inactive, off, buf, (uint32_t)n);
        if (rc != BOOT_OK) { return rc; }
        off += (uint32_t)n;
    }

    /* D2 SRAM clocks are gated at reset on STM32H7 */
    RCC->AHB2ENR |= RCC_AHB2ENR_D2SRAM1EN | RCC_AHB2ENR_D2SRAM2EN;
    (void)RCC->AHB2ENR;                                    /* read back for the clock delay */

    rc = api->verify_slot(&g_io, inactive, (uint8_t *)WORK_BASE, WORK_SIZE);
    if (rc != BOOT_OK) { return rc; }                      /* ERR_VERIFY_FAILED, ... */

    rc = api->mark_pending(&g_io, inactive);               /* only after a successful verify */
    if (rc != BOOT_OK) { return rc; }

    api->system_reset();
    for (;;) { }                                           /* never reached */
}
```

The streamed data must be the **signed image** produced by `xtask sign`, with the 1 KB header at offset 0. Images larger than the work buffer need a larger buffer outside the region your application occupies in AXI SRAM. `WORK_BASE` here avoids the running image because the app is linked into AXI SRAM, but the work buffer must still be large enough for header plus payload.

### B.5 Request rescue mode

```c
void enter_rescue(const boot_api_t *api)
{
    if (api->request_rescue() == BOOT_OK) {    /* sets the TAMP flag only */
        api->system_reset();                   /* the bootloader then enters UART rescue */
        for (;;) { }
    }
}
```

### B.6 `main()` skeleton

```c
extern void clock_init(void);                  /* your own 400 MHz setup, as in init_clocks() */
extern void board_led_toggle(void);

int main(void)
{
    clock_init();                              /* do not rely on the bootloader's clock state */
    init_millis();

    const boot_api_t *api = boot_api_get();
    if (api == NULL) {
        for (;;) { app_yield(); }              /* no bootloader table found */
    }

    boot_handshake(api);

    for (;;) {
        app_yield();                           /* keep the watchdog fed */
        board_led_toggle();
        /* ... application work ... */
    }
}
```

As with the Rust application, configure the clocks, GPIO and USART yourself instead of relying on state left by the bootloader.
