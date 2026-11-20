# FerroBoot Development & Test Tool Specification

**Document:** `docs/host-test-tool-spec.md`
**Project:** FerroBoot
**Target:** STM32H750 NetLite
**Debug Probe:** DAP-Link V2 (SWD + UART)
**UART Device:** `/dev/ttyACM0`
**Initial Transport:** UART Rescue Mode
**Implementation:** Python 3

---

## 1. Purpose

Develop a dedicated PC-side development and test tool for FerroBoot.

The tool is intended for firmware development, bootloader debugging, A/B slot validation, update testing, and regression testing.

It is NOT intended to be a production OTA client.

The primary requirements are:

1. Upload firmware images through the DAP-Link V2 UART interface.
2. Preserve all UART output for debugging and later analysis.
3. Inspect firmware image metadata before uploading.
4. Record device information and A/B slot states before and after updates.
5. Verify the update process and Trial Boot behavior.
6. Support Confirm and Rollback testing.
7. Provide repeatable test procedures.
8. Generate machine-readable test reports.
9. Keep SWD debugging available during development.
10. Never silently report an update as successful when only the UART transfer has completed.

The tool must preserve the original UART output, including boot messages, diagnostic messages, errors, and Journal dumps.

## 2. Hardware and Connection

### 2.1 DAP-Link V2

The development board is connected through DAP-Link V2.

The probe provides two logically separate interfaces:

- SWD: firmware debugging and optional memory inspection.
- UART: Bootloader Rescue Mode communication and debug output.

The initial UART device is:

`/dev/ttyACM0`

The tool must allow the port to be overridden.

Example:

```bash
ferrotest --port /dev/ttyACM0 info
```

Default serial settings:

| Parameter | Default |
|---|---|
| Port | `/dev/ttyACM0` |
| Baud rate | 115200 |
| Data bits | 8 |
| Parity | None |
| Stop bits | 1 |
| Flow control | None |
| Encoding | Latin-1 for lossless byte-to-text display |
| Read timeout | 50 ms |

Use `pyserial`.

Do not assume that the SWD interface and UART interface share the same device node.

### 2.2 SWD Interface

SWD is reserved for optional debugging and test assistance.

The initial implementation should not require SWD to perform a normal update.

Future optional features may include:

- Read CPU halt/running state.
- Read PC and fault registers.
- Detect HardFault.
- Reset the MCU.
- Inspect SRAM BootInfo.
- Inspect selected NOR/Journal memory through target-side debug support.

Possible integration: `pyOCD` or OpenOCD with CMSIS-DAP support.

SWD operations must not interfere with UART communication or modify firmware state unless the test explicitly requests it.

## 3. Software Architecture

Project directory:

```text
tools/
└── ferrotest/
    ├── pyproject.toml
    ├── README.md
    ├── ferrotest/
    │   ├── __init__.py
    │   ├── cli.py
    │   ├── config.py
    │   ├── transport.py
    │   ├── uart_logger.py
    │   ├── rescue_protocol.py
    │   ├── image.py
    │   ├── device.py
    │   ├── updater.py
    │   ├── boot_monitor.py
    │   ├── test_runner.py
    │   ├── test_cases.py
    │   └── report.py
    └── tests/
        ├── test_image.py
        ├── test_protocol.py
        ├── test_uart_logger.py
        └── test_updater.py
```

Responsibilities:

| Module | Responsibility |
|---|---|
| `cli.py` | Command-line interface |
| `config.py` | Serial and test configuration |
| `transport.py` | UART connection and byte transfer |
| `uart_logger.py` | Capture and persist all UART output |
| `rescue_protocol.py` | Existing Rescue UART command implementation |
| `image.py` | Parse and inspect firmware image |
| `device.py` | Query device and Slot information |
| `updater.py` | Firmware upload procedure |
| `boot_monitor.py` | Monitor reset and boot messages |
| `test_runner.py` | Execute test plans |
| `test_cases.py` | Individual test scenarios |
| `report.py` | JSON and human-readable reports |

Keep the implementation modular. The initial version should not introduce a GUI or an asynchronous framework unless required by actual hardware behavior.

## 4. UART Logging Requirements

This is a critical requirement.

All UART data received from the device must be recorded, regardless of whether it is a protocol response or diagnostic output.

### 4.1 Log Files

Each test session creates a unique directory:

