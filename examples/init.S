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

	/* Power the machine off: the kernel turns this into an SBI system
	   reset, which the emulator answers by stopping. The whole path --
	   userspace to kernel to firmware to emulator -- closes here. */
	li	a7, 142			/* __NR_reboot */
	li	a0, 0xfee1dead		/* LINUX_REBOOT_MAGIC1 */
	li	a1, 672274793		/* LINUX_REBOOT_MAGIC2 */
	li	a2, 0x4321fedc		/* LINUX_REBOOT_CMD_POWER_OFF */
	li	a3, 0
	ecall

	/* Only reached if the reboot was refused. */
	li	a7, 93			/* __NR_exit */
	li	a0, 0
	ecall
