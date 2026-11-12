# Rust Bootloader（MCUboot-like）專案規劃與任務清單

> 平台：STM32H750VB（128KB 內部 Flash）＋ SPI NOR（非 QSPI）
> 模式：App 存放於 SPI NOR，Bootloader 驗證後載入 RAM 執行（RAM_LOAD，無 swap）
> 文件版本：v1.4（BOOT0 已確認引出）　狀態：規劃完成，尚未開始實作

---

## 0. 如何使用本文件

- 實作時以本文件為唯一事實來源。**任何與本文件衝突的實作決定，必須先更新本文件（寫 ADR）再改程式。**
- 任務以 `Tx.y` 編號，完成後把 `- [ ]` 改成 `- [x]`，並在 PR/commit 中引用編號。
- 每個里程碑有 **DoD（Definition of Done）**，未滿足不得進入下一里程碑，除非在 §14 風險表登記原因。
- 「待決事項」見 §3，每項有預設假設。實作者可以先按預設假設進行，但必須在指定的任務中驗證，結果寫回 §3。
- 實驗結果記錄在 `docs/experiments.md`，決策記錄在 `docs/adr/NNNN-title.md`。

---

## 1. 目標與非目標

### 目標
1. 從 SPI NOR 的 A/B slot 載入 App（≥ 450KB，上限 512KB）到 AXI SRAM 並執行。
2. 每次開機驗證 image（SHA-256 ＋ 簽章），任何竄改一律拒絕。
3. 支援 test boot / confirm / 自動 revert，任意時間點斷電不致變磚（Bootloader 本體更新除外，見 §9）。
4. Bootloader 提供**常駐服務庫**（固定位址 ABI），App 透過它完成 OTA，不重複實作 NOR / journal / 驗簽。
5. 更新通道（UART / Ethernet / CAN）只是 App 端的 transport，協議核心在 Bootloader 內，所有通道行為一致。
6. Image 格式相容 MCUboot（可用 `imgtool` 簽章）。
7. **保留** Bootloader 自我更新的擴展性（本階段不實作）。
8. OTA 映像以 project key 加密傳輸，Bootloader 解密後以**裝置專屬 device key** 重新加密才寫入 NOR；開機時串流解密。NOR 複製到其他裝置無法開機（§16）。

### 非目標（v1 不做）
- 壓縮、多 segment 載入、DMA 加速載入（只預留旗標與欄位）。
- 抵禦持有設備且可除錯的攻擊者（需 RDP2 與硬體信任根，H750 做不到，見 §16.1）。
- Bootloader 自我更新的實作（只預留 hook，見 §9）。
- Ethernet / CAN 協議棧進入 Bootloader。
- 安全啟動等級的抗實體攻擊（fault injection 只做基礎防護）。

---

## 2. 已鎖定的決策（ADR 摘要）

| ID | 決策 | 理由 |
|---|---|---|
| D-01 | 採用 RAM_LOAD，不做 swap/scratch | App 在 RAM 執行，NOR 只是儲存區，省下複雜度與磨損 |
| D-02 | 以 **RAM 中的副本**驗證，不是 NOR 上的內容 | 防 TOCTOU；外部 NOR 不受信任 |
| D-03 | Image 格式相容 MCUboot header/TLV | 可用 imgtool，沿用生態 |
| D-04 | App image 載入 AXI SRAM（0x24000000），v1 單一 segment | 450KB 只有 AXI SRAM 放得下 |
| D-05 | 狀態存 NOR journal（ping-pong），不存內部 Flash | 內部 Flash 僅單一 128KB sector，無法局部寫 |
| D-06 | Bootloader 常駐服務，固定位址 ABI 函式表（`repr(C)`，只增不改） | 單一驗證來源，App 開發簡化 |
| D-07 | **硬體所有權反轉**：NOR 邏輯在 Bootloader，SPI 位元傳輸由 App 以 `IoOps` 提供 | 避免 SPI 雙重所有者 |
| D-08 | 服務只用 polling，不依賴中斷、SysTick、Bootloader 的 .data/.bss | 服務跑在 App 的 VTOR 與 stack 下 |
| D-09 | Ethernet stack 只在 App；Bootloader 只收 payload | 128KB 放不下，且風險隔離 |
| D-10 | Bootloader 救援通道 v1 只編入**一個**（預設 UART），feature 切換 | 控制 size 與驗證面 |
| D-11 | Release profile：`opt-level="z"`, `lto="fat"`, `codegen-units=1`, `panic="abort"` | 128KB 預算 |
| D-12 | `unsafe` 只允許出現在指定模組（§10.3） | 可稽核 |
| D-13 | Bootloader 與 App 的握手旗標用 **TAMP 備份暫存器**，豐富資料用 SRAM4 Boot Info | 備份暫存器在 reset 後保留，且不受 RAM ECC 初始化影響 |
| D-14 | OTA 以 project key 加密；Bootloader 內解密後以 device key 重新加密再寫 NOR；**簽章簽在明文** | 傳輸保密＋儲存綁定裝置；簽章不因裝置而異 |
| D-15 | Device key = HKDF(`global_secret` ‖ NOR OTP seed；salt = MCU UID ‖ NOR Unique ID)；**NOR OTP 不視為秘密** | OTP 可被 SPI 讀出，機密性來自 `global_secret`；UID 提供綁定 |
| D-16 | 加密演算法 XChaCha20（可 seek、常數時間、192-bit 隨機 nonce），不用 AEAD；每次 ingest session 新 nonce | 完整性由每次開機的簽章驗證保證；避免 keystream 重複 |
| D-17 | header `ENCRYPTED` 旗標在簽章涵蓋範圍內；加密方案由 unprotected TLV `ENC_INFO` 指示；量產 Bootloader 拒絕未加密 image | 防止以明文版本降級載入 |
| D-18 | 金鑰與明文只在 Bootloader 服務內處理，session 結束／abort 一律 zeroize | App 不接觸金鑰 |

### 對先前討論的修正（請以此為準）

1. **「不可變 stage-0」在內部 Flash 內做不到。** H750 只有一個 128KB sector，erase 是整顆，無法只保護前面一小段。Bootloader 自更新的方案已改寫為 §9。
2. **MCUboot header 大小改為 0x400（不是 0x200）。** H750 向量表約 664 bytes，VTOR 需對齊 1KB；`load_addr` 對齊 1KB 且 header 佔 1KB，向量表才會落在對齊位址（`0x24000400`）。
3. **DMA 限制修正：** DMA1/2 不能存取 ITCM/DTCM，但**可以**存取 AXI SRAM 與 SRAM1-3，因此 SPI→AXI SRAM 的 DMA 載入是可行的（列為選配優化，非 v1）。
4. **`IoOps` 增加 `yield_`（餵 IWDG、讓出 CPU）**：NOR 擦除可能持續數十到數百毫秒，Trial 期間 IWDG 已啟動，不能長時間阻塞。
5. **更新協議預設改為 SMP 子集（mcumgr 相容）**，原先的自訂 UDS 風格降為備案，理由是主機端工具免費可用（見 U-04）。

---

## 3. 待決事項登錄表（Unknowns Register）

狀態：`OPEN` 待驗證、`ASSUMED` 以預設假設進行、`CLOSED` 已確認。

