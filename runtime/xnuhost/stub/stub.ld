/* The stub as a flat blob linked at 0: position independent, entry at offset 0. */
ENTRY(xs_entry)
SECTIONS {
  . = 0;
  __xs_start = .;
  .text : { *(.text.entry) *(.text .text.*) *(.rodata .rodata.*) }
  __xs_end = .;
  /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) *(.ARM.*) *(.gnu*) *(.data .data.* .bss .bss.* COMMON) }
}
