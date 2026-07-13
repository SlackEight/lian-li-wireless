//! I/O abstraction over the USB transport, enabling hardware-free tests.

use crate::Result;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

/// The three operations `Dongle` needs from a transport.
pub trait UsbIo {
    fn write(&self, data: &[u8], timeout: Duration) -> Result<usize>;
    fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize>;
    fn read_flush(&self);
}

impl UsbIo for crate::transport::UsbTransport {
    fn write(&self, data: &[u8], timeout: Duration) -> Result<usize> {
        crate::transport::UsbTransport::write(self, data, timeout)
    }
    fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        crate::transport::UsbTransport::read(self, buf, timeout)
    }
    fn read_flush(&self) {
        crate::transport::UsbTransport::read_flush(self)
    }
}

/// Scripted in-memory transport for tests and simulations.
/// Writes are recorded; reads pop from a script queue (empty queue = timeout,
/// matching real-dongle silence).
// Deliberately always-compiled (not cfg(test)-gated) so downstream crates'
// simulation tests (llw-daemon) can use it via a normal dependency. ~40
// dependency-free lines in the release artifact; hidden from docs.
#[doc(hidden)]
#[derive(Default)]
pub struct FakeIo {
    writes: Mutex<Vec<Vec<u8>>>,
    reads: Mutex<VecDeque<Result<Vec<u8>>>>,
    write_errors: Mutex<VecDeque<crate::ProtocolError>>,
}

impl FakeIo {
    pub fn push_read(&self, data: Vec<u8>) {
        self.reads.lock().unwrap().push_back(Ok(data));
    }
    pub fn push_read_err(&self, err: crate::ProtocolError) {
        self.reads.lock().unwrap().push_back(Err(err));
    }
    pub fn written(&self) -> Vec<Vec<u8>> {
        self.writes.lock().unwrap().clone()
    }
    /// Clear all scripted reads — models a real pipe drain for tests that
    /// need flush semantics.
    pub fn drain_reads(&self) {
        self.reads.lock().unwrap().clear();
    }
    pub fn push_write_err(&self, err: crate::ProtocolError) {
        self.write_errors.lock().unwrap().push_back(err);
    }
}

impl UsbIo for FakeIo {
    fn write(&self, data: &[u8], _timeout: Duration) -> Result<usize> {
        if let Some(e) = self.write_errors.lock().unwrap().pop_front() {
            return Err(e);
        }
        self.writes.lock().unwrap().push(data.to_vec());
        Ok(data.len())
    }
    fn read(&self, buf: &mut [u8], _timeout: Duration) -> Result<usize> {
        match self.reads.lock().unwrap().pop_front() {
            Some(Ok(data)) => {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                Ok(n)
            }
            Some(Err(e)) => Err(e),
            None => Err(crate::ProtocolError::Usb(rusb::Error::Timeout)),
        }
    }
    // NOTE: the real transport's read_flush DRAINS the pipe; this no-op keeps
    // pre-staged request-response scripts intact. Tests that model stale-pipe
    // scenarios should call `drain_reads()` explicitly at the flush boundary.
    fn read_flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_write_err_returns_err_then_subsequent_writes_succeed() {
        let io = FakeIo::default();
        io.push_write_err(crate::ProtocolError::Other("injected write failure".into()));
        // First write: returns Err and is NOT recorded.
        let result = io.write(b"first", Duration::from_millis(100));
        assert!(result.is_err(), "scripted write error must surface");
        assert_eq!(io.written().len(), 0, "failed write must not be recorded");
        // Second write: no error queued → succeeds and is recorded.
        let result = io.write(b"second", Duration::from_millis(100));
        assert!(result.is_ok(), "subsequent write must succeed");
        assert_eq!(io.written(), vec![b"second".to_vec()]);
    }
}