| ID | 問題 | 預設假設 | 影響 | 解決任務 | 狀態 |
|---|---|---|---|---|---|
| U-01 | NOR 型號與介面 | **Boya BY25Q32ES**（4MB、2.7–3.6V、3-byte 位址、SPI mode 0/3、Fast Read ≤120MHz）；備選 Winbond W25Q32JV；SPI1（PA4 CS／PA5 SCK／PA6 MISO／PB5 MOSI），見 §4.4 | NOR 驅動、slot 佈局 | T0.4（/CS 上拉、/WP、/HOLD 接法） | CLOSED（接線細節 OPEN） |
| U-02 | 板上是否有外部 SDRAM/PSRAM | 無（腳位表中沒有 FMC／OCTOSPI 腳位）；512KB 上限成立 | 決定 512KB 上限能否放寬 | T0.4 以原理圖複核 | CLOSED（待複核） |
| U-03 | 簽章演算法 | ECDSA P-256（imgtool 相容），待 size 實測 | Bootloader size | T2.2 | OPEN |
| U-04 | 更新協議：SMP 子集 vs 自訂 | SMP 子集（CBOR，`minicbor`） | 主機工具、code size | T5.1 | OPEN |
| U-05 | BOOT0 與 ROM bootloader（UART/USB DFU）是否可從板子接觸 | 需要可接觸；USART1 與 USB FS 腳位已在板上（§4.4），BOOT0 已引出到 2.54mm 排針、預設下拉到地 | **決定是否可啟用 Bootloader 自更新** | T0.5 實機演練 | CLOSED（待 T0.5 演練通過） |
| U-06 | 能否/值不值得從 DTCM 執行指令 | 不從 DTCM 執行；熱點放 ITCM | App linker 設計 | T1.4(E1) | OPEN |
| U-07 | App 成長目標 | image 硬上限 512KB | RAM 策略 | 產品端確認 | ASSUMED |
| U-08 | 映像加密 | **v1 實作**（§16）；壓縮不做 | 載入時間、size | T2.8（size/時間實測） | 方案 CLOSED，實測 OPEN |
| U-09 | 量產 RDP 等級 | RDP L1（BOOT0 已引出，量產至少 RDP1）；**自更新驗證完成前不用 L2**（L2 不可逆） | 量產政策 | M8 | ASSUMED |
| U-10 | 威脅模型 | 防誤燒與遠端偽造 image；不防實體 glitch（僅基礎 double-check） | 加固範圍 | M8 | ASSUMED；加密目標為防複製 NOR 與 OTA 竊聽（§16.1），不防持有設備且可除錯者 |
| U-11 | 救援 UART | **USART1（PA9 TX／PA10 RX）**；鮑率 115200 起，目標 921600 | 救援模式 | T0.5 確認與 ROM bootloader 腳位一致 | 腳位 CLOSED／鮑率 OPEN |
| U-12 | App 是否使用 RTOS/Embassy | 不假設；API 為 blocking＋`yield_` | `IoOps` 設計 | 產品端確認 | ASSUMED |
| U-13 | Anti-rollback 強度 | NOR journal 單調計數（可被擦除 NOR 繞過） | 安全性 | M8 | ASSUMED |
| U-14 | 更新 session 是否需要認證（HMAC/挑戰） | v1 不做（image 簽章已把關），預留欄位 | 協議 | M8 | ASSUMED |
| U-15 | 量產 NOR 初次燒錄方式 | 工廠用 UART 救援協議或外部燒錄器 | 產線 | M5 | OPEN |
| U-16 | RAM ECC 實際覆蓋範圍（AXI/TCM/SRAM1-4）與讀未初始化的行為 | 全部先整塊對齊寫入初始化 | 載入前置 | T1.4(E2) | OPEN |
| U-17 | Bootloader 工作頻率 | 400MHz / VOS1（不使用 480MHz） | 穩定性 | T0.4 前 | ASSUMED |
| U-18 | NOR Security Register／Unique ID 規格 | 已查 datasheet：BY25Q32ES 為 3×1024B＋128-bit UID；W25Q32JV 為 3×256B＋64-bit UID；指令與鎖定位元相同（§16.3）。v1 只用 SR#1 前 256B | seed 與綁定 | T4.8 以實機驗證鎖定行為 | 規格 CLOSED／實機驗證 OPEN |
| U-19 | H750 RNG 可用、健康測試通過 | 可用 | seed 與 nonce 產生 | T4.8 | OPEN |
| U-20 | project key 輪替策略 | `.secrets` 預留 2 槽 | 外洩處置 | M8 | ASSUMED |
| U-21 | 是否額外在內部 Flash 放 per-device secret | 否（自更新會擦除；v1 不依賴） | 安全強度 | Phase F 評估 | ASSUMED |

---

## 4. 記憶體與儲存佈局

### 4.1 內部 Flash（0x08000000，128KB，單一 sector）

| 位址 | 大小 | 內容 |
|---|---|---|
| 0x08000000 | 0x400 | 向量表（保留 1KB） |
| 0x08000400 | 0x40 | `.bl_info`：magic、`bl_version`、`bl_size`、build hash、flags、reserved（**自更新與 App 相容性檢查用**） |
| 0x08000440 | 0x100 | `.boot_api`：服務函式表（§8），預留空位 |
| 0x08000600 | — | Bootloader 程式碼與唯讀資料 |
| 0x0801F000 | 0x800 | `.secrets`：project 加密金鑰槽 ×2（含 key id）、`global_secret`，固定位址（RDP 保護，見 §16） |
| 0x0801F800 | 0x800 | `.keytable`：公鑰槽 ×3（含 key id）＋ metadata，固定位址 |

Size 預算：**v1 ≤ 96KB（含 keytable），超過 100KB 為紅燈**，需 ADR 說明。

### 4.2 RAM

| 區域 | 位址 | 大小 | 用途 |
|---|---|---|---|
| AXI SRAM | 0x24000000 | 512KB | **App image 載入區**（header 0x400 ＋ 內容），向量表在 0x24000400 |
| ITCM | 0x00000000 | 64KB | App 熱點 code（App 啟動碼搬入） |
| DTCM | 0x20000000 | 128KB | App stack、.bss、一般變數 |
| SRAM1-3 | 0x30000000 | 288KB | Ethernet DMA、heap、大 buffer（MPU non-cacheable） |
| SRAM4 | 0x38000000 | 64KB | 0x38000000–0x380000FF：Boot Info；其後：Bootloader .data/.bss/stack |
| TAMP BKP regs | — | 32×4B | Bootloader↔App 握手旗標（§7.3） |

載入白名單（v1）：`[0x24000000, 0x24080000)`，且不得與 Bootloader RAM（SRAM4）重疊。

### 4.3 SPI NOR（BY25Q32ES：4MB，位址 0x000000–0x3FFFFF；以下佈局使用 0x000000–0x230000，其後為 App data）

```
0x000000  Journal              2 × 4KB  (0x000000, 0x001000)
0x002000  Meta/Reserved        56KB     (對齊到 0x010000)
0x010000  Slot A               640KB    (→ 0x0B0000)
0x0B0000  Slot B               640KB    (→ 0x150000)
0x150000  BL staging (未來)     256KB    (→ 0x190000)  ← v1 保留不使用
0x190000  Factory slot (選配)   640KB    (→ 0x230000)
0x230000  App data / 其餘
```

所有分區 4KB 對齊；Slot 起點 64KB 對齊（便於 64KB block erase）。

NOR 晶片內建的 Security Register（OTP）與 Unique ID（64 或 128-bit，依晶片）用於 device key（§16），不佔 main array；晶片支援度見 U-18。

### 4.4 板級資訊（STM32H750 NetLite，依賣家範例）

**電源與時鐘**：LDO supply（H750 僅支援 LDO，CubeMX 的警告可忽略）；HSE 25MHz；LSE 32.768kHz（RTC）。Rust 端 LDO 為重設後預設，設定 VOS1 後需等待 VOSRDY（細節對照 RM0433）；Bootloader 以 400MHz／VOS1 運作（U-17）。

**腳位與用途分工**

| 功能 | 腳位 | Bootloader | App |
|---|---|---|---|
| SPI1 → NOR（BY25Q32ES） | PA5 SCK、PA6 MISO、PB5 MOSI、PA4 CS（GPIO，軟體 NSS） | 載入、journal、佈建 | 擁有 SPI1，經 `IoOps` 提供給 ABI |
| USART1 | PA9 TX、PA10 RX | UART 救援、開發 log／燒錄 | 自有；與 ROM bootloader 同腳位（T0.5 確認） |
| USB OTG FS（Device） | PA11 DM、PA12 DP | v1 不使用（ROM DFU 救援） | 自有 |
| FDCAN1（TJA1042） | PD0 RX、PD1 TX | v1 不使用 | CAN transport（M7） |
| ETH RMII（LAN8720A） | PA1、PA2、PA7、PB11、PB12、PB13、PC1、PC4、PC5；PA8（MCO1 → PHY 25MHz） | 不碰 | Ethernet transport（M6） |
| SDMMC1（SD 卡 4-bit） | PC8–PC12、PD2 | 不碰 | 選配 transport（T6.4） |
| LED | PC3 | 狀態閃爍碼 | — |
| KEY2 | PC13 | 開機按住 → 救援模式（極性待確認） | — |
| WKUP | PA0 | v1 不使用 | — |
| SWD | PA13、PA14 | **不得重新配置**（開發期保持可除錯） | — |
| RTC | PB2、PB15 | 不碰 | — |

