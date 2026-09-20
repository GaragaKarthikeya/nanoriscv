//! The platform-level interrupt controller, in the SiFive layout that QEMU's
//! `virt` machine and most Linux device trees expect.
//!
//! The PLIC routes device interrupts to *contexts* -- a hart at a privilege
//! level -- and arbitrates between them by priority. The part worth
//! understanding is the claim/complete handshake: a handler claims an
//! interrupt, which atomically tells it which device fired and stops that
//! source from interrupting again, and completes it once the device has been
//! serviced. Without the second half a level-triggered device would re-raise
//! the instant the handler returned.

pub const PLIC_BASE: u64 = 0x0C00_0000;
pub const PLIC_SIZE: u64 = 0x0400_0000;

/// Sources are numbered from 1; 0 means "no interrupt" in a claim.
pub const MAX_SOURCES: u32 = 32;
/// Context 0 is hart 0 in machine mode, context 1 is hart 0 in supervisor
/// mode. That is the arrangement a single-hart device tree describes.
pub const CONTEXTS: usize = 2;
pub const CONTEXT_MACHINE: usize = 0;
pub const CONTEXT_SUPERVISOR: usize = 1;

const PENDING_BASE: u64 = 0x1000;
const ENABLE_BASE: u64 = 0x2000;
const ENABLE_STRIDE: u64 = 0x80;
const CONTEXT_CONTROL_BASE: u64 = 0x20_0000;
const CONTEXT_STRIDE: u64 = 0x1000;

pub struct Plic {
    /// Per-source priority; 0 means "never interrupt".
    priority: [u32; MAX_SOURCES as usize],
    /// The current state of each device's interrupt line. Sources here are
    /// level-triggered, so this is what the device is asserting *now* rather
    /// than a latched event.
    levels: u32,
    /// Sources claimed by a handler but not yet completed. A claimed source
    /// stops contributing to pending even while its line is still high.
    claimed: u32,
    enable: [u32; CONTEXTS],
    threshold: [u32; CONTEXTS],
}

impl Default for Plic {
    fn default() -> Self {
        Plic {
            priority: [0; MAX_SOURCES as usize],
            levels: 0,
            claimed: 0,
            enable: [0; CONTEXTS],
            threshold: [0; CONTEXTS],
        }
    }
}

impl Plic {
    /// Sets or clears a device's interrupt line.
    pub fn set_level(&mut self, source: u32, asserted: bool) {
        if source == 0 || source >= MAX_SOURCES {
            return;
        }
        if asserted {
            self.levels |= 1 << source;
        } else {
            self.levels &= !(1 << source);
        }
    }

    /// Sources that are asserting and have not been claimed.
    fn pending(&self) -> u32 {
        self.levels & !self.claimed
    }

    /// The highest-priority source this context should take, or 0 for none.
    ///
    /// Ties go to the lowest source number, which is what the spec requires
    /// so that arbitration is deterministic.
    fn best(&self, context: usize) -> u32 {
        let candidates = self.pending() & self.enable[context];
        let mut best = 0;
        let mut best_priority = self.threshold[context];
        for source in 1..MAX_SOURCES {
            if candidates & (1 << source) == 0 {
                continue;
            }
            // Strictly greater: a priority equal to the threshold is masked.
            if self.priority[source as usize] > best_priority {
                best = source;
                best_priority = self.priority[source as usize];
            }
        }
        best
    }

    /// Whether this context has an interrupt waiting, which is what drives
    /// the external-interrupt bit in `mip`.
    pub fn is_pending(&self, context: usize) -> bool {
        self.best(context) != 0
    }

    /// Claims the highest-priority interrupt for a context.
    ///
    /// This is a read with a side effect by design: it hands back the source
    /// number and marks it claimed in one step, so two harts racing on the
    /// same interrupt cannot both service it.
    fn claim(&mut self, context: usize) -> u32 {
        let source = self.best(context);
        if source != 0 {
            self.claimed |= 1 << source;
        }
        source
    }

    /// Marks a source serviced. If its line is still asserted it becomes
    /// pending again immediately, which is correct for a level-triggered
    /// device that has not been quietened.
    fn complete(&mut self, source: u32) {
        if source < MAX_SOURCES {
            self.claimed &= !(1 << source);
        }
    }

    pub fn read(&mut self, offset: u64) -> u64 {
        match offset {
            o if o < PENDING_BASE => {
                let source = (o / 4) as usize;
                self.priority.get(source).copied().unwrap_or(0) as u64
            }
            o if (PENDING_BASE..ENABLE_BASE).contains(&o) => {
                // Only the first word exists with 32 sources.
                if o == PENDING_BASE {
                    self.pending() as u64
                } else {
                    0
                }
            }
            o if (ENABLE_BASE..CONTEXT_CONTROL_BASE).contains(&o) => {
                let context = ((o - ENABLE_BASE) / ENABLE_STRIDE) as usize;
                let word = (o - ENABLE_BASE) % ENABLE_STRIDE;
                if word == 0 && context < CONTEXTS {
                    self.enable[context] as u64
                } else {
                    0
                }
            }
            o => {
                let context = ((o - CONTEXT_CONTROL_BASE) / CONTEXT_STRIDE) as usize;
                let field = (o - CONTEXT_CONTROL_BASE) % CONTEXT_STRIDE;
                if context >= CONTEXTS {
                    return 0;
                }
                match field {
                    0 => self.threshold[context] as u64,
                    4 => self.claim(context) as u64,
                    _ => 0,
                }
            }
        }
    }

    pub fn write(&mut self, offset: u64, value: u64) {
        let value = value as u32;
        match offset {
            o if o < PENDING_BASE => {
                let source = (o / 4) as usize;
                // Source 0 does not exist and its priority is hardwired to 0.
                if source > 0 {
                    if let Some(p) = self.priority.get_mut(source) {
                        *p = value;
                    }
                }
            }
            // The pending register is read-only; a device asserts its line
            // instead of software setting the bit.
            o if (PENDING_BASE..ENABLE_BASE).contains(&o) => {}
            o if (ENABLE_BASE..CONTEXT_CONTROL_BASE).contains(&o) => {
                let context = ((o - ENABLE_BASE) / ENABLE_STRIDE) as usize;
                let word = (o - ENABLE_BASE) % ENABLE_STRIDE;
                if word == 0 && context < CONTEXTS {
                    // Source 0 can never be enabled.
                    self.enable[context] = value & !1;
                }
            }
            o => {
                let context = ((o - CONTEXT_CONTROL_BASE) / CONTEXT_STRIDE) as usize;
                let field = (o - CONTEXT_CONTROL_BASE) % CONTEXT_STRIDE;
                if context >= CONTEXTS {
                    return;
                }
                match field {
                    0 => self.threshold[context] = value,
                    4 => self.complete(value),
                    _ => {}
                }
            }
        }
    }
}
