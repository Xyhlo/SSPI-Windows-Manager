#define RX_IMPORT(module, symbol, name) \
    .section .bss; .balign 8; .globl rx_import_##symbol; .hidden rx_import_##symbol; \
    rx_import_##symbol: .zero 8; \
    .section .text; .globl symbol; .hidden symbol; .type symbol,@function; \
    symbol: jmp *rx_import_##symbol(%rip); .size symbol, .-symbol;
#include "imports.inc"
.section .note.GNU-stack,"",@progbits
