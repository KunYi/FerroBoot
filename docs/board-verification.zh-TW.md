# 板上驗證步驟（H750 NetLite）

> English version: [board-verification.md](board-verification.md)

對應 `docs/bsp split plan.md` 附錄 E.1「驗證狀態」中**尚無板上紀錄**的項目。

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
| `corrupt.bin` | 正常 image 的 body 翻轉 1 bit，預期 `ERR_VERIFY` |
| `big-a.bin`、`big-b.bin` | 補到 `BIG_SIZE`（預設 327680 B = 320 KiB，> 64KB），填充值分別為 `0xA5`／`0x5A`（build 2／3）；兩個搭配驗整 slot 擦除 |
| `ver-2.bin`、`ver-3.bin` | 同一份 app 不同 build 號，驗版本選擇 |

上傳：`make upload SLOT=0`，或 `python3 tools/rescue_upload.py --port /dev/ttyACM0 --image <檔> --slot <0|1>`。

## 案例與預期

| # | 案例 | 做法 | 預期 |
|---|---|---|---|
| 1 | Testing 累計與回退 | slot 1 上傳 `noconfirm.bin`，slot 0 為已 confirm 的 build 1；手動重置 4 次 | 第 1–3 次開機執行該 image，trial 為 1/3、2/3、3/3；第 4 次該 slot 已用完 trial，改啟動另一個 slot |
| 2 | 看門狗回退 | 上傳 `hang.bin`，不動板子 | 板子自行重置，同樣累計 trial 並回退 |
| 3 | 損毀 image | 上傳 `corrupt.bin` | `ERR_VERIFY`，slot 未被標為可啟動 |
| 4–5 | 範圍、長度、slot 錯誤 | 板子在 rescue 模式下執行 `make probe`（`rescue_upload.py --probe`，不寫入任何東西） | 每一行都是 `ok`：`ERR_RANGE`（偏移在 slot 尾端、位址溢位）、`ERR_LEN`（長度 0、超過 chunk）、`ERR_SLOT`（`w`／`e`／`m` 的 slot 為 2） |
| 6 | 大 image 與整 slot 擦除 | 先上傳 `big-a.bin` 到某個 slot，再把 `big-b.bin` 上傳到**同一個** slot（中間不手動擦除） | 每次上傳擦除都印出 10 個 `.`，兩次都到 `m` 回 `OK`。若擦除沒涵蓋整個 slot，殘留的 0xA5 與 0x5A 相 AND，`m` 會回 `ERR_VERIFY` |
| 7 | 版本選擇 | `ver-2.bin` 上傳到 slot 0、`ver-3.bin` 上傳到 slot 1（兩邊都是 Pending），各自 confirm | 開機順序：先 build 3（兩邊 Pending 時較新者先），再 build 2（Pending 優先於 Confirmed），兩邊都 Confirmed 後 build 3 勝出。中間 build 2 那一輪是預期行為 |
| 8 | 冷開機 | 先 `j`，完全斷電再上電，再 `j` | 啟動同一個 slot；兩次 dump 的 `seq` 和 `trial` 不變 |

## 回報格式

每個案例貼一段完整的串口 log，開頭先寫：

```
案例: <編號與名稱>
image: <檔名>
build tag: <開機 log 中的 build tag>
```

log 範圍：從上傳或上電開始，到結果出現為止。案例 1、2、7、8 結尾請再送一次 `j`（journal dump）。
