# 板上驗證步驟（H750 NetLite）

> English version: [board-verification.md](board-verification.md)

對應 `docs/bsp split plan.md` 附錄 E.1「驗證狀態」中**尚無板上紀錄**的項目。

**下面的預期結果是從原始碼讀出來的預測**（`enter_rescue_mode`、`decide`、`trial_step`、`boot-rescue`），不是觀察到的。若實際不同，那就是一個發現，可能出在韌體也可能出在本文件，請照實回報。

案例 1、3、7、8 的開機序列（哪個 slot 執行、`decide`、trial 計數、journal 的內容）也已在 host 上用真正的 `decide`、`trial_step` 與 journal 跑過：`crates/boot-core/tests/board_verification_sequences.rs`。那只能確認表格與程式碼一致，不能確認板子真的照程式碼做。

## 準備

```sh
make flash-boot        # 刷入 bootloader
make sign              # 正常 image（build 1）
make test-imgs         # 產生全部測試 image 到 target/testimg/
```

| 檔案 | 用途 |
|---|---|
| `noconfirm.bin` | 試開機不 confirm（build 2），每次開機用掉一次 trial |
| `hang.bin` | 同上且不餵看門狗，板子自行重置（build 2） |
| `corrupt.bin` | 正常 image（build 1）的 body 翻轉 1 bit，預期 `ERR_VERIFY` |
| `big-a.bin`、`big-b.bin` | 補到 `BIG_SIZE`（預設 327680 B = 320 KiB，> 64KB），填充值分別為 `0xA5`／`0x5A`（build 2／3）；兩個搭配驗整 slot 擦除 |
| `ver-2.bin`、`ver-3.bin` | 同一份 app 不同 build 號，驗版本選擇 |

**Rescue 模式：** 重置時按住 KEY2；log 會印 `[BL] Rescue mode requested via TAMP / KEY2.`。上傳工具在這個模式下與 bootloader 溝通。

**上傳。** `make upload SLOT=0` 只會上傳 `app_signed.bin`。測試 image 請直接用工具：

```sh
python3 tools/rescue_upload.py --port /dev/ttyACM0 --image target/testimg/<檔> --slot <0|1> [--no-reboot]
```

工具預設在傳完後讓板子重開機，板子接著就啟動剛上傳的 image（它是 Pending）。`--no-reboot` 讓板子留在 rescue 模式，這樣才能接著傳第二個。下面各案例會註明哪裡需要。

## 怎麼讀開機 log

每次開機依序印出：`[BL] build: <tag>`、journal（`[BL] J gen=… next_seq=… next_off=… sector=…`，接著 `[BL] J A …`、`[BL] J B …`）、`[BL] decide: primary=P fallback=F`（沒有就是 `-`）、`[BL] Slot verified successfully…`，若該 slot 在試開機中還有 `[BL] Trial recorded: N/3`。接著是 application：`[APP] build: <tag>, variant: <名稱>`、`[APP] Boot Slot: 0X, Trial Count: 0N`。

`J` 行中的 slot 狀態：`02` Pending、`03` Testing、`04` Confirmed。`decide` 的優先順序是 Testing、Pending、Confirmed；版本只用來在**狀態相同**的兩個 slot 之間分高下（再相同則選 slot A）。Testing 且已用完 3 次 trial 的 slot 不能開機。

## 案例 1：Testing 累計與回退

前置：slot 0 是已 confirm 的 build 1（`make upload SLOT=0`，讓它開機一次，它會自己 confirm）。

1. 進入 rescue 模式，把 `noconfirm.bin` 上傳到 slot 1（預設工具會讓板子重開機，那就是第 1 次開機）。
2. 再手動重置 3 次（第 2、3、4 次開機）。

預期（slot 1 即 `B`）：

| 開機 | `J B` | `decide` | Trial 行 | App |
|---|---|---|---|---|
| 1 | `state=02 trial=0 ver=1.0.0+2` | `primary=1 fallback=0` | `Trial recorded: 1/3` | `Boot Slot: 01, Trial Count: 01`、`On trial and NOT confirming` |
| 2 | `state=03 trial=1` | `primary=1 fallback=0` | `Trial recorded: 2/3` | `Trial Count: 02` |
| 3 | `state=03 trial=2` | `primary=1 fallback=0` | `Trial recorded: 3/3` | `Trial Count: 03` |
| 4 | `state=03 trial=3` | `primary=0 fallback=-` | 無 | `Boot Slot: 00`（slot 0 執行） |

## 案例 2：看門狗回退

前置同案例 1。把 `hang.bin` 上傳到 slot 1，之後不動板子。

預期：同樣四次開機，但完全不需要手動重置。第 1–3 次開機各自印出 `Not kicking the watchdog either`、幾行 `[APP] tick = …`，然後板子自己重置（bootloader 的開機訊息再次出現）；第 4 次啟動 slot 0。**請記下每次重置前最後一個 `tick`**（bootloader 要求約 3 秒，實際時間取決於板子看門狗的時脈）。

