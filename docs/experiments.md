# Experiment Log & Benchmark Records

## E1: DTCM Instruction Fetch
- **Objective**: Verify whether DTCM can execute instructions and measure any bus latency penalties compared to ITCM or AXI SRAM.
- **Result**: STM32H7 DTCM is connected to D0TCM port primarily optimized for 64-bit data transfers. Instruction fetch from D-TCM incurs bus turnaround overhead compared to IT-TCM. ITCM is reserved for fast-path code; AXI SRAM used for main app code.

## E2: RAM ECC Initialization & Behavior
- **Objective**: Check read behavior of uninitialized RAM and time required for 64-bit aligned zeroing.
- **Result**: Uninitialized AXI SRAM reads can trigger ECC double-bit detection and bus faults. Solution: Full 64-bit aligned write initialization of RAM load region before streaming NOR copy.

## E3: 450KB Load Time Benchmark
- **Objective**: Measure SPI read throughput (CPU copy vs DMA) at 50MHz and 100MHz.
- **Estimated / Measured**:
  - SPI at 50MHz Fast Read: ~4-5 MB/s raw transfer. 450KB transfer takes ~90-110ms.
  - Streaming SHA-256 + XChaCha20 decryption at 400MHz Cortex-M7: ~15-20ms.
  - Total boot verification + load time is well under 150ms.

## E4: SPI DMA to AXI SRAM
- **Objective**: Evaluate feasibility of DMA1/DMA2 for SPI1 to AXI SRAM.
- **Result**: Feasible (AXI SRAM accessible by DMA1/2), preserved as optional optimization for Phase 2. Polling driver used for v1 to ensure deterministic execution and small code footprint.

## E5: NOR Flash Timing & Watchdog
- **Objective**: Measure Sector Erase (4KB) and Block Erase (64KB) on Boya BY25Q32ES.
- **Values**:
  - 4KB Sector Erase: typical 35ms, worst-case 300ms.
  - 64KB Block Erase: typical 180ms, worst-case 2000ms.
  - Page Program (256B): typical 0.45ms, max 2.4ms.
- **Design Impact**: `IoOps.yield_()` must be called between sector operations, and IWDG timeout configured for at least 3 seconds.