```text
logs/
└── 2026-11-17_153000_update_001/
    ├── session.json
    ├── uart.raw
    ├── uart.log
    ├── host.log
    ├── image.json
    └── report.json
```

File descriptions:

| File | Content |
|---|---|
| `session.json` | Test configuration and session metadata |
| `uart.raw` | Original received UART bytes |
| `uart.log` | Decoded UART output with timestamps |
| `host.log` | Host commands, actions, and errors |
| `image.json` | Firmware metadata |
| `report.json` | Final test results |

### 4.2 Logging Rules

1. Never discard received UART output.
2. Record all received bytes before parsing protocol responses.
3. Include timestamps with millisecond precision.
4. Preserve original bytes in `uart.raw`.
5. Decode text without losing the original data.
6. Record connection and disconnection events.
7. Record command transmission and corresponding responses.
8. Record timeout events.
9. Record unexpected responses.
10. Flush logs regularly so that a host crash does not destroy the entire test record.

Example `uart.log`:

```text
[15:30:00.123] [RX] [RESCUE] Entering UART Rescue Mode. Ready.
[15:30:01.100] [TX] p
[15:30:01.105] [RX] PONG
[15:30:02.001] [TX] i
[15:30:02.006] [RX] [RESCUE] BL v1.0, STM32H750, NOR BY25Q32ES
[15:30:03.000] [TX] j
[15:30:03.010] [RX] Journal dump ...
```

The logger must not consume data independently from the protocol handler in a way that causes responses to disappear.

Implement a single serial receive path that distributes received bytes to:

- Persistent logger.
- Protocol response parser.
- Optional live terminal display.

For binary write commands, record the command header and transfer metadata in `host.log`. Preserve the exact received UART bytes in `uart.raw`. Do not duplicate large binary image payloads in the decoded text log.

### 4.3 Live UART Console

The CLI should display received UART output while the device is running.

Example:

```text
[UART] [RESCUE] Entering UART Rescue Mode. Ready.
[HOST] Ping...
[UART] PONG
[HOST] Erasing Slot B...
[UART] OK
```

The console must remain useful during reset and Trial Boot.

If the USB serial device temporarily disappears during reset, the tool should attempt to reconnect within a configurable timeout and continue recording the session.

## 5. Firmware Image Inspector

Before uploading, parse the firmware image.

The inspector must extract:

- Image magic.
- Header size.
- Load address.
- Image body size.
- Protected TLV size.
- Total image size.
- Image version.
- Board ID.
- Minimum Bootloader version.
- Encryption flag.
- Encryption scheme and key ID, when available.
- SHA-256 of the complete file.
- Signature type, when available.

The version format is:

`MAJOR.MINOR.REVISION+BUILD`

Example:

`1.2.3+42`

The tool must reject:

- Invalid image magic.
- Truncated header.
- Invalid header size.
- Invalid or overflowing image lengths.
- Image exceeding the target Slot capacity.
- Wrong Board ID.
- Unsupported image type.
- Missing required metadata.

The tool should distinguish structural validation from cryptographic verification.

A successful host-side parser check does not mean the image signature is trusted. The Bootloader remains the authoritative verification component.

Example:

```bash
ferrotest image inspect app_signed.bin
```

Expected output:

```text
Image Information
-----------------
Board ID       : H750_NETLITE
Version        : 1.2.3+42
Load Address  : 0x24000000
Image Size     : 412672 bytes
Total Size     : 413824 bytes
Encrypted      : No
SHA256         : ...
Host Check     : PASS
```

## 6. Existing Rescue UART Protocol

The first implementation must support the existing protocol without requiring an immediate protocol redesign.

Current commands:

| Command | Description | Expected response |
|---|---|---|
| `p` | Ping | `PONG` |
| `i` | Device information | Information line |
| `e <slot>` | Erase Slot | `OK` |
| `w <slot> <offset_hex> <len_hex>` | Write binary chunk | `OK` |
| `m <slot>` | Mark Pending | `OK` |
| `j` | Dump Journal | Multiple diagnostic lines |
| `r` | Reboot | `REBOOT` |

Slot identifiers:

- `0`: Slot A
- `1`: Slot B

Binary write format:

```text
w <slot> <offset_hex> <length_hex>\n
<binary payload>
```

Chunk size must not exceed 512 bytes.

The Host must use stop-and-wait transfer:

1. Send one command header.
2. Send its binary payload.
3. Wait for the complete response.
4. Validate the response.
5. Advance the offset only after receiving a valid ACK.