**設計結論**
1. **NOR 的 /CS 是 GPIO（PA4）**，對應 `IoOps.cs`；SPI1 的硬體 NSS 關閉（SSM=1、SSI=1）。MCU 重置期間 PA4 為浮空，板上 /CS 必須有外部上拉（BY25Q32ES datasheet 建議），Bootloader 的第一個動作是把 PA4 設為高電位輸出（T0.4 確認）。
2. **/WP 與 /HOLD 沒有出現在腳位表**：BY25Q32ES 這兩腳在 QE=0 時為 WP／HOLD 功能且有內部上拉，可懸空；若板上把它們接地會造成問題（T0.4 確認）。H750 以標準 SPI 連接，QE 必須維持 0。
3. **SPI1 在 APB2，可使用 DMA1/2**，且 DMA 可存取 AXI SRAM，因此 E4（DMA 載入）可行；SCK 頻率先以 50MHz 起步，E3 實測能否提高到 100MHz（受 STM32 SPI 上限與走線品質限制，BY25Q32ES 的 Fast Read 上限為 120MHz）。
4. **救援 UART 定為 USART1（PA9/PA10）**，而且這個腳位通常也是 ROM bootloader 的 UART 腳位（以我的記憶，USB FS DFU 的 PA11/PA12 與 FDCAN1 的 PD0/PD1 也在支援清單內，T0.5 對照 AN2606 與實測確認）。同一條 USB-UART 線可同時用於 Bootloader 救援與 ROM 救援。
5. **BOOT0 已引出到 2.54mm 排針，預設下拉到地**：正常開機從內部 Flash 啟動；以跳線把 BOOT0 接到 3.3V 再 reset，即進入 ROM bootloader，可由 USART1 或 USB FS DFU 救援內部 Flash（U-05 關閉，待 T0.5 演練）。這讓 §9 的 Bootloader 自更新風險較可控。排針另一排是否為 3.3V 需對照原理圖，避免誤接 5V。**量產注意**：BOOT0 暴露意味著任何人都能強制進入 ROM bootloader，RDP0 下可直接讀出整顆內部 Flash（含 `global_secret`），因此量產**必須啟用 RDP1**（RDP 回退會整顆擦除 Flash，secret 隨之消失）。
6. **KEY2（PC13）開機按住進救援模式**，不依賴 TAMP／VBAT，也不需要 App 協助；LED（PC3）用閃爍碼顯示狀態。極性與上下拉待對照原理圖。
7. **SD 卡可作為 App 端選配 transport**：App 讀檔後經 ABI `slot_write` 寫入 NOR，Bootloader 不含檔案系統。
8. **Ethernet**：Bootloader 不開 ETH 與 MCO1；PHY 的 25MHz 時鐘由 App 在啟動後輸出，Bootloader 階段 PHY 不工作無妨。
9. **SRAM 時鐘**：H7 重置後 RAM_D2（SRAM1–3）與 RAM_D3（SRAM4）預設不供應時鐘，使用前須在 RCC 啟用；Bootloader 啟用 SRAM4 供 Boot Info，App 若使用 RAM_D2 做 Ethernet DMA，需自行啟用並設為 non-cacheable。
10. **記憶體映射與 §4.1／§4.2 一致**。賣家範例把大部分內容放 AXI SRAM；我們的 App linker 要改為：image 載入區在 AXI SRAM（`0x24000400` 起）、stack 與 `.bss` 在 DTCM、DMA buffer 在 RAM_D2。賣家表格標示 ITCM 為「Write-through cached」，TCM 本身不經 cache，應是指 MPU 屬性設定，請確認。

---

## 5. Image 格式規格

相容 MCUboot（請對照 MCUboot 原始碼 `bootutil/include/bootutil/image.h` 確認欄位與 TLV 型號）。

### 5.1 Header（32 bytes，`#[repr(C)]`，little-endian）

| 欄位 | 型別 | 說明 |
|---|---|---|
| `ih_magic` | u32 | `0x96f3b83d` |
| `ih_load_addr` | u32 | RAM 載入位址（本專案必須是 `0x24000000`，1KB 對齊） |
| `ih_hdr_size` | u16 | **0x400**（imgtool `--header-size 0x400`） |
| `ih_protect_tlv_size` | u16 | protected TLV 區大小 |
| `ih_img_size` | u32 | image 本體大小（不含 header/TLV） |
| `ih_flags` | u32 | 必須含 `IMAGE_F_RAM_LOAD (0x20)`；**量產版必須含私有位元 `ENCRYPTED`**（位元位置先對照 `image.h`，避開 `AES128 0x04`／`AES256 0x08` 等既有定義，§16）；壓縮位元預留且 v1 必須為 0 |
| `ih_ver` | struct | major u8、minor u8、revision u16、build_num u32 |
| `_pad1` | u32 | 0 |

### 5.2 TLV
- TLV info magic：`0x6907`（一般）、`0x6908`（protected）。
- 必要：SHA-256、KEYHASH（key id）、簽章（ECDSA P-256 或 Ed25519，依 U-03）。
- **自訂 protected TLV**（私有範圍，先對照 MCUboot 文件確認不衝突）：
  - `IMAGE_TYPE`：`APP = 0`、`BOOTLOADER = 1`（預留，v1 遇到非 APP 一律拒絕）
  - `BOARD_ID`：板型/硬體版本，防止刷錯板
  - `MIN_BL_VERSION`：此 image 需要的最低 Bootloader / ABI 版本
- 自訂 **unprotected** TLV `ENC_INFO`（固定長度）：`scheme`（1 = PROJECT 傳輸、2 = DEVICE 儲存）、`key_id`、`nonce[24]`、`kdf_ver`（§16）。
- 簽章涵蓋 header（含 `ENCRYPTED` 旗標）＋ **明文** image ＋ protected TLV。

### 5.3 Parser 規則（不信任 NOR 內容）
1. 所有欄位先做邊界檢查再使用，禁止以未驗證的長度做算術。
2. 所有 offset/size 運算用 `checked_*`。
3. `hdr_size + img_size + tlv 總長 ≤ slot 大小`，`load_addr + hdr_size + img_size ≤ 白名單上界`。
4. TLV 遍歷必須有總長與數量上限。
5. 未知 critical TLV → 拒絕；未知非 critical → 忽略。
6. 量產版（feature `require-encrypted`）：header 未設 `ENCRYPTED` 一律拒絕；`ENC_INFO.scheme` 與情境不符（NOR 內必須是 DEVICE，OTA 傳輸必須是 PROJECT）一律拒絕。

---

## 6. 開機流程

```
Reset
 ├─ 時脈(400MHz/VOS1)、MPU、Cache、SPI、TAMP 讀取旗標
 ├─ 若 TAMP 有 "enter rescue" 旗標，或 KEY2（PC13）在開機時被按住 → 進入救援模式（清旗標）
 ├─ NOR 初始化：先等 WIP=0（BY25Q32ES 的 64KB erase 最長 2s）並處理 SUS，再 66h/99h 軟體重置（等待 ≥0.5ms）、JEDEC ID 檢查、確認 BP/CMP/QE 狀態
 ├─ 讀 journal → 取得 slot 狀態
 ├─ 派生 device key（NOR Security Register＋Unique ID＋MCU UID＋`global_secret`，§16；失敗視為 slot 無法解密）
 ├─ decide()（純函式，§11 T3.2）→ 候選 slot 順序
 └─ 對每個候選 slot：
      ├─ 解析 header（§5.3）
      ├─ 初始化目標 RAM（64-bit 對齊寫入，滿足 ECC）
      ├─ NOR → 解密（device key）→ RAM 串流複製，同時對**明文**算 SHA-256
      ├─ 驗證：hash、簽章、key id、BOARD_ID、版本 / anti-rollback
      ├─ 失敗：清零 RAM、journal 記 Invalid、換下一個候選
      └─ 成功：
           ├─ 若為 Pending → journal 記 Testing(trial+1)，啟動 IWDG
           ├─ 寫 Boot Info（SRAM4）
           └─ 跳轉（§6.1）
 全部候選失敗 → 救援模式（UART）
```

### 6.1 跳轉檢查清單（`jump()`）
- [ ] 關閉全域中斷，清 NVIC enable/pending，停用 SysTick
- [ ] 反初始化 SPI、GPIO、UART、DMA、使用到的外設（**IWDG 無法停止，保持運行**）
- [ ] RCC 回到已知狀態（HSI、無 PLL），Flash latency 同步回復；**SRAM4 時鐘保持啟用**（App 啟動碼須在重新配置 RCC 之前先讀取 Boot Info）
- [ ] D-cache 範圍 clean、I-cache invalidate 全部、`DSB`/`ISB`
- [ ] 設定 `VTOR = load_addr + hdr_size`
- [ ] `CONTROL.SPSEL = 0`，`CONTROL.FPCA = 0`，FPU 狀態乾淨
- [ ] 載入 MSP、跳 reset handler（Thumb bit 檢查）
- [ ] zeroize device key、KDF 中間值與解密 buffer（含 SRAM4 內 Bootloader 使用過的區域），App 不得讀到任何金鑰殘留

---

## 7. 持久化狀態

### 7.1 Journal（NOR，ping-pong 兩個 4KB sector）

