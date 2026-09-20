//! A NS16550a UART, the console every RISC-V Linux device tree expects.
//!
//! The register set is from 1987 and it shows: eight byte-wide registers, two
//! of which change meaning depending on a bit in a third. That bit is DLAB,
//! and it swaps the first two registers for the baud-rate divisor. Nothing
//! here cares about baud rate, but the divisor has to be writable and
//! readable or driver probing fails.

use std::collections::VecDeque;

pub const UART_BASE: u64 = 0x1000_0000;
pub const UART_SIZE: u64 = 0x100;
/// The interrupt line this UART is wired to on the `virt` machine.
pub const UART_IRQ: u32 = 10;

// Register offsets.
const RBR_THR: u64 = 0; // receive buffer / transmit holding (or divisor low)
const IER: u64 = 1; // interrupt enable (or divisor high)
const IIR_FCR: u64 = 2; // interrupt identification / FIFO control
const LCR: u64 = 3; // line control
const MCR: u64 = 4; // modem control
const LSR: u64 = 5; // line status
const MSR: u64 = 6; // modem status
const SCR: u64 = 7; // scratch

// Line status bits.
const LSR_DATA_READY: u8 = 1 << 0;
const LSR_THR_EMPTY: u8 = 1 << 5;
const LSR_TRANSMITTER_EMPTY: u8 = 1 << 6;

// Interrupt enable bits.
const IER_RX: u8 = 1 << 0;
const IER_THR: u8 = 1 << 1;

const LCR_DLAB: u8 = 1 << 7;

#[derive(Default)]
pub struct Uart {
    rx: VecDeque<u8>,
    /// Everything the guest has written, kept so tests can assert on it.
    pub tx: Vec<u8>,
    /// Whether to also write transmitted bytes to the host's stdout.
    pub echo: bool,
    ier: u8,
    fcr: u8,
    lcr: u8,
    mcr: u8,
    scr: u8,
    divisor_low: u8,
    divisor_high: u8,
}

impl Uart {
    /// Queues bytes for the guest to read, as though someone typed them.
    pub fn push_input(&mut self, bytes: &[u8]) {
        self.rx.extend(bytes);
    }

    /// Takes one queued input byte, if any. Used by the SBI console, which
    /// reports "nothing waiting" rather than blocking.
    pub fn take_input(&mut self) -> Option<u8> {
        self.rx.pop_front()
    }

    /// Everything written so far, as text. Invalid UTF-8 is replaced rather
    /// than rejected, since a console carries whatever the guest emits.
    pub fn output(&self) -> String {
        String::from_utf8_lossy(&self.tx).into_owned()
    }

    fn line_status(&self) -> u8 {
        // The transmitter is always idle here: a write completes instantly,
        // so THR is empty the moment it is written.
        let mut s = LSR_THR_EMPTY | LSR_TRANSMITTER_EMPTY;
        if !self.rx.is_empty() {
            s |= LSR_DATA_READY;
        }
        s
    }

    /// Whether the UART is asserting its interrupt line.
    ///
    /// A 16550 with the transmit interrupt enabled asserts continuously,
    /// because the holding register is always empty. That is not a bug to
    /// work around: drivers enable it only while they have something to send.
    pub fn is_interrupting(&self) -> bool {
        (self.ier & IER_RX != 0 && !self.rx.is_empty()) || self.ier & IER_THR != 0
    }

    /// The interrupt identification register, highest priority first.
    fn iir(&self) -> u8 {
        // Bits 6:7 report that the FIFOs are enabled; bit 0 set means "no
        // interrupt pending", which is the inverted sense the hardware uses.
        const FIFO_ENABLED: u8 = 0xC0;
        if self.ier & IER_RX != 0 && !self.rx.is_empty() {
            FIFO_ENABLED | 0x04 // received data available
        } else if self.ier & IER_THR != 0 {
            FIFO_ENABLED | 0x02 // transmitter holding register empty
        } else {
            FIFO_ENABLED | 0x01 // none
        }
    }

    pub fn read(&mut self, offset: u64) -> u64 {
        let dlab = self.lcr & LCR_DLAB != 0;
        let v = match offset {
            RBR_THR if dlab => self.divisor_low,
            RBR_THR => self.rx.pop_front().unwrap_or(0),
            IER if dlab => self.divisor_high,
            IER => self.ier,
            IIR_FCR => self.iir(),
            LCR => self.lcr,
            MCR => self.mcr,
            LSR => self.line_status(),
            // No modem is attached, so report the lines a driver wants to see
            // asserted: clear-to-send, data-set-ready, carrier detect.
            MSR => 0xB0,
            SCR => self.scr,
            _ => 0,
        };
        v as u64
    }

    pub fn write(&mut self, offset: u64, value: u64) {
        let value = value as u8;
        let dlab = self.lcr & LCR_DLAB != 0;
        match offset {
            RBR_THR if dlab => self.divisor_low = value,
            RBR_THR => {
                self.tx.push(value);
                if self.echo {
                    use std::io::Write;
                    let mut out = std::io::stdout();
                    let _ = out.write_all(&[value]);
                    let _ = out.flush();
                }
            }
            IER if dlab => self.divisor_high = value,
            IER => self.ier = value,
            IIR_FCR => self.fcr = value,
            LCR => self.lcr = value,
            MCR => self.mcr = value,
            SCR => self.scr = value,
            // LSR and MSR are read-only status.
            _ => {}
        }
    }
}
