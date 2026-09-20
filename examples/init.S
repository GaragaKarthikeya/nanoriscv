/*
 * The smallest possible Linux userspace: a static binary that writes to
 * stdout and exits. No libc, so the syscalls are made directly.
 *
 * Reaching this proves the whole path works -- the kernel booted, mounted
 * the initramfs, created a process, and returned to user mode.
 */
	.section .rodata
message:
	.ascii	"\n*** userspace is alive on nanoriscv ***\n\n"
message_end:
	/* li needs an absolute value, so the length is folded here, where both
	   labels are in the same section and the difference is known. */
	.equ	message_len, message_end - message

	.section .text
	.globl _start
_start:
	li	a7, 64			/* __NR_write */
	li	a0, 1			/* stdout */
	la	a1, message
	li	a2, message_len
	ecall

	li	a7, 93			/* __NR_exit */
	li	a0, 0
	ecall
