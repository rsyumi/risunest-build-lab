use crate::{Message, Snapshot};
use std::io::{self, BufRead, Read, Write};

fn json_error(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
fn fragment(output: &mut impl Write, text: &str) -> io::Result<()> {
    let escaped = serde_json::to_vec(text).map_err(json_error)?;
    output.write_all(&escaped[1..escaped.len() - 1])
}
fn string(output: &mut impl Write, mut input: impl Read, binary: bool) -> io::Result<()> {
    output.write_all(b"\"")?;
    let mut buffer = [0u8; 64 * 1024];
    let mut carry = Vec::with_capacity(buffer.len() + 4);
    loop {
        let read = input.read(&mut buffer)?;
        if binary {
            if read == 0 {
                break;
            }
            output.write_all(crate::masking::hex(&buffer[..read]).as_bytes())?;
            continue;
        }
        carry.extend_from_slice(&buffer[..read]);
        let mut consumed = 0;
        while consumed < carry.len() {
            match std::str::from_utf8(&carry[consumed..]) {
                Ok(text) => {
                    fragment(output, text)?;
                    consumed = carry.len();
                }
                Err(error) => {
                    let end = consumed + error.valid_up_to();
                    fragment(output, std::str::from_utf8(&carry[consumed..end]).unwrap())?;
                    consumed = end;
                    if let Some(length) = error.error_len() {
                        fragment(output, "\u{fffd}")?;
                        consumed += length;
                    } else if read == 0 {
                        fragment(output, "\u{fffd}")?;
                        consumed = carry.len();
                    } else {
                        break;
                    }
                }
            }
        }
        carry.drain(..consumed);
        if read == 0 {
            break;
        }
    }
    output.write_all(b"\"")
}
fn message(output: &mut impl Write, message: &Message, input: impl Read) -> io::Result<()> {
    let mut metadata = serde_json::to_value(message).map_err(json_error)?;
    metadata.as_object_mut().unwrap().remove("body");
    let bytes = serde_json::to_vec(&metadata).map_err(json_error)?;
    output.write_all(&bytes[..bytes.len() - 1])?;
    output.write_all(b",\"body\":")?;
    let bytes = serde_json::to_vec(&message.body).map_err(json_error)?;
    output.write_all(&bytes[..bytes.len() - 1])?;
    output.write_all(if message.body.binary {
        b",\"encoding\":\"hex\",\"data\":"
    } else {
        b",\"encoding\":\"utf8\",\"data\":"
    })?;
    string(
        output,
        input.take(message.body.captured_bytes),
        message.body.binary,
    )?;
    output.write_all(b"}}")
}
impl Snapshot {
    pub fn export_json(&mut self, mut output: impl Write) -> Result<(), String> {
        let result = (|| -> io::Result<()> {
            let metadata = serde_json::json!({
                "format": "risunest-network-log", "version": 1, "id": self.record.id,
                "source": self.record.source, "startedAt": self.record.started_at, "elapsedMs": self.record.elapsed_ms,
                "capture": { "state": self.record.state, "error": self.record.capture_error,
                    "headerMasking": self.record.header_masking,
                    "truncated": self.record.request.body.truncated || self.record.response.body.truncated },
            });
            let bytes = serde_json::to_vec(&metadata).map_err(json_error)?;
            output.write_all(&bytes[..bytes.len() - 1])?;
            output.write_all(b",\"request\":")?;
            message(&mut output, &self.record.request, &mut self.request)?;
            output.write_all(b",\"response\":")?;
            message(&mut output, &self.record.response, &mut self.response)?;
            if let Some(kind) = &self.record.stream_kind {
                output.write_all(b",\"stream\":{\"kind\":")?;
                serde_json::to_writer(&mut output, kind).map_err(json_error)?;
                output.write_all(b",\"observations\":[")?;
                let reader = io::BufReader::new((&mut self.events).take(self.record.event_bytes));
                for (index, line) in reader.lines().enumerate() {
                    let observation: crate::Observation =
                        serde_json::from_str(&line?).map_err(json_error)?;
                    if index != 0 {
                        output.write_all(b",")?;
                    }
                    serde_json::to_writer(&mut output, &observation).map_err(json_error)?;
                }
                output.write_all(b"]}")?;
            }
            output.write_all(b"}")?;
            output.flush()
        })();
        result.map_err(|_| "network-log-export-failed".into())
    }
}