- Sector header：`magic`、`generation`、`crc`。
- Record（固定 32 bytes，32B 對齊 append）：`magic u16`、`fmt_ver u8`、`kind u8`、`seq u32`、`slot u8`、`state u8`、`trial u8`、`flags u8`、`img_ver (8B)`、`min_version u32`、`key_revoke_mask u8`、reserved、`crc32 u32`。
- `kind` 可擴充（預留 `BL_UPDATE_*` 類型，v1 遇到未知 kind 必須忽略並保留）。
- Sector 滿時：先在新 sector 寫入「目前狀態快照」並驗證 CRC，**再**擦除舊 sector。
- 讀取：取 generation 最大且 header 有效的 sector，在其中取 seq 最大且 CRC 正確的 record。
- 損毀的 record（斷電造成的半寫入）以 CRC 排除，不得影響其他 record。
- 斷電中斷的 erase 可能留下「弱擦除」狀態（讀起來是 1，之後可能翻轉）：任何曾被中斷、或標頭無效但非空白的 sector，重新使用前一律再擦除一次。

### 7.2 Slot 狀態機

```
Empty ──write──▶ Written ──verify+mark_pending──▶ Pending
Pending ──boot──▶ Testing(trial n)
Testing ──App confirm()──▶ Confirmed
Testing ──trial 用盡/驗證失敗──▶ Invalid(reverted)
任何狀態 ──驗證失敗──▶ Invalid
```

不變量（需用 proptest 驗證）：
1. 永不啟動未通過驗證的 image。
2. 永不啟動 Invalid 的 slot。
3. `min_version` 單調不減。
4. Trial 次數不超過上限（預設 1，可設 3）。
5. 任意時間點斷電後，重新開機一定能得到合法決策（至少有一個可啟動 slot，或進入救援模式）。

### 7.3 Boot 握手
- **TAMP 備份暫存器**：`BKP0R` = magic ＋ 命令（`ENTER_RESCUE`、`REQUEST_BL_UPDATE`(預留)）；`BKP1R` = boot reason 碼。需啟用備份域存取（`PWR_CR1.DBP`）。注意：無 VBAT 時斷電會清除。
- **SRAM4 Boot Info**（0x38000000，含 magic、struct version、size、CRC）：boot slot、boot reason、trial、image 版本、BL 版本、ABI 版本、flags。App 讀取前必須啟用 SRAM4 時鐘；Bootloader 必須在首次使用前對 SRAM4 做初始化（ECC）。

---

## 8. Bootloader 服務 ABI

### 8.1 呼叫規則
- AAPCS、`extern "C"`、Thumb 位址；於 Thread mode、特權等級呼叫；**非可重入、不得在 ISR 呼叫**。
- 使用呼叫者（App）的 stack；每個函式文件需標明最低 stack（M4 量測）。
- 不修改 RCC、MPU、Cache；不使用中斷；時間只透過 `IoOps.millis`。
- App 需保證 Flash 區（0x08000000–0x0801FFFF）可讀可執行（含 MPU 設定）。

### 8.2 函式表（`boot-abi` crate，兩端共用）

```rust
#[repr(C)]
pub struct BootApi {
    pub magic: u32,                 // 'BAPI'
    pub abi_version: u16,
    pub table_len: u16,             // 只增不減，App 用它判斷函式是否存在
    pub ctx_size: u32,              // update context 所需 RAM
    pub min_stack: u32,             // 最壞情況 stack 需求
    pub get_boot_info:  unsafe extern "C" fn(out: *mut BootInfo) -> i32,
    pub slot_status:    unsafe extern "C" fn(slot: u8, out: *mut SlotInfo) -> i32,
    pub slot_erase:     unsafe extern "C" fn(io: *const IoOps, slot: u8) -> i32,
    pub slot_write:     unsafe extern "C" fn(io: *const IoOps, slot: u8, off: u32, data: *const u8, len: u32) -> i32,
    pub verify_slot:    unsafe extern "C" fn(io: *const IoOps, slot: u8, work: *mut u8, work_len: u32) -> i32,
    pub mark_pending:   unsafe extern "C" fn(io: *const IoOps, slot: u8) -> i32,
    pub confirm:        unsafe extern "C" fn(io: *const IoOps) -> i32,
    pub request_rescue: unsafe extern "C" fn() -> i32,
    pub update_init:    unsafe extern "C" fn(ctx: *mut u8, io: *const IoOps) -> i32,
    pub update_feed:    unsafe extern "C" fn(ctx: *mut u8, rx: *const u8, rx_len: u32,
                                             tx: *mut u8, tx_cap: u32) -> i32,
    pub update_abort:   unsafe extern "C" fn(ctx: *mut u8) -> i32,   // 中止 session 並 zeroize ctx（§16.8）
    pub system_reset:   unsafe extern "C" fn() -> !,
    pub reserved:       [usize; 8], // 預留空位，新函式追加於此後
}

#[repr(C)]
pub struct IoOps {
    pub spi_transfer: unsafe extern "C" fn(tx: *const u8, rx: *mut u8, len: u32) -> i32,
    pub cs:           unsafe extern "C" fn(assert: bool),
    pub millis:       unsafe extern "C" fn() -> u32,
    pub yield_:       unsafe extern "C" fn(),   // 餵 IWDG／讓出 CPU
    pub reserved:     [usize; 4],
}
```

### 8.3 強制規則（由 Bootloader 內程式碼保證，不靠 App 自律）
1. `slot_erase` / `slot_write` **只能作用於非執行中的 slot**（依 Boot Info 判斷），否則回 `ERR_ACTIVE_SLOT`。
2. 寫入位址必須落在該 slot 範圍內，且遵守 NOR 頁邊界。
3. `mark_pending` 內部先呼叫 `verify_slot` 的邏輯，驗證失敗不可標記。
4. `confirm` 只能對 Testing 狀態的 slot 生效。
5. 所有指標參數以長度檢查，禁止信任 App 傳入的長度（對 `data+len`、`work+work_len` 做範圍限制）。
6. 錯誤碼穩定、跨版本不重編號。

### 8.4 ABI 管理
- 函式表只能追加，不能修改簽名或順序。
- App 端 `boot-api-client` 在 `table_len` 不足時回傳 `Unsupported`，允許 App 提供 fallback。
- v1 發布前做 ABI 凍結檢查（M8）。
- **注意：這不是安全邊界。** 同為特權模式，惡意 App 可繞過；信任根仍是每次開機的簽章驗證。

---

## 9. Bootloader 自我更新（預留擴展性，本階段不實作）

### 9.1 物理限制
- H750 內部 Flash 為單一 sector：更新 Bootloader 必須擦除整顆 Flash，**擦除到寫完之間（約秒級，請查 datasheet）斷電即變磚**，沒有 bank swap。
- 內部 Flash 以 256-bit（32 bytes）為單位程式化，且帶 ECC：只能對每個 32B word 寫一次，讀取「部分程式化」的 word 可能觸發 ECC 錯誤。
- 因此「完全斷電安全」做不到，只能**縮短風險窗口、並確保有硬體救援路徑**。

### 9.2 預留方案（未來實作的設計方向）

**方案 A（建議）：由「舊 Bootloader」在開機時執行自更新**

```
App 下載新 BL image（IMAGE_TYPE=BOOTLOADER）→ 存入 NOR "BL staging" 區 → 簽章驗證
→ journal 記 BL_UPDATE_PENDING → reset
→ 舊 Bootloader 開機看到旗標（此時 App 未運行，AXI SRAM 可用）：
    1. 驗證 staging 內的 BL image（簽章、BOARD_ID、版本單調）
    2. 將 flasher stub 與新 BL 載入 RAM（≤128KB）
    3. 從 RAM 執行 stub：擦除 → 寫入 → 回讀驗證 → reset
→ 失敗/斷電：靠 ROM bootloader（BOOT0 + UART/USB DFU）硬體救援
```

**方案 B（若未來需要真正免變磚）：兩階段 Bootloader**
Stage-0（內部 Flash，極小，永不更新，只做 NOR 讀取＋驗簽＋載入）→ Stage-1（完整 Bootloader，存 NOR A/B slot，載入 RAM 常駐）。代價：服務庫常駐 RAM（50–85KB，與 450KB App 競爭 AXI SRAM；可考慮 ITCM/SRAM4）、開機時間增加、ABI 位址搬到 RAM。**若 Stage-1 的常駐 RAM 無法與 App 預算並存，此方案不可行。** 此為 ADR 待寫，v1 不採用。

### 9.3 現在就要預留的 hook（成本低、事後補不回來）

