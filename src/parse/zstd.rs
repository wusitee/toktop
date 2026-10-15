//! Minimal bindings to the system libzstd, for logs written as concatenated
//! Zstandard frames.

use std::ffi::{c_int, c_uint, c_void};

#[repr(C)]
struct InBuffer {
    src: *const c_void,
    size: usize,
    pos: usize,
}

#[repr(C)]
struct OutBuffer {
    dst: *mut c_void,
    size: usize,
    pos: usize,
}

enum DCtx {}

/// `ZSTD_reset_session_only`.
const RESET_SESSION_ONLY: c_int = 1;

#[link(name = "zstd")]
unsafe extern "C" {
    fn ZSTD_isError(code: usize) -> c_uint;
    fn ZSTD_findFrameCompressedSize(src: *const c_void, src_size: usize) -> usize;
    fn ZSTD_createDCtx() -> *mut DCtx;
    fn ZSTD_freeDCtx(dctx: *mut DCtx) -> usize;
    fn ZSTD_DCtx_reset(dctx: *mut DCtx, reset: c_int) -> usize;
    fn ZSTD_decompressStream(
        dctx: *mut DCtx,
        output: *mut OutBuffer,
        input: *mut InBuffer,
    ) -> usize;
}

/// Size of the complete frame at the start of `data`, or `None` when the
/// frame is cut off (still being written) or `data` holds no frame.
pub fn frame_size(data: &[u8]) -> Option<usize> {
    // SAFETY: the pointer and length describe a live slice.
    let size = unsafe { ZSTD_findFrameCompressedSize(data.as_ptr().cast(), data.len()) };
    // SAFETY: pure function of the code.
    (unsafe { ZSTD_isError(size) } == 0 && size > 0 && size <= data.len()).then_some(size)
}

/// Reusable decompression context.
pub struct Decoder(*mut DCtx);

impl Decoder {
    pub fn new() -> Option<Self> {
        // SAFETY: returns an owned context or null.
        let dctx = unsafe { ZSTD_createDCtx() };
        (!dctx.is_null()).then_some(Decoder(dctx))
    }

    /// Decompresses one complete frame into `out`, replacing its contents.
    /// Returns `false` when the frame is corrupt.
    pub fn frame(&mut self, frame: &[u8], out: &mut Vec<u8>) -> bool {
        out.clear();
        // SAFETY: `self.0` is a live context.
        if unsafe { ZSTD_isError(ZSTD_DCtx_reset(self.0, RESET_SESSION_ONLY)) } != 0 {
            return false;
        }
        let mut input = InBuffer {
            src: frame.as_ptr().cast(),
            size: frame.len(),
            pos: 0,
        };
        loop {
            out.reserve(64 * 1024);
            let spare = out.spare_capacity_mut();
            let mut output = OutBuffer {
                dst: spare.as_mut_ptr().cast(),
                size: spare.len(),
                pos: 0,
            };
            // SAFETY: both buffers point into live memory of the given sizes;
            // zstd writes at most `output.size` bytes into `out`'s spare
            // capacity and reports how many in `output.pos`.
            let hint = unsafe { ZSTD_decompressStream(self.0, &mut output, &mut input) };
            // SAFETY: the first `output.pos` spare bytes were initialized.
            unsafe { out.set_len(out.len() + output.pos) };
            // SAFETY: pure function of the code.
            if unsafe { ZSTD_isError(hint) } != 0 {
                return false;
            }
            if hint == 0 {
                // The frame is complete and its checksum verified.
                return input.pos == input.size;
            }
            if input.pos == input.size && output.pos < output.size {
                // Input exhausted mid-frame.
                return false;
            }
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: `self.0` is owned and freed once.
        unsafe { ZSTD_freeDCtx(self.0) };
    }
}
