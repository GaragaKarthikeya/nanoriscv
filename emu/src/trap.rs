//! Exceptions and interrupts, numbered per the privileged spec's cause encoding.

/// Privilege modes. The numeric values are the ones the spec uses in `MPP`
/// and `SPP`, and the ordering is what makes "at least as privileged as"
/// a comparison rather than a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priv {
    User = 0,
    Supervisor = 1,
    Machine = 3,
}

impl Priv {
    pub fn from_bits(v: u64) -> Priv {
        match v & 0x3 {
            0 => Priv::User,
            1 => Priv::Supervisor,
            _ => Priv::Machine,
        }
    }
}

/// What a memory access is for. Each kind has its own fault and page-fault
/// cause, so the access type has to travel with the address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Fetch,
    Load,
    Store,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    InstructionAddressMisaligned(u64),
    InstructionAccessFault(u64),
    IllegalInstruction(u32),
    Breakpoint,
    LoadAddressMisaligned(u64),
    LoadAccessFault(u64),
    StoreAddressMisaligned(u64),
    StoreAccessFault(u64),
    /// The cause depends on the mode the ECALL was executed in, which is why
    /// this carries no number of its own.
    EnvironmentCall,
    InstructionPageFault(u64),
    LoadPageFault(u64),
    StorePageFault(u64),
}

impl Exception {
    /// The value for `mcause`/`scause`. ECALL needs the current privilege
    /// mode, since it has a distinct cause per mode.
    pub fn cause(&self, from: Priv) -> u64 {
        match self {
            Exception::InstructionAddressMisaligned(_) => 0,
            Exception::InstructionAccessFault(_) => 1,
            Exception::IllegalInstruction(_) => 2,
            Exception::Breakpoint => 3,
            Exception::LoadAddressMisaligned(_) => 4,
            Exception::LoadAccessFault(_) => 5,
            Exception::StoreAddressMisaligned(_) => 6,
            Exception::StoreAccessFault(_) => 7,
            Exception::EnvironmentCall => match from {
                Priv::User => 8,
                Priv::Supervisor => 9,
                Priv::Machine => 11,
            },
            Exception::InstructionPageFault(_) => 12,
            Exception::LoadPageFault(_) => 13,
            Exception::StorePageFault(_) => 15,
        }
    }

    /// The value that lands in `mtval`/`stval` when this exception traps.
    pub fn tval(&self) -> u64 {
        match self {
            Exception::InstructionAddressMisaligned(a)
            | Exception::InstructionAccessFault(a)
            | Exception::LoadAddressMisaligned(a)
            | Exception::LoadAccessFault(a)
            | Exception::StoreAddressMisaligned(a)
            | Exception::StoreAccessFault(a)
            | Exception::InstructionPageFault(a)
            | Exception::LoadPageFault(a)
            | Exception::StorePageFault(a) => *a,
            Exception::IllegalInstruction(i) => *i as u64,
            Exception::Breakpoint | Exception::EnvironmentCall => 0,
        }
    }

    /// Builds the access fault appropriate to what the access was for.
    pub fn access_fault(access: Access, addr: u64) -> Exception {
        match access {
            Access::Fetch => Exception::InstructionAccessFault(addr),
            Access::Load => Exception::LoadAccessFault(addr),
            Access::Store => Exception::StoreAccessFault(addr),
        }
    }

    /// Builds the page fault appropriate to what the access was for.
    pub fn page_fault(access: Access, addr: u64) -> Exception {
        match access {
            Access::Fetch => Exception::InstructionPageFault(addr),
            Access::Load => Exception::LoadPageFault(addr),
            Access::Store => Exception::StorePageFault(addr),
        }
    }
}

/// Interrupt cause numbers. In `mcause` these appear with the top bit set,
/// which is what distinguishes an interrupt from an exception.
pub mod interrupt {
    pub const SUPERVISOR_SOFTWARE: u64 = 1;
    pub const MACHINE_SOFTWARE: u64 = 3;
    pub const SUPERVISOR_TIMER: u64 = 5;
    pub const MACHINE_TIMER: u64 = 7;
    pub const SUPERVISOR_EXTERNAL: u64 = 9;
    pub const MACHINE_EXTERNAL: u64 = 11;

    /// The bit set in `mcause` for an interrupt rather than an exception.
    pub const FLAG: u64 = 1 << 63;
}
