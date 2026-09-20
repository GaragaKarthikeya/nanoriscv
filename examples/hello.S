# Prints through the 16550 UART the way a bare-metal console driver does:
# spin until the transmit holding register is empty, then write a byte.
        .section .text.init
        .globl _start
_start:
        li      s0, 0x10000000          # UART base
        la      s1, message
1:      lbu     a0, 0(s1)
        beqz    a0, done
2:      lbu     t0, 5(s0)               # line status
        andi    t0, t0, 0x20            # transmitter holding register empty
        beqz    t0, 2b
        sb      a0, 0(s0)
        addi    s1, s1, 1
        j       1b
done:
        li      a7, 93
        li      a0, 0
        ecall
        .section .rodata
message:
        .asciz  "hello from nanoriscv\n"