- [ ] `.bl_info` 區段（版本、大小、hash、flags），固定位址 0x08000400
- [ ] Image header/TLV 的 `IMAGE_TYPE`、`BOARD_ID`、`MIN_BL_VERSION`；v1 遇到非 APP 一律拒絕
- [ ] NOR 佈局保留 BL staging 區（256KB）
- [ ] Journal `kind` 預留 `BL_UPDATE_*`；未知 kind 忽略且保留
- [ ] TAMP `REQUEST_BL_UPDATE` 命令碼保留
- [ ] 公鑰表 3 槽＋ key id＋ journal 中的 `key_revoke_mask`（為金鑰輪替預留；Bootloader 本身無法更新時，這是唯一的緩衝）
- [ ] ABI 表 `reserved` 空位、`IoOps.reserved`
- [ ] 開發期**不啟用** WRP（寫保護）與 RDP L2，這兩者會阻止自更新
- [ ] **device key 的 KDF 輸入（`global_secret`、info 字串 `dev-enc-v1`、UID、NOR seed）不得隨 Bootloader 版本改變**，否則自更新後所有 slot 變成無法解密；新版 Bootloader 必須內含相同的 `.secrets`，flasher 不得遺失它（§16）
- [ ] 硬體檢查（T0.4）：BOOT0 可接觸、UART/USB 腳位可連接 ROM bootloader
- [ ] **T0.5 做一次 ROM bootloader 救援演練**，確認「把內部 Flash 清空後能救回來」

### 9.4 未來任務（Phase F，Bootloader 自更新啟用條件：U-05 CLOSED 且 T0.5 通過）

- [ ] F1：BL image 驗證路徑（IMAGE_TYPE=BOOTLOADER，簽章鏈、版本單調、BOARD_ID）
- [ ] F2：Flasher stub（位置無關、純 RAM、僅操作 FLASH 暫存器，單獨 crate `boot-flasher`）
- [ ] F3：Journal `BL_UPDATE_*` 記錄與斷電恢復策略（stub 執行前後的狀態）
- [ ] F4：硬體斷電演練（在擦除/程式化期間隨機斷電，統計救援成功率）
- [ ] F5：量產 RDP 政策重新評估

---

## 10. 儲存庫結構與工程規範

### 10.1 目錄

```
bootloader/
├─ rust-toolchain.toml           # 固定 toolchain
├─ .cargo/config.toml
├─ crates/
│  ├─ boot-abi/        # repr(C) 型別、錯誤碼、函式表（兩端共用）
│  ├─ boot-image/      # header/TLV 解析、驗證規則（no_std，host 可測）
│  ├─ boot-crypto/     # SHA-256 + 簽章後端（feature 切換）
│  ├─ boot-nor/        # SPI NOR 邏輯，底層由 trait/IoOps 抽象
│  ├─ boot-journal/    # journal 邏輯（host 可做斷電注入）
│  ├─ boot-core/       # slot 決策 decide()、載入流程（硬體無關）
│  ├─ boot-update/     # 更新協議核心（Bootloader 內，transport 無關）
│  ├─ boot-api-client/ # App 端 safe 封裝
│  ├─ boot-stm32h7/    # 板級：時脈、SPI、RAM loader、jump、cache、MPU、TAMP、IWDG
│  └─ boot-flasher/    # （Phase F 預留，v1 不建立內容）
├─ bootloader-h750/    # 可執行 crate（memory.x、link.x、main）
├─ app-template/       # App 範本：linker、Boot API 呼叫示範、blinky
├─ tools/
│  ├─ xtask/           # sign、size-check、flash-nor 等自動化
│  └─ host-update/     # 主機端更新工具（依 U-04 結果）
├─ fuzz/               # cargo-fuzz 目標
├─ tests/              # host sim、HIL 腳本（pytest）
└─ docs/
   ├─ plan.md          # 本文件
   ├─ experiments.md
   └─ adr/
```

### 10.2 技術選型（預設）

- HAL：`stm32-metapac` 直接操作暫存器為主（最小 size）；必要時 `embassy-stm32` blocking 模式。Bootloader 不用 async。
- 雜湊/簽章：`sha2`、`p256`（verify-only）或 `salty`/`ed25519-dalek`（T2.2 決定）。
- 解析：`zerocopy`；CBOR：`minicbor`（若採 SMP）。
- 日誌：`defmt` ＋ `probe-rs`，release 關閉。
- 測試：`proptest`、`cargo-fuzz`、host 端 `std` mock。

### 10.3 `unsafe` 政策（D-12）
僅允許出現在：`jump`、cache/MPU 操作、RAM 初始化與 Boot Info 寫入、ABI 邊界（`extern "C"` 與裸指標轉 slice）、flasher stub。其餘 crate `#![forbid(unsafe_code)]`。每個 `unsafe` 區塊必須有 `// SAFETY:` 註解。

### 10.4 CI 要求（每個 PR）
- `cargo fmt --check`、`cargo clippy -- -D warnings`
- host 測試（含 proptest 固定種子）
- thumbv7em-none-eabihf release 編譯＋ **size 檢查（超預算即失敗）**
- `cargo deny`、`cargo audit`
- 夜間：cargo-fuzz 每個目標至少 10 分鐘

---

## 11. 里程碑與任務

### M0：專案骨架與硬體確認

- [ ] **T0.1** 建立 workspace、`rust-toolchain.toml`、`--locked` 建置、CI 骨架
- [ ] **T0.2** Linker script：固定 `.bl_info`、`.boot_api`、`.keytable` 位址；SRAM4 配置；App 範本 linker（`0x24000400` 起）
- [ ] **T0.3** 板級 bring-up：時脈 400MHz/VOS1、UART log、LED、probe-rs 燒錄與除錯流程
- [ ] **T0.4** 硬體審查清單（結果回填 §3）：
  - NOR 的 SPI 實例與腳位（型號已定為 BY25Q32ES，U-01）；/WP、/HOLD 接法（QE=0 時這兩腳為 WP/HOLD 功能；H750 以標準 SPI 連接，QE 必須維持 0）；外部 RAM（U-02）
  - BOOT0 可接觸、ROM bootloader 用的 UART/USB 腳位（U-05）
  - PA4（NOR /CS）外部上拉、/WP 與 /HOLD 接法、KEY2（PC13）與 WKUP（PA0）的極性與上下拉、LED（PC3）極性、VBAT 是否接（影響 TAMP）、IWDG 可用性
  - 以原理圖核對 §4.4 的腳位表（腳位已提供，仍須對照原理圖）
- [ ] **T0.5** **ROM bootloader 救援演練**：故意清除內部 Flash，經 BOOT0 分別以 USART1（PA9/PA10）與 USB FS DFU（PA11/PA12）各救回一次，寫成步驟文件；同時確認 ROM bootloader 支援的介面與腳位（對照 ST AN2606）；演練前先讀出並記錄 option bytes（BOOT_ADD0／BOOT_ADD1、RDP、WRP），確認 BOOT0=0 時從 0x08000000 啟動；演練須包含「RDP1 → 回退 RDP0（整顆擦除）→ 重新燒錄」的流程
- [ ] **T0.6** 建立板級設定模組（`boot-stm32h7` 內的 `board` 模組）：腳位常數、按鍵／LED 極性、時鐘與電壓參數、SRAM 時鐘啟用清單；所有硬體相關常數只出現在此模組

**DoD**：Bootloader 空殼可啟動並輸出 log；CI 綠；§3 的 U-01/02/05/11 已更新；救援演練成功並有紀錄。

### M1：SPI NOR 驅動與無驗證載入

- [ ] **T1.1** `boot-nor`：JEDEC/SFDP、fast read、page program（頁邊界）、4KB/64KB erase、WIP 輪詢＋逾時、開機序列（先等 WIP=0 並處理 SUS，再 0x66/0x99 並等待 ≥0.5ms）、開機時檢查 SR1 的 BP 位元與 SR2 的 CMP 為 0（否則寫入會被晶片靜默忽略）以及 QE=0；BY25Q32ES 為 3-byte 位址，不需 4-byte 模式
- [ ] **T1.2** Host mock `RamNor`：模擬「只能 1→0、erase 才能 0→1」，支援每次 program/erase 注入失敗與斷電
- [ ] **T1.3** 開發用 NOR 燒錄方式（dev feature 的 UART 指令或 `tools/xtask flash-nor`）
- [ ] **T1.4** 實驗（結果寫 `docs/experiments.md`，回填 §3）：
  - **E1**：DTCM 取指令可行性與效能（U-06）
  - **E2**：RAM ECC 行為：未初始化讀取、對齊寫入初始化耗時（U-16）
  - **E3**：450KB 載入時間（SPI 頻率 vs 吞吐，CPU copy）
  - **E4**：SPI→AXI SRAM DMA 可行性與加速比（僅評估，不納入 v1）
  - **E5**：NOR 實測 sector／block erase 與 page program 時間（datasheet 最壞值：BY25Q32ES 4KB erase 300ms、64KB erase 2s、page program 2.4ms），據此決定 IWDG 逾時與 `yield_` 呼叫密度；並觀察被中斷的 erase 之後再讀取的行為
