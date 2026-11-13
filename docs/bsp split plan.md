# FerroBoot：BSP 分離與多平台（RAM_LOAD / XIP）擴展規劃

> 狀態：**草案 v0.8，供討論**（尚未有任何程式碼變更）
> 基準：上傳的 repo，HEAD = `10ba7bc fix(stm32h7): disable PLL1 before jumping to app`
> 範圍：把 `H750 NetLite（RAM_LOAD）` 與 `NUCLEO-F429ZI（內部 Flash XIP）` 放進同一個架構；`Forlinx OK1061-S（i.MX RT1061，FlexSPI NOR XIP）` 先在架構上預留、**後做**
> 標註約定：**[已讀碼]** 來自原始碼；**[已查]** 本版以網路資料交叉查證過（來源見附錄 A，仍建議最後對照原廠手冊）；**[假設]** 我的推論；**[待查證]** 憑記憶，動工前必須對照手冊；**[TBD-n]** 你明確決定「拿到板子再定」的項目（§0.3）

### v0.8 變更紀錄

- **P0 patch A 已完成並驗證**
- §1.4 更新：`local_millis` 已修；新增**兩項重要發現**：`init_sram4_ecc()` 會清零整個 SRAM4（含 Bootloader 自己的 `.bss`／`.data`／stack）、`overflow-checks` 下 `millis() - start` 會在計時器 wrap 時 panic。
- 新增 **R-16**（Bootloader 的 RAM 靜態變數必須在 `init_sram4_ecc()` 之後初始化），並註記它與 TBD-1（RAM 常駐例程）的關聯。

### v0.7 變更紀錄

- **P0 patch B（`boot-crc`）已完成並驗證** 實測 `text` 52,816 → 36,824 B（−15,992 B，−30%）（§7.2）。採用完整 256 項表（1KiB，編譯期算好、放在 flash，不佔 RAM）。
- 沙盒環境已建立 Rust 1.91 與 `arm-none-eabi-ld` 的交叉建置流程，之後的 patch 都能實際編譯、跑 host 測試與量 size（§12）。

### v0.6 變更紀錄

- **`nm` 已證實：`crc32fast::table::CRC32_TABLE` 恰為 `0x4000`（16,384 B）**，是 `.rodata` 的最大單一項目（§7.2）。`boot-crc` 取代方案由「推論」升為「已驗證的 P0 項目」。
- 把前一版以估計值推算的固定項目，改為 `-A` 的實測值（向量表 664 B、`.keytable` 1,340 B、固定項目小計 2,276 B、`.text`+`.rodata` = 49,232 B）；T64 餘裕相應微調（差 2 B，結論不變）。
- 補上 `nm` 的最大符號表；`load_and_verify` 單一函式 6,756 B，列為 P4 管線重構時的觀察點。

### v0.5 變更紀錄

- **G0 以縮小範圍通過**（§0.6）：H750 的「載入 App 執行」與「rescue 上傳」已驗證；其餘項目暫不測試。附錄 C 補上結果，並記錄由此帶來的基線限制（R-05）。
- **取得 Bootloader 的 size 組成分解**（`arm-none-eabi-size -A` 與 `cargo bloat`）（§7.2）：`.text` 31,452 B、**`.rodata` 17,780 B**。
- **重要發現：`.rodata` 很可能有約 16KiB 是 `crc32fast` 的查表**（推論，待 `nm` 驗證）。若成立，換成同演算法的小型實作可讓 T64 的餘裕從約 10KiB 增加到約 26KiB，**對 H750 本身也有好處**（§7.2）。
- P0 新增「以 `boot-crc` 取代 `crc32fast`」一步（§9）；R-10 風險下調（§11）。

### v0.4 變更紀錄

- **採納 v0.3 §12 的全部建議（O1～O3）**：G0 驗收清單（附錄 C）、T64 切換規則、`prod-enforce` 預設關（§0.5）。
- **納入 H750 Bootloader 的實測 size：`text = 51,508 B`、`data = 0`、`bss = 0`**，並以程式碼中的固定表格大小換算 T64 的實際餘裕（§7.2）：**初步結論是 T64 可行，但 F429 新增功能尚未量測**。
- 修正 v0.3 的一個說法：「≤ 56KB（餘裕約 8KB）」的餘裕是名目值；扣掉固定保留區後，**實際餘裕約 4.7KiB**（§7.2）。規則本身不變。
- §12 改為「下一步行動」；目前沒有開放問題。

### v0.3 變更紀錄

- 納入你對 v0.2 §12 的 7 項回答（§0.4）。
- **F429 佈局改為兩階段**（§7.2）：先以 128KB Bootloader + 128KB journal 為基準（B128）；目標是 **Bootloader ≤ 64KB、journal 64KB（T64）**，此時 slot 由 768KB 增加到 **896KB**，且兩個 bank 的佈局恰好對齊 sector 邊界。是否切換由實測 size 決定（TBD-5）。
- 首次試開機改為 **Quick**（安裝時才 Full）；Quick 的 CRC 範圍涵蓋 header + image + 全部 TLV（§5.3）。
- 傳輸加密在 F429 **開發 profile 預設關閉、量產 profile 才開**（§5.3）；同時也是縮小 Bootloader 的手段。
- **STM32 家族擴充保持最大彈性**：新增 `boot-stm32-common`、`boot-flash-stm32`（依 Flash IP 世代分模組）、`RemapKind` 設為 `#[non_exhaustive]`（§6.1、附錄 D）。
- 新增 **§5.7 量產保護檢查**（`xtask prod-check`、救援 `s` 指令、可選的 Bootloader feature）。
- 新增 **G0 關卡**：H750 測試完成才開始 F429 工作；驗收清單見附錄 C（§9）。

### v0.2 變更紀錄

- 納入你對 §12 的 12 項回答（§0.2）。
- **Q2（單一連結位址）使 v0.1 的 X1「各 slot 各自連結」淘汰**；改以**硬體位址重映射**實現單一位址：F429 用 bank swap、RT1061 用 FlexSPI remap，不需要軟體 swap 演算法（§5.2）。
- **XIP 不做軟體儲存加密**，改依賴 SoC 硬體機制（§5.3）。
- **XIP 預設 Quick 驗證**，Full 只在安裝與首次試開機等時機（§5.3）。
- **範圍收斂為先 STM32（H750、F429）**；RT1061 的實作移到最後，只保留低成本的擴充點（§9）。
- 「XIP 下由 RAM 執行 Flash 寫入」改為 **[TBD-1]**，F429 可能完全不需要（§5.1）。
- 新增：UART 救援需涵蓋整個 slot（§5.6）、各 crate 獨立版本（§6.6）。
- 已查到 OK1061-S 的規格（528MHz、1MB SRAM、4MB QSPI NOR、可選 16MB）與 FlexSPI remap 機制。

---
## 0. 摘要與決策紀錄

### 0.1 摘要

1. **BSP 分離不只是把 `boot-stm32h7` 搬走。** H750 的假設滲透到名義上「硬體無關」的 crate（`boot-abi` 的位址常數、`boot-image`、`boot-journal`、`boot-update`、`boot-core`、`xtask`）。第一步是把佈局資料化（`Layout`），否則加第二個平台就要複製整個 repo。（§1）
2. **平台拆成四個正交軸**：`SoC` × `Flash 後端` × `執行模型` × `板級`。H750 NetLite = `stm32h7 × SPI NOR × RamLoad × netlite`；F429 = `stm32f4 × 內部 Flash × XipRemap(BankSwap) × nucleo-f429zi`；RT1061 = `imxrt10xx × FlexSPI NOR × XipRemap(FlexSpiRemap) × ok1061-s`（後做）。（§3）
3. **「單一連結位址」可以由硬體重映射達成**：F429 的 `SYSCFG_MEMRMP` bank swap、RT1060 系列 FlexSPI 的 AHB remap。兩者都讓 Bootloader 驗證完某個 slot 後「把它映射到固定的執行位址」再跳進去，**沒有 swap 演算法、沒有額外 Flash 磨損、沒有 per-slot 建置**；journal 與 `decide()` 完全沿用。（§5.2）
4. **XIP 的保密改由 SoC 硬體負責**（F429：RDP/WRP；RT1061：BEE 等，待定）。軟體端只保留選配的傳輸加密。（§5.3）
5. **驗證分級**：安裝時與首次試開機做 Full（hash + 簽章），已確認的 image 在後續開機做 Quick（結構檢查 + CRC 對照 journal）。（§5.3）
6. **建議路線**：**G0（H750 測試完成）→ P0–P4** 讓 H750 在新結構下行為不變並建立 XIP 執行模型，**P5 上 F429**（先 B128 佈局，再依實測 size 決定是否切到 Bootloader ≤ 64KB 的 T64），P6 凍結 ABI；RT1061 在其後。G0 之後，F429 的四個 spike 可與 P1–P4 並行。（§9）

### 0.2 對 v0.1 §12 的回答與影響

| # | 你的決定 | 對文件的影響 |
|---|---|---|
| Q1 | 可以 break ABI | ABI v2 在 P1 直接進行，不需要 v1 相容路徑（§5.4） |
| Q2 | 希望單一位址，較單純 | **X1 淘汰**；採硬體重映射 `XipRemap`；軟體 swap（X2）降為「重映射不可用時的備援」，不排入計畫（§5.2、§13） |
| Q3 | 接受 XIP 不做軟體儲存加密，靠 SoC 硬體機制 | 移除 XIP 的 `DeviceBinding`；`ENC_SCHEME_DEVICE` 在 XIP profile 一律拒絕；RT1061 的 BEE 為 [TBD-2]（§5.3） |
| Q4 | 接受 XIP 不必每次 Full | 新增 `VerifyPolicy::{Full, Quick}`，XIP 預設 Quick（§5.3） |
| Q5 | 板子是 Forlinx OK1061-S | 已查規格並更新 RT1061 佈局草案（§7.3）；F429 仍為 NUCLEO-F429ZI |
| Q6 | 契約確定，但看實現複雜度 | 契約保留為預設；F429 優先嘗試「不用 RAM」的路徑（§5.1）；RT1061 屬 [TBD-1] |
| Q7 | 先 (a) 同 repo crate 邊界，之後 (b) | trait 設計成可升級為 out-of-tree，但現階段不承諾 semver 穩定（§6.6） |
| Q8 | 各 crate 可獨立發布 | 移除 `version.workspace`，每個 crate 自有版本 + 相容矩陣（§6.6） |
| Q9 | 先簡單做 | 單一 workspace + `xtask build --board`（§6.5） |
| Q10 | 按建議 | Bootloader 一律 PAC + 薄驅動；F429 用 `stm32-metapac`；RT1061 屆時用 `imxrt-ral` |
| Q11 | 都要保留基本 UART rescue，且涵蓋全 slot | `boot-rescue` 以 slot 相對偏移與整個 slot 的擦寫為基本要求（§5.6） |
| Q12 | 先 STM32，RT1061 之後 | 階段重排；RT1061 spike 延後到拿到板子（§9） |

### 0.3 明確「待拿到板子再定」的項目

| ID | 內容 | 目前的處理 |
|---|---|---|
| **TBD-1** | XIP 平台上「寫 Flash 的程式碼由誰、在哪裡執行」（RAM 常駐例程，或 F429 利用跨 bank RWW 直接在 Flash 執行） | 架構只預留擴充點（`Layout.ramfunc`、`BlInfo` 欄位、`CAP_*`、`Flash::PROGRAM_IN_RAM`），不寫死 |
| **TBD-2** | RT1061 的加密執行（BEE/OTFAD）與 HAB 是否使用、金鑰與區域怎麼配 | RT1061 階段決定；ingest 若需寫入密文，要符合該機制的格式 |
| **TBD-3** | RT1061 重映射採 overlay 還是 swap 形式、NOR 實際分區 | 見 §5.2、§7.3 草案 |
| **TBD-4** | F429 的 bank swap 是否如預期（reset 後狀態、ABI 位址、sector 編號），不如預期時是否退回軟體 swap | F429 spike S-F1 |
| **TBD-5** | F429 佈局採 B128 還是 T64（Bootloader ≤ 64KB、journal 64KB、slot 896KB） | 先 B128；**H750 總量已量測（text 51,508 B）**，P0 補組成分解，S-F4 估 F429 版，P5 以實測決定（§7.2） |

### 0.4 對 v0.2 §12（N1–N7）的回答與影響

| # | 你的決定 | 對文件的影響 |
|---|---|---|
| N1 | 目標 Bootloader ≤ 64KB、journal 64KB；先以 128KB 為基礎，之後依實際需要決定 | §7.2 兩階段佈局：**B128（起點）→ T64（目標）**；`Layout` 參數化使切換只是改常數；新增 size spike S-F4 與切換條件（TBD-5） |
| N2 | 首次試開機只做 Quick 可以 | §5.3 表格更新：安裝時 Full，首次試開機 Quick（缺 `KIND_VERIFIED` 才降級 Full） |
| N3 | 傳輸加密按建議 | F429 開發 profile 預設關；量產（RDP1）profile 才開；§5.3 |
| N4 | 保留最大可能性 | `boot-stm32-common` + `boot-flash-stm32`（分世代模組）；`RemapKind` `#[non_exhaustive]`；附錄 D 列出已知家族差異；**只在有板子時才實作變體**，其餘僅保證介面不被擋住 |
| N5 | 先把 H750 測完，再進行 F429 | 新增 **G0 關卡**（§9）與 **H750 驗收清單**（附錄 C）；F429 spike 與 P5 在 G0 之後 |
| N6 | Quick 的 CRC 範圍全包含 | header + image + protected TLV + TLV 全部，與 hash 範圍一致（§5.3） |
| N7 | 量產檢查要有選項 | 新增 §5.7：`Platform::protection_status()`、`xtask prod-check`、救援 `s` 指令、可選 feature `prod-flag`／`prod-enforce`（預設不強制） |