## 案例 3：損毀 image

在 rescue 模式下，把 `corrupt.bin` 上傳到 slot 0（`--no-reboot`）。

預期：工具以 `ERROR: sent b'm 0\n', expected 'OK', got 'ERR_VERIFY'` 結束。**不會寫入任何 journal record**，所以 journal 裡該 slot 仍是原本的狀態。若此時重置，而 slot 0 原本是 Confirmed，下次開機會嘗試它並失敗：有 fallback 時印 `Primary candidate failed. Trying fallback slot.`，沒有則印 `All candidates failed verification. Entering rescue.`。測完請重新上傳正常 image。

## 案例 4–5：範圍、長度、slot 錯誤

在 rescue 模式下執行 `make probe`（`rescue_upload.py --probe`，不寫入任何東西）。

預期：每一行都是 `ok`：`ERR_RANGE`（偏移在 slot 尾端、位址溢位）、`ERR_LEN`（長度 0、超過 chunk）、`ERR_SLOT`（`w`、`e`、`m` 的 slot 為 2）。H750 是以 byte 寫入，看不到 `ERR_ALIGN`。

## 案例 6：大 image 與整 slot 擦除

在 rescue 模式下，兩次都用**同一個 slot**，中間不手動擦除，且兩次都加 `--no-reboot`：

```sh
python3 tools/rescue_upload.py --port /dev/ttyACM0 --image target/testimg/big-a.bin --slot 1 --no-reboot
python3 tools/rescue_upload.py --port /dev/ttyACM0 --image target/testimg/big-b.bin --slot 1 --no-reboot
```

預期：每次上傳擦除印出 10 個 `.`，並以 `Done.` 結束，代表 `m` 回了 `OK`。328,860 byte 在 115200 baud 下光是傳輸線路時間就至少 28 秒，所以每次上傳要等一陣子。若擦除沒涵蓋整個 slot，殘留的 0xA5 與 0x5A 相 AND 會得到 0x00，`m` 會回 `ERR_VERIFY`。（在 host 上，這組 image 只要擦除少於六個 64 KiB block 就會被拒。）若 `m` 逾時（工具等 20 秒），請回報花了多久。

之後 slot 1 是 Pending 的 build 3：請上傳正常 image 還原。

## 案例 7：版本選擇

前置：兩個 slot 都有有效的 image，狀態不拘。在 rescue 模式下：

```sh
python3 tools/rescue_upload.py --port /dev/ttyACM0 --image target/testimg/ver-2.bin --slot 0 --no-reboot
python3 tools/rescue_upload.py --port /dev/ttyACM0 --image target/testimg/ver-3.bin --slot 1
```

第一個必須加 `--no-reboot`：否則板子會在 build 3 上傳之前就啟動並 confirm build 2，兩個 slot 就不會同時是 Pending。每次開機印出 `CONFIRMED` 之後，手動重置。

預期：

| 開機 | `J A` / `J B` | `decide` | 執行 |
|---|---|---|---|
| 1 | 兩邊都 `state=02` | `primary=1 fallback=0`（狀態相同：版本較新者） | build 3 |
| 2 | A `02`、B `04` | `primary=0 fallback=1`（Pending 優先於 Confirmed） | build 2 |
| 3 | 兩邊都 `04` | `primary=1 fallback=0`（狀態相同：版本較新者） | build 3 |

中間 build 2 那一輪是預期行為。

## 案例 8：冷開機（完全斷電，不是重置）

**8a，Confirmed。** 讓一個 Confirmed 的 slot 開機並保存 log。完全斷電（若板子由 USB 供電，USB 線也要拔），等幾秒再上電。

預期：兩次開機 log 的 `J gen=… next_seq=… next_off=…` 與 `J A`／`J B` 行相同，`decide` 行與開機的 slot 相同，沒有 `Trial recorded`。（Confirmed slot 的開機不會寫 journal。）

**8b，試開機中。** 上傳 `noconfirm.bin`（第 1 次開機：`Trial recorded: 1/3`），然後**斷電**而不是重置，再上電。

預期：下一次開機一開始印出的 journal 仍是 `state=03 trial=1`，接著 `Trial recorded: 2/3`：trial 計數撐過了斷電。

（`j` 只存在於 rescue 模式；這裡要看的 journal dump 就是每次正常開機一開始印出的那幾行。）

## 回報格式

每個案例貼一段完整的串口 log，開頭先寫：

```
案例: <編號與名稱>
image: <檔名>
build tag: <開機 log 中的 build tag>
```

log 範圍：從上傳或上電開始，到結果出現為止。案例 1、2、7、8 請多附上「最後一次開機之後再一次開機」的 journal 行，讓最終狀態看得到。上傳類案例（3、6、7）請連工具本身的輸出一起貼。