- [ ] **T1.5** RAM loader＋§6.1 跳轉清單完整實作
- [ ] **T1.6** `app-template` blinky 以 `0x24000400` 向量表從 NOR 載入執行

**DoD**：Blinky 由 NOR 載入並執行；E1–E4 有數據；載入 450KB 時間已記錄。

### M2：Image 格式與驗證

- [ ] **T2.1** `boot-image`：header/TLV parser（§5.3 規則），用 imgtool 產出的測試向量驗證
- [ ] **T2.2** 簽章 spike：P-256 vs Ed25519 的 size 與時間（400MHz）→ 寫 ADR，關閉 U-03
- [ ] **T2.3** 串流 SHA-256＋ RAM 副本驗證流程
- [ ] **T2.4** 公鑰表（3 槽、key id、`key_revoke_mask` 讀取）
- [ ] **T2.5** `xtask sign`：包裝 imgtool（`--header-size 0x400`、RAM load、`--align`、自訂 TLV），開發金鑰與量產金鑰分離
- [ ] **T2.6** Fuzz：header/TLV parser，無 panic、無越界
- [ ] **T2.7** 負向測試：竄改 header／image／TLV 任一 byte、錯誤金鑰、截斷、超大、`load_addr` 越界或與 Bootloader RAM 重疊、BOARD_ID 不符、`IMAGE_TYPE≠APP`

- [ ] **T2.8** `boot-crypto` 加入 XChaCha20、HKDF-SHA256（RustCrypto `chacha20`、`hkdf`），附官方 KAT 測試向量；量測 size 與 450KB 解密時間（併入 T2.2 的 size 預算評估）
- [ ] **T2.9** `xtask encrypt`：先簽章（明文），再以 project key 加密 image 本體並寫入 `ENC_INFO`；`ENCRYPTED` 旗標須在簽章前設定（imgtool 是否支援需確認，否則自行實作簽章 wrapper）

**DoD**：負向測試全拒絕（含未加密 image、`ENC_INFO.scheme` 不符、被篡改的 nonce）；Bootloader size 已量測並對照 §4.1 預算；簽章、雜湊、解密時間已量測。

### M3：Slot 狀態機與 journal

- [ ] **T3.1** `boot-journal`（§7.1）＋ host 斷電注入（**每一個 program/erase 操作點**都測）
- [ ] **T3.2** `decide()` 純函式＋ proptest 驗證 §7.2 不變量
- [ ] **T3.3** 整合 test/confirm/revert、IWDG（Trial 期間）、TAMP 握手、Boot Info
- [ ] **T3.4** 救援模式入口（雙 slot 皆失效、TAMP 要求，或開機時按住 KEY2）；LED（PC3）以閃爍碼表示狀態（正常啟動、救援、驗證失敗）
- [ ] **T3.5** HIL 腳本：自動重複更新流程並於隨機時間斷電（可用可控電源/繼電器，選配）

**DoD**：斷電注入全數通過；不變量測試通過；test→confirm 與 test→revert 在板上各跑通。

### M4：Boot Services ABI

- [ ] **T4.1** `boot-abi`、`.boot_api` 固定位址、`.bl_info`
- [ ] **T4.2** `boot-api-client`：safe wrapper，檢查 `magic`/`table_len`
- [ ] **T4.3** `IoOps` 與 §8.3 強制規則實作（slot 範圍、active slot 保護、指標長度檢查）
- [ ] **T4.4** 實作 `get_boot_info`、`slot_status`、`slot_erase`、`slot_write`、`verify_slot`、`mark_pending`、`confirm`、`request_rescue`、`system_reset`
- [ ] **T4.5** 量測各函式最壞 stack（`-Z emit-stack-sizes`／`cargo-call-stack`），填入 `min_stack` 與文件
- [ ] **T4.6** App 於 RAM 執行時呼叫 Flash 服務的 HIL 測試（驗證 linker 位址、cache、MPU、stack）
- [ ] **T4.7** ABI 凍結檢查清單草稿（此時不凍結）
- [ ] **T4.8** `SecureRegion` trait（讀 Unique ID、讀／寫一次／鎖定 Security Register，實作 BY25Q32ES 與 W25Q32JV 兩種 `NorChip` 設定；§16.5 的鎖定流程需在實機驗證）、`derive_device_key()`（HKDF，info 字串版本化）、RNG 驅動（U-19）、Loader 串流解密；`update_abort` 與 ctx zeroize

**DoD**：App 能經 ABI 完整做一次「寫 slot B → 驗證 → mark_pending → reset → 啟動 → confirm」；嘗試寫 active slot 被拒。

### M5：更新協議與 UART

- [ ] **T5.1** 協議 spike：SMP 子集 vs 自訂 → ADR，關閉 U-04
- [ ] **T5.2** `boot-update` 核心（host 可測）：session 狀態機、offset-based 分塊、按需擦除（以 erase sector 為續傳粒度）、整體驗證、mark_pending
- [ ] **T5.3** UART framing、App 端 transport、主機端工具
- [ ] **T5.4** Bootloader UART 救援模式（沿用同一核心，只接受簽章合法的 image），關閉 U-15
- [ ] **T5.5** Fuzz：`update_feed` 輸入
- [ ] **T5.6** 吞吐量量測（鮑率 vs 450KB 傳輸時間）
- [ ] **T5.7** 加密 ingest 管線（§16.4）：解密 → 串流 hash → device key 加密 → 寫 NOR；host 測試以固定向量驗證往返一致；offset 單調規則與 nonce 不重複測試
- [ ] **T5.8** 佈建工具與流程（§16.5）：seed 產生／寫入／回讀／鎖定，`provision` feature，量產 Bootloader 的拒絕行為；維修流程文件
- [ ] **T5.9** HIL：將一塊板子的 NOR 內容（含 Security Register）複製到另一塊板子，確認無法開機且不洩漏明文

**DoD**：端到端 UART 更新含 revert；兩個 slot 皆損毀時可由救援模式復原；加密 ingest 端到端成功；跨板複製無法開機；fuzz 通過。

### M6：Ethernet（App 端 transport）
- [ ] **T6.1** smoltcp / embassy-net UDP 或 TCP，payload 直接餵 `update_feed`
- [ ] **T6.2** SRAM1-3 DMA buffer 與 MPU non-cacheable 設定
- [ ] **T6.3** 斷線續傳測試
- [ ] **T6.4**（選配）SD 卡 transport（SDMMC1，檔案系統在 App 端，經 ABI `slot_write` 寫入 NOR；SDMMC IDMA 緩衝放 AXI SRAM 或 RAM_D2）

**DoD**：網路更新端到端成功，斷線後可從 sector 邊界續傳。

### M7：CAN（FDCAN）
- [ ] **T7.1** FDCAN1（PD0/PD1，TJA1042）＋ISO-TP transport（CAN-FD 64B）
- [ ] **T7.2** （選配）Bootloader CAN 救援（與 UART 救援二選一 feature）

**DoD**：CAN 更新端到端成功，傳輸時間已記錄並告知產品端。

### M8：加固與 v1 凍結
- [ ] **T8.1** Bootloader 自身 MPU 設定、IWDG 策略、關鍵分支 double-check、簽章比對常數時間
- [ ] **T8.2** Size 優化與預算檢查，必要時更換密碼學後端
- [ ] **T8.3** Fuzz 納入 CI，長時間 fuzz 無 crash
- [ ] **T8.4** ABI v1 凍結檢查與 `abi_version` 定案
- [ ] **T8.5** 簽章金鑰管理流程文件（開發/量產分離、備份、輪替程序）
- [ ] **T8.6** 量產政策：RDP 等級（U-09）、SWD 關閉時機、NOR 預燒流程
- [ ] **T8.7** Release 流程：可重現建置、版本號、hash 發佈
- [ ] **T8.8** zeroize 稽核：檢查組合語言／記憶體傾印，確認金鑰不殘留在 stack、ctx、SRAM4
- [ ] **T8.9** 降級與混用測試：未加密 image、dev 金鑰簽章／加密的 image、舊版明文 image 於量產 Bootloader 一律拒絕；`min_version` 在加密版本切換時提高
- [ ] **T8.10** project key 輪替演練（切換到第二槽）；撤銷舊槽測試

**DoD**：所有 fuzz/斷電/負向測試綠；ABI 凍結；文件完整；Phase F 的 §9.3 hook 逐項勾選完畢。

---

## 12. 測試策略