### 0.5 對 v0.3 §12（O1–O3）的回答與影響

你的回答是「全部按照目前的建議」，因此：

| # | 採納的建議 | 效果 |
|---|---|---|
| O1 | 附錄 C 的 H750 驗收清單就是「H750 測完」的定義；`update_*` 空殼等列為已知缺口，不在 G0 修 | G0 範圍確定；附錄 C 作為基線 |
| O2 | F429 量產 profile 的 Release Bootloader **`arm-none-eabi-size` 的 text ≤ 57,344 B（56KiB）** 才切 T64，否則留在 B128 | 切換規則確定；H750 現況數據見 §7.2 |
| O3 | `prod-enforce` 預設**關**；只做 `prod-flag` 與 `xtask prod-check`，量產 profile 才由你明確打開 | §5.7 預設值確定 |

### 0.6 G0 結果與 size 分解的影響

**G0（H750 驗收）結果（你的回報，2026-11-12）**：載入 App 並執行成功、rescue 上傳成功；其餘項目暫不測試（部分也不容易測試）。**決定：G0 以縮小範圍通過**，F429 工作可以開始。

這個決定的後果（已寫進 R-05 與附錄 C）：
- 板上可用的回歸基線只有兩項：**(1) Bootloader 載入並跳轉到 App；(2) UART rescue 上傳 image。** 每個重構階段結束後都以這兩項做 smoke test。
- test→confirm、IWDG revert、ABI 服務呼叫、負向測試、斷電等行為**沒有板上基線**；它們由 host 端測試（`RamNor` + 既有整合測試）覆蓋。因此 P0 要先把這些行為的 host 特性測試補齊並固定，之後的重構才有可比對的依據。
- 如果日後在板上發現這些未驗證行為的問題，**無法區分是重構造成還是原本就有**。這是接受縮小範圍的代價，不是阻礙。

**size 分解的主要結論**（細節見 §7.2）：
1. Bootloader 的 flash 用量 = `.text` 31,452 + `.rodata` 17,780 + 向量表／info／api／secrets／keytable 共 2,276 = 51,508 B。
2. **`.rodata` 佔 34.5%，比 `p256` 的程式碼（7.2KiB）還大**；`nm` 已證實其中 16,384 B 是 `crc32fast::table::CRC32_TABLE`。
3. 已證實，因此：以同演算法的小型 CRC 實作取代，可省約 15KiB，且 **CRC 結果完全相同，不影響既有 journal 與 NOR Security Register 內容**。


## 1. 現況盤點

### 1.1 現在的結構 [已讀碼]

```
boot-abi ─┬─ boot-image ─┐
          ├─ boot-nor ───┼─ boot-journal ─┐
          └─ boot-crypto ┘                ├─ boot-core ──┐
                                          └─ boot-update ┤
boot-stm32h7 (clock/GPIO/SPI1/USART1/TAMP/IWDG/jump) ────┴─ bootloader-h750 (main.rs 651 行)
boot-stm32h7 ──────────────────────────────────────────── app-template（App 也直接用它的 UART/SPI/LED）
tools/xtask (sign/encrypt/size-check)   tools/host-update   tools/rescue_upload.py   Makefile
```

### 1.2 H750 假設滲透清單 [已讀碼]

| # | 位置 | 假設了什麼 | 影響 | 建議處置 |
|---|---|---|---|---|
| 1 | `boot-abi`：`FLASH_BASE_ADDR`、`BL_INFO_ADDR`、`BOOT_API_ADDR=0x08000440`、`SECRETS_ADDR`、`KEYTABLE_ADDR` | 內部 Flash 在 `0x0800_0000`、BL 128KB | F429 同在 `0x0800_0000` 但佈局不同；RT1061 在 `0x6000_0000` | 移入 `Layout`；`boot-abi` 只留結構體/錯誤碼 |
| 2 | `boot-abi`：`AXI_SRAM_*`、`APP_LOAD_ADDR`、`APP_VTOR_*`、`SRAM4_*` | App 載入 AXI SRAM，BootInfo 放 SRAM4 | XIP 沒有 load 區；F429 沒有 SRAM4 | 移入 `ExecModel` 與 `Layout.boot_info` |
| 3 | `boot-abi`：`NOR_SLOT_A/B_OFFSET`、`NOR_SLOT_SIZE`、`NOR_JOURNAL_*`、`NOR_SECTOR_SIZE=4096`、`NOR_BLOCK_SIZE`、`NOR_PAGE_SIZE` | 均勻的 4KB/64KB erase、256B page | F429 sector 是 16/64/128KB **不均勻**；program 單位也不同 | `Flash` trait 暴露 erase unit 幾何；常數移入 `Layout` |
| 4 | `boot-abi`：`IoOps{spi_transfer, cs, millis, yield_}` | Flash 是 SPI NOR，由 App 提供 SPI | 內部 Flash 與 FlexSPI 不需要（也不能）由 App 提供 SPI | `spi_transfer`/`cs` 變可選；新增 caps（§5.4） |
| 5 | `boot-abi`：`TAMP_CMD_*` | 握手用 STM32 備份暫存器 | RT1061 沒有 TAMP | 命令碼保留在 ABI，儲存媒介由 `Handshake` trait 決定 |
| 6 | `boot-image`：`ih_load_addr != AXI_SRAM_BASE` 即拒絕、`DEFAULT_BOARD_ID = 'H750'`、`validate(NOR_SLOT_SIZE)` | RAM_LOAD only、單一板 | XIP 的 `ih_load_addr` 語意不同（§8） | `validate(&Layout)`；board id 由 Layout 提供 |
| 7 | `boot-journal`：`NOR_JOURNAL_SECTOR_0/1`、`NOR_SECTOR_SIZE` | 兩個 4KB sector | F429 最小 erase unit 是 16KB | 參數化 `JournalRegion{sector_a, sector_b, unit_size}` |
| 8 | `boot-update`：`BOOT_SLOT_B` 起始、`NOR_SLOT_*`、以 4KB 為擦除粒度 | 同上 | 同上 | 吃 `&Layout` + `Flash` |
| 9 | `boot-core::SlotLoader`：`F: NorFlash + SecureRegion`、固定「NOR → RAM → 解密 → 驗」 | 一定有 RAM 副本、一定有 NOR Security Register | XIP 沒有 RAM 副本；內部 Flash 沒有 Security Register | 管線拆段（§5.3）；`DeviceBinding` 變成選配 |
| 10 | `bootloader-h750/main.rs`：整個開機流程、**UART 救援協議**、ABI 的 12 個 `abi_*` 函式、金鑰表 static、`IoOps` 實作 | 全部寫在一個 651 行的 bin | 第二個板子只能複製貼上 | 流程進 `boot-runtime`、救援進 `boot-rescue`、ABI shim 進 `boot-runtime`，bin 只剩板級 glue |
| 11 | `boot-stm32h7`：同時服務 Bootloader 與 App（`app-template` 直接呼叫 `spi1_transfer`、`init_clocks` 等） | BSP 是「給兩邊用的一個 crate」 | 「Bootloader 專用」與「App 也要用」沒有分界 | 分成 bootloader-only（jump、RAM 初始化、救援）與 app-facing（`IoOps` 提供者、millis） |
| 12 | `.cargo/config.toml`：`probe-rs run --chip STM32H750VBTx`；`Makefile`：`CHIP`、`boot-h750` 目標名；`xtask`：`AXI_SRAM_BASE`、`DEFAULT_BOARD_ID`、固定測試金鑰 | 單一板 | 建置/燒錄/簽章都要知道「現在是哪塊板」 | 板子 manifest + `xtask --board`（§6.5、§8） |
| 13 | `Cargo.toml`：`stm32-metapac` 以 `stm32h750vb` 啟用 | 單一晶片 | **metapac 的晶片 feature 互斥**；F4 與 H7 放同一 workspace，`--workspace` 建置會因 feature 統一而衝突 | 見 §6.5 的建置策略 |

### 1.3 已經做對、可以直接沿用的部分 [已讀碼]

- `NorFlash` / `SecureRegion` 已是 trait；`SlotLoader`、`JournalManager` 對它們是 generic，所以 host 端用 `RamNor` 就能測。這是最大的資產。
- `decide()` 是純函式，已有 proptest 與整合測試。狀態機（Empty→Written→Pending→Testing→Confirmed／Invalid）與平台**完全無關**，不需要動。
- `IoOps` 回呼表已經把「硬體位元傳輸」與「Flash 命令邏輯」切開，這個思路對 H750 仍然正確，只是不能被當成所有平台的通用模型。
- `board.rs` 已經是「所有硬體常數集中」的雛形（T0.6），只是還停在 SoC crate 裡面。

### 1.4 順便發現的問題（不屬本案，但 BSP 化時一定會碰到）

- ✅ **`local_millis()` 永遠回傳 0 **：`bootloader-h750/src/main.rs` 的 `static mut SYSTEM_TICKS` 沒有任何地方遞增，Bootloader 階段沒有啟用 SysTick。後果比原本以為的更廣：(1) `wait_ready` 的逾時永遠不會觸發；(2) `software_reset()` 的「等 1 ms」迴圈 `while millis() - start < 1` 一旦被呼叫就**永遠不會結束**（目前 Bootloader 沒有呼叫它，所以沒有被觸發）；(3) release profile 開著 `overflow-checks`，`millis() - start` 在計時器 wrap 時會 **panic**。修法：新增 `boot-time`（可在 host 測試的 `MillisClock`）與 `boot-stm32h7` 的 `init_poll_millis()`／`poll_millis()`（DWT 週期計數器，**不使用中斷**，符合 D-08），`boot-nor` 改用 `wrapping_sub`。既有的 NOR 逾時（100／200／1000／3000 ms）都大於資料手冊最壞值（page 2.4 ms、4KB erase 300 ms、64KB erase 2000 ms），所以啟用真實時鐘不會在正常操作中引入新的逾時。
- ⚠️ **新發現：`init_sram4_ecc()` 會把整個 SRAM4（64KB）寫成 0**，而 Bootloader 自己的 `.bss`／`.data`／stack 都在 SRAM4（`memory.x` 的 `RAM` 起點 `0x38000100`）。目前能運作，是因為 `.data` 與 `.bss` 本來都是 0，且 `main` 在呼叫它時還沒有重要的 stack 內容。**任何新增的 RAM 靜態變數都必須在 `init_sram4_ecc()` 之後才初始化**（patch 0002 的 `init_poll_millis()` 就是這樣處理）。這也影響 TBD-1：若 F429／RT1061 將來需要把 Flash 例程複製到 RAM，複製必須排在對應的 RAM 初始化之後（R-16）。
- 觀察（**不修改，只記錄**）：`JournalManager::init` 在兩個 journal sector 的 header 都無效時，會**擦除 sector 0 並重新建立空的 journal**。也就是說，若唯一有效的 header 因位元錯誤而失效，狀態會被整個重置（兩個 slot 都變 Empty → 進入救援）。這是設計上的選擇（fail-safe 進救援），但要知道有這個行為；`power_cut_during_append…` 等特性測試涵蓋了 record 層級的容錯，header 層級未改變。
- `jump_to_app` 沒有實作 plan.md §6.1 的 cache clean/invalidate、`CONTROL` 清除；目前因為沒開 cache 所以無妨，但 RT1061（M7 + cache）會要求。這會被正式化為 `Platform::prepare_jump()`。
- README 已記載「UART 救援每個 slot 只擦第一個 64KB block」。救援協議目前以「64KB block」為單位，與 F429 的 128KB sector 不相容；救援協議應改為以「slot 相對偏移」表達、擦除粒度交給 `Flash`。
- 救援 `m` 指令寫入的版本號是寫死的 `1.0.0+1`。

---

## 2. 目標與非目標

### 目標
1. H750 NetLite 的**行為與現在完全一致**（重構階段不改功能；這是每個階段的驗收底線）。
2. 新增一個平台 = 新增一個 `boards/<name>/` + 必要時一個 SoC crate / Flash 後端，**不修改 `boot-core`、`boot-journal`、`boot-image`、`boot-update`**（理想；§9 DoD 以此衡量）。
3. 平台無關的邏輯（`decide`、journal、image 解析、驗證、更新 session、救援協議）只有一份，用同一套 host 測試覆蓋所有平台的 `Layout`。
4. XIP 平台的 A/B、test boot、confirm、revert 與斷電安全，語意與 RAM_LOAD 一致。
5. App 端的 `boot-api-client` 可以在三個平台上以同一套 API 使用。
6. 保留未來的擴展點：Bootloader 自更新（Phase F）、新增 SoC、第三方（out-of-tree）板級支援。

### 非目標（本規劃不處理）
- 同時支援多顆 NOR、外接 SDRAM 載入、多 segment 載入。
- XIP 下的壓縮。
- XIP 板的軟體儲存加密與裝置綁定；改依 SoC 硬體機制（F429：RDP/WRP；RT1061：BEE 等，§5.3）。
- 把 Bootloader 改成 async 或引入 RTOS。

---

## 3. 平台模型：四個正交軸

