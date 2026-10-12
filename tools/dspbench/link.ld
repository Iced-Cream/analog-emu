SECTIONS {
  . = 0x40400000;
  .text : { *(.text.bench_init) *(.text*) }
  .rodata : { *(.rodata*) }
  .data : { *(.data*) }
  .bss : { *(.bss*) *(COMMON) }
  /DISCARD/ : { *(.eh_frame*) *(.comment) }
}
