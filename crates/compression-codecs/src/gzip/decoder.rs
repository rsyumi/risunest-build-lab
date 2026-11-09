use super::header;
use crate::{DecodeV2, FlateDecoder};
use compression_core::util::{PartialBuffer, WriteBuffer};
use flate2::Crc;
use std::io::{Error, ErrorKind, Result};

#[derive(Debug)]
enum State {
    Header(header::Parser),
    Decoding,
    Footer(PartialBuffer<[u8; 8]>),
    Done,
    HttpTail,
}

#[derive(Debug)]
pub struct GzipDecoder {
    inner: FlateDecoder,
    crc: Crc,
    state: State,
    http: bool,
    failed: bool,
}

fn check_footer(crc: &Crc, input: &[u8; 8]) -> Result<()> {
    let crc_sum = crc.sum().to_le_bytes();
    let bytes_read = crc.amount().to_le_bytes();

    if crc_sum != input[0..4] {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "CRC computed does not match",
        ));
    }

    if bytes_read != input[4..8] {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "amount of bytes read does not match",
        ));
    }

    Ok(())
}

impl Default for GzipDecoder {
    fn default() -> Self {
        Self {
            inner: FlateDecoder::new(false),
            crc: Crc::new(),
            state: State::Header(header::Parser::default()),
            http: false,
            failed: false,
        }
    }
}

impl GzipDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the Chromium HTTP mode. Finish only after clean upstream EOF.
    pub fn new_http_chromium() -> Self {
        Self { http: true, state: State::Header(header::Parser::http_compatible()), ..Self::default() }
    }

    fn process(
        &mut self,
        input: &mut PartialBuffer<&[u8]>,
        output: &mut WriteBuffer<'_>,
        inner: impl Fn(&mut Self, &mut PartialBuffer<&[u8]>, &mut WriteBuffer<'_>) -> Result<bool>,
    ) -> Result<bool> {
        loop {
            match &mut self.state {
                State::Header(parser) => {
                    if parser.input(&mut self.crc, input)?.is_some() {
                        self.crc.reset();
                        self.state = State::Decoding;
                    }
                }

                State::Decoding => {
                    let prior = output.written_len();

                    let res = inner(self, input, output);

                    if output.written_len() > prior {
                        // update CRC even if there was an error
                        self.crc.update(&output.written()[prior..]);
                    }

                    let done = res?;

                    if done {
                        self.state = State::Footer([0; 8].into());
                    }
                }

                State::Footer(footer) => {
                    footer.copy_unwritten_from(input);

                    if footer.unwritten().is_empty() {
                        if self.http {
                            self.state = State::HttpTail;
                        } else {
                            check_footer(&self.crc, footer.get_mut())?;
                            self.state = State::Done;
                        }
                    }
                }

                State::Done => {}
                State::HttpTail => {
                    input.advance(input.unwritten().len());
                }
            };

            if let State::Done = self.state {
                return Ok(true);
            }

            if input.unwritten().is_empty() || output.has_no_spare_space() {
                return Ok(false);
            }
        }
    }
}

impl DecodeV2 for GzipDecoder {
    fn reinit(&mut self) -> Result<()> {
        self.inner.reinit()?;
        self.crc.reset();
        self.state = State::Header(if self.http { header::Parser::http_compatible() } else { header::Parser::default() });
        self.failed = false;
        Ok(())
    }

    fn decode(
        &mut self,
        input: &mut PartialBuffer<&[u8]>,
        output: &mut WriteBuffer<'_>,
    ) -> Result<bool> {
        if self.http && self.failed {
            return Err(Error::other("HTTP gzip decoder previously failed"));
        }
        let result = self.process(input, output, |this, input, output| {
            if this.http && input.unwritten().is_empty() { Ok(false) }
            else { this.inner.decode(input, output) }
        });
        if self.http && result.is_err() { self.failed = true; }
        result
    }

    fn flush(&mut self, output: &mut WriteBuffer<'_>) -> Result<bool> {
        if self.http {
            if self.failed { return Err(Error::other("HTTP gzip decoder previously failed")); }
            let result = match self.state {
                State::Decoding => self.inner.flush(output),
                _ => Ok(true),
            };
            if result.is_err() { self.failed = true; }
            return result;
        }
        loop {
            match self.state {
                State::Header(_) | State::Footer(_) | State::Done | State::HttpTail => return Ok(true),

                State::Decoding => {
                    let prior = output.written_len();
                    let done = self.inner.flush(output)?;
                    self.crc.update(&output.written()[prior..]);
                    if done {
                        return Ok(true);
                    }
                }
            };

            if output.has_no_spare_space() {
                return Ok(false);
            }
        }
    }

    fn finish(&mut self, _output: &mut WriteBuffer<'_>) -> Result<bool> {
        if self.http { return self.flush(_output); }
        // Because of the footer we have to have already flushed all the data out before we get here
        if let State::Done = self.state {
            Ok(true)
        } else {
            Err(Error::from(ErrorKind::UnexpectedEof))
        }
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn http_ignores_footer_and_later_members_while_default_checks_crc() {
        let payload = b"synthetic payload";
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(payload).unwrap();
        let mut encoded = encoder.finish().unwrap();
        let size = encoded.len();
        encoded[size - 8] ^= 0xff;
        let mut input = PartialBuffer::new(encoded.as_slice());
        let mut bytes = [0; 256];
        let mut output = WriteBuffer::new_initialized(&mut bytes);
        assert!(GzipDecoder::new().decode(&mut input, &mut output).is_err());
        let first = encoded.clone();
        encoded.extend_from_slice(&first);
        let mut decoder = GzipDecoder::new_http_chromium();
        let mut input = PartialBuffer::new(encoded.as_slice());
        let mut bytes = [0; 256];
        let mut output = WriteBuffer::new_initialized(&mut bytes);
        assert!(!decoder.decode(&mut input, &mut output).unwrap());
        assert_eq!(input.written_len(), encoded.len());
        assert_eq!(output.written(), payload);
        assert!(decoder.finish(&mut output).unwrap());
        let mut decoder = GzipDecoder::new_http_chromium();
        let mut input = PartialBuffer::new(&first[..first.len() - 8]);
        let mut bytes = [0; 256];
        let mut output = WriteBuffer::new_initialized(&mut bytes);
        decoder.decode(&mut input, &mut output).unwrap();
        assert!(decoder.finish(&mut output).unwrap());
        assert_eq!(output.written(), payload);
    }

    #[test]
    fn http_invalid_deflate_cannot_become_clean_eof() {
        let encoded = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 7];
        let mut input = PartialBuffer::new(&encoded[..]);
        let mut bytes = [0; 64];
        let mut output = WriteBuffer::new_initialized(&mut bytes);
        let mut decoder = GzipDecoder::new_http_chromium();
        assert!(decoder.decode(&mut input, &mut output).is_err());
        assert!(decoder.finish(&mut output).is_err());
    }
}