```
            ┌────────────── Board（板級）─────────────────────────────────────────┐
            │ 腳位、LED/按鍵極性、HSE 頻率、救援 UART 選哪個、BOARD_ID、Layout     │
            └───────────────┬─────────────────────────────────────────────────────┘
                            │ 選用
   ┌────────────────────────┼────────────────────────────┬─────────────────────────┐
   ▼                        ▼                            ▼
 SoC 支援                Flash 後端                   執行模型（Exec model）
 時脈、GPIO、UART、       「Slot 與 journal 放在哪裡」    「驗證後怎麼讓 App 跑起來」
 WDG、重置、UID、         SPI NOR(IoOps) /             RamLoad：複製進 RAM、驗 RAM 副本
 握手暫存器、跳轉、       內部 Flash /                  XipRemap：就地驗證、硬體重映射後跳到固定位址
 cache                   FlexSPI NOR                   （bank swap／FlexSPI remap，§5.2）
```

**為什麼要拆成四軸而不是一個 `Board` trait？**
- H750 NetLite 與 F429 的 SoC、Flash 後端、執行模型**三項都不同**，如果只有一個巨大的 `Board` trait，每個板子都要重寫所有東西。
- 同一個 SoC 會有多塊板子（例如另一塊 H750 板用 QSPI 而不是 SPI1），此時只換 Board；同一個 Flash 後端（FlexSPI NOR）也會出現在不同 i.MX RT 晶片上。
- 執行模型是**策略**，不是硬體：RamLoad 在 STM32H7 與 i.MX RT 上邏輯相同，只是目標 RAM 位址不同。把它做成 `boot-core` 內的 enum 而不是 trait，就能被 host 測試完整覆蓋。

**實作方式：靜態分派（generics + const）。** 不用 `dyn`（size 預算、也沒有需要）。每個板子的 bin 在編譯期選好 `P: Platform`，`boot_runtime::run::<P>()` 被單態化。

---

## 4. 三個 profile 對照

| 項目 | H750 NetLite（現況） | NUCLEO-F429ZI | Forlinx OK1061-S（後做） |
|---|---|---|---|
| 核心／時脈 | Cortex-M7，400MHz | Cortex-M4F，最高 180MHz（需 overdrive）[待查證] | Cortex-M7，**528MHz** [已查] |
| Rust target | `thumbv7em-none-eabihf` | 同左 | 同左（FPU 配置需確認）[待查證] |
| 儲存 | 外部 SPI NOR 4MB（SPI1，軟體 CS） | **內部 Flash 2MB，固定雙 bank**，sector 16/64/128KB [已查，社群資料；待 RM0090 核對] | 外部 QSPI NOR **4MB（可選 16MB）** [已查]，經 FlexSPI memory-mapped |
| 板上 RAM | AXI 512KB／DTCM 128KB／SRAM1-3 288KB／SRAM4 64KB | 256KB 級（含 CCM）[待查證] | 1MB on-chip SRAM，其中 512KB 可配置為 TCM [已查] |
| 執行模型 | **RamLoad**（NOR→AXI SRAM，驗 RAM 副本） | **XipRemap / BankSwap**（就地驗證，bank swap 後從固定位址執行） | **XipRemap / FlexSpiRemap**（IOMUXC_GPR30–32）[已查機制；細節 TBD-3] |
| 單一連結位址的實現 | RAM 載入位址本身固定 | 兩個 bank 同佈局，B 版啟動時 swap 到 `0x0800_0000` 視圖 | 以 FlexSPI remap 把 slot B 映射到 slot A 的位址範圍（僅單一區段、4KB 對齊）[已查] |
| 誰擁有 Flash 控制器 | App 擁有 SPI，經 `IoOps` 借給 Bootloader（D-07） | Bootloader（`FLASH` 外設） | Bootloader（FlexSPI IP 命令） |
| 寫 Flash 時能否同時取指 | 能（NOR 與執行 Flash 分離） | 跨 bank 可（RM0090 §3.6 記載 RWW）[已查]；有使用者回報 stall，**需 spike S-F2** | **不能**（單顆 NOR）；需 RAM 常駐例程 [TBD-1] |
| 保密機制 | 裝置綁定儲存加密（現狀，D-14～D-17） | RDP level 1、WRP [待查證] | BEE/OTFAD 加密執行（廠商標示支援）[TBD-2] |
| BootInfo 位置 | SRAM4 `0x38000000` | 選一塊跨重置保留的 RAM（如 CCM 或 SRAM 頂端）[假設] | 保留一塊 DTCM/OCRAM [假設] |
| 握手旗標 | TAMP 備份暫存器 | RTC 備份暫存器 [待查證] | SNVS_LP GPR 或 SRC GPR [待查證] |
| Watchdog | IWDG（啟動後不可停） | IWDG | WDOG/RTWDOG [待查證] |
| 向量表對齊 | 1KB（header 0x400 → VTOR=`+0x400`） | 512B 級（91 IRQ）；沿用 0x400 仍滿足 [待查證] | 1KB（160 IRQ）[待查證] |
| 救援通道 | USART1（PA9/PA10） | USART3 經 ST-LINK VCP（PD8/PD9）[待查證] | 某個 LPUART（依板） |
| 開機啟動鏈 | ROM → `0x0800_0000` | 同左（`BFB2` 保持 0）[待查證] | Boot ROM 讀 NOR 的 FCB + IVT [待查證]；RT1061 的 XIP 只能從 FlexSPI1 `0x6000_0000` 起 [已查] |
| 本輪狀態 | 重構後行為不變 | **P5 實作** | 只預留擴充點 |

## 5. XIP 帶來的架構影響（核心）

### 5.1 在 XIP 平台上寫 Flash [TBD-1]

**問題本質**：App 與 ABI 服務都從 Flash 執行時，對 Flash 的 erase/program 可能讓取指停住（stall），期間無法餵 IWDG、無法服務中斷。

| 平台 | 狀況 |
|---|---|
| **F429（雙 bank）** | RM0090 §3.6 寫明 F42x/F43x 有兩個 bank，可以在一個 bank 讀的同時對另一個 bank 寫/擦 [已查，ST 員工引用原文]；但也有使用者回報從 bank1 擦 bank2 仍 stall [已查，未解]。**必須用 spike S-F2 在 NUCLEO 上實測**，不能只信文件 |
| **RT1061（單顆 NOR）** | erase/program 期間整顆 NOR 的 AHB 讀取都受影響，Bootloader 與 App 都是 XIP，**必須由 RAM 內的程式碼**執行 Flash 命令。remap 只影響讀取位址，不影響 IP 命令寫入 |

**F429 的優先策略（「能不用 RAM 就不用」，回應 Q6）**
- Bootloader 在兩個 bank 各有一份相同的副本（§5.2 為了 bank swap 本來就需要）。因此**「正在執行的 App」與「它呼叫的 ABI 服務碼」永遠位於同一個物理 bank**，而被寫的永遠是**另一個** bank → 若 RWW 如文件所述，**完全不會 stall，不需要 RAM 常駐例程**。
- 例外：journal sector 若恰好在「正在執行的 bank」，換頁時（每數千筆 record 才一次）會 stall 一次 sector erase。處置：接受並把 IWDG 逾時設為大於該最壞時間，或日後再決定是否為 journal 換頁走 RAM 例程。
- 這個策略成立與否取決於 S-F2；不成立就退回「RAM 常駐例程」。

**RT1061（之後）**：預設採 RAM 常駐例程（Bootloader 開機時把小段位置固定的 Flash 命令碼複製到保留 RAM，ABI 的寫入類服務經由它執行；進入時遮蔽 IRQ、期間在 RAM 內輪詢並餵狗，結束後處理 cache）。細節待拿到板子與文件後定案 [TBD-1]。

**現在就預留的低成本擴充點**（不實作）：
- `Layout.ramfunc: Option<AddrRegion>` 與 `BlInfo.reserved` 內的 `ramfunc_base/size`；
- `Flash::PROGRAM_IN_RAM: bool`；
- ABI `caps` 位元 `CAP_SERVICE_RAM_RESIDENT`、`CAP_SERVICE_MASKS_IRQ`；
- ABI 追加 `slot_erase_step()`（一次擦一個 erase unit，讓 App 能排程；F429 的 128KB sector 單次可能到秒級 [待查證]）。

### 5.2 單一連結位址：以硬體重映射實現（`XipRemap`）

v0.1 的 X1（每個 slot 各自連結）已因 Q2 淘汰。三個候選：

| 選項 | 做法 | 結論 |
|---|---|---|
| **R1 硬體 bank swap（F429）** | 兩個 bank 同佈局；Bootloader 依 journal 決定 slot，若選 slot B 則設 `SYSCFG_MEMRMP.UFB_MODE`（bit 8）把 bank2 映射到 `0x0800_0000` 視圖，再跳轉 | **F429 採用**（待 S-F1 驗證） |
| **R2 FlexSPI remap（RT106x）** | `IOMUXC_GPR30/31/32` 設定 start/end/offset，把 AHB 位址 `[start, end)` 讀取重導到 `addr + offset`；**只能作用於一個區段，4KB 對齊** [已查]。NXP 的 MCUboot 範例稱之為 flash remap，分 overlay 與 swap 兩種形式，且映像「以 primary slot 位址建置」[已查] | **RT1061 之後採用**（形式 TBD-3） |
| R3 軟體 swap（MCUboot swap-move/scratch） | 開機時搬資料交換 | **不排入**；只在硬體重映射不可用時作備援。理由：要驗證斷電恢復、增加磨損與開機時間 |

**R1（F429）細節**

```
Reset（UFB_MODE 初始狀態需讀取確認，不可假設）
 └─ Bootloader（物理 Bank1 的副本，位於 0x0800_0000）
     ├─ 讀 journal → decide() → 候選 slot（A = Bank1 區域，B = Bank2 區域）
     ├─ 以「物理偏移」就地讀取並驗證候選 slot（見 §5.3 的 Full/Quick）
     ├─ 設定映射：slot A → UFB=0；slot B → UFB=1（映射不持久，每次開機重設）
     ├─ 寫 BootInfo
     └─ 跳到 exec 位址（兩個 slot 相同）+ header_size
```

- **A/B 兩份 image 完全相同的連結位址**，同一份簽章後的 image 可以放進任一 slot（前提是兩個 slot 的 exec 位址視圖一致，見 §7.2）。
- **revert 就是 journal 狀態**：試開機失敗（IWDG 重啟）後 `decide()` 選另一個 slot，Bootloader 重設映射。不需要任何搬移。
- **Bootloader 在兩個 bank 各有一份副本**：swap 後 `0x0800_0440` 的 ABI 表會落在 bank2 的副本上，所以 bank2 開頭必須有相同的 ABI 表與服務碼。代價：Bootloader 自更新（Phase F）要寫兩份；Bootloader 區佔 128KB × 2。
- **Flash 後端以「物理位址／sector 編號」定址，自己負責映射轉換**。已知陷阱 [已查，社群]：swap 後 `0x0800_0000` 視圖是 bank2，但 FLASH 控制器擦除用的 **sector 編號不會跟著 swap**；因此 `boot-flash-stm32f4` 必須同時知道「目前映射狀態」與「物理 sector」。這正是 `Flash` trait 用抽象偏移、後端內部處理映射的理由。
- `BFB2` 選項位元保持 0（只影響 ROM 啟動）[待查證]。

**R2（RT1061）預告**：Bootloader 與 slot 都在同一顆 NOR；slot A 的範圍是 exec 視窗，slot B 啟動時以 remap 疊在它上面（overlay 形式），或雙向交換（swap 形式）。Bootloader 自己不在 remap 範圍內。其餘（FCB/IVT、ramfunc、BEE）全部待拿到板子後處理。

**抽象方式**：`ExecModel::XipRemap { exec_base, kind: RemapKind::{BankSwap, FlexSpiRemap} }`，Platform 提供 `select_mapping(slot)` 與 `current_mapping()`。`boot-core` 只知道「驗證完某 slot 後，要求平台把它映射到 `exec_base` 再跳」。

### 5.3 驗證與保護

**驗證（D-02 的修訂）**
- RamLoad：維持現狀（驗 RAM 副本，防 TOCTOU）。
- XipRemap：沒有副本，**就地串流 hash**。內部 Flash 只有 Bootloader/ABI 能寫，ABI 已強制「不得寫 active slot」，TOCTOU 風險可接受；外部 NOR（RT1061）的實體替換與 plan §16.1 已列的「以 SPI 設備模擬 NOR」同級，不新增威脅類別，但在 ADR 明文記錄。

**VerifyPolicy（回應 Q4）**

| 時機 | 政策 | 內容 |
|---|---|---|
| 更新完成（`verify_slot`／`mark_pending`） | **Full** | 完整 SHA-256 + 簽章 + 全部 header/TLV 規則；成功後在 journal 追加 `KIND_VERIFIED{slot, image_crc32, hash 前綴}`（利用 journal 已預留「未知 kind 忽略並保留」的機制） |
| Pending → Testing 的首次試開機 | **Quick**（N2） | 安裝時已做 Full 並記錄 `KIND_VERIFIED`；首次試開機只做 Quick，缺記錄才降級 Full |
| Confirmed 的 slot 日常開機 | **Quick**（XIP 預設） | 重新解析 header/TLV 並檢查邊界與 exec 位址、比對 journal 的 `KIND_VERIFIED`、對 **header + image + protected TLV + TLV 全部**（與 hash 範圍一致，N6）做 CRC32（STM32 可用硬體 CRC）；任一項不符 → **降級為 Full**；Full 失敗 → 該 slot 標記 Invalid |
| 救援、journal 異常、`KIND_VERIFIED` 缺失 | Full | — |
| RamLoad（H750） | **Full（維持現狀）** | 因為本來就要把整份 image 讀進 RAM，Full 沒有額外 I/O 代價 |