Never send the next chunk before receiving the previous chunk's ACK.

### 6.1 Protocol Behaviour and Former Limitations

Earlier Bootloaders had defects in the rescue commands. They are fixed in the Bootloader (`boot-rescue`
crate and `enter_rescue_mode`); there is no firmware version query yet, so the Host cannot tell an old
Bootloader from a fixed one and must treat any reply other than `OK` as a failure.

**E command** `e <slot>`

Erases the whole Slot (every 64KB block; 10 blocks on the H750 NetLite) and answers `OK` afterwards. It takes
several seconds (64KB block erase: typical 180 ms, worst case 2 s), so the Host must wait up to about 40 s.
Older Bootloaders erased only the first 64KB block of a 640KB Slot.

**W command** `w <slot> <offset_hex> <len_hex>\n<bytes>`

The whole write must lie inside the Slot and be 1..512 bytes. Replies: `OK`, `ERR_SLOT`, `ERR_LEN`,
`ERR_RANGE`, `ERR_WR`. A refused write still reads and drops its announced payload (at most 4096 bytes), so
the Host stays in sync. Older Bootloaders did no range check and cut a longer length to 512, which could write
outside the Slot and desynchronize the Host.

**M command** `m <slot>`

Runs the full image verification, then marks the Slot Pending with the version from the image header.
Replies: `OK`, `ERR_SLOT`, `ERR_VERIFY`, `ERR_JRN`, `ERR_INIT`. The reply is a single line; use `j` to see the
journaled version. Older Bootloaders wrote a fixed `1.0.0+1` without looking at the image.

**Slot argument**

Exactly `0` or `1`, otherwise `ERR_SLOT`. Older Bootloaders treated every other byte as Slot 1.

## 7. Device Information and Slot Inspection

The initial protocol provides limited information.

Available commands:

- `i`: basic Bootloader information.
- `j`: Journal dump.

The Host must preserve the full Journal dump and parse known fields when possible.

The parser should be tolerant of additional diagnostic fields in future firmware versions.

The following information is desired:

```text
Device
------
MCU             : STM32H750
Board           : H750_NETLITE
Bootloader      : ...
ABI Version     : ...
NOR             : BY25Q32ES

Boot Status
-----------
Boot Slot       : A
Boot Reason     : NORMAL
Trial Count     : 0

Slot A
------
State           : CONFIRMED
Version         : 1.0.0+1

Slot B
------
State           : EMPTY
Version         : N/A
```

Fields not available from the current protocol must be displayed as `UNKNOWN`, not guessed.

Future Bootloader diagnostic commands should expose structured BootInfo and SlotInfo.

## 8. Update Procedure

Command:

```bash
ferrotest update app_signed.bin
```

Optional arguments:

```bash
ferrotest update app_signed.bin \
    --slot auto \
    --chunk-size 512 \
    --no-mark \
    --no-reboot
```

`auto` should select the non-running Slot only when the Host can reliably determine the current boot Slot.

If the current Slot cannot be determined, require an explicit Slot selection.

### 8.1 Update Steps

1. Create a test session and log directory.
2. Open UART.
3. Start continuous UART logging.
4. Send Ping.
5. Query device information.
6. Dump Journal.
7. Inspect firmware image.
8. Determine target Slot.
9. Verify target Slot is not the running Slot.
10. Check image size against actual Slot capacity.
11. Erase the complete target Slot.
12. Upload image in 512-byte chunks.
13. Wait for ACK after every chunk.
14. Record progress and transfer speed.
15. Compare expected and transmitted byte counts.
16. Request device-side image verification.
17. Mark Pending only after successful verification.
18. Dump Journal again.
19. Reboot if requested.
20. Continue UART monitoring.

The Host must not treat successful chunk transmission as successful firmware installation.

The current Rescue protocol does not yet provide a trustworthy complete erase, verify, and mark sequence. Until these firmware capabilities are fixed, the tool must clearly mark the relevant steps as unsupported or unsafe.

## 9. Trial Boot and Confirmation

The update test must distinguish:

- Uploaded.
- Written.
- Verified.
- Pending.
- Testing.
- Confirmed.

These are different states.

After reboot, the Host should monitor UART output and reconnect to `/dev/ttyACM0` if necessary.

The Host must identify whether the new image actually booted.

Expected information:

- Boot Slot.
- Boot Reason.
- Trial Count.
- Firmware Version.
- Bootloader Version.

