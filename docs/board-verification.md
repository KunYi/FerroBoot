# On-board verification steps (H750 NetLite)

> 繁體中文版: [board-verification.zh-TW.md](board-verification.zh-TW.md)

These steps cover the items in Appendix E.1 ("verification status") of `docs/bsp split plan.md` that have **no on-board record yet**.

**The expected results below are predictions read from the sources** (`enter_rescue_mode`, `decide`, `trial_step`, `boot-rescue`), not observations. A difference is a finding, either in the firmware or in this file: report it as it is.

The boot sequences of cases 1, 3, 7 and 8 (which slot runs, `decide`, the trial counts, what the journal shows) are also run through the real `decide`, `trial_step` and journal on the host: `crates/boot-core/tests/board_verification_sequences.rs`. That checks that the tables follow from the code, not that the board does what the code says.

## Setup

```sh
make flash-boot        # flash the bootloader
make sign              # normal image (build 1)
make test-imgs         # build every test image into target/testimg/
```

| File | Purpose |
|---|---|
| `noconfirm.bin` | Does not confirm on trial (build 2); each boot uses up one trial |
| `hang.bin` | As above, and does not kick the watchdog, so the board resets itself (build 2) |
| `corrupt.bin` | A normal image with one body bit flipped (build 1); expect `ERR_VERIFY` |
| `big-a.bin`, `big-b.bin` | Padded to `BIG_SIZE` (default 327680 B = 320 KiB, > 64KB) with fill `0xA5` / `0x5A` (builds 2 / 3); together they check whole-slot erase |
| `ver-2.bin`, `ver-3.bin` | The same app signed with different build numbers; checks version selection |

**Rescue mode:** hold KEY2 while resetting; the log shows `[BL] Rescue mode requested via TAMP / KEY2.` The upload tool talks to the bootloader in this mode.

**Uploading.** `make upload SLOT=0` uploads `app_signed.bin` only. For a test image use the tool directly:

```sh
python3 tools/rescue_upload.py --port /dev/ttyACM0 --image target/testimg/<file> --slot <0|1> [--no-reboot]
```

By default the tool reboots the board when it is done, and the board then starts the image it just uploaded (it is Pending). `--no-reboot` leaves the board in rescue mode, so a second upload can follow. The cases below say where it is needed.

## How to read the boot log

Every boot prints, in this order: `[BL] build: <tag>`, the journal (`[BL] J gen=… next_seq=… next_off=… sector=…`, then `[BL] J A …` and `[BL] J B …`), `[BL] decide: primary=P fallback=F` (`-` if there is none), `[BL] Slot verified successfully…` and, if the slot is on trial, `[BL] Trial recorded: N/3`. Then the application: `[APP] build: <tag>, variant: <name>`, `[APP] Boot Slot: 0X, Trial Count: 0N`.

Slot state in the `J` lines: `02` Pending, `03` Testing, `04` Confirmed. `decide` prefers Testing, then Pending, then Confirmed; the version only breaks a tie between slots in the same state (then slot A). A Testing slot with 3 trials used is not bootable.

## Case 1: Testing trial count and rollback

Setup: slot 0 holds a confirmed build 1 (`make upload SLOT=0`, let it boot once; it confirms).

1. Enter rescue mode and upload `noconfirm.bin` to slot 1 (default: the tool reboots the board; that is boot 1).
2. Reset by hand three more times (boots 2, 3, 4).

Expected, per boot (slot 1 is `B`):

| Boot | `J B` | `decide` | Trial line | App |
|---|---|---|---|---|
| 1 | `state=02 trial=0 ver=1.0.0+2` | `primary=1 fallback=0` | `Trial recorded: 1/3` | `Boot Slot: 01, Trial Count: 01`, `On trial and NOT confirming` |
| 2 | `state=03 trial=1` | `primary=1 fallback=0` | `Trial recorded: 2/3` | `Trial Count: 02` |
| 3 | `state=03 trial=2` | `primary=1 fallback=0` | `Trial recorded: 3/3` | `Trial Count: 03` |
| 4 | `state=03 trial=3` | `primary=0 fallback=-` | none | `Boot Slot: 00` (slot 0 runs) |

## Case 2: Watchdog rollback

Setup as case 1. Upload `hang.bin` to slot 1 and leave the board alone.

