use crate::{DecodeV2, FlateDecoder};
use compression_core::util::{PartialBuffer, WriteBuffer};
use std::io::{self, Result};

const HTTP_SNIFF_LIMIT: usize = 1000;
#[derive(Debug, Clone, Copy)]
enum HttpPhase { Sniff, Replay, Body, Tail }
#[derive(Debug)]
struct HttpDeflate {
    phase: HttpPhase,
    replay: [u8; HTTP_SNIFF_LIMIT - 1],
    length: usize,
    position: usize,
    retried: bool,
    failed: bool,
}
impl Default for HttpDeflate {
    fn default() -> Self {
        Self { phase: HttpPhase::Sniff, replay: [0; HTTP_SNIFF_LIMIT - 1], length: 0,
            position: 0, retried: false, failed: false }
    }
}

#[derive(Debug)]
pub struct ZlibDecoder {
    inner: FlateDecoder,
    http: Option<Box<HttpDeflate>>,
}
impl Default for ZlibDecoder {
    fn default() -> Self { Self { inner: FlateDecoder::new(true), http: None } }
}
impl ZlibDecoder {
    pub fn new() -> Self { Self::default() }

    /// Creates the Chromium HTTP mode. Finish only after clean upstream EOF.
    pub fn new_http_chromium() -> Self {
        Self { inner: FlateDecoder::new(true), http: Some(Box::default()) }
    }

    /// Additional owned context that the caller reserves before HTTP construction.
    pub fn http_context_bytes() -> usize { std::mem::size_of::<HttpDeflate>() }

    /// Upper bound on internal replay input for the next operation.
    pub fn http_replay_work_bound(&self) -> usize {
        self.http.as_ref().map_or(0, |state| match state.phase {
            HttpPhase::Sniff => HTTP_SNIFF_LIMIT + 1,
            HttpPhase::Replay => state.length - state.position,
            _ => 0,
        })
    }

    fn http_decode(&mut self, input: &mut PartialBuffer<&[u8]>, output: &mut WriteBuffer<'_>) -> Result<bool> {
        let state = self.http.as_mut().expect("HTTP deflate context");
        if state.failed { return Err(io::Error::other("HTTP deflate decoder previously failed")); }
        loop {
            match state.phase {
                HttpPhase::Tail => {
                    input.advance(input.unwritten().len());
                    return Ok(false);
                }
                HttpPhase::Replay => {
                    if state.position == state.length {
                        state.length = 0;
                        state.position = 0;
                        state.phase = HttpPhase::Body;
                        continue;
                    }
                    let mut replay = PartialBuffer::new(&state.replay[state.position..state.length]);
                    let prior = output.written_len();
                    let done = self.inner.decode(&mut replay, output)?;
                    let used = replay.written_len();
                    state.position += used;
                    if done { state.phase = HttpPhase::Tail; }
                    if output.written_len() > prior { return Ok(false); }
                    if !done && used == 0 { return Err(io::Error::other("HTTP deflate replay made no progress")); }
                }
                HttpPhase::Sniff => {
                    if input.unwritten().is_empty() { return Ok(false); }
                    let source = *input.get_mut();
                    let source = &source[input.written_len()..];
                    let prior_in = input.written_len();
                    let prior_out = output.written_len();
                    match self.inner.decode(input, output) {
                        Err(error) => {
                            if state.retried { return Err(error); }
                            state.retried = true;
                            self.inner.reinit()?;
                            let mut header = PartialBuffer::new(&[0x78, 0x01][..]);
                            let mut bytes = [0; 4];
                            let mut discard = WriteBuffer::new_initialized(&mut bytes);
                            let done = self.inner.decode(&mut header, &mut discard)?;
                            if done || header.written_len() != 2 || discard.written_len() != 0 {
                                return Err(io::Error::other("HTTP deflate reset header was not consumed"));
                            }
                            state.position = 0;
                            state.phase = HttpPhase::Replay;
                        }
                        Ok(done) => {
                            let used = input.written_len() - prior_in;
                            let produced = output.written_len() - prior_out;
                            if done || produced != 0 || state.length + used >= HTTP_SNIFF_LIMIT {
                                state.length = 0;
                                state.phase = if done { HttpPhase::Tail } else { HttpPhase::Body };
                                if done { input.advance(input.unwritten().len()); }
                            } else {
                                state.replay[state.length..state.length + used].copy_from_slice(&source[..used]);
                                state.length += used;
                            }
                            return Ok(false);
                        }
                    }
                }
                HttpPhase::Body => {
                    if input.unwritten().is_empty() { return Ok(false); }
                    if self.inner.decode(input, output)? {
                        state.phase = HttpPhase::Tail;
                        input.advance(input.unwritten().len());
                    }
                    return Ok(false);
                }
            }
            if output.has_no_spare_space() { return Ok(false); }
        }
    }
}

