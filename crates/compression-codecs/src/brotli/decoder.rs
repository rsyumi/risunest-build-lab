use crate::DecodeV2;
use brotli::{enc::StandardAlloc, BrotliDecompressStream, BrotliResult};
use compression_core::util::{PartialBuffer, WriteBuffer};
use std::{fmt, io};

type BrotliState = brotli::BrotliState<StandardAlloc, StandardAlloc, StandardAlloc>;

pub struct BrotliDecoder {
    // `BrotliState` is very large (over 2kb) which is why we're boxing it.
    state: Box<BrotliState>,
    http: bool,
    tail: bool,
    failed: bool,
}

impl Default for BrotliDecoder {
    fn default() -> Self {
        Self {
            state: Box::new(Self::new_brotli_state()),
            http: false,
            tail: false,
            failed: false,
        }
    }
}

impl BrotliDecoder {
    fn new_brotli_state() -> BrotliState {
        BrotliState::new(
            StandardAlloc::default(),
            StandardAlloc::default(),
            StandardAlloc::default(),
        )
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the Chromium HTTP mode. Finish only after clean upstream EOF.
    pub fn new_http_chromium() -> Self {
        Self { http: true, ..Self::default() }
    }

    fn decode(
        &mut self,
        input: &mut PartialBuffer<&[u8]>,
        output: &mut WriteBuffer<'_>,
    ) -> io::Result<BrotliResult> {
        let in_buf = input.unwritten();
        let out_buf = output.initialize_unwritten();

        let mut input_len = 0;
        let mut output_len = 0;

        let status = match BrotliDecompressStream(
            &mut in_buf.len(),
            &mut input_len,
            in_buf,
            &mut out_buf.len(),
            &mut output_len,
            out_buf,
            &mut 0,
            &mut self.state,
        ) {
            BrotliResult::ResultFailure => return Err(io::Error::other("brotli error")),
            status => status,
        };

        input.advance(input_len);
        output.advance(output_len);

        Ok(status)
    }
}

impl DecodeV2 for BrotliDecoder {
    fn reinit(&mut self) -> io::Result<()> {
        *self.state = Self::new_brotli_state();
        self.tail = false;
        self.failed = false;
        Ok(())
    }

    fn decode(
        &mut self,
        input: &mut PartialBuffer<&[u8]>,
        output: &mut WriteBuffer<'_>,
    ) -> io::Result<bool> {
        if self.http && self.failed { return Err(io::Error::other("HTTP Brotli decoder previously failed")); }
        if self.http && self.tail {
            input.advance(input.unwritten().len());
            return Ok(false);
        }
        let result = self.decode(input, output);
        if self.http && result.is_err() { self.failed = true; }
        match result? {
            BrotliResult::ResultSuccess if self.http => {
                self.tail = true;
                input.advance(input.unwritten().len());
                Ok(false)
            }
            BrotliResult::ResultSuccess => Ok(true),
            BrotliResult::NeedsMoreOutput | BrotliResult::NeedsMoreInput => Ok(false),
            BrotliResult::ResultFailure => unreachable!(),
        }
    }

    fn flush(&mut self, output: &mut WriteBuffer<'_>) -> io::Result<bool> {
        if self.http && self.failed { return Err(io::Error::other("HTTP Brotli decoder previously failed")); }
        if self.http && self.tail { return Ok(true); }
        let result = self.decode(&mut PartialBuffer::new(&[][..]), output);
        if self.http && result.is_err() { self.failed = true; }
        match result? {
            BrotliResult::ResultSuccess | BrotliResult::NeedsMoreInput => Ok(true),
            BrotliResult::NeedsMoreOutput => Ok(false),
            BrotliResult::ResultFailure => unreachable!(),
        }
    }

    fn finish(&mut self, output: &mut WriteBuffer<'_>) -> io::Result<bool> {
        if self.http { return self.flush(output); }
        match self.decode(&mut PartialBuffer::new(&[][..]), output)? {
            BrotliResult::ResultSuccess => Ok(true),
            BrotliResult::NeedsMoreOutput => Ok(false),
            BrotliResult::NeedsMoreInput => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "reached unexpected EOF",
            )),
            BrotliResult::ResultFailure => unreachable!(),
        }
    }
}

impl fmt::Debug for BrotliDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrotliDecoder")
            .field("decompress", &"<no debug>")
            .finish()
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;

    #[test]
    fn http_clean_eof_does_not_change_default_finalization() {
        let mut bytes = [0; 64];
        let mut output = WriteBuffer::new_initialized(&mut bytes);
        assert!(BrotliDecoder::new().finish(&mut output).is_err());
        assert!(BrotliDecoder::new_http_chromium().finish(&mut output).unwrap());
        assert_eq!(output.written_len(), 0);
    }

    #[test]
    fn http_real_brotli_error_remains_failed() {
        let encoded = [0xff; 64];
        let mut input = PartialBuffer::new(&encoded[..]);
        let mut bytes = [0; 64];
        let mut output = WriteBuffer::new_initialized(&mut bytes);
        let mut decoder = BrotliDecoder::new_http_chromium();
        assert!(decoder.decode(&mut input, &mut output).is_err());
        assert!(decoder.finish(&mut output).is_err());
    }
}
