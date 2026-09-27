ENTRY(_start)
PHDRS
{
    text PT_LOAD FLAGS(5);
    data PT_LOAD FLAGS(6);
    dynamic PT_DYNAMIC FLAGS(6);
}
SECTIONS
{
    __payload_base = 0;
    . = 0x100;
    .text : { KEEP(*(.text.entry)) *(.text .text.*) } :text
    .rodata : { *(.rodata .rodata.*) } :text
    .dynsym : { *(.dynsym) } :text
    .dynstr : { *(.dynstr) } :text
    .gnu.hash : { *(.gnu.hash) } :text
    .hash : { *(.hash) } :text
    . = ALIGN(0x4000);
    .data : { *(.data .data.*) *(.got .got.*) } :data
    .dynamic : { *(.dynamic) } :data :dynamic
    .rela.dyn : {
        __rela_start = .;
        *(.rela.dyn)
        __rela_end = .;
    } :data
    .bss (NOLOAD) : {
        __bss_start = .;
        *(.bss .bss.*) *(COMMON)
        __bss_end = .;
    } :data
    /DISCARD/ : { *(.eh_frame .eh_frame_hdr .comment .note*) }
}