**Quick 的誠實界線**：它偵測的是**資料損毀**（bit rot、寫入中斷），**不是偽造**；能同時改寫 Flash 與 journal 的攻擊者可以讓 Quick 通過。這與 Q3 的前提一致：真實性在**安裝時**由 Full 保證，之後的保護交給硬體機制。需要在 README 的「安全能力表」寫清楚（見 R-11）。Quick 的實際耗時要由 spike S-F3 量測（F429 預期以 Flash 讀取頻寬為主，遠小於 Full，待實測）。

**儲存保護（回應 Q3）**

| 平台 | 軟體儲存加密 | 依賴的硬體機制 |
|---|---|---|
| H750 | **保留**（裝置綁定，D-14～D-17，行為不變） | RDP（量產） |
| F429 | 無 | RDP level 1（防讀出）、WRP（保護 Bootloader 區）；PCROP 是否可用 [待查證]。**不使用 RDP level 2**（沿用 plan U-09 的理由） |
| RT1061 | 無（軟體端） | BEE/OTFAD 加密執行 [TBD-2]。若採用，OTA ingest 寫入 slot 時要以該機制要求的格式寫入密文（區域金鑰、位址相關計數器）；驗證在 AHB 讀出（已被硬體解密）的明文上進行 |

- **傳輸加密（project key）保留為選配 feature**（`transit-encryption`）：OTA 封包傳輸中是密文，Bootloader ingest 時解密後以明文寫入 slot，可防止竊聽 OTA 檔案。**F429 開發 profile 預設關閉，量產（RDP1）profile 才開啟**（N3）：RDP0 下 project key 可從 Flash 讀出，開了也沒有保護；關閉同時能縮小 Bootloader（T64，§7.2）。
- 程式影響：`require-encrypted` 改為 profile 政策 `StoragePolicy::{DeviceEncrypted, Hardware, Plain}`；`SlotLoader` 的裝置金鑰派生變成 `Platform::device_binding() -> Option<…>`，XIP 平台回 `None`；XIP profile 遇到 `ENC_INFO.scheme = DEVICE` **一律拒絕**（新增負向測試）。

### 5.4 ABI 影響（可以 break，回應 Q1）

| 現況 | 問題 | ABI v2 |
|---|---|---|
| `BOOT_API_ADDR = 0x08000440` 寫死 | 各平台位址不同 | 位址由 `Layout` 提供；`BootApiClient::at(addr)`；App 的 `memory.x`（由 Layout 產生）以 `PROVIDE(__boot_api = …)` 傳入。F429 因 Bootloader 雙副本，**兩種映射下位址相同** |
| `IoOps.spi_transfer`/`cs` 必填 | XIP 平台沒有 App 提供的 SPI | 改 `Option<unsafe extern "C" fn …>`（FFI 安全）；`BootApi` 新增 `caps: u32`：`CAP_IO_FROM_APP`、`CAP_BL_OWNS_FLASH`、`CAP_SERVICE_MASKS_IRQ`、`CAP_SERVICE_RAM_RESIDENT`、`CAP_REMAP_SLOT_MAPPING`（TBD 項目的位元先保留） |
| `BlInfo.reserved[4]` | — | 放 `platform_id`、`exec_model`、`ramfunc_base/size` |
| `slot_erase` 一次擦整個 slot | 128KB sector 的阻塞時間 | 追加 `slot_erase_step()`；`slot_erase` 在 `CAP_SERVICE_MASKS_IRQ` 平台回 `ERR_UNSUPPORTED` |
| `BootInfo` 固定在 SRAM4 | F429/RT1061 沒有 SRAM4 | App 只用 `get_boot_info()`；位址由 `Layout.boot_info_addr` 決定，不再是 ABI 常數 |
| `BootInfo.boot_slot` | XIP 下兩個 slot 位址相同 | 語意不變（A/B 邏輯編號）；新增 `flags` 位元標示「目前映射狀態」供除錯 |
| `update_init/feed/abort` 目前是**空殼回傳 `BOOT_OK`** [已讀碼] | 尚未實作 | M5 工作；BSP 分離時順便接上 `boot-update` |

### 5.5 對既有決策（plan.md D-01～D-18）的影響矩陣

| ID | 決策 | H750 | XIP 平台 | 動作 |
|---|---|---|---|---|
| D-01 | RAM_LOAD，不做 swap | 不變 | 不適用 → 改 `XipRemap` | 新 ADR-0002：Exec model 為 profile 屬性 |
| D-02 | 驗 RAM 副本 | 不變 | 就地驗證 | ADR-0002 記錄 TOCTOU 取捨 |
| D-03 | MCUboot 相容 header | 不變 | 相容；旗標語意見 §8 | 文件 |
| D-04 | 載入 AXI SRAM | 不變 | 不適用 | 進 `RamLoad` 專屬參數 |
| D-05 | 狀態存 journal | 不變 | journal 放平台指定區（F429：兩個 128KB sector） | journal 參數化 |
| D-06 | 固定位址 ABI | 不變 | 位址依平台 | ABI v2 |
| D-07 | 硬體所有權反轉 | 不變 | **不適用**（Bootloader 擁有 Flash 控制器） | `IoOps` 可選 + caps |
| D-08 | 服務只 polling、不依賴 BL `.data` | 不變 | 視 TBD-1 補充：IRQ 遮蔽／RAM 常駐 | §5.1 |
| D-09 | Ethernet 只在 App | 不變 | 不變 | — |
| D-10 | 救援通道編入一個 | 不變 | 各板選 UART | `Console` trait |
| D-11 | size 預算 | 不變 | F429：Bootloader 區 128KB（×2 副本）；預算進 Layout | CI |
| D-12 | `unsafe` 政策 | 不變 | SoC／Flash 後端 crate 屬允許範圍 | 更新允許清單 |
| D-13 | TAMP + SRAM4 握手 | 不變 | 換媒介 | `Handshake` trait |
| D-14～D-17 | 傳輸/儲存加密、KDF、XChaCha20 | 不變 | **儲存加密不適用**（改靠硬體）；傳輸加密選配 | §5.3 |
| D-18 | zeroize | 不變 | 不變 | — |

### 5.6 UART rescue（回應 Q11）

所有平台都要有 Bootloader 內建的基本 UART 救援，**以整個 slot 為單位**：

- 協議以 **slot 相對偏移** 表達，不再出現 64KB block；`erase <slot>` 由 `Flash::unit_at` 逐個 erase unit 擦完整個 slot（F429 是 128KB sector），過程輸出進度、持續餵狗、不依賴 `millis()` 逾時以外的假設。
- `write` 的單次長度與對齊交由 `Flash::PROGRAM_ALIGN` 檢查（F429 以 word 程式化）。
- 新增 `verify <slot>`（執行 Full 驗證並回報）與 `mark <slot>`（驗證通過才可標記，修掉現在寫死版本號、不驗證的行為）。
- 救援時**沒有 App 在執行**，所以即使 Bootloader 與目標 slot 在同一個 bank 而 stall，也只是等待，沒有併發問題（RT1061 之後仍需 RAM 例程，屬 TBD-1）。
- 救援協議與 `rescue_upload.py`、`host-update` 同步更新，`README` 的 64KB 限制條目移除。
- 新增 `s`（status）指令：回報 `ProtectionStatus` 與 Bootloader/ABI 版本，供量產檢查使用（§5.7）。
- ROM bootloader（F429 的 system memory、RT 的 Serial Downloader）是額外的硬體救援路徑，不取代 Bootloader 內的 UART 救援。

### 5.7 量產保護檢查（回應 N7）

目的：量產時確認「該鎖的都鎖了」，且**可選擇**是否由 Bootloader 強制。

**`Platform::protection_status() -> ProtectionStatus`**：由各平台回報保護狀態（`#[non_exhaustive]`，欄位依平台）：

| 平台 | 回報內容（預期；細節 [待查證]） |
|---|---|
| H750 | RDP level、Bootloader 區的 WRP、是否仍為開發金鑰 |
| F429 | RDP level（**不得為 2**）、Bootloader 兩份副本所在 sector 的 WRP、BOR、`BFB2`、雙 bank 狀態、是否仍為開發金鑰 |
| RT1061（之後） | HAB 是否 closed、BEE/OTFAD 是否啟用、相關 fuse、是否仍為開發金鑰 |

**三種使用方式（皆為選項）**

1. **主機端 `xtask prod-check --board X`**：以 probe-rs 讀 option bytes／fuse，對照該板的量產期望值（`prod` profile 檔），並檢查 keytable 的公鑰 hash 不在「已知開發金鑰」清單內。用於上鎖**前**的燒錄檢查。
2. **裝置端救援指令 `s`（status）**：經 UART 回報 `ProtectionStatus`。RDP1 上鎖後 SWD 無法讀 Flash，這是上鎖**後**仍可由產線驗證的路徑；`xtask prod-check --uart PORT` 會呼叫它。
3. **Bootloader feature（預設關）**：
   - `prod-flag`：把結果寫入 `BootInfo.flags` 的 `PROT_OK` 位元，App 可讀取並自行決定行為；
   - `prod-enforce`：保護不符時**拒絕跳轉 App、只留救援**（LED 閃碼提示）。僅建議用在量產 profile，且必須保證救援路徑仍可用（見 R-15）。

**開發／量產 profile**：每個板子有 `dev`／`prod` 兩種 profile，控制 `transit-encryption`、`prod-flag`、`prod-enforce`、金鑰來源（`prod` 不得含開發金鑰）。`xtask build --board X --profile prod`。

## 6. 目標架構

### 6.1 目錄結構（提議）

```
crates/
  # ── 平台無關（no_std，host 可測）──
  boot-abi/            # 只留 repr(C) 結構、錯誤碼、狀態值、caps 位元；不再有位址常數
  boot-layout/         # 新：Layout/Region/ExecModel 型別 + 各板的 const Layout（純資料，無 HAL）
  boot-image/          # header/TLV；validate(&Layout)；board id 由 Layout 提供
  boot-crypto/
  boot-flash/          # 新：Flash trait、FlashError、RamFlash(host mock，支援不均勻 sector)
  boot-journal/        # 參數化 sector 位址與 unit 大小
  boot-core/           # decide() + 驗證管線（ImageSource）+ 載入/就地驗證
  boot-update/         # 更新 session，吃 &Layout + Flash
  boot-rescue/         # 新：從 main.rs 抽出的 UART 救援協議（generic over Console + Flash）
  boot-platform/       # 新：Platform / Console / Handshake / Watchdog / DeviceBinding 等 trait
  boot-runtime/        # 新：run::<P>()：開機流程 + ABI shim（12 個 abi_* 函式）
  boot-api-client/     # App 端；位址參數化

  # ── Flash 後端 ──
  boot-flash-spinor/   # 現 boot-nor 的 SPI NOR 命令邏輯（IoOps 或 SpiBus 驅動）
  boot-flash-stm32/    # STM32 內部 Flash：依 Flash IP 世代分模組（先做 f4；其他見附錄 D），由 metapac chip feature 選用
  boot-flash-flexspi/  # FlexSPI NOR IP 命令，RAM-resident 版本

  # ── SoC ──
  boot-stm32-common/   # 新：STM32 跨家族共用（IWDG、UID、備份暫存器抽象、BankSwapVariant 等）
  boot-soc-stm32h7/    # 現 boot-stm32h7 中屬於 SoC 的部分
  boot-soc-stm32f4/
  boot-soc-imxrt10xx/

boards/
  h750-netlite/        # bootloader bin（main ≈ 30～50 行）+ board.rs + 金鑰表 + app-template
  nucleo-f429zi/
  imxrt1061-ok1061s/

tools/
  xtask/               # --board 參數；從 boot-layout 取得 Layout
  host-update/
```

> 命名只是提議，`boot-nor` 是否改名可以討論（改名的成本是 import 與文件，好處是語意清楚；不改也可以，由 `boot-flash` 新增 trait 並讓 `boot-nor` 實作它）。

> **STM32 家族擴充原則（N4：保留最大可能性）**
> 1. `boot-stm32-common` 放跨家族共用的東西（IWDG、UID 讀取、備份暫存器抽象、`cortex-m` 時基、`BankSwapVariant` 列舉）；`boot-flash-stm32` 依 **Flash IP 世代**分模組（先做 F4；F7／L4／G4／H7／U5…的差異見附錄 D），以 `stm32-metapac` 的 chip feature 選用。
> 2. 公開的列舉與設定型別一律 `#[non_exhaustive]`（`RemapKind`、`ProtectionStatus`、`FlashError` 等），日後新增變體不破壞相容。
> 3. **規則：沒有板子就不寫變體。** 只保證介面不會擋住未來的家族；避免為了「也許會用到」而增加 size 與測試負擔（R-06）。
> 4. metapac 的 chip feature 互斥（R-07）仍適用：每個板子的 bin 只啟用自己的 chip。

### 6.2 依賴規則（用 CI 強制）

1. 「平台無關」crate **不得**依賴任何 `boot-soc-*`、`boot-flash-<hw>`、`stm32-metapac`、`imxrt-*`。
2. `boot-layout` 只能是純資料，**不得**依賴任何 HAL；因為 `xtask`、host 工具與 bootloader 都要讀它。
3. 只有 `boards/*` 的 bin 知道「選了哪個 SoC / Flash 後端」。
4. 依賴方向：`boards → runtime → core/journal/update/rescue → flash/image/crypto/abi/layout`；`boards → soc/flash-backend → platform`。

### 6.3 Trait 草案（形狀，不是最終 API）

