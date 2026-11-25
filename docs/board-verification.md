# On-board verification steps (H750 NetLite)

> 繁體中文版: [board-verification.zh-TW.md](board-verification.zh-TW.md)

These steps cover the items in Appendix E.1 ("verification status") of `docs/bsp split plan.md` that have **no on-board record yet**.

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
| `corrupt.bin` | A normal image with one body bit flipped; expect `ERR_VERIFY` |
| `big-a.bin`, `big-b.bin` | Padded to `BIG_SIZE` (default 327680 B = 320 KiB, > 64KB) with fill `0xA5` / `0x5A` (builds 2 / 3); together they check whole-slot erase |
| `ver-2.bin`, `ver-3.bin` | The same app signed with different build numbers; checks version selection |

Upload with `make upload SLOT=0`, or `python3 tools/rescue_upload.py --port /dev/ttyACM0 --image <file> --slot <0|1>`.

## Cases and expected results

| # | Case | How | Expected |
|---|---|---|---|
| 1 | Testing trial count and rollback | Upload `noconfirm.bin` to slot 1; slot 0 holds a confirmed build 1. Reset by hand 4 times | Boots 1-3 run the image with trial 1/3, 2/3, 3/3; on boot 4 the slot is out of trials and the other slot boots |
| 2 | Watchdog rollback | Upload `hang.bin` and leave the board alone | The board resets itself, counts trials the same way and rolls back |
| 3 | Corrupt image | Upload `corrupt.bin` | `ERR_VERIFY`; the slot is not marked bootable |
| 4–5 | Range, length and slot errors | With the board in rescue mode, run `make probe` (`rescue_upload.py --probe`; writes nothing) | Every line is `ok`: `ERR_RANGE` (offset at the end of the slot, offset overflow), `ERR_LEN` (length 0, longer than a chunk), `ERR_SLOT` (slot 2 for `w`/`e`/`m`) |
| 6 | Large image and whole-slot erase | Upload `big-a.bin` to a slot, then `big-b.bin` to the **same** slot (no manual erase in between) | Each upload prints 10 `.` for the erase; both uploads reach `m` with `OK`. If the erase stopped short of the slot, the 0xA5 left behind AND 0x5A would make `m` answer `ERR_VERIFY` |
| 7 | Version selection | Upload `ver-2.bin` to slot 0 and `ver-3.bin` to slot 1 (both Pending) and let each confirm | Boot order: build 3 first (newer, both Pending), then build 2 (Pending beats Confirmed), then with both Confirmed build 3 wins. The middle boot of build 2 is expected |
| 8 | Cold boot | Run `j`, remove power completely, power on, run `j` again | The same slot boots; `seq` and `trial` are unchanged between the two dumps |

## Reporting

For each case, paste the complete serial log. Start with:

```
case: <number and name>
image: <file name>
build tag: <build tag from the boot log>
```

The log runs from the upload or power-on to the result. For cases 1, 2, 7 and 8, finish with a `j` (journal dump).