Expected: the same four boots as case 1, without any reset by hand. Boots 1–3 each print `Not kicking the watchdog either`, a few `[APP] tick = …` lines, and then the board resets itself (the bootloader banner appears again); boot 4 starts slot 0. **Note the last `tick` before each reset** (the bootloader asks for about 3 s; the real time comes from the board's watchdog clock).

## Case 3: Corrupt image

In rescue mode, upload `corrupt.bin` to slot 0 (`--no-reboot`).

Expected: the tool ends with `ERROR: sent b'm 0\n', expected 'OK', got 'ERR_VERIFY'`. **No journal record is written**, so the journal still shows slot 0 as it was. If you reset now and slot 0 had been Confirmed, the next boot tries it and fails: `Primary candidate failed. Trying fallback slot.` if there is one, otherwise `All candidates failed verification. Entering rescue.` Upload the normal image again afterwards.

## Cases 4–5: Range, length and slot errors

In rescue mode: `make probe` (`rescue_upload.py --probe`; writes nothing).

Expected: every line is `ok`: `ERR_RANGE` (offset at the end of the slot, offset overflow), `ERR_LEN` (length 0, longer than a chunk), `ERR_SLOT` (slot 2 for `w`, `e` and `m`). The H750 programs bytes, so `ERR_ALIGN` cannot be seen.

## Case 6: Large image and whole-slot erase

In rescue mode, use the **same slot** for both, with no manual erase in between, and `--no-reboot` on both:

```sh
python3 tools/rescue_upload.py --port /dev/ttyACM0 --image target/testimg/big-a.bin --slot 1 --no-reboot
python3 tools/rescue_upload.py --port /dev/ttyACM0 --image target/testimg/big-b.bin --slot 1 --no-reboot
```

Expected: each upload prints 10 `.` for the erase and ends with `Done.`, which means `m` answered `OK`. 328,860 bytes at 115200 baud take at least 28 s of line time, so each upload takes a while. If the erase stopped short of the slot, the 0xA5 left behind AND 0x5A would give 0x00 and `m` would answer `ERR_VERIFY`. (On the host, this image pair is rejected whenever fewer than six 64 KiB blocks were erased.) If `m` times out (the tool waits 20 s), report how long it took.

Afterwards slot 1 is Pending with build 3: upload the normal image to restore it.

## Case 7: Version selection

Setup: both slots hold valid images in any state. In rescue mode:

```sh
python3 tools/rescue_upload.py --port /dev/ttyACM0 --image target/testimg/ver-2.bin --slot 0 --no-reboot
python3 tools/rescue_upload.py --port /dev/ttyACM0 --image target/testimg/ver-3.bin --slot 1
```

The first needs `--no-reboot`: otherwise the board starts and confirms build 2 before build 3 is uploaded, and the slots are not both Pending. After each boot has printed `CONFIRMED`, reset by hand.

Expected:

| Boot | `J A` / `J B` | `decide` | Runs |
|---|---|---|---|
| 1 | both `state=02` | `primary=1 fallback=0` (same state: the newer version) | build 3 |
| 2 | A `02`, B `04` | `primary=0 fallback=1` (Pending beats Confirmed) | build 2 |
| 3 | both `04` | `primary=1 fallback=0` (same state: the newer version) | build 3 |

The middle boot of build 2 is expected.

## Case 8: Cold boot (power removed, not a reset)

**8a, Confirmed.** Let a Confirmed slot boot and keep its log. Remove power completely (the USB cable too, if it powers the board), wait a few seconds and power on.

Expected: the two boot logs have the same `J gen=… next_seq=… next_off=…` and `J A`/`J B` lines, the same `decide` line and the same slot, and no `Trial recorded`. (A boot of a Confirmed slot writes nothing to the journal.)

**8b, on trial.** Upload `noconfirm.bin` (boot 1: `Trial recorded: 1/3`), then **remove power** instead of resetting, and power on.

Expected: the journal printed at the start of the next boot still shows `state=03 trial=1`, and then `Trial recorded: 2/3`: the trial count survived the power loss.

(`j` exists only in rescue mode; the journal printed at the start of every normal boot is the dump to use here.)

## Reporting

Paste the complete serial log of each case, starting with:

```
case: <number and name>
image: <file name>
build tag: <build tag from the boot log>
```

The log runs from the upload or power-on to the result. For cases 1, 2, 7 and 8, include the journal lines of the boot after the last one you are testing, so the final state is visible. For the upload cases (3, 6, 7), include the tool's output too.
