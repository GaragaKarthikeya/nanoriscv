//! The Supervisor Binary Interface: the calls a RISC-V kernel makes into the
//! firmware beneath it.
//!
//! On real hardware this is OpenSBI running in machine mode. Here the
//! emulator plays that part itself, so the kernel is entered directly in
//! supervisor mode and its `ecall`s are answered in Rust. That removes a whole
//! firmware binary from the boot path, and with it a second thing that can be
//! wrong while the first one is being debugged.
//!
//! Spec: <https://github.com/riscv-non-isa/riscv-sbi-doc>

use crate::cpu::Cpu;
use crate::csr::{self, int};

// Extension IDs. The modern ones spell a name in ASCII.
const EXT_BASE: u64 = 0x10;
const EXT_TIME: u64 = 0x5449_4D45; // "TIME"
const EXT_IPI: u64 = 0x0073_5049; // "sPI"
const EXT_RFENCE: u64 = 0x5246_4E43; // "RFNC"
const EXT_HSM: u64 = 0x0048_534D; // "HSM"
const EXT_SRST: u64 = 0x5352_5354; // "SRST"
const EXT_DBCN: u64 = 0x4442_434E; // "DBCN"

// The legacy extensions, from before calls returned a pair. A kernel still
// uses these for `earlycon=sbi`, which is the console that works before any
// driver is probed -- and therefore the one that shows early boot failures.
const LEGACY_SET_TIMER: u64 = 0x00;
const LEGACY_CONSOLE_PUTCHAR: u64 = 0x01;
const LEGACY_CONSOLE_GETCHAR: u64 = 0x02;
const LEGACY_CLEAR_IPI: u64 = 0x03;
const LEGACY_SEND_IPI: u64 = 0x04;
const LEGACY_REMOTE_FENCE_I: u64 = 0x05;
const LEGACY_REMOTE_SFENCE_VMA: u64 = 0x06;
const LEGACY_REMOTE_SFENCE_VMA_ASID: u64 = 0x07;
const LEGACY_SHUTDOWN: u64 = 0x08;

/// What a call returns: an error code in a0 and a value in a1.
pub struct SbiRet {
    pub error: i64,
    pub value: u64,
}

impl SbiRet {
    fn ok(value: u64) -> SbiRet {
        SbiRet { error: 0, value }
    }
    fn success() -> SbiRet {
        SbiRet { error: 0, value: 0 }
    }
    fn not_supported() -> SbiRet {
        SbiRet {
            error: -2,
            value: 0,
        }
    }
}

impl Cpu {
    /// Handles one `ecall` from supervisor mode.
    ///
    /// Returns false if the guest asked to shut down, which is the only call
    /// that does not return to the caller.
    pub fn handle_sbi(&mut self) -> bool {
        let (eid, fid) = (self.regs[17], self.regs[16]); // a7, a6
        let args = [self.regs[10], self.regs[11], self.regs[12]]; // a0, a1, a2

        // The legacy calls predate the error/value split and return a single
        // value in a0, so they are answered separately.
        if eid < 0x10 {
            match eid {
                LEGACY_SET_TIMER => {
                    self.set_timer(args[0]);
                    self.regs[10] = 0;
                }
                LEGACY_CONSOLE_PUTCHAR => {
                    self.mem.uart.write(0, args[0]);
                    self.regs[10] = 0;
                }
                LEGACY_CONSOLE_GETCHAR => {
                    // -1 means "nothing waiting", which is how a polling
                    // console driver knows to come back later.
                    self.regs[10] = match self.mem.uart.take_input() {
                        Some(b) => b as u64,
                        None => (-1i64) as u64,
                    };
                }
                LEGACY_SHUTDOWN => return false,
                // IPIs and remote fences are nothing to do on one hart with
                // no caches to keep coherent.
                LEGACY_CLEAR_IPI
                | LEGACY_SEND_IPI
                | LEGACY_REMOTE_FENCE_I
                | LEGACY_REMOTE_SFENCE_VMA
                | LEGACY_REMOTE_SFENCE_VMA_ASID => self.regs[10] = 0,
                _ => self.regs[10] = (-2i64) as u64,
            }
            return true;
        }

        let ret = match (eid, fid) {
            (EXT_BASE, 0) => SbiRet::ok(2 << 24), // spec version 2.0
            // Implementation id 3 is "rustsbi"; there is no number for "a
            // simulator's own", and claiming OpenSBI would be a lie a kernel
            // could act on.
            (EXT_BASE, 1) => SbiRet::ok(3),
            (EXT_BASE, 2) => SbiRet::ok(1),
            (EXT_BASE, 3) => {
                let supported = matches!(
                    args[0],
                    EXT_BASE
                        | EXT_TIME
                        | EXT_IPI
                        | EXT_RFENCE
                        | EXT_HSM
                        | EXT_SRST
                        | EXT_DBCN
                        | LEGACY_SET_TIMER
                        | LEGACY_CONSOLE_PUTCHAR
                        | LEGACY_CONSOLE_GETCHAR
                        | LEGACY_SHUTDOWN
                );
                SbiRet::ok(supported as u64)
            }
            (EXT_BASE, 4..=6) => SbiRet::ok(0), // vendor, arch and impl ids

            (EXT_TIME, 0) => {
                self.set_timer(args[0]);
                SbiRet::success()
            }

            // One hart: an IPI to oneself is already delivered, and there are
            // no remote caches or TLBs to shoot down.
            (EXT_IPI, 0) => SbiRet::success(),
            (EXT_RFENCE, _) => SbiRet::success(),

            // Hart state management. Hart 0 is the only hart and it is
            // already started, so there is nothing to start and nothing to
            // report but "started".
            (EXT_HSM, 2) => SbiRet::ok(0),
            (EXT_HSM, _) => SbiRet::not_supported(),

            (EXT_SRST, 0) => return false,

            // The debug console. console_write takes a buffer in supervisor
            // memory, so the address has to be translated before it is read.
            (EXT_DBCN, 0) => {
                let (len, addr) = (args[0], args[1]);
                for i in 0..len {
                    match self.read_guest_byte(addr + i) {
                        Some(b) => self.mem.uart.write(0, b as u64),
                        None => break,
                    }
                }
                SbiRet::ok(len)
            }
            (EXT_DBCN, 2) => {
                self.mem.uart.write(0, args[0]);
                SbiRet::success()
            }

            _ => SbiRet::not_supported(),
        };

        self.regs[10] = ret.error as u64;
        self.regs[11] = ret.value;
        true
    }

    /// Schedules the next supervisor timer interrupt.
    ///
    /// The CLINT belongs to machine mode, which the kernel cannot see, so the
    /// firmware owns `mtimecmp` and turns the expiry into `mip.STIP`. Setting
    /// a timer also retracts any interrupt already pending, which is how the
    /// kernel cancels one it no longer wants.
    fn set_timer(&mut self, when: u64) {
        self.mem.clint.mtimecmp = when;
        self.timer_armed = true;
        self.csrs.clear_bits(csr::MIP, int::STIP);
    }
}