```rust
// boot-flash
pub trait Flash {
    /// 最小程式化單位：SPI NOR=1，F4 內部 Flash=4（x32），其他=更大
    const PROGRAM_ALIGN: u32;
    fn capacity(&self) -> u32;
    fn read(&mut self, off: u32, buf: &mut [u8]) -> Result<(), FlashError>;
    fn program(&mut self, off: u32, data: &[u8]) -> Result<(), FlashError>;
    /// 純查詢（host 可測）：含 off 的 erase unit 範圍。F4 回 16K/64K/128K 不等
    fn unit_at(&self, off: u32) -> Range<u32>;
    fn erase_unit(&mut self, off: u32) -> Result<(), FlashError>;
}

// boot-platform
pub trait Platform {
    type Flash: Flash;
    type Console: Console;          // 救援與 log
    const LAYOUT: &'static Layout;

    fn early_init();                                   // 時脈、關鍵 GPIO（例如 NOR /CS 先拉高）
    fn flash() -> Self::Flash;
    fn console() -> Self::Console;
    fn millis() -> u32;                                // 強制成員（§1.4 的 bug）
    fn watchdog_start(timeout_ms: u32);
    fn watchdog_kick();
    fn uid() -> [u8; 16];                              // 長度前綴編碼，供 KDF
    fn take_rescue_request() -> bool;                  // 握手暫存器
    fn request_rescue();
    fn rescue_button_pressed() -> bool;
    fn protection_status() -> ProtectionStatus;        // §5.7
    fn led(on: bool);
    fn boot_info_slot() -> *mut BootInfo;              // 由 Layout.boot_info 決定
    fn device_binding() -> Option<&'static dyn DeviceBinding>; // XIP 平台回 None
    /// 跳轉前置：關中斷、反初始化、cache、VTOR、CONTROL（plan §6.1 的平台部分）
    unsafe fn prepare_and_jump(entry: Entry) -> !;
}
```

```rust
// boot-layout（純資料）
pub struct Region { pub off: u32, pub len: u32 }          // Flash 後端位址空間內的偏移
pub enum ExecModel {
    RamLoad { load: AddrRegion, vtor_offset: u32 },       // H750
    XipRemap { exec_base: u32, kind: RemapKind },         // 單一連結位址；kind = BankSwap(variant) | FlexSpiRemap，皆 #[non_exhaustive]
}
pub struct Layout {
    pub board_id: u32,
    pub header_size: u32,                                  // 0x400
    pub journal: [Region; 2],
    pub slots: [Region; 2],
    pub exec: ExecModel,
    pub bl_info_addr: u32, pub boot_api_addr: u32,
    pub boot_info_addr: u32,
    pub ramfunc: Option<AddrRegion>,                       // §5.1
    pub bootloader_budget: u32,
}
```

**驗證管線的拆法（`boot-core`）**：把現在的 `load_and_verify` 拆成可組合的段落：

```
ImageSource(Flash)  ──►  [Decrypt?]  ──►  Sink(RamCopy | None)  ──►  Hash  ──►  Verify  ──►  ExecEntry
                           │DeviceBinding         │ExecModel::RamLoad     │              └─ XipRemap：先映射到 exec 位址，entry = exec + header_size
```

RAM_LOAD 路徑的輸出（位元組、hash、時序）必須與現在一致，這是 P4 的回歸基準。

### 6.4 App 端

- `app-template` 變成 `boards/<name>/app-template`，使用該板的 SoC crate 的 **app-facing 子集**（`millis`、UART、LED、`IoOps` 提供者）；與 Bootloader 專用部分（跳轉、RAM 初始化、救援）分開（對應 §1.2 第 11 項）。
- XIP 板子的 App `memory.x` 由 manifest 產生（FLASH 起點 = slot 的 exec 位址 + header_size，另排除 ramfunc 保留區與 BootInfo 區）。
- RT1061：App 若使用 `imxrt-rt`，它預設會輸出自己的 boot header 並假設自己是由 Boot ROM 啟動；被 Bootloader 跳入的 App 需要「無 header、不重設 FlexRAM 配置」的連結方式 [待查證]，見 §9 的 S-R5。

### 6.5 建置與工具

- **board manifest**：每板一個 `board.toml`（或直接用 `boot-layout` 內的 const，`xtask` 取用）。內容：target triple、probe-rs chip、Layout、救援埠、記憶體區域、大小預算。`bootloader`/`app` 的 `build.rs` **由 Layout 產生 `memory.x`**，避免 linker script、Rust 常數、xtask、App 四處漂移（目前 H750 就是四處各寫一次）。
- **`stm32-metapac` 晶片 feature 互斥**：同一 workspace 內同時有 `stm32h750vb` 與 `stm32f429zi` 的 crate 時，`cargo build --workspace` / `cargo check --workspace` 會因 feature 統一而失敗。對策：
  - `default-members` 只放平台無關 crate（host 可測）；
  - target 建置一律 `cargo build -p <board-bin>`，由 `xtask build --board X` 與 `make BOARD=X` 包裝；
  - CI 以明確的 `-p`／`--exclude` 列表，不使用 `--workspace`。
  - 備選：每個板子一個獨立 workspace（共用 lib 以 path 依賴）。代價是多份 `Cargo.lock`，與「`--locked` 並提交 Cargo.lock」的現有政策要重新定義。（Q9 已決定：先簡單做，單一 workspace）
- `.cargo/config.toml` 的 `runner = probe-rs --chip STM32H750VBTx` 改由 `xtask flash --board X` 提供。

---

### 6.6 版本與相容性（回應 Q7、Q8）

- **各 crate 獨立版本**：移除 `[workspace.package] version`，每個 crate 在自己的 `Cargo.toml` 宣告版本；可獨立打 tag 與發布（例如 `boot-soc-stm32f4-v0.2.0`）。
- 兩個**相容性常數**：`boot_abi::BOOT_ABI_VERSION`（Bootloader ↔ App 的 ABI）與 `boot_platform::PLATFORM_API_VERSION`（`boot-platform` trait 的版本，SoC／Flash 後端／板級 crate 對它編譯）。
- 在 repo 內維護 `docs/compat.md`：列出每個 board bin 使用的各 crate 版本與 ABI/Platform API 版本；CI 檢查每個 board 的 `Cargo.lock` 與該表一致。
- **BSP 邊界（Q7）**：現階段是 repo 內的 crate 邊界（a）。為日後升級為 repo 外的第三方 BSP（b）預作準備：`boot-platform` 保持小而穩定、公開型別使用 `#[non_exhaustive]`、trait 不洩漏內部型別；但**在 P6 ABI 凍結之前，不承諾 semver 穩定**。

## 7. 各平台儲存佈局（草案）

### 7.1 H750 NetLite（不變，只是搬進 Layout）
沿用 plan §4.1–§4.3：內部 Flash 128KB 放 Bootloader；NOR 的 journal 2×4KB、Slot A/B 各 640KB、BL staging、Factory slot。

### 7.2 NUCLEO-F429ZI（`XipRemap / BankSwap`）[sector 幾何待 RM0090 核對]

`Layout` 參數化，兩種佈局只差常數；**以 B128 起步，T64 為目標**（N1、TBD-5）。以下用**物理位址**（未 swap 的視圖）。

#### B128：基準（先做）

```
Bank1  0x0800_0000  S0–S3 (4×16KB) + S4 (64KB)  = 128KB   Bootloader 副本 #1
       0x0802_0000  S5   128KB                            Journal sector 0
       0x0804_0000  S6–S11 (6×128KB) = 768KB              Slot A      exec = 0x0804_0000
Bank2  0x0810_0000  S12–S15 + S16               = 128KB   Bootloader 副本 #2（同一份 binary）
       0x0812_0000  S17  128KB                            Journal sector 1
       0x0814_0000  S18–S23 (6×128KB) = 768KB             Slot B      exec = 0x0804_0000（swap 後視圖）
```

#### T64：目標（Bootloader ≤ 64KB，journal 64KB）

```
Bank1  0x0800_0000  S0–S3 (4×16KB)               =  64KB   Bootloader 副本 #1（.bl_info@+0x400、.boot_api@+0x440；
                                                           .secrets/.keytable 在這 64KB 的尾端）
       0x0801_0000  S4   64KB                             Journal sector 0
       0x0802_0000  S5–S11 (7×128KB) = 896KB              Slot A      exec = 0x0802_0000
Bank2  0x0810_0000  S12–S15 (4×16KB)             =  64KB   Bootloader 副本 #2
       0x0811_0000  S16  64KB                             Journal sector 1
       0x0812_0000  S17–S23 (7×128KB) = 896KB             Slot B      exec = 0x0802_0000（swap 後視圖）
```

- **T64 的兩個優點**：(1) Bootloader 與 journal 各自**剛好對齊 sector 邊界**（S0–S3 = 64KB、S4/S16 = 64KB），沒有浪費；(2) slot 由 768KB 增加到 **896KB**（7 個 128KB sector），且 slot 起點落在 128KB sector 邊界。
- **兩種佈局的 slot 都是同 exec 位址**；簽章後的同一份 image 可放進任一 slot（前提是兩個 slot 的 exec 位址視圖一致）。
- **T64 的前提是 Bootloader（含 `.secrets` 與 `.keytable` 各約 2KB）≤ 64KB**。這與 H750 的 96KB 預算不同，需要靠：
  - F429 **不需要裝置綁定**，可省掉 HKDF、裝置金鑰派生；
  - 開發 profile 預設不含 `transit-encryption`（XChaCha20/HKDF），量產 profile 才加入（§5.3），因此**量產 profile 的 size 才是 T64 的關鍵**；
  - 簽章後端仍是最大宗（P-256 驗簽）。
  `.secrets`／`.keytable` 的位址在 T64 下改由 `Layout` 計算（BL 區尾端往前 4KB／2KB），不再是 H750 的固定 `0x0801_F000/0x0801_F800`。
- **切換條件（已採納，O2）**：F429 **量產 profile** 的 Release Bootloader，`arm-none-eabi-size` 的 `text` ≤ **57,344 B（56KiB）** 則採 T64，否則留在 B128。因為 `Layout` 是純資料，切換只改常數與重新驗證，不影響 `boot-core`。

#### H750 實測數據與 T64 餘裕推算（2026-11-12）

量測：`arm-none-eabi-size bootloader-h750` → `text = 51,508`、`data = 0`、`bss = 0`（單位 B）。`text` 包含向量表、`.bl_info`、`.boot_api`、`.secrets`、`.keytable`、程式碼與唯讀資料，但**不含**各固定位址之間的空隙。`data = bss = 0` 表示 Bootloader 目前沒有任何需要初始化的 RAM 變數。

| 項目 | 大小（B） | 來源 |
|---|---:|---|
| `.keytable`（結構 1,338 B + 對齊 2 B） | 1,340 | `-A` 實測 |
| `.secrets` | 112 | `-A` 實測 |
| `.bl_info` | 64 | `BlInfo` |
| `.boot_api` | 96 | `BootApi`（12 個函式指標＋8 個保留） |
| `.vector_table` | 664 | `-A` 實測（`__INTERRUPTS` 600 B + 核心 64 B） |
| **以上固定項目小計** | **2,276** | |
| **`.text` + `.rodata`** | **49,232** | `-A` 實測（31,452 + 17,780） |

| 推算 | 結果 |
|---|---|
| 對 64KiB（65,536 B）的名目餘裕 | 14,028 B |
| 對 56KiB 切換線（57,344 B）的餘裕 | 5,836 B |
| **假設把這份 H750 程式碼放進 T64 的實際 footprint**：`0x600`（向量表＋info＋api 保留）＋ 49,232 ＋ 4,096（`.secrets`、`.keytable` 各保留 2KB） | ≈ 54,864 B → 距 64KiB 約 **10,672 B（10.4KiB）** |
| **text 剛好在 56KiB 切換線時的實際 footprint** | ≈ 60,700 B → 距 64KiB 約 **4,836 B（4.7KiB）** |

**解讀**
- 以 H750 現況（已含 XChaCha20、HKDF、裝置金鑰派生、NOR Security Register 邏輯）就已落在切換線下，且放進 T64 仍有約 10KiB 餘裕。F429 版會**拿掉**裝置綁定相關程式碼，並（量產 profile）可能再加回傳輸加密；會**新增** F4 Flash 驅動、bank swap、`protection_status`／`s` 指令、Quick 驗證（CRC32 + `KIND_VERIFIED`）、`Flash`／`Layout` 泛型化的額外程式碼。**這些增減都還沒有量測**，所以現階段只能說「T64 看起來可行」，不能保證。
- v0.3 寫的「≤ 56KB（餘裕約 8KB）」是名目值（64 − 56）。扣掉固定保留區後，實際餘裕約 4.7KiB；切換規則本身不變，只是把數字寫準。
- 目前 `release` profile 開著 `overflow-checks = true`，會增加程式碼量；之後若需要縮 size，這是一個可評估的槓桿（但 plan 的安全原則偏好明確的 `checked_*`，需要另外決定，不在本輪處理）。
- 組成分解已取得（見下方）。
- journal 單 sector 64KB（B128 為 128KB）：record 32B → 約 2000 筆換一次頁；換頁時若該 sector 在正在執行的 bank，會 stall 一次 sector erase（§5.1）。
- 板上需確認：是否確實為雙 bank、Option bytes（RDP、BOR、WRP、`BFB2`）、時脈與 Flash wait states。

#### 組成分解（2026-11-12，`arm-none-eabi-size -A` 與 `cargo bloat --crates`）

**依 section（flash 實際用量）**

