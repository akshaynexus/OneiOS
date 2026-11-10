/* The test kernel: linked high, like XNU, so that running it proves the loader's slide. */
ENTRY(_start)
SECTIONS {
  . = 0xfffffff007004000;
  __base = .;
  . += 0x1000;                 /* the Mach-O header page mkkernel writes */
  .text : { *(.text.start) *(.text .text.*) *(.rodata .rodata.*) }
  .data : { *(.data .data.*) }
  . = ALIGN(0x4000);
  __bss_start = .;
  .bss : { *(.bss .bss.* COMMON) }
  . = ALIGN(0x4000);
  __bss_end = .;
  /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) *(.ARM.*) *(.gnu*) *(.got*) }
}
