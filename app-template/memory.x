MEMORY
{
  /* App vector table and executable code in AXI SRAM */
  FLASH (rx) : ORIGIN = 0x24000400, LENGTH = 512K - 0x400
  /* App data, bss, and stack in DTCM */
  RAM (rwx)  : ORIGIN = 0x20000000, LENGTH = 128K
}