impl DecodeV2 for ZlibDecoder {
    fn reinit(&mut self) -> Result<()> {
        self.inner.reinit()?;
        if let Some(state) = self.http.as_mut() { **state = HttpDeflate::default(); }
        Ok(())
    }

    fn decode(&mut self, input: &mut PartialBuffer<&[u8]>, output: &mut WriteBuffer<'_>) -> Result<bool> {
        if self.http.is_none() { return self.inner.decode(input, output); }
        let result = self.http_decode(input, output);
        if result.is_err() { self.http.as_mut().expect("HTTP deflate context").failed = true; }
        result
    }

    fn flush(&mut self, output: &mut WriteBuffer<'_>) -> Result<bool> {
        let Some(state) = self.http.as_ref() else { return self.inner.flush(output) };
        if state.failed { return Err(io::Error::other("HTTP deflate decoder previously failed")); }
        if matches!(state.phase, HttpPhase::Tail) { return Ok(true); }
        if matches!(state.phase, HttpPhase::Replay) {
            let prior = output.written_len();
            self.decode(&mut PartialBuffer::new(&[][..]), output)?;
            if output.written_len() > prior || output.has_no_spare_space() { return Ok(false); }
        }
        let prior = output.written_len();
        let result = self.inner.flush(output);
        let state = self.http.as_mut().expect("HTTP deflate context");
        if result.is_err() { state.failed = true; }
        if output.written_len() > prior && matches!(state.phase, HttpPhase::Sniff) {
            state.phase = HttpPhase::Body;
            state.length = 0;
        }
        result
    }

    fn finish(&mut self, output: &mut WriteBuffer<'_>) -> Result<bool> {
        if self.http.is_some() { self.flush(output) } else { self.inner.finish(output) }
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn http_accepts_raw_deflate_but_preserves_bad_adler_errors() {
        let payload = b"synthetic payload";
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(payload).unwrap();
        let mut encoded = encoder.finish().unwrap();
        let raw = &encoded[2..encoded.len() - 4];
        let mut input = PartialBuffer::new(raw);
        let mut bytes = [0; 256];
        let mut output = WriteBuffer::new_initialized(&mut bytes);
        let mut decoder = ZlibDecoder::new_http_chromium();
        assert!(!decoder.decode(&mut input, &mut output).unwrap());
        assert_eq!(output.written(), payload);
        assert!(decoder.finish(&mut output).unwrap());
        let length = encoded.len();
        encoded[length - 4..].fill(0xff);
        for body in [encoded.as_slice(), &encoded[2..]] {
            let mut input = PartialBuffer::new(body);
            let mut bytes = [0; 256];
            let mut output = WriteBuffer::new_initialized(&mut bytes);
            let mut decoder = ZlibDecoder::new_http_chromium();
            assert!(decoder.decode(&mut input, &mut output).is_err());
            assert!(decoder.finish(&mut output).is_err());
        }
    }

    #[test]
    fn http_sniff_retention_commits_at_one_thousand_consumed_bytes() {
        let mut encoded = vec![0x78, 1];
        for _ in 0..200 { encoded.extend_from_slice(&[0, 0, 0, 0xff, 0xff]); }
        let mut decoder = ZlibDecoder::new_http_chromium();
        let mut bytes = [0; 64];
        let mut output = WriteBuffer::new_initialized(&mut bytes);
        let mut input = PartialBuffer::new(&encoded[..999]);
        decoder.decode(&mut input, &mut output).unwrap();
        assert_eq!(input.written_len(), 999);
        assert_eq!(output.written_len(), 0);
        assert_eq!(decoder.http_replay_work_bound(), 1001);
        let mut input = PartialBuffer::new(&encoded[999..1000]);
        decoder.decode(&mut input, &mut output).unwrap();
        assert_eq!(input.written_len(), 1);
        assert_eq!(decoder.http_replay_work_bound(), 0);
    }
}
