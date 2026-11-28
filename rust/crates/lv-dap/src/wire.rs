//! The two DAP wires and the reset line, as a probe drives them.

use crate::bits::Bits;

/// One step on the wires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Drive DAP1 with these bits, one DAP0 clock each. The bit changes
    /// while the clock is low; the device takes it on the rising edge.
    Out(Bits),
    /// Release DAP1 and clock this many bits in.
    In(usize),
    /// Clock this many times with DAP1 released.
    Idle(usize),
    /// Pull the reset line (PORST) low, or let it go.
    Reset(bool),
    /// Wait, in milliseconds.
    Sleep(u32),
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("USB: {0}")]
    Usb(String),
    #[error("프로브가 응답하지 않습니다")]
    Timeout,
    #[error("프로브가 빠졌습니다")]
    Disconnected,
    #[error("프로브가 이 기능을 지원하지 않습니다: {0}")]
    Unsupported(String),
    #[error("프로브의 응답이 예상과 다릅니다: {0}")]
    Protocol(String),
}

/// A probe that drives the wires.
pub trait Wire: Send {
    /// What the probe calls itself, with its serial number.
    fn name(&self) -> String;

    /// Asks for a DAP0 clock of `hz`; returns the clock the probe took.
    fn set_clock(&mut self, hz: u32) -> Result<u32, WireError>;

    fn clock(&self) -> u32;

    /// Runs the steps in order, as closely together as the probe can, and
    /// returns what each [`Op::In`] read.
    fn run(&mut self, ops: &[Op]) -> Result<Vec<Bits>, WireError>;
}

impl<W: Wire + ?Sized> Wire for Box<W> {
    fn name(&self) -> String {
        (**self).name()
    }

    fn set_clock(&mut self, hz: u32) -> Result<u32, WireError> {
        (**self).set_clock(hz)
    }

    fn clock(&self) -> u32 {
        (**self).clock()
    }

    fn run(&mut self, ops: &[Op]) -> Result<Vec<Bits>, WireError> {
        (**self).run(ops)
    }
}