| 層級 | 內容 | 工具 |
|---|---|---|
| 單元 | parser、journal、`decide()`、NOR 邏輯 | `cargo test`、`proptest` |
| 斷電注入 | 對每個 program/erase 呼叫在其前、中、後中斷，驗證重啟後狀態合法 | host `RamNor` fault injection（仿 MCUboot sim） |
| Fuzz | header/TLV、`update_feed`、journal 讀取 | `cargo-fuzz` |
| HIL | ABI 呼叫、完整更新、revert、救援 | pytest ＋ probe-rs ＋ 序列埠，選配可控電源 |
| 負向 | §M2 T2.7 清單 | 自動化腳本 |
| 加密 | KAT 向量、ingest 往返（傳輸密文 → NOR 密文 → RAM 明文 hash 一致）、跨裝置複製失敗、nonce 不重複 | host 測試＋HIL |
| Size/Stack | 每個 PR 檢查 | `xtask size-check`、`cargo-bloat`、stack-sizes |

---

## 13. 安全檢查清單

- [ ] 每次開機都完整驗證（hash＋簽章），不快取「已驗證」旗標
- [ ] 驗證對象是 RAM 副本
- [ ] 驗證失敗清零載入的 RAM 區
- [ ] 公鑰在 Flash 固定區，多槽＋ key id
- [ ] 簽章/雜湊比對常數時間；關鍵判斷 double-check
- [ ] Anti-rollback 強度與限制已文件化（U-13）
- [ ] 所有來自 NOR/App/通道的長度欄位均經過邊界檢查
- [ ] 量產：RDP、SWD、金鑰分離（U-09）
- [ ] 量產 Bootloader 啟用 `require-encrypted`，並關閉 `provision` 以外的佈建入口
- [ ] 同一 (device key, nonce) 不會對同一 offset 寫入不同明文
- [ ] 所有金鑰路徑皆 zeroize（§16.8），log/Boot Info/journal 不含金鑰

---

## 14. 風險登錄

| ID | 風險 | 機率 | 影響 | 對策 |
|---|---|---|---|---|
| R-01 | Bootloader 超過 128KB 預算 | 中 | 高 | M2 即量測；簽章後端可切換；功能不因有空間就加 |
| R-02 | RAM ECC / cache 一致性造成難以重現的錯誤 | 中 | 高 | E2；載入前整塊對齊初始化；跳轉清單嚴格執行 |
| R-03 | 450KB 之後 App 成長超出 512KB | 高 | 高 | 產品端確認 U-07；優化、拆資料到 NOR、必要時第二 segment |
| R-04 | ABI 一旦出貨無法修改 | 高 | 高 | 只增不改、reserved、M8 凍結檢查、client fallback |
| R-05 | Bootloader 自更新期間斷電變磚 | 低（未啟用） | 極高 | 本階段不實作；ROM 救援演練；啟用前走 Phase F 條件 |
| R-06 | 服務在 App 環境下 stack/MPU/時序衝突 | 中 | 中 | M4 HIL；`min_stack` 文件；MPU 需求寫入 ABI 文件 |
| R-07 | NOR 在 MCU reset 時仍處於 erase 中 | 中 | 中 | 開機 WIP 等待與逾時處理；T1.1 覆蓋 |
| R-08 | 初次 NOR 燒錄與量產流程未定 | 中 | 中 | U-15；M5 完成救援燒錄 |
| R-09 | 簽章函式庫執行時間過長影響開機 | 低 | 中 | T2.2 量測；必要時改演算法 |
| R-10 | 更新時 App 長時間阻塞導致 IWDG 重啟 | 中 | 中 | `IoOps.yield_`；API 文件要求；分塊處理 |
| R-11 | NOR OTP 鎖定不可逆，佈建失誤造成 NOR 報廢 | 中 | 中 | 佈建指令先回讀驗證再鎖；已鎖定絕不重產生；開發板不鎖 |
| R-12 | 更換 MCU 或 NOR 後 device key 改變，slot 無法解密 | 中 | 中 | 維修流程：救援模式重新 ingest；換 NOR 需重新佈建 |
| R-13 | 同 key+nonce 重複使用造成 keystream 外洩 | 低 | 高 | 每 session 隨機 24B nonce、offset 強制單調、重送僅限相同內容 |
| R-14 | project key 外洩（Bootloader 被 dump） | 中 | 中 | 只洩漏機密性不洩漏簽章；2 個 key 槽可輪替；RDP1 |
| R-15 | NOR 換料（BY↔Winbond）或 BY25Q32ES／舊款 BS 的規格差異（Security Register 大小、UID 長度）造成驅動誤判 | 中 | 中 | v1 只用 SR#1 前 256B；`NorChip` 設定表明確指定；KDF 對 UID 長度與 JEDEC ID 做編碼；換料需重跑 T4.8、T5.9 |

---

## 15. 實作者工作約定

1. 先讀 §2、§3、§9.3；動手前確認任務的前置 DoD 已達成。
2. 遇到本文件未涵蓋的選擇：先寫 ADR，再實作，不要在程式裡默默決定。
3. 每個任務完成時更新勾選狀態與 §3；實驗結果寫入 `docs/experiments.md`。
4. 不為了「方便」放寬 §8.3 的強制規則或 §5.3 的 parser 規則。
5. 不在 Bootloader 內加入未列於本文件的功能；新增功能需先評估 size 與驗證成本。
6. 每個 PR 需附 size 變化（byte 數）與測試證據。
7. 若硬體實測結果推翻本文件的假設（如 E1/E2），立即更新 §2/§3 並通知相關任務。

---

## 16. 映像加密與裝置綁定

### 16.1 目標與界線

目標：(1) OTA 檔案與 NOR 內容無法直接讀出；(2) 把 NOR 複製到另一塊裝置無法開機。威脅等級定位為「應付一般仿製與竊聽」，不是抗實體攻擊。

| 情境 | 能否擋住 | 條件／說明 |
|---|---|---|
| 竊聽 OTA 傳輸、取得 OTA 檔案 | 能 | project key 未外洩 |
| 單獨 dump NOR（含 Security Register） | 能 | 攻擊者沒有 `global_secret` |
| 複製 NOR 到另一塊板子或另一顆 NOR | 能 | KDF 含 MCU UID 與 NOR Unique ID，換任一顆都派生出不同 key |
| 取得 Bootloader（RDP0）＋ NOR | 不能（該裝置） | 所有 KDF 輸入皆可得；RDP1 起才有意義 |
| 持有設備並接上 debugger 取明文 | 不能 | 明文必然在 RAM；需要 RDP2，與 Bootloader 自更新／救援互斥，且仍不是硬體信任根 |
| 單一裝置被攻破 | 韌體明文外洩 | 所有裝置明文相同，加密保護不了這點 |
| App 被入侵（遠端執行程式碼） | 可讀出 flash 內的 secret | 無 TrustZone，App 為特權模式 |
| 以 SPI 設備模擬 NOR 並偽造 UID | 不能 | 需要硬體攻擊，列為已知限制 |

### 16.2 金鑰與資料流

```
OTA 封包(project key 加密) ─▶ Bootloader ingest ─▶ NOR(device key 加密) ─▶ 開機解密 ─▶ RAM 明文 ─▶ 驗簽 ─▶ 執行
```

- **Transit key** = HKDF-SHA256(ikm = `project_key[key_id]`, salt = `build_nonce`(24B，每次建置隨機), info = `"ota-v1"`)；XChaCha20 nonce = `build_nonce`。
- **Device key** = HKDF-SHA256(ikm = `global_secret ‖ nor_otp_seed`(32B)，salt = `mcu_uid`(12B) ‖ `jedec_id`(3B) ‖ `nor_uid_len`(1B) ‖ `nor_uid`(8B 或 16B)，info = `"dev-enc-v1"`)。
- **Storage nonce**：每個 ingest session 以 H750 RNG 產生 24B，寫入 NOR 的 unprotected TLV `ENC_INFO`。
- **Block counter** = 檔案內 offset / 64，可隨機存取、分塊寫入；chunk 長度須為 64B 的倍數（最後一塊除外）。
- **加密範圍**：image 本體；header、protected TLV、TLV 區保持明文，Bootloader 才能在解密前解析。
- **簽章簽在明文**。不使用 AEAD：完整性由每次開機對 RAM 明文的簽章驗證保證。
- 選 XChaCha20 的理由：無查表、天生常數時間、可 seek、192-bit nonce 隨機不會碰撞；預估 450KB 解密約 10~20ms（需 T2.8 實測）。
- 加密方案放在 unprotected `ENC_INFO.scheme`，不放 header，因為同一份簽章要同時適用於「傳輸中」與「NOR 中」兩種密文。

### 16.3 NOR Security Register 與 Unique ID

