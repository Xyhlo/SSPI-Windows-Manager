.section .text.entry,"ax",@progbits
.global _start
.hidden _start
.type _start,@function
_start:
    /* Accept a normal BinLoader thread/function call; never read argc from rdi. */
    push %rbp
    mov %rsp, %rbp
    and $-16, %rsp
    cld
    lea __bss_start(%rip), %rdi
    lea __bss_end(%rip), %rcx
    sub %rdi, %rcx
    xor %eax, %eax
    rep stosb

    /* Simple ELF loaders copy PT_LOAD without resolving relocations. */
    lea __payload_base(%rip), %r8
    lea __rela_start(%rip), %rsi
    lea __rela_end(%rip), %rdi
1:
    cmp %rdi, %rsi
    jae 2f
    cmpq $8, 8(%rsi) /* R_X86_64_RELATIVE, with symbol index zero */
    jne 3f
    mov (%rsi), %rdx
    mov 16(%rsi), %rax
    add %r8, %rax
    mov %rax, (%r8,%rdx)
    add $24, %rsi
    jmp 1b
2:
    call payload_bootstrap
    leave
    ret
3:
    mov $-1, %eax
    leave
    ret
.size _start, .-_start

/* GoldHEN's dynlib patches expose these bootstrap syscalls to injected payloads. */
.section .text,"ax",@progbits
.global payload_dlsym
.hidden payload_dlsym
.type payload_dlsym,@function
payload_dlsym:
    mov $591, %rax
    mov %rcx, %r10
    syscall
    jnc 4f
    neg %eax
4:  ret
.size payload_dlsym, .-payload_dlsym

.global payload_load_module
.hidden payload_load_module
.type payload_load_module,@function
payload_load_module:
    mov $594, %rax
    mov %rcx, %r10
    syscall
    jnc 5f
    neg %eax
5:  ret
.size payload_load_module, .-payload_load_module
.section .note.GNU-stack,"",@progbits