| Section | 大小（B） | 佔比 | 說明 |
|---|---:|---:|---|
| `.text` | 31,452 | 61.1% | 程式碼 |
| **`.rodata`** | **17,780** | **34.5%** | 唯讀資料（見下方分析） |
| `.keytable` | 1,340 | 2.6% | 公鑰表（含 1KB 保留欄位） |
| `.vector_table` | 664 | 1.3% | |
| `.secrets` | 112 | 0.2% | |
| `.boot_api` / `.bl_info` | 96 / 64 | 0.3% | |
| **合計** | **51,508** | 100% | 與 `arm-none-eabi-size` 的 `text` 一致 |

> `.debug_*` 的 Total 約 1MB 是除錯資訊，**不會燒進 flash**，與 size 預算無關。`.data`、`.bss` 皆為 0。

**依 crate（`.text` 31.4KiB 的歸屬；LTO 下 attribution 只是粗估）**

| Crate | `.text` | 佔比 | 備註 |
|---|---:|---:|---|
| `p256` | 7.2KiB | 23.3% | 驗簽主體 |
| `boot_core` | 6.6KiB | 21.5% | `SlotLoader`；LTO 後內聯了不少密碼學程式碼 |
| `bootloader_h750` | 3.1KiB | 10.0% | `main.rs`：流程、救援、ABI 函式 |
| `boot_crypto` | 2.5KiB | 8.0% | |
| `sha2` | 2.1KiB | 6.7% | |
| `std`（core／compiler builtins） | 1.9KiB | 6.1% | `memcpy` 等 |
| `boot_journal` | 1.5KiB | 5.0% | |
| `ecdsa` + `der` + `sec1` + `crypto_bigint` + `digest` | 約 2.9KiB | 約 9% | 簽章驗證周邊 |
| `boot_image` | 834B | 2.7% | |
| `boot_nor` | 722B | 2.3% | |
| `crc32fast` | 596B | 1.9% | **只是程式碼；它的查表在 `.rodata`** |
| `boot_stm32h7` | 308B | 1.0% | |
| `chacha20` | 88B | 0.3% | 數字偏小，應是被內聯進 `boot_core`／`boot_crypto` |
| 其他 | 約 0.8KiB | 約 2.6% | |

**`.rodata` 17,780 B 的分析（已由 `nm` 證實）**
- `crc32fast`（1.5.4）在 `no_std`、非 x86/aarch64 的目標上使用「slice-by-16」的 baseline 實作，其查表是 **16 × 256 × 4 = 16,384 B**。
- 17,780 − 16,384 = 1,396 B，剛好是合理的「字串（UART 訊息）＋ SHA-256 常數表（256 B）＋ P-256 常數」的量級。
- `crc32fast` 只在 `boot-journal`（record／sector header CRC）與 `boot-nor`（`SeedRecord` CRC）使用，且 workspace 以 `default-features = false` 引入。
- **驗證結果**：`arm-none-eabi-nm -S --size-sort -C` 的最大符號是 `crc32fast::table::CRC32_TABLE`，大小 `0x4000` = **16,384 B**，位於 `0x0800864c`。推論成立。

**`nm` 最大符號（`arm-none-eabi-nm -S --size-sort`，前 15 名）**

| 大小（B） | 符號 | 備註 |
|---:|---|---|
| **16,384** | `crc32fast::table::CRC32_TABLE`（`r`，唯讀） | **`.rodata` 的主要來源** |
| 6,756 | `SlotLoader::load_and_verify` | **單一函式佔 `.text` 的 21.5%**（含內聯的密碼學與解密路徑） |
| 2,692 | `p256 Scalar::mul` | |
| 1,764 | `sha2::sha256::compress256` | |
| 1,420 | `__cortex_m_rt_main` | `main.rs` 的主流程（內聯後） |
| 1,340 | `PUBLIC_KEY_TABLE` | 與 `.keytable` 一致 |
| 1,148 | `p256 field::fe_mul` | |
| 944 | `p256 field::montgomery_reduce` | |
| 884 | `der … UintRef::decode` | |
| 740 | `JournalManager::init` | |
| 722 | `boot_image::parse_image` | |
| 660 | `compiler_builtins::mem::memcpy` | |
| 600 | `__INTERRUPTS` | 向量表的中斷部分 |
| 556 | `primeorder … add` | |
| 532 | `enter_rescue_mode` | |

> `nm` 的十六進位大小已換算為十進位。`load_and_verify` 目前把 NOR 讀取、金鑰派生、解密、hash、驗簽、清零全部寫在一個函式裡；P4 把它拆成 `ImageSource → Decrypt? → Sink → Hash → Verify` 時，是檢視重複的清零／錯誤路徑、順便縮小的機會，但**不作為目標承諾**（優先級低於行為正確與位元一致）。

**處置（`boot-crc`）**
- 新增一個很小的 `boot-crc` crate，實作**同一個演算法**（CRC-32/ISO-HDLC，反射、多項式 `0xEDB88320`、初值 `0xFFFFFFFF`、輸出 XOR `0xFFFFFFFF`），讓 `boot-journal` 與 `boot-nor` 改用它。結果與 `crc32fast` 相同，**已寫入 NOR 的 journal 與 Security Register 的 `SeedRecord` 完全相容**，不需要遷移。
- **採用：256 項表**（編譯期由 `const fn` 算好、放在 flash，不佔 RAM）。實作選項曾比較：256 項表（1KiB，最快）、16 項 nibble 表（64 B，約為前者的一半速度）、純位元運算（無表，最慢）。**journal 與 SeedRecord 的資料量極小，任何一種都夠；Quick 驗證（§5.3）要掃整個 slot，才需要較快的版本。**
- Platform 可選擇提供硬體加速（`Platform::crc32_update`）：STM32 的 CRC 外設可用，但它的位元順序／多項式設定與 zlib 的 CRC 不同；因為 Quick 的 CRC 是由裝置自己寫入、自己比對（`KIND_VERIFIED`），**不需要與 zlib 相同**，可以用硬體版；而 journal／SeedRecord 的 CRC 要與既有資料相容，必須用上面的軟體版。這兩種 CRC 要用不同名稱區分，避免混用。
- 回歸驗證：host 端用 `crc32fast` 當 oracle 對 `boot-crc` 做隨機比對（只在 `dev-dependencies`），再加板上兩個 smoke test。

**對 T64 餘裕的影響（以現有 `.text` + `.rodata` = 49,232 B 為基準；T64 的程式碼預算 = 65,536 − 0x600 − 4,096 = 59,904 B）**

| 情境 | `.text` + `.rodata` | 距 T64 預算的餘裕 |
|---|---:|---:|
| 現況（H750 程式碼原樣放進 T64） | 49,232 | 10,672 B（10.4KiB） |
| 以 `boot-crc` 取代 `crc32fast`（1KiB 表） | 約 33,900 | 約 26,000 B（約 25KiB） |
| 同上，改用 nibble 表（64 B） | 約 32,900 | 約 27,000 B（約 26KiB） |

**實測結果（P0 patch B，已完成）**：在同一環境（rustc 1.91、`opt-level = "z"`、`lto = "fat"`、`arm-none-eabi-size`）下比較 `10ba7bc` 與套用 `boot-crc` 後：

| | `.text` | `.rodata` | `text` 合計 | 差異 |
|---|---:|---:|---:|---:|
| `10ba7bc`（`crc32fast`） | 32,744 | 17,796 | 52,816 | — |
| 套用 `boot-crc`（256 項表） | 32,112 | 2,436 | **36,824** | **−15,992（−30%）** |

- `.rodata` 少了 15,360 B（= 16,384 的查表 − 1,024 的新表）；`.text` 反而少 632 B。新表 `boot_crc::TABLE` 為 `0x400` B、位於唯讀區（`nm` 顯示 `r`），`.data`／`.bss` 仍為 0，**不佔 RAM**。
- 絕對值與你本機的 51,508 不同，是因為編譯器版本不同（我的環境是 rustc 1.91，你的是 stable 工具鏈）；**以同一環境的差值為準**。換算到你的環境，預期約 **51,508 − 15,992 ≈ 35,516 B**（推算，請以你本機實測為準）。
- 以我的環境的 `.text` + `.rodata` = 34,548 B 計，放進 T64 的程式碼預算（59,904 B）仍有約 **25,356 B（24.8KiB）** 餘裕，與上表的預估一致。
- 驗證：host 測試全過（`boot-crc` 5 個，含以 `crc32fast` 為 oracle 的隨機輸入至 65,537 B 與任意切塊比對；其餘 crate 既有測試維持通過）；`boot-journal`／`boot-nor` 的依賴圖不再包含 `crc32fast`（它只剩 `boot-crc` 的 dev-dependency）。**尚未做的是板上 smoke test（載入 App 執行 + rescue 上傳）**，需要你執行。

F429 版的增減（拿掉裝置綁定；加入 F4 驅動、bank swap、`protection_status`、Quick 驗證、量產 profile 的傳輸加密）**仍未量測**。但就算完全不做 CRC 優化，T64 在現有數據下也有餘裕；做了之後餘裕變得寬鬆。因此 **T64 的可行性從「看起來可行」上調為「很有可能可行」**；最終仍以 P5f 的實測為準（O2 的規則不變）。


### 7.3 Forlinx OK1061-S（strawman，後做；僅供預留擴充點時參考）

已查到的硬體：RT1061 @ 528MHz、1MB on-chip SRAM（512KB 可配置為 TCM）、**4MB QSPI NOR（可選 16MB）**、SWD、7 路 UART 等 [已查]。以 4MB 估算：

```
0x6000_0000  +0x00_0000  boot header 區（FCB@+0x400、IVT@+0x1000 等，由工具產生；≤ 8KB）
             +0x00_2000  Bootloader（含 .bl_info/.boot_api/.keytable/.secrets）≤ 248KB
             +0x04_0000  Journal 2×4KB（保留到 +0x05_0000）
             +0x05_0000  Slot A   1.75MB   ← exec 視窗（remap 的目標範圍）
             +0x21_0000  Slot B   1.75MB   ← 以 FlexSPI remap 疊到 Slot A 的位址範圍
             +0x3D_0000  App 資料 192KB
```

- remap 只能作用於**單一區段、4KB 對齊** [已查]，與「slot A 範圍 = exec 視窗」的配置相容。
- 4MB 總量下 slot 約 1.75MB；若 App 更大，需用 16MB 版本。
- 其餘（FCB 與 NOR 型號的 LUT/時序、BEE、ramfunc、remap 形式）全部待拿到板子。

## 8. Image 格式與工具鏈

- **Header 大小維持 0x400**：M4（512B 對齊）與 M7（1KB 對齊）都能滿足，H750 現狀不變。Slot 起點需 1KB 對齊（§7 佈局皆滿足）。
- **`ih_load_addr` 的語意**：
  - RamLoad：`IMAGE_F_RAM_LOAD` + `ih_load_addr` = RAM 位址（現狀）。
  - XipRemap：`IMAGE_F_ROM_FIXED`（我記憶是 `0x100`，**需對照 MCUboot `image.h`**）+ `ih_load_addr` = **exec 位址（兩個 slot 相同）**。Bootloader 驗證它必須等於 `Layout` 的 exec 位址。
  - 這取代了現在寫死的 `ih_load_addr != AXI_SRAM_BASE`。
- **因為是單一連結位址，不需要 per-slot 建置，也不需要雙 payload 的 OTA 封包**（v0.1 的 `xtask package` 與 `--slot` 參數取消）。同一份簽章後的 image 可寫入任一 slot；ingest 只需要依 `BootInfo.boot_slot` 選「另一個」slot。
- **`BOARD_ID`**：每個板子一個常數，由 `Layout.board_id` 提供；`xtask sign --board X` 寫入。H750 保持 `0x4837_3530`（`'H750'`）。BOARD_ID 是「板」不是「SoC」。
- **`xtask`**：
  - `build --board X`：依板子取 target、feature、linker 輸入建置。
  - `sign --board X`：依 Layout 組 header、旗標、board id、exec 位址。
  - `size-check --board X`：預算取自 Layout。
  - `flash --board X`：probe-rs chip 取自 board 設定。
  - 檢查：App 的 ELF 不得與保留區（BootInfo、ramfunc）重疊。
- 現有的固定測試金鑰（`[0x01; 32]`）與 Bootloader 內的開發金鑰表，維持 dev-only，不在本案處理。

## 9. 遷移計畫

**總原則**：每個階段結束時 H750 都能完整跑既有流程（開機、test→confirm、revert、UART 救援、更新）。重構階段**不改任何對外行為**，除非明確標示（ABI v2、救援全 slot 為例外）。

### G0 關卡：H750 測試完成（N5）

在開始 P0 之前，先完成 H750 的測試（驗收清單見**附錄 C**）。**G0 通過之後**：(1) 打基線 tag；(2) F429 的 spike 與 P5 才開始；(3) P1～P4 的重構可與 F429 spike 並行。G0 的目的是讓「重構前的行為」有一個確定、可比對的定義。

