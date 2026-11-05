MEMORY
{
  FLASH (rx) : ORIGIN = 0x08000000, LENGTH = 128K
  RAM (rwx)  : ORIGIN = 0x38000100, LENGTH = 64K - 0x100
}

_stext = 0x08000600;

SECTIONS
{
  .bl_info 0x08000400 :
  {
    KEEP(*(.bl_info));
  } > FLASH

  .boot_api 0x08000440 :
  {
    KEEP(*(.boot_api));
  } > FLASH

  .secrets 0x0801F000 :
  {
    KEEP(*(.secrets));
  } > FLASH

  .keytable 0x0801F800 :
  {
    KEEP(*(.keytable));
  } > FLASH
} INSERT AFTER .rodata;
