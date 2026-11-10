
# FerroBoot

> A signed, encrypted, RAM-loading bootloader for STM32H7, written in Rust.

**Status:** Planning complete, implementation starting (M0). There is no usable release yet, and interfaces and formats may still change.

---

## Overview

FerroBoot follows the RAM_LOAD mode of [MCUboot](https://github.com/mcu-tools/mcuboot). The application is stored in an external SPI NOR flash. On every boot, the bootloader loads the image into RAM, verifies the signature on the RAM copy, and jumps to it only if verification passes.

The first target platform is the **STM32H750VB**. Its internal flash is only 128KB, so it cannot hold the application, and it cannot execute in place from an external SPI NOR (not QSPI). Loading into RAM is therefore a necessity of the design, not an option.

## Features (planned)

- **MCUboot-compatible image format** (header + TLV), signable with `imgtool`.
- **RAM_LOAD**: SPI NOR to AXI SRAM, single segment, image size limit 512KB (target application: roughly 450KB or more).
- **Verification on every boot**: SHA-256 plus signature, performed on the RAM copy. On failure the RAM region is wiped and the other slot is tried.
- **A/B slots and power-fail safety**: test boot, confirm, and automatic revert. State is kept in a journal on the NOR flash, so a power cut at any point does not brick the device.
- **Image encryption bound to the device**: OTA images are encrypted in transit with a project key. The device decrypts them and re-encrypts them with a device key before writing to NOR. The device key is derived from a secret inside the bootloader, a random seed in the NOR OTP area, the MCU UID, and the NOR Unique ID, so a NOR image copied to another device will not boot.
- **Resident service ABI**: the bootloader exposes a function table at a fixed address, and the application performs OTA through it. NOR access, the journal, verification, and the update protocol are implemented once and verified once.
- **Transport-independent update protocol**: UART, Ethernet, CAN, and SD card are only application-side transports. The protocol core lives in the bootloader.
- **UART rescue mode**: recovery is possible even if both slots are corrupted.
- **Room for bootloader self-update** is kept in the design (not implemented in v1).

## Target hardware

| Item | Specification |
|---|---|
| MCU | STM32H750VB (128KB flash, LDO supply, 25MHz HSE) |
| External storage | Boya Micro BY25Q32ES (4MB SPI NOR) on SPI1: PA4 CS / PA5 SCK / PA6 MISO / PB5 MOSI |
| Rescue UART | USART1: PA9 TX / PA10 RX |
| Other peripherals (used by the application) | Ethernet (LAN8720A, RMII), FDCAN1 (TJA1042), USB OTG FS, SDMMC1 |
| ROM bootloader recovery | BOOT0 is broken out to a 2.54mm header (pulled low by default); recovery is possible over USART1 or USB DFU |

See [`docs/plan.md`](docs/plan.md), section 4.4, for the full pin map and the design conclusions drawn from it.

## Architecture overview

```text
SPI NOR (A/B slots, journal)               Internal flash, 128KB
┌──────────────────────┐                  ┌───────────────────────┐
│ Slot A / Slot B      │                  │ FerroBoot             │
│  header | image      │                  │  NOR driver, journal  │
│  (ciphertext)        │                  │  verification,        │
│  | TLV (hash, sig)   │                  │  decryption,          │
└──────────┬───────────┘                  │  state machine,       │
           │ read -> decrypt ->           │  update protocol,     │
           │ hash while copying           │  rescue mode,         │
           ▼                              │  Boot Services ABI    │
   AXI SRAM 0x24000000  <-- verify sig ───┴───────────────────────┘
           │
           ▼  jump (VTOR = 0x24000400)
      Application (runs from RAM) -- calls ABI --> OTA update, confirm, Boot Info
```

The complete specification of the memory map, NOR layout, image format, journal, and ABI is in [`docs/plan.md`](docs/plan.md).

## Repository layout (planned)

```text
crates/
├─ boot-abi/         # repr(C) types, error codes, function table (shared by bootloader and app)
├─ boot-image/       # header/TLV parsing and validation rules
├─ boot-crypto/      # SHA-256, signatures, XChaCha20, HKDF
├─ boot-nor/         # SPI NOR logic
├─ boot-journal/     # power-fail-safe state journal
├─ boot-core/        # slot decision and load flow (hardware independent, host-testable)
├─ boot-update/      # update protocol core
├─ boot-api-client/  # safe wrapper for the application side
└─ boot-stm32h7/     # board support: clocks, SPI, RAM loader, jump, cache, MPU
bootloader-h750/     # bootloader executable crate
app-template/        # application template and linker script
tools/               # xtask, host-side signing / encryption / update tools
fuzz/                # cargo-fuzz targets
docs/                # plan, experiment notes, ADRs
```

## Documentation

- [`docs/plan.md`](docs/plan.md): project plan, decision records, open questions, milestones, and task list (single source of truth).
- `docs/experiments.md`: hardware experiment results (created during implementation).
- `docs/adr/`: architecture decision records (created during implementation).

## Development Environment

### Prerequisites

Install the Rust target and required development tools:

```sh
# Rust embedded target
rustup target add thumbv7em-none-eabihf

# Probe-RS tools for debugging and flashing
cargo install probe-rs-tools --locked

# Python dependencies for UART rescue upload
python3 -m pip install pyserial
```

The project uses a Rust-based `xtask` tool for image signing. No external signing utility is required for the current development workflow.

**Build requirements:**

- GNU Make.
- Always use `--locked` for Cargo build and run commands.
- `Cargo.lock` must be committed to the repository.
- Install `arm-none-eabi-binutils` to provide `arm-none-eabi-objcopy`.

Concrete hardware build and flash instructions will be documented after M0 is complete.

## Verify Steps

The Makefile provides separate targets for building firmware, signing the application image, and uploading it through the UART rescue interface.

### 1. Build Bootloader and Application

Build both firmware components without signing the application image:

```sh
make build
```

The individual components can also be built separately:

```sh
make boot    # Build only the bootloader
make app     # Build only the application
```

The application target uses file-based dependency tracking. Cargo is invoked again only when relevant application build inputs have changed or the application ELF is missing.

### 2. Flash the Bootloader

Connect a supported debug probe to the STM32H750 SWD interface and run:

```sh
make flash-boot
```

### 3. Sign the Application Image

Generate the signed application image:

```sh
make sign
```

This target:

1. Builds the application if its ELF is missing or its build inputs have changed.
2. Converts the application ELF into a raw binary image.
3. Signs the binary using the project's Rust-based `xtask` utility.

Signing depends only on the application and the signing tool. It does **not** build the bootloader.

The generated files are stored under `target/thumbv7em-none-eabihf/release/`:

- `app-template`: application ELF.
- `app.bin`: raw application binary.
- `app_signed.bin`: signed application image.

The default target builds both firmware components and signs the application:

```sh
make
```

### 3. Upload Through UART Rescue Mode

Connect the STM32H750 UART rescue interface and execute:

```sh
make upload
```

The default serial port is `/dev/ttyACM0`. Override it when necessary:

```sh
make upload PORT=/dev/ttyUSB0
```

The upload target builds and signs the application image when required. It does not build the bootloader.

### 4. Clean Build Artifacts

Remove Cargo build artifacts and generated application images:

```sh
make clean
```

### Manual Build and Upload

The equivalent commands without Make are:

```sh
# Build the bootloader and application
cargo build --release --locked \
    --target thumbv7em-none-eabihf \
    -p bootloader-h750 \
    -p app-template

# download bootloader into chip flash
probe-rs download --chip STM32H750VBTx target/thumbv7em-none-eabihf/release/bootloader-h750
probe-rs reset --chip STM32H750VBTx

# Convert the application ELF to a raw binary
arm-none-eabi-objcopy \
    -O binary \
    target/thumbv7em-none-eabihf/release/app-template \
    target/thumbv7em-none-eabihf/release/app.bin

# Sign the application image
cargo run --release --locked -p xtask -- \
    sign \
    --bin target/thumbv7em-none-eabihf/release/app.bin \
    --out target/thumbv7em-none-eabihf/release/app_signed.bin

# Upload the signed image
python3 tools/rescue_upload.py \
    --port /dev/ttyACM0 \
    --image target/thumbv7em-none-eabihf/release/app_signed.bin \
    --slot 0
```

**UART rescue limitation:** The current implementation supports images up to 64KB because it only erases the first 64KB block of a slot. Larger application images require an updated erase implementation before they can be uploaded safely.

## Security Notes

- **Never commit private keys or production keys.** The default `.gitignore` blocks `*.pem`, `*.key`, and `keys/prod/`.
- `keys/dev/` holds only development keys that are clearly marked as insecure, for CI and hardware-in-the-loop testing.
- BOOT0 is broken out, so anyone can force the device into the ROM bootloader. **Production devices must enable RDP level 1**, and RDP level 2 (irreversible) must not be used until bootloader self-update has been validated.
- Image encryption is meant to prevent cloning of the NOR flash and eavesdropping on OTA traffic. It does **not** protect against an attacker who holds the device and can use a debugger. The threat model and known limitations are in `docs/plan.md`, section 16.

## License

To be decided (MIT OR Apache-2.0 is suggested, to match the Rust ecosystem).

## Acknowledgments

- [MCUboot](https://github.com/mcu-tools/mcuboot): image format and overall concept.
- [embassy-boot](https://github.com/embassy-rs/embassy/tree/main/embassy-boot): reference for the state machine and power-fail handling.