| 階段 | 內容 | DoD |
|---|---|---|
| **P0 基線與安全網** | 打 tag；記錄 H750 release 的 `.bin` hash/size；**size 組成分解已取得（§7.2）；`nm` 已驗證 `.rodata` 最大符號為 CRC 查表（0x4000）；在基線 tag 之後、作為獨立的一步，以 `boot-crc` 取代 `crc32fast`（同演算法，預期省約 15KiB），並以兩個板上 smoke test 與 host oracle 比對回歸**；host 測試全綠；補特性測試固定現有行為；**修 `local_millis`**（§1.4）；刪重複的 plan 文件。**狀態：patch 0001（`boot-crc`）、0002（時鐘）、0003（特性測試）已完成，等你在板上驗證；刪重複文件請依 §1.4 的兩行指令手動做** | 基線數據入庫；G0 的 log 存檔；板上 smoke test 通過 |
| **P1 佈局資料化 + ABI v2** | 新增 `boot-layout`；`boot-journal`/`boot-update`/`boot-core`/`boot-image`/`xtask` 改吃 `&Layout`；`boot-abi` 位址常數搬走；ABI v2（§5.4）；移除 `version.workspace`（§6.6）；`.secrets`／`.keytable` 位址改由 `Layout` 提供 | host 測試全過且**測試語意不變**；H750 功能等價 |
| **P2 Flash 抽象 + 救援全 slot** | 新增 `boot-flash`（trait + `RamFlash`：不均勻 sector、memory-mapped 讀、bank swap 映射模擬）；`boot-nor` 實作它；journal/update 以 erase unit 為單位；`boot-rescue` 以 slot 相對偏移、整個 slot 擦寫，加入 `s`（status）指令骨架（§5.6、§5.7） | 斷電注入在「均勻 4KB」與「F429 式不均勻＋bank swap」兩組幾何上都通過；H750 救援可上傳 > 64KB 的 image |
| **P3 Platform 抽象與 bin 瘦身** | 新增 `boot-platform`、`boot-runtime`；`bootloader-h750/main.rs` 縮成板級 glue；`boot-stm32h7` 改名並分出 app-facing 子集，抽出 `boot-stm32-common`；目錄遷到 `boards/h750-netlite/`；`Platform::protection_status()` 先在 H750 實作（RDP） | H750 端到端與基線一致；`main.rs` ≤ 約 100 行 |
| **P4 執行模型與驗證管線** | `ExecModel::{RamLoad, XipRemap}`；`SlotLoader` 拆為 `ImageSource → Decrypt? → Sink → Hash → Verify`；`VerifyPolicy`（首次試開機 Quick）與 `KIND_VERIFIED` journal 記錄（CRC 範圍全包含）；`DeviceBinding` 選配；XIP 的 host 模擬（含 remap） | RamLoad 路徑輸出與基線**位元一致**；XIP 的 host 模擬可跑完 test→confirm→revert |
| **P5 NUCLEO-F429ZI**（G0 之後） | **5a** `boot-flash-stm32`（F4 模組）、`boot-soc-stm32f4`、時脈（先保守頻率）、USART3、LED/按鍵，**Layout 用 B128**；**5b** 單 slot XIP 開機；**5c** bank swap A/B（S-F1 結論落地）；**5d** OTA 寫入路徑（TBD-1：RWW 或 RAM 常駐，依 S-F2）；**5e** Quick/Full 驗證、救援全 slot、斷電測試；**5f** 依實測 size 決定是否切到 T64（TBD-5）；**5g** 量產檢查（`protection_status`、`xtask prod-check`、`s` 指令） | 板上完成 A→B→revert 與隨機斷電；Quick/Full 耗時入文件；量產 profile size 與佈局決定入 ADR |
| **P6 STM32 ABI 凍結與文件** | ABI v2 凍結檢查（plan T8.4）；`Application Boot API Guide` 改為多平台版；ADR 補齊；各板 size/stack 預算入 CI；`docs/compat.md` | ABI 凍結；H750 與 F429 的 CI 全綠 |
| **P7（之後）RT1061 / OK1061-S** | 拿到板子後：S-R1～S-R5，再實作 `boot-soc-imxrt10xx`、`boot-flash-flexspi`、boot header、FlexSPI remap、RAM 常駐 Flash 例程、cache、（BEE） | 同 P5 的板上驗收 |

### 風險 spike

**F429（G0 之後開始，可與 P1～P4 並行）**

| Spike | 內容 | 要回答的問題 |
|---|---|---|
| **S-F1** bank swap 行為 | 在 NUCLEO 上設 `UFB_MODE` 並跳轉；觀察 reset 後該位元的狀態、sector 編號與位址對應、ABI 表在副本上的可用性 | 映射是否真的不持久？Bootloader 雙副本是否可行？不如預期時要不要退回軟體 swap（TBD-4）？ |
| **S-F2** RWW 與 stall | 在 bank A 執行的碼擦/寫 bank B；量測 sector erase 時間（16/64/128KB 各一）、是否 stall、IWDG 是否被餵到 | §5.1「不需要 RAM 常駐」成立嗎？journal 換頁最壞阻塞多久（64KB／128KB sector）？ |
| **S-F3** 驗證耗時 | 量 768～896KB 的 SHA-256 + P-256 驗簽（Full）與 CRC32（Quick）；比較不同時脈 | Quick 的實際收益？Full 是否可接受？ |
| **S-F4** 64KB 可行性（N1） | **不需要 F429 板**：用 H750 Bootloader 的 size 組成（已取得，§7.2）扣掉 XChaCha20/HKDF/裝置金鑰派生，再加上 F4 的 Flash 驅動估計；之後在 F429 上以真實 build 驗證 | 量產 profile 能否 ≤ 約 56KB？是否切 T64（TBD-5）？ |

**RT1061（拿到 OK1061-S 後）**

| Spike | 內容 |
|---|---|
| S-R1 | 啟動鏈：FCB/IVT/boot header、板上 NOR 型號的 FlexSPI LUT 與時序、probe-rs 能否燒錄該 NOR |
| S-R2 | FlexSPI remap（`IOMUXC_GPR30/31/32`）：overlay 與 swap 形式的行為、與 IP 命令寫入、cache 的交互 |
| S-R3 | RAM 常駐 Flash 寫入：阻塞時間、IRQ 與 cache 處理 [TBD-1] |
| S-R4 | BEE/OTFAD 與 HAB：該晶片/板是否可用、金鑰與區域配置、ingest 寫密文的方式 [TBD-2] |
| S-R5 | `imxrt-rt` 與「被 Bootloader 跳入的 App」的相容性（boot header、FlexRAM 重設） |

### 現在就為 RT1061 與其他 STM32 預留、成本很低的擴充點（不實作）
`ExecModel::XipRemap { kind }`（`#[non_exhaustive]`）、`Layout.ramfunc`、`Flash::PROGRAM_IN_RAM`、ABI `caps` 位元、`Flash` 對 memory-mapped 讀取的支援、`RamFlash` 的 remap 模擬、`ProtectionStatus` 的 `#[non_exhaustive]`、`boards/` 目錄命名慣例。除此之外**不寫任何 RT1061 或其他 STM32 家族的專屬程式碼**。

## 10. 測試策略

- **同一套測試、多組 Layout**：把現有的整合測試與 proptest 參數化，對 `[H750_NETLITE, NUCLEO_F429ZI, IMXRT1061_STRAWMAN]` 三組 Layout 都跑一遍（Layout 是純資料，host 可直接引用）。
- **`RamFlash`**：支援「只能 1→0、erase 才能 0→1」、**不均勻 erase unit**、程式化對齊限制、memory-mapped 讀取、**位址重映射（bank swap／remap）**，以及在每個 program/erase 點注入斷電（現有 `RamNor` 的泛化）。
- **XIP 特有案例**：`ih_load_addr` 與 Layout 的 exec 位址不符；XIP 收到 `scheme=DEVICE` 的 image 一律拒絕；映射狀態與 journal 決策不一致（例如殘留的 UFB）；Quick 的 CRC 不符時降級為 Full；`BootInfo.boot_slot` 與實際映射不一致；同一份 image 放進 A 或 B 都能開機。
- **依賴規則檢查**：CI 以 `cargo tree`/`cargo deny` 腳本驗證 §6.2 的四條規則。
- **回歸基準**：P0 的 H750 `.bin` hash 與 log；P3、P4 重構後比對（P4 要求 RAM_LOAD 位元一致）。
- **HIL**：沿用 plan §12 的腳本，改成 `--board` 參數；F429 的 NUCLEO 板載 ST-LINK 讓 HIL 比較容易自動化。
- **Size/Stack 預算**：每個板子各自的預算進 Layout，CI 失敗即擋。

---

## 11. 風險

| ID | 風險 | 機率 | 影響 | 對策 |
|---|---|---|---|---|
| R-01 | F429 bank swap 行為與預期不符（映射持久性、sector 編號、ABI 副本） | 中 | 高 | S-F1 最先做；不成立時退回軟體 swap（TBD-4），計畫要重估 |
| R-02 | F429 RWW 不如文件所述（有使用者回報 stall） | 中 | 中 | S-F2；退回 RAM 常駐例程（TBD-1） |
| R-03 | RT1061 的 RAM 常駐 Flash 例程比預期複雜 | 高 | 高 | 已延後；S-R3 |
| R-04 | 服務呼叫期間 IRQ 遮蔽或 stall 影響 App 即時性 | 中 | 中 | `slot_erase_step`；文件標明各平台最壞阻塞時間 |
| R-05 | 重構破壞 H750 行為 | 中 | 高 | G0 以縮小範圍通過（§0.6），**板上基線只有「載入 App 執行」與「rescue 上傳」兩項**；其餘行為靠 host 特性測試固定（P0 補齊）；每階段結束做這兩項板上 smoke test；P3／P4 要求 RamLoad 輸出位元一致 |
| R-06 | Trait 過度抽象、為「可能的家族」增加 size 或難理解 | 中 | 中 | 靜態分派；「沒有板子就不寫變體」規則（§6.1）；PR 附 size 變化 |
| R-07 | `stm32-metapac` 晶片 feature 互斥導致 workspace 建置問題 | 高 | 低 | §6.5 的建置策略；CI 不用 `--workspace` |
| R-08 | Bootloader 雙副本（F429）使自更新（Phase F）與 Flash 預算變複雜 | 中 | 中 | 文件化「必須兩份一起更新」；T64 下副本 #2 剛好佔 S12–S15（64KB） |
| R-09 | Quick 驗證被誤解為安全機制 | 中 | 中 | README 安全能力表；`BlInfo.caps`；ADR 明文「Quick 僅偵測損毀」 |
| R-10 | **T64 的 64KB 不夠放量產 profile 的 Bootloader**（P-256 驗簽、journal、Flash 驅動、救援、選配傳輸加密） | 低 | 中 | 已取得組成分解：H750 等價物放進 T64 約有 10KiB 餘裕；`.rodata` 的 CRC 查表（已證實 16,384 B）可移除，餘裕約 25KiB（§7.2）；F4 的增減尚未量測；S-F4 先估；P5f 實測決定；不夠就留在 B128，`Layout` 切換成本很低 |
| R-11 | XIP 板無軟體儲存加密，與 H750 的安全承諾不一致造成產品混淆 | 中 | 中 | 每個 profile 在 README 列「安全能力表」；依賴的硬體保護以 §5.7 檢查 |
| R-12 | RT1061 的 FlexSPI remap 僅支援單一區段，限制 slot 配置 | 低 | 中 | §7.3 已以「slot A 範圍 = exec 視窗」配置；S-R2 確認 |
| R-13 | ABI v2 與未來 RT1061 需求再度衝突 | 中 | 中 | `caps`、`reserved`、`BlInfo` 欄位保留；凍結（P6）只針對 STM32 集合，RT1061 若需要變更走「只增不改」 |
| R-14 | G0 的 H750 測試延後，拖慢 F429 | 中 | 中 | 附錄 C 的清單先明確；F429 的 S-F4（不需要板子）與文件工作可先做 |
| R-15 | `prod-enforce` 設定錯誤導致量產裝置無法啟動 App | 低 | 高 | 預設關；救援路徑永遠可用；先以 `prod-flag` 觀察；量產前在 HIL 驗證 |
| R-16 | Bootloader 的 RAM 靜態變數若在 `init_sram4_ecc()` 之前初始化，會被清零（SRAM4 整個被寫成 0） | 中 | 中 | 規則：RAM 靜態變數一律在 `init_sram4_ecc()` 之後初始化；P3 的 `Platform::early_init()` 要把這個順序寫成文件與測試；TBD-1 的 RAM 常駐例程複製同樣遵守 |

## 12. 下一步行動

目前**沒有開放的決策問題**。P0 的三個 patch 都已完成（見 §9 與 §1.4）。順序如下：

| 順序 | 行動 | 負責 | 狀態／說明 |
|---|---|---|---|
| 1 | ✅ 驗證 `.rodata` 最大符號 | 你 | `CRC32_TABLE` = 16,384 B |
| 2 | ✅ 打基線 tag、提交 patch 0001 | 你 | 已回報提交 |
| 3 | **套用 patch 0002、0003** | 你 | `git am 0002-*.patch 0003-*.patch`；host 測試 `cargo test`（預期 49 個通過，含 `boot-time` 9、`boot-nor` 9、`boot-journal` 10、`boot-core` 10） |
| 4 | **板上 smoke test** | 你 | 載入 App 執行 + rescue 上傳。**特別觀察**：(a) UART 輸出與開機時間是否與之前一致；(b) 若方便，故意把 NOR 的 /CS 或 MISO 接壞，確認 Bootloader 現在會**逾時並進入錯誤路徑**而不是無限等待（選做）；(c) 貼上 `arm-none-eabi-size`（我這邊 `text` 36,928、`bss` 12） |
| 5 | 手動整理文件：`git rm rust-bootloader-plan.md && git add docs/plan.md` | 你 | 見 §1.4 |
| 6 | **P1：`boot-layout` 與 ABI v2** | 我 | 純重構；現在有 49 個 host 測試與黃金位元組當安全網 |
| 7 | F429 spike S-F1～S-F3 | 你（需要板子）＋我寫測試程式 | G0 已通過，可以開始；S-F4 的 H750 數據已齊 |