The application should eventually provide a diagnostic interface that reports:

- Firmware version.
- Boot Slot.
- Trial status.
- Application initialization result.
- Peripheral self-test result.
- Application health status.

### 9.1 Confirmation Policy

The Python tool must not directly modify Journal records.

Confirmation must be performed by the running application through the Bootloader ABI `confirm()` function.

The Host may request or trigger an application-level test command, but the application must decide whether its self-tests are successful before calling Confirm.

If the test fails, do not Confirm.

Allow the Bootloader's Trial and Rollback mechanism to operate.

The Host should verify the resulting Slot state after reboot.

## 10. Test Scenarios

The initial test suite should include:

### T01: Rescue Connection

- Connect to `/dev/ttyACM0`.
- Receive Rescue banner.
- Send Ping.
- Verify `PONG`.
- Save UART log.

### T02: Device Information

- Send `i`.
- Capture Bootloader information.
- Dump Journal.
- Save raw and decoded output.

### T03: Image Inspection

- Parse a valid image.
- Check image version.
- Check Board ID.
- Check image size.
- Check Header and TLV boundaries.

### T04: Basic Upload

- Erase the selected inactive Slot.
- Upload a valid signed image.
- Check all chunk ACKs.
- Compare transmitted byte count.

### T05: Invalid Image

Test images with:

- Invalid magic.
- Wrong Board ID.
- Truncated payload.
- Invalid TLV.
- Invalid signature.
- Unsupported image version.

The Bootloader must reject invalid images.

### T06: Trial Boot

- Upload a valid image.
- Mark Pending.
- Reboot.
- Verify the new Slot enters Testing.
- Verify the Trial Counter.

### T07: Confirm

- Boot the new image.
- Run application self-tests.
- Call Confirm.
- Reboot.
- Verify the new Slot is Confirmed.

### T08: Rollback

- Upload a valid image.
- Enter Trial Boot.
- Deliberately prevent confirmation.
- Trigger watchdog reset.
- Verify Trial Counter behavior.
- Verify rollback to the last known-good Slot.

### T09: Interrupted Transfer

- Begin uploading an image.
- Stop transmission at a selected offset.
- Reconnect.
- Inspect Journal and Slot state.
- Verify that the incomplete image cannot boot.

### T10: Journal Inspection

- Dump Journal before update.
- Dump Journal after update.
- Dump Journal after Trial Boot.
- Dump Journal after Confirm.
- Compare records and sequence numbers.

### T11: Reboot Monitoring

- Trigger reset.
- Capture boot output.
- Verify that the UART logger preserves output during the reset and reconnection interval.

### T12: Power Failure Tests

Manual test procedure:

- Interrupt power during erase.
- Interrupt power during write.
- Interrupt power during Journal update.
- Interrupt power during Trial Boot.
- Interrupt power before Confirm.
- Interrupt power after Confirm.

The Host must record the test stage and allow the operator to annotate the physical power interruption.

Do not automate power control in the initial version.

## 11. CLI Interface

Proposed commands:

```bash
# Device connection
ferrotest info
ferrotest ping

# Journal and Slot information
ferrotest journal
ferrotest slots

# Image inspection
ferrotest image inspect app_signed.bin

# Upload
ferrotest update app_signed.bin --slot 1

# Upload without marking Pending
ferrotest update app_signed.bin --slot 1 --no-mark

# Upload without reboot
ferrotest update app_signed.bin --slot 1 --no-reboot

# Monitor UART
ferrotest monitor

# Run basic test
ferrotest test basic

# Trial boot test
ferrotest test trial

# Rollback test
ferrotest test rollback

# View last session
ferrotest report --latest
```

All commands should support:

```bash
--port /dev/ttyACM0
--baud 115200
--log-dir logs/
--verbose
```

The port and baud rate should also be configurable in a project-level configuration file.

## 12. Test Report

Every test session must generate a JSON report.

Example:

```json
{
  "session_id": "2026-11-17_153000_update_001",
  "target": "STM32H750_NETLITE",
  "transport": {
    "type": "UART",
    "port": "/dev/ttyACM0",
    "baud": 115200
  },
  "image": {
    "version": "1.2.3+42",
    "size": 413824,
    "sha256": "..."
  },
  "update": {
    "target_slot": 1,
    "bytes_sent": 413824,
    "chunks_sent": 808,
    "transfer_result": "PASS",
    "device_verification": "UNKNOWN",
    "pending_result": "UNKNOWN"
  },
  "boot": {
    "boot_slot": "UNKNOWN",
    "trial_count": null,
    "result": "NOT_TESTED"
  },
  "confirmation": {
    "result": "NOT_TESTED"
  },
  "uart_log": "uart.log",
  "raw_uart_log": "uart.raw"
}
```