**原則：這些區域任何人用 SPI 都讀得到**，鎖定只防止被改寫。它的角色是「每顆 NOR 獨一無二、一次寫入的 seed」，不是機密儲存。device key 的機密性來自 `global_secret`（在 Bootloader，RDP 保護）；因此 device key 必須同時混入 `global_secret`、NOR seed、MCU UID、NOR UID 與 JEDEC ID，缺一不可。

**晶片差異（已查 datasheet；Winbond 的時序數字尚未查證）**

| 項目 | Boya BY25Q32ES（Rev 2.3） | Winbond W25Q32JV |
|---|---|---|
| 容量／JEDEC ID | 4MB／`68 40 16` | 4MB／`EF 40 16` |
| Security Register | 3 × **1024B** | 3 × **256B** |
| Unique ID | **128-bit**（4Bh＋4 dummy bytes） | **64-bit**（4Bh＋4 dummy bytes） |
| 讀／擦／寫指令 | 48h／44h／42h，各需 3-byte 位址 | 同 |
| Register 位址 | #1 `0x001000`、#2 `0x002000`、#3 `0x003000`，加 byte offset | 同 |
| 程式化粒度 | 單次最多 256B（1024B 需分 4 次） | 256B |
| 鎖定位元 | SR2 的 LB1/LB2/LB3（S11/S12/S13，即 SR2 bit3/4/5），OTP，逐個鎖 | 同位置，OTP；datasheet 標示 LB 可為 volatile 或 non-volatile OTP 寫入 |
| 4KB／64KB erase（typ／max） | 35ms／300ms；0.18s／2s | 待 T0.4 補查 |
| Page program（typ／max） | 0.45ms／2.4ms | 待 T0.4 補查 |
| Fast Read 時脈 | ≤120MHz（3.0–3.6V） | ≤133MHz |

**對設計的限制**
- BY25Q32ES 與舊款 BY25Q32BS（3×256B）的 JEDEC ID 可能相同，無法靠 ID 區分。因此 v1 **只使用 Security Register #1 的前 256B**，三種情況都相容。
- `NorChip` 設定表明確指定每種晶片的參數，**不用 SFDP 猜測** Security Register 大小。
- UID 長度因晶片而異（8 或 16B），KDF 的 salt 以「JEDEC ID ＋ 長度前綴 ＋ UID」編碼，避免換料後產生歧義。
- 鎖定 LB 位元一律使用 Write Enable（06h）的**非揮發**寫入，不可用 50h；寫入後必須回讀驗證，並在軟體重置或重新上電後再驗證一次。
- 寫 SR2 一律用 31h，並保留其他位元（尤其 QE 必須維持 0）。

**Security Register 分配**
- SR#1 offset 0x00–0x3F：`SeedRecord`（64B）＝ magic、`fmt_ver`、`seed[32]`、保留、`crc32`，其餘保持 0xFF。佈建完成後鎖 LB1。
- SR#2、SR#3：保留（序號、產線資料或未來用途），v1 不使用也不鎖定。
- 不依賴內部 Flash 的 per-device secret：單一 sector，Bootloader 自更新會擦除它（U-21）。

### 16.4 OTA Ingest 流程（Bootloader 的 `boot-update` 內）

```
session 開始：
  派生 device key；RNG 產生 storage nonce；選定非執行中 slot；sector-on-demand 擦除
收到 chunk(offset, ciphertext_p)：
  ① 檢查 offset == 期望值，或為已確認 chunk 的「相同內容」重送
  ② XChaCha20(transit key, build_nonce, counter=offset/64) 解密 → plaintext
  ③ header 區：解析並檢查（§5.3、BOARD_ID、IMAGE_TYPE、ENCRYPTED 旗標）
  ④ plaintext 餵入串流 SHA-256（順序：header、image、protected TLV）
  ⑤ XChaCha20(device key, storage nonce, counter=offset/64) 加密 → 寫入 NOR
  ⑥ plaintext buffer zeroize
結束：
  ⑦ 對已算出的 hash 驗簽（不需保留明文）；寫入 TLV，ENC_INFO 改為 scheme=DEVICE、nonce=storage nonce
  ⑧ 重新讀 NOR → 解密 → hash（verify_slot，不留明文）
  ⑨ mark_pending；zeroize ctx；reset
```

- **跨 reset 續傳 v1 不支援**（需把 nonce 存入 journal，列為選配）；同一 session 內可重送。新 session 一律新 nonce 並重新擦除，不會有 keystream 重複。
- 嚴禁在同一個 (device key, nonce) 下對同 offset 寫入不同明文，由步驟 ① 強制。
- 開機路徑與此對稱：NOR 密文 → device key 解密 → RAM 明文 → 對 RAM 明文驗簽（D-02 不變）。

### 16.5 佈建（Provisioning）

1. 產線使用 Bootloader 的佈建指令（`provision` feature），對 Security Register #1 依序執行：
   1. 讀 SR2（35h）確認 LB1 = 0；讀 SR#1 前 256B，全為 0xFF 才繼續。若已有有效 `SeedRecord` → 結束，不得重寫；若有內容但無效 → 回報錯誤，**不自動擦除**，交人工判斷。
   2. RNG 產生 32B seed（需通過健康測試，U-19），組成 64B `SeedRecord`。
   3. Write Enable（06h）→ Program Security Registers（42h，位址 `0x001000`，一次 64B）→ 等 WIP 清除。
   4. 以 48h 回讀並比對。
   5. 鎖定：讀 SR2，把 LB1（bit3）置 1 並保留其他位元 → Write Enable（06h，**不可用 50h**）→ Write Status Register-2（31h）→ 等 WIP 清除。
   6. 再讀 SR2 確認 LB1 = 1；對 SR#1 試寫一次（應被晶片忽略），回讀確認內容未變。
   7. 軟體重置或重新上電後，再讀 SR2 與 `SeedRecord` 確認持久。
2. 若已鎖定且有資料，**絕不重新產生**（否則既有加密 image 全部作廢）。
3. 佈建完成後，經救援通道 ingest 第一版 App（project key 加密的封包），與日後 OTA 走同一條路徑。
4. 開發板使用 dev 金鑰與 dev Bootloader（RDP0），seed **不鎖定**。
5. 記錄序號與佈建結果（不含任何 secret）。

維修流程：更換 MCU 或 NOR 後 device key 改變，原 slot 無法解密、開機失敗，進入救援模式重新 ingest；若更換的是 NOR，需先重新佈建 seed。

### 16.6 版本切換與降級防護

- `ENCRYPTED` 旗標在簽章範圍內，量產 Bootloader（`require-encrypted`）拒絕未加密 image。
- 第一個加密版本發布時把 `min_version` 提高到該版本，避免以舊的、簽章仍合法的明文 image 降級載入。
- Dev 與 prod 的簽章金鑰、project key、`key_id` 完全分離，prod Bootloader 的 keytable 與 `.secrets` 不含 dev 金鑰。

### 16.7 project key 輪替與外洩處置

- `.secrets` 預留 2 個 project key 槽，TLV 以 `key_id` 指定，新建置改用另一槽即可。
- project key 外洩：攻擊者可解密 OTA 封包，**但無法偽造**（簽章金鑰獨立）。處置為新封包改用另一槽並撤銷舊槽（`key_revoke_mask` 已在 journal 預留）。
- Bootloader 無法輕易更新，備用槽必須在出貨前就放好；兩槽用完需 Bootloader 更新（Phase F）。

### 16.8 記憶體衛生規則

1. 所有金鑰與中間值使用 `zeroize` 型別，離開作用域前清除。
2. 金鑰只存在於服務執行期間的 stack／ctx；session 成功、失敗、`update_abort` 都必須 zeroize ctx。
3. 跳轉前清除 SRAM4 內 Bootloader 使用過的區域（§6.1）。
4. 不在 log、Boot Info、journal 輸出任何金鑰或 seed。
5. 簽章與雜湊比對使用常數時間。

### 16.9 相關任務
T2.8、T2.9（M2）、T4.8（M4）、T5.7、T5.8、T5.9（M5）、T8.8、T8.9、T8.10（M8）。選配：跨 reset 續傳（需在 journal 增加 SESSION 記錄並調整 record 大小）。

---

## 附錄 A：術語

| 術語 | 說明 |
|---|---|
| RAM_LOAD | MCUboot 模式：image 複製到 RAM 後驗證與執行 |
| TLV | Type-Length-Value，image 尾端的附加資料區（hash、簽章等） |
| Trial / Test boot | 新 image 試開機，App 需 `confirm()` 否則 revert |
| Journal | NOR 上的 append-only 狀態記錄（ping-pong 兩個 sector） |
| ROM bootloader | ST 出廠於 System Memory 的 bootloader（AN2606），可經 BOOT0 進入 |
| TAMP | 備份暫存器模組，reset 後內容保留 |
| ADR | Architecture Decision Record，決策紀錄 |
