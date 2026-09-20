//! Exceptions, numbered per the privileged spec's mcause encoding.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    InstructionAccessFault(u64),
    IllegalInstruction(u32),
    Breakpoint,
    LoadAddressMisaligned(u64),
    LoadAccessFault(u64),
    StoreAddressMisaligned(u64),
    StoreAccessFault(u64),
    EnvironmentCall,
}

impl Exception {
    pub fn cause(&self) -> u64 {
        match self {
            Exception::InstructionAccessFault(_) => 1,
            Exception::IllegalInstruction(_) => 2,
            Exception::Breakpoint => 3,
            Exception::LoadAddressMisaligned(_) => 4,
            Exception::LoadAccessFault(_) => 5,
            Exception::StoreAddressMisaligned(_) => 6,
            Exception::StoreAccessFault(_) => 7,
            // Machine mode is the only mode implemented so far.
            Exception::EnvironmentCall => 11,
        }
    }

    /// The value that lands in `mtval` when this exception traps.
    pub fn tval(&self) -> u64 {
        match self {
            Exception::InstructionAccessFault(a)
            | Exception::LoadAddressMisaligned(a)
            | Exception::LoadAccessFault(a)
            | Exception::StoreAddressMisaligned(a)
            | Exception::StoreAccessFault(a) => *a,
            Exception::IllegalInstruction(i) => *i as u64,
            _ => 0,
        }
    }
}
