use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Default)]
struct Counts {
    uploaded: AtomicU64,
    downloaded: AtomicU64,
    body_reads: AtomicU64,
    body_bytes: AtomicU64,
    requests: AtomicU64,
}

#[derive(Clone, Default)]
pub struct IoCounters(Arc<Counts>);

#[derive(Debug, PartialEq, Eq)]
pub struct IoSnapshot {
    pub uploaded: u64,
    pub downloaded: u64,
    pub body_reads: u64,
    pub body_bytes: u64,
    pub requests: u64,
}

impl IoCounters {
    pub fn request_started(&self) { self.0.requests.fetch_add(1, Ordering::Relaxed); }
    pub fn snapshot(&self) -> IoSnapshot {
        IoSnapshot {
            uploaded: self.0.uploaded.load(Ordering::Relaxed), downloaded: self.0.downloaded.load(Ordering::Relaxed),
            body_reads: self.0.body_reads.load(Ordering::Relaxed), body_bytes: self.0.body_bytes.load(Ordering::Relaxed),
            requests: self.0.requests.load(Ordering::Relaxed),
        }
    }
    pub fn download<R: Read>(&self, inner: R) -> CountedReader<R> {
        CountedReader { inner, counters: self.clone(), body: false }
    }
    pub fn body<R: Read>(&self, inner: R) -> CountedReader<R> {
        self.0.body_reads.fetch_add(1, Ordering::Relaxed);
        CountedReader { inner, counters: self.clone(), body: true }
    }
    pub fn upload<W: Write>(&self, inner: W) -> CountedWriter<W> {
        CountedWriter { inner, counters: self.clone() }
    }
}

pub struct CountedReader<R> { inner: R, counters: IoCounters, body: bool }
impl<R: Read> Read for CountedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        let target = if self.body { &self.counters.0.body_bytes } else { &self.counters.0.downloaded };
        target.fetch_add(count as u64, Ordering::Relaxed);
        Ok(count)
    }
}

pub struct CountedWriter<W> { inner: W, counters: IoCounters }
impl<W: Write> Write for CountedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.counters.0.uploaded.fetch_add(count as u64, Ordering::Relaxed);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> { self.inner.flush() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn io_counts_observed_bytes_attempted_requests_and_opened_bodies() {
        let counters = IoCounters::default();
        counters.request_started();
        let mut downloaded = counters.download(io::Cursor::new(b"download"));
        let mut bytes = Vec::new();
        downloaded.read_to_end(&mut bytes).unwrap();
        counters.body(io::Cursor::new(b"asset")).read_to_end(&mut bytes).unwrap();
        let _unread_body = counters.body(io::Cursor::new([]));
        struct PartialWriter;
        impl Write for PartialWriter {
            fn write(&mut self, input: &[u8]) -> io::Result<usize> { Ok(input.len().min(2)) }
            fn flush(&mut self) -> io::Result<()> { Ok(()) }
        }
        let mut upload = counters.upload(PartialWriter);
        assert_eq!(upload.write(b"123456").unwrap(), 2);
        upload.write_all(b"abcdef").unwrap();
        assert_eq!(counters.snapshot(), IoSnapshot { uploaded: 8, downloaded: 8, body_reads: 2, body_bytes: 5, requests: 1 });
    }
    #[test]
    fn failed_io_does_not_count_unsent_bytes() {
        struct Refusal;
        impl Write for Refusal {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::ErrorKind::BrokenPipe.into()) }
            fn flush(&mut self) -> io::Result<()> { Ok(()) }
        }
        let counters = IoCounters::default();
        counters.request_started();
        assert!(counters.upload(Refusal).write(b"failed").is_err());
        assert_eq!(counters.snapshot().uploaded, 0);
        assert_eq!(counters.snapshot().requests, 1);
    }
}
