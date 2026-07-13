//! Rust FFI bindings for the vendored tinyuz compression library.
//!
//! tinyuz is an LZ77 variant designed for embedded systems.
//! The wireless device firmware uses it to decompress RGB frame data.

use crate::{ProtocolError, Result};
use std::os::raw::c_uchar;

extern "C" {
    fn tuz_compress_mem(
        input: *const c_uchar,
        input_len: usize,
        output: *mut c_uchar,
        output_capacity: usize,
        dict_size: usize,
    ) -> usize;

    fn tuz_max_compressed_size(input_len: usize) -> usize;
}

/// Default dictionary size (4KB) — must match what the device firmware expects.
const DICT_SIZE_4K: usize = 4096;

/// Compress data using tinyuz with a 4KB dictionary.
pub fn compress(input: &[u8]) -> Result<Vec<u8>> {
    if input.is_empty() {
        return Err(ProtocolError::Compression(
            "cannot compress empty input".into(),
        ));
    }

    let max_size = unsafe { tuz_max_compressed_size(input.len()) };
    let mut output = vec![0u8; max_size];

    let compressed_len = unsafe {
        tuz_compress_mem(
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            output.len(),
            DICT_SIZE_4K,
        )
    };

    if compressed_len == 0 {
        return Err(ProtocolError::Compression("compressor returned 0".into()));
    }

    output.truncate(compressed_len);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_solid_color() {
        let mut rgb_data = Vec::new();
        for _ in 0..20 {
            rgb_data.extend_from_slice(&[255, 0, 0]);
        }
        let compressed = compress(&rgb_data).expect("compression should succeed");
        assert!(!compressed.is_empty());
        assert!(compressed.len() < rgb_data.len());
    }

    #[test]
    fn compress_gradient() {
        let mut rgb_data = Vec::new();
        for i in 0..80u8 {
            rgb_data.extend_from_slice(&[i, i, i]);
        }
        let compressed = compress(&rgb_data).expect("compression should succeed");
        assert!(!compressed.is_empty());
    }

    #[test]
    fn compress_empty_fails() {
        assert!(compress(&[]).is_err());
    }
}