**補充**：沙盒有 Rust 1.91（host 測試）與 `arm-none-eabi-ld`（以 `-Zbuild-std` 交叉建置），所以 patch 都經過實際編譯、host 測試與 size 量測；**板上行為仍需你驗證**。

## 13. 替代方案（為什麼不選）

| 方案 | 說明 | 不選的理由 |
|---|---|---|
| **只用 Cargo feature 切換平台**（`#[cfg(feature = "h750")]` 散落各處） | 改動最小 | 第三個平台時 `cfg` 會爆炸；host 測試無法同時涵蓋多個平台；違反 §2 目標 2 |
| **一個巨大的 `Board` trait** | 簡單直觀 | 無法重用（同 SoC 不同板、同 Flash 後端不同 SoC）；每板重寫 |
| **每個板子一個 repo／fork** | 隔離最徹底 | 平台無關邏輯（journal、驗證、狀態機）的修正要同步 N 份；這些恰恰是最需要一致的部分 |
| **用 `dyn Trait` 做 runtime 分派** | 單一 binary 支援多板 | size 與 `unsafe`/vtable 在 128KB 預算下不划算；也沒有「同一 binary 跑多板」的需求 |
| **軟體 swap（MCUboot swap-move/scratch）實現單一位址** | 單一連結位址 | 硬體重映射（F429 bank swap、RT106x FlexSPI remap）已能達成單一位址，不需要驗證軟體 swap 的斷電恢復；僅在重映射不可用時作備援（TBD-4） |
| **每個 slot 各自連結（DIRECT_XIP 的 per-slot 建置）** | 不需 swap | 需要雙份建置與雙 payload OTA 封包；與 Q2「單一位址」不符 |
| **XIP 板不放 ABI 服務、更新一律在 Bootloader 內完成** | 避開「App 執行中寫 Flash」 | 失去 D-06「App 經 ABI 做 OTA」的核心價值；且 Bootloader 仍然是 XIP，同樣需要 RAM 例程 |

---

## 附錄 A：查證狀態與來源

### A.1 本版已查證（網路資料，仍建議最後對照原廠手冊）

| 項目 | 結論 | 來源 |
|---|---|---|
| OK1061-S 規格 | i.MX RT1061、Cortex-M7 528MHz、1MB on-chip SRAM（512KB 可配置為 TCM）、4MB QSPI NOR（可選 16MB）、SWD；廠商標示支援資料加密與 NXP-MCUBootUtility | forlinx.net 的 OK1061-S／FET1061-S 產品頁與 Product Brief |
| F429 2MB 為雙 bank | 社群資料稱「2MB 版永遠是雙 bank」；`stm32f4xx-hal` 的實作也只對 1MB 版依設定區分 | ST 社群討論串、docs.rs（stm32f4xx-hal `flash.rs`） |
| F42x/F43x 的 RWW | ST 員工引用 RM0090 §3.6：兩個 bank 允許一個 bank 讀的同時對另一個 bank 寫/擦。**但有使用者回報從 bank1 擦 bank2 仍 stall，未解** | ST 社群討論串 |
| F429 bank swap | `SYSCFG_MEMRMP.UFB_MODE`（bit 8）控制 bank1/bank2 映射於 `0x0800_0000` 與 `0x0810_0000`；**erase 的 sector 編號不會跟著 swap** | ST 社群討論串（引用 RM 暫存器說明） |
| RT106x FlexSPI remap | `IOMUXC_GPR30/31/32` 設定 start/end/offset；映射 `[start,end)` 內的 AHB 位址到 `addr+offset`；**只能作用於一個區段、4KB 對齊**；RT1061/1062 的 XIP 只能從 FlexSPI1（`0x6000_0000`）起 | NXP 社群（NXP 員工回覆）、痞子衡的 remap 文章 |
| NXP 的 flash remap 做法 | MCUboot DIRECT-XIP + remap，映像以 primary slot 位址建置，有 overlay 與 swap 兩種機制 | NXP MCUXpresso SDK 文件（flash remap readme） |

### A.2 仍須對照手冊確認（[待查證]）

**STM32F429ZI / NUCLEO-F429ZI**（RM0090、DS、UM1974）
- 各 sector 大小與 sector 編號對應；sector erase / word program 的典型與最壞時間（影響 IWDG 與 `slot_erase_step`）。
- `UFB_MODE` 的 reset 行為與 `BFB2` 的交互（S-F1 實測）。
- 最高時脈與 overdrive 條件、Flash wait states；NUCLEO 的 HSE 來源（ST-LINK MCO 8MHz bypass）。
- 向量表對齊要求。
- 板上腳位：USART3（PD8/PD9）、LED（PB0/PB7/PB14）、使用者按鍵（PC13）與 SB 跳線。
- RTC 備份暫存器、IWDG 行為；Option bytes：RDP、BOR、WRP/PCROP。

**i.MX RT1061 / OK1061-S**（RM、DS、OK1061-S 原理圖與廠商 SDK）
- RT1061 的安全功能實際有哪些（BEE、HAB、DCP 等）。
- Boot ROM 對 FlexSPI NOR 的 FCB/IVT 要求。
- FlexSPI remap 與 IP 命令寫入、cache 的交互；remap 暫存器的 reset 行為。
- FlexRAM 配置由誰設定、跳入 App 後能否保留。
- 握手暫存器、Watchdog 行為、FPU 與 Rust target。
- OK1061-S 板上 NOR 的確切型號與介面。

**通用**
- MCUboot `image.h` 的 `IMAGE_F_ROM_FIXED` 數值與語意、自訂 TLV 範圍是否衝突。
- `IMAGE_F_ENCRYPTED = 0x0001_0000` 是否與 MCUboot 後續版本衝突。

## 附錄 B：現行 `boot-abi` 常數的去向（P1 清單）

| 常數 | 去向 |
|---|---|
| `FLASH_BASE_ADDR`, `BL_INFO_ADDR`, `BOOT_API_ADDR`, `SECRETS_ADDR`, `KEYTABLE_ADDR`, `VECTOR_TABLE_SIZE` | `Layout`（每板） |
| `AXI_SRAM_BASE/SIZE`, `APP_LOAD_ADDR`, `APP_VTOR_OFFSET/ADDR` | `ExecModel::RamLoad`（H750） |
| `SRAM4_BASE/SIZE`, `SRAM4_BOOT_INFO_ADDR` | `Layout.boot_info_addr` + H7 SoC crate（SRAM4 初始化） |
| `NOR_JOURNAL_SECTOR_0/1`, `NOR_RESERVED_OFFSET`, `NOR_SLOT_A/B_OFFSET`, `NOR_SLOT_SIZE` | `Layout.journal/slots`（Flash 後端偏移） |
| `NOR_PAGE_SIZE`, `NOR_SECTOR_SIZE`, `NOR_BLOCK_SIZE` | Flash 後端（`boot-flash-spinor` 內部常數 / `Flash::unit_at`） |
| `TAMP_CMD_*`, `BOOT_REASON_*`, `SLOT_STATE_*`, `ERR_*`, `BOOT_SLOT_*` | **留在 `boot-abi`**（平台無關的 ABI 語意） |
| `IoOps`, `BootApi`, `BootInfo`, `SlotInfo`, `BlInfo`, `ImageVersion` | 留在 `boot-abi`，依 §5.4 修改 |

## 附錄 C：H750 驗收清單（G0）

> 依 plan.md 的 DoD 與目前程式碼整理。目的：定義「重構前行為」的基線。每項請記錄日期、版本（commit）、結果與 log。

**G0 結果（你的回報，2026-11-12）**：✅ C.1 冷開機載入並執行 App；✅ C.4 UART rescue 上傳 image。其餘項目（C.2 revert／confirm 全流程、C.3 ABI 服務、C.5 負向、C.6 斷電、C.4 的其他救援入口）**暫不測試**，視為「未驗證」。**決定：G0 以縮小範圍通過**（§0.6）。

> 若之後想低成本補測，兩項最划算：(a) 把已簽章 image 的任一 byte 翻轉後經 rescue 上傳，預期 Bootloader 拒絕並進入救援；(b) 建一個「不呼叫 `confirm()`」的 App 變體，預期約 3 秒 IWDG 重啟後回到已確認的 slot。**不要求，也不影響 G0 的結論。**

**C.1 開機與載入**
- [ ] 冷開機：Bootloader banner → NOR JEDEC ID 正確 → 載入 slot → App 在 AXI SRAM 執行（UART 有 App 輸出、LED 閃、`tick` 遞增）。
- [ ] 記錄 Bootloader 的 `.bin` size、hash，以及載入 N KB image 的時間（E3）。（已知：ELF `arm-none-eabi-size` 為 text 51,508 B、data 0、bss 0；仍需記錄 hash 與載入時間。）

**C.2 A/B、test boot、confirm、revert**
- [ ] 以 UART 救援把簽章 image 寫入 slot 0／slot 1，`mark` 為 Pending，重開機後進入 Testing 並由 App `confirm()`。
- [ ] 不 confirm：IWDG（3s）重啟、trial 計數增加、超過上限後**回到已 Confirmed 的 slot**。
- [ ] 兩個 slot 皆為 Confirmed 時依版本選擇較新者。

**C.3 ABI（App 在 RAM 執行時呼叫 Flash 服務）**
- [ ] `get_boot_info`、`slot_status`、`slot_erase`、`slot_write`、`verify_slot`、`mark_pending`、`confirm`、`request_rescue`、`system_reset` 皆能呼叫成功。
- [ ] 對 **active slot** 的 erase/write 被拒（`ERR_ACTIVE_SLOT`）；範圍外寫入被拒。
- [ ] App 以 `IoOps` 提供 SPI 時，`millis`／`yield_` 行為正常（App 端有 SysTick）。

**C.4 救援**
- [ ] 按住 KEY2 開機進入救援；`request_rescue` 設 TAMP 旗標後重開機進入救援；兩個 slot 皆失效時自動進入救援。
- [ ] 救援指令 `p`／`i`／`e`／`w`／`m`／`r` 皆可用。

**C.5 負向測試（plan T2.7）**
- [ ] 竄改 header／image／TLV 任一 byte、截斷、錯誤金鑰、`BOARD_ID` 不符、`load_addr` 越界 → 一律拒絕且 RAM 被清零。
- [ ] 註：目前 `main.rs` 以 `require_encrypted = false` 呼叫 `load_and_verify`，因此**未加密 image 不會被拒**。請確認這是開發階段的預期行為並記錄。

**C.6 斷電**
- [ ] 在更新寫入、`mark`、journal 換頁等時機斷電（手動或 HIL），重開機後狀態合法（至少有可啟動 slot 或進入救援）。

**C.7 已知缺口（記錄，不在 G0 修復；多數會在後續階段處理）**
- [ ] 救援 `e` 每個 slot 只擦第一個 64KB block（README 已載明）→ P2 修。
- [ ] ABI 的 `update_init/feed/abort` 是空殼 → M5 工作。
- [ ] Bootloader 的 `local_millis()` 恆為 0，NOR `wait_ready` 逾時在 Bootloader 內不會觸發 → **P0 修**。
- [ ] `jump_to_app` 未做 cache／`CONTROL` 處理（目前未啟用 cache）→ P3 的 `prepare_and_jump`。
- [ ] 救援 `m` 寫死版本號 `1.0.0+1` → P2 修。
- [ ] 開發金鑰與固定測試簽章金鑰仍在使用 → §5.7 的量產檢查處理。

## 附錄 D：STM32 家族差異備忘（N4，供「保留最大可能性」；全部 [待查證]）

> 憑記憶整理，**只用來確認介面不會擋住未來的家族**，不是實作依據。動工前逐項對照各家族 RM。

| 家族 | Flash 組織（印象） | 程式化單位 | 雙 bank／bank swap（印象） | 對介面的提醒 |
|---|---|---|---|---|
| F4（F42x/F43x） | sector 16/64/128KB 不均勻；雙 bank | 8/16/32/64 bit（依電壓與 PSIZE） | `SYSCFG_MEMRMP.UFB_MODE`（已查）；erase 的 sector 編號不 swap（已查） | `Flash::unit_at` 必須支援不均勻 |
| F7（部分型號） | sector 不均勻；部分型號可選雙 bank | 8/16/32/64 bit | 以選項位元切換，機制與 F4 不同 | `BankSwapVariant` 要能表示「由 option byte 切換」 |
| L4／G4／L5 | 2KB 或 4KB page；多數為雙 bank | **64 bit（雙字）**；program 前通常要求目標為已擦除 | `SYSCFG_MEMRMP.FB_MODE` 一類 | `PROGRAM_ALIGN = 8`；journal record 大小與對齊要檢查 |
| H7（雙 bank 型號） | 128KB sector；雙 bank | **256 bit flash word（32B）+ ECC** | `FLASH_OPTSR` 的 swap 位元（option byte，需重啟生效？） | 與現有 H750 內部 Flash 觀念相同（只能寫一次、ECC）；swap 若要重啟才生效，`select_mapping` 的語意要能處理 |
| U5／H5 | 8KB page；雙 bank | 128 bit | `SWAP_BANK` 一類 | `PROGRAM_ALIGN = 16`；可能帶 TrustZone，不在本案範圍 |

**這張表對目前設計的唯一要求**：(1) `PROGRAM_ALIGN` 是 trait 常數，不假設 1 或 4；(2) `unit_at` 不假設均勻；(3) `select_mapping` 的回傳能表示「需要 reset 才生效」；(4) `RemapKind::BankSwap` 帶 `#[non_exhaustive]` 的變體資訊。其餘全部等有板子再做。
