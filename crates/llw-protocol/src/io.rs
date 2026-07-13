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
#[derive(Default)]
pub struct FakeIo {
    pub writes: Mutex<Vec<Vec<u8>>>,
    pub reads: Mutex<VecDeque<Result<Vec<u8>>>>,
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
}

impl UsbIo for FakeIo {
    fn write(&self, data: &[u8], _timeout: Duration) -> Result<usize> {
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
    fn read_flush(&self) {}
}
