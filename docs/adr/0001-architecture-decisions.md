# STM32H750 Bootloader Architecture Decisions (ADR Summary)

## D-01: RAM_LOAD Architecture
App is loaded from external SPI NOR into AXI SRAM (`0x24000000`) and executed directly in RAM. No NOR swap or scratch partitions are needed, saving flash wear and complexity.

## D-02: Verification of RAM Copy (Anti-TOCTOU)
Image verification is performed on the copy already in RAM after copying/decryption, not on external NOR flash. External NOR flash is untrusted and subject to bus interception or tampering.

## D-03: MCUboot Compatible Image Format
Header (32 bytes at offset 0, expanded to 1KB header area `--header-size 0x400`) and TLV encoding match MCUboot format so `imgtool` can be used.

## D-04: RAM Execution Address
AXI SRAM starts at `0x24000000` (512KB). Header occupies 0x400, Vector table (VTOR) is at `0x24000400` (1KB aligned).

## D-05: Journal in SPI NOR Ping-Pong Sectors
Status records are kept in external SPI NOR (2 x 4KB sectors: 0x000000 and 0x001000) using ping-pong append-only records with CRC32. Internal flash has only one 128KB sector and cannot be partially erased.

## D-06: Resident Service ABI
Bootloader exposes a function table at fixed address `0x08000440` (`BootApi`), enabling the App to perform OTA writes, verifications, and slot management without duplicating NOR/journal/crypto drivers.

## D-07: Inversion of Hardware Ownership
Bootloader retains NOR logic and journal operations. When App runs, SPI bit transport is provided to Bootloader service via `IoOps` callback table.

## D-08: Polling Only in Services
Bootloader ABI services run under App's VTOR and stack. Services never enable interrupts, SysTick, or rely on Bootloader `.data`/`.bss`.

## D-09: Ethernet & High-Level Transports in App
Ethernet (lwIP/smoltcp/embassy) runs exclusively in App. App receives OTA chunks and feeds them into `update_feed()`.

## D-10: Single Rescue Channel in Bootloader v1
USART1 (PA9 TX, PA10 RX) serves as the default rescue channel in Bootloader.

## D-11: Release Profile Optimization
`opt-level="z"`, `lto="fat"`, `codegen-units=1`, `panic="abort"` to satisfy the <= 96KB size budget in 128KB Flash.

## D-12: Unsafe Code Policy
`unsafe` is restricted to hardware jumps, cache/MPU management, RAM initialization, and C-ABI boundary. All other crates `#![forbid(unsafe_code)]`.

## D-13: Handshake via TAMP Backup Registers & SRAM4
TAMP backup registers (BKP0R, BKP1R) survive resets and ECC initialization for rescue flags; SRAM4 holds detailed `BootInfo`.

## D-14: Transit Key Encryption vs Device Key Storage Encryption
OTA image is encrypted during transit with project key. Bootloader ingests chunk, decrypts with project key, re-encrypts with device key, and writes to NOR. Signatures are calculated on the plaintext.

## D-15: Device Key Derivation
`DeviceKey = HKDF(ikm = global_secret || nor_otp_seed, salt = mcu_uid || jedec_id || nor_uid_len || nor_uid, info = "dev-enc-v1")`.

## D-16: XChaCha20 Stream Cipher
XChaCha20 with 192-bit nonce and 64-byte block seek counter (`offset / 64`).

## D-17: ENCRYPTED Header Flag & Protected TLVs
Header contains private `IMAGE_F_ENCRYPTED` flag covered by signature. Production bootloader enforces encrypted images.

## D-18: Zeroization
All cryptographic keys and plaintexts are zeroized immediately after use.
