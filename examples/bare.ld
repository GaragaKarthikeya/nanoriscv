/* Links a bare-metal image at the reset address.
 *
 * Without this, the toolchain places the ELF headers in the first loadable
 * segment, which lands below DRAM_BASE and the loader rejects it.
 */
OUTPUT_ARCH("riscv")
ENTRY(_start)
SECTIONS {
  . = 0x80000000;
  .text   : { *(.text.init) *(.text) }
  .rodata : { *(.rodata*) }
  .data   : { *(.data*) }
  .bss    : { *(.bss*) }
}