Use explicit result values:

- `PASS`
- `FAIL`
- `UNKNOWN`
- `NOT_TESTED`
- `UNSUPPORTED`

Do not convert missing device information into PASS.

## 13. Error Handling

The tool must handle:

- UART connection failure.
- Device disconnected.
- Ping timeout.
- Unexpected protocol response.
- Chunk ACK timeout.
- Image parsing error.
- Invalid Slot.
- Image too large.
- Upload interrupted.
- Reboot timeout.
- Device fails to re-enumerate.
- Unexpected Bootloader output.
- Journal parsing failure.

When an error occurs:

1. Record the error.
2. Preserve all received UART data.
3. Preserve the current transfer offset.
4. Save the current test state.
5. Stop further destructive commands.
6. Return a non-zero exit status.

Do not automatically retry erase or mark Pending after an ambiguous timeout.

For write retries, only retry if the Bootloader protocol provides a way to establish the current accepted offset or safely overwrite the exact same range.

## 14. Development Phases

### Phase 1 — UART Diagnostic Foundation

Implement:

- Python CLI.
- UART connection.
- Ping.
- Device information.
- Journal dump.
- Continuous UART logging.
- Session directories.
- Raw UART capture.

Acceptance criteria:

- Can connect through DAP-Link V2.
- Can communicate with current Rescue Mode.
- Every received UART byte is preserved.
- Logs remain available after errors and disconnects.

### Phase 2 — Image Inspector and Upload

Implement:

- Image parser.
- Version display.
- Board ID validation.
- Image size validation.
- Existing Rescue write protocol.
- Chunk ACK handling.
- Transfer progress.
- Upload report.

Acceptance criteria:

- Can upload a valid image.
- Can detect transfer failures.
- Never silently loses UART output.
- Never reports a complete update based only on transmitted byte count.

### Phase 3 — Boot and Slot Validation

Requires corresponding Bootloader diagnostic improvements.

Implement:

- Structured device status.
- Slot status.
- Complete Slot erase.
- Device-side image verification.
- Safe Pending operation.
- Reboot monitoring.

Acceptance criteria:

- Can distinguish Written, Pending, Testing, Confirmed, and Invalid.
- Can identify the actual booted Slot.
- Can confirm whether the device entered Trial Boot.

### Phase 4 — Application Test and Confirmation

Requires an App diagnostic interface.

Implement:

- Application health query.
- Peripheral self-tests.
- Trial validation.
- Confirm monitoring.
- Post-confirm reboot verification.

Acceptance criteria:

- A successful test can be confirmed.
- A failed test is not confirmed.
- The final report contains the actual resulting Slot state.

### Phase 5 — Regression and Fault Testing

Implement:

- Rollback tests.
- Interrupted transfer tests.
- Journal consistency tests.
- Reboot tests.
- Power-failure test annotations.
- Automated regression reports.

Acceptance criteria:

- Test cases can be repeated.
- Every run produces a complete report.
- Failures retain UART output and test context.

## 15. Design Constraints

1. Python is the Host-side test controller.
2. Bootloader remains responsible for image verification and persistent state transitions.
3. Host must never directly modify Journal contents.
4. Host must never assume the newest image is the active image.
5. Host must distinguish current boot Slot from confirmed Slot and pending Slot.
6. UART logs must be preserved even when protocol parsing fails.
7. Existing Rescue protocol is supported as a compatibility layer.
8. New protocol features must be documented before implementation.
9. No GUI in the initial release.
10. SWD support is optional and must not be a dependency for basic UART tests.

## 16. Definition of Done

The first usable release is complete when:

- `ferrotest info` works.
- `ferrotest ping` works.
- `ferrotest journal` works.
- `ferrotest image inspect` works.
- `ferrotest update` can upload an image with per-chunk ACK.
- UART output is continuously captured.
- Test sessions produce persistent logs.
- Test reports are generated.
- Errors are reported accurately.
- The tool does not falsely claim that an uploaded image has been verified, booted, or confirmed.

The complete Trial/Confirm/Rollback workflow is a later milestone and requires corresponding Bootloader and Application support.
