# Crate versions and compatibility

Every crate has its **own** version (`version = "…"` in its `Cargo.toml`; there is no shared
workspace version). Pre-1.0 semver applies: a breaking API change bumps the **minor** number.

## Interface versions

| Constant | Where | Current | Meaning |
| --- | --- | --- | --- |
| `BOOT_ABI_VERSION` | `boot-abi` | **2** | Bootloader ↔ application ABI (`BootApi`, `IoOps`, `BlInfo`, `BootInfo`). `BootApiClient::at()` refuses a table with a different value. |

ABI v2 (this release) changed, compared with the never-released v1: `IoOps::spi_transfer` / `cs` are
optional, `BootApi::caps` was added (formerly reserved), `BlInfo` gained `platform_id`,
`exec_model`, `ramfunc_base`, `ramfunc_size` (formerly reserved), and `boot-abi` no longer carries any
board address (see `boot-layout`). All structure sizes are unchanged.

## Crate versions

| Crate | Version | Note |
| --- | --- | --- |
| `boot-abi` | 0.2.0 | ABI v2 |
| `boot-layout` | 0.1.0 | pure-data board layouts (new) |
| `boot-crc` | 0.1.0 | CRC-32/ISO-HDLC, replaces `crc32fast` (new) |
| `boot-time` | 0.1.0 | interrupt-free millisecond clock (new) |
| `boot-image` | 0.2.0 | `validate`/`parse_image` take `&Layout`; `DEFAULT_BOARD_ID` removed |
| `boot-journal` | 0.2.0 | `JournalManager::init(nor, &Layout)` |
| `boot-nor` | 0.2.0 | owns SPI-NOR geometry; optional transport (`NorError::NoTransport`) |
| `boot-core` | 0.2.0 | `SlotLoader::new(.., &Layout, ..)` |
| `boot-update` | 0.2.0 | `UpdateSession::new(&'static Layout)` |
| `boot-rescue` | 0.1.0 | pure UART-rescue helpers: slot argument, write range, slot erase (new) |
| `boot-crypto` | 0.1.0 | unchanged |
| `boot-api-client` | 0.2.0 | `BootApiClient::at(addr)` replaces `new()` |
| `boot-stm32h7` | 0.2.0 | `init_poll_millis()`/`poll_millis()`; owns SRAM4 constants |
| `bootloader-h750` | 0.2.0 | |
| `app-template` | 0.2.0 | |
| `xtask` | 0.2.0 | global `--board` |
| `host-update` | 0.1.0 | unchanged |

Rule of thumb when releasing one crate on its own: bump it, bump (and list here) every crate whose
public API it changes, and record the `BOOT_ABI_VERSION` it was built against.
