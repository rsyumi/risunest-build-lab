//! Job-owned immutable source and staging sink. No path-bearing renderer DTOs.
use super::contract::*;
use std::{
    path::{Path, PathBuf},
    pin::Pin,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, ReadBuf};

fn io_error(error: impl std::fmt::Display + 'static) -> ProviderError {
    super::packaging::transient(error)
}
pub(crate) struct SpoolSource {
    path: PathBuf,
    length: u64,
}
impl SpoolSource {
    /// Only a native owner holding the capture's lifetime may construct this.
    /// Verify again after reopening a journal or recovering a process.
    pub fn verified(path: &Path, length: u64, sha256: &str) -> Result<Self> {
        verify_file(path, length, sha256)?;
        Ok(Self {
            path: path.into(),
            length,
        })
    }
}
fn verify_file(path: &Path, length: u64, sha256: &str) -> Result<()> {
    let mut file = crate::trust_boundary::open_regular_source(path).map_err(io_error)?;
    if file.metadata().map_err(io_error)?.len() != length {
        return Err(ProviderError::new(ErrorKind::Corrupt));
    }
    let hash = risunest_external_storage_format::content_identity::hash_reader(&mut file, length)
        .map_err(|_| ProviderError::new(ErrorKind::Corrupt))?;
    if hex::encode(hash) != sha256 {
        return Err(ProviderError::new(ErrorKind::Corrupt));
    }
    Ok(())
}
impl TransferSource for SpoolSource {
    fn byte_length(&self) -> u64 {
        self.length
    }
    fn open<'a>(
        &'a self,
        offset: u64,
        length: u64,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, Pin<Box<dyn AsyncRead + Send>>> {
        Box::pin(async move {
            cancel.check()?;
            if offset
                .checked_add(length)
                .is_none_or(|end| end > self.length)
            {
                return Err(ProviderError::new(ErrorKind::Corrupt));
            }
            let file = crate::trust_boundary::open_regular_source(&self.path).map_err(io_error)?;
            if file.metadata().map_err(io_error)?.len() != self.length {
                return Err(ProviderError::new(ErrorKind::Corrupt));
            }
            let mut file = tokio::fs::File::from_std(file);
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(io_error)?;
            Ok(Box::pin(CancelledReader {
                reader: file.take(length),
                cancel: cancel.clone(),
            }) as Pin<Box<dyn AsyncRead + Send>>)
        })
    }
}

struct CancelledReader {
    reader: tokio::io::Take<tokio::fs::File>,
    cancel: Cancellation,
}
impl AsyncRead for CancelledReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.cancel.check().is_err() {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "cancelled",
            )));
        }
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

pub(crate) struct SpoolSink {
    path: PathBuf,
    max_length: u64,
    verified: bool,
    progress: Option<std::sync::Arc<dyn Fn(u64) + Send + Sync>>,
}
impl SpoolSink {
    pub fn create(path: &Path, max_length: u64) -> Result<Self> {
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(io_error)?;
        Ok(Self {
            path: path.into(),
            max_length,
            verified: false,
            progress: None,
        })
    }
    pub(crate) fn observe(&mut self, progress: std::sync::Arc<dyn Fn(u64) + Send + Sync>) { self.progress = Some(progress); }
    pub fn is_verified(&self) -> bool {
        self.verified
    }
}
/// The writer refuses a provider overrun before it reaches the staging file.
struct LimitedWriter {
    file: tokio::fs::File,
    remaining: u64,
    position: u64,
    progress: Option<std::sync::Arc<dyn Fn(u64) + Send + Sync>>,
    cancel: Cancellation,
}
impl AsyncWrite for LimitedWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.cancel.check().is_err() {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "cancelled",
            )));
        }
        if bytes.len() as u64 > self.remaining {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "transfer-limit-exceeded",
            )));
        }
        match Pin::new(&mut self.file).poll_write(cx, bytes) {
            std::task::Poll::Ready(Ok(written)) => {
                self.remaining -= written as u64;
                self.position += written as u64;
                if let Some(progress)=&self.progress {progress(self.position);}
                std::task::Poll::Ready(Ok(written))
            }
            other => other,
        }
    }
    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.file).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.file).poll_shutdown(cx)
    }
}
impl TransferSink for SpoolSink {
    fn open<'a>(
        &'a mut self,
        offset: u64,
        max_length: u64,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, Pin<Box<dyn AsyncWrite + Send>>> {
        Box::pin(async move {
            cancel.check()?;
            if self.verified
                || offset
                    .checked_add(max_length)
                    .is_none_or(|end| end > self.max_length)
            {
                return Err(ProviderError::new(ErrorKind::Corrupt));
            }
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&self.path)
                .await
                .map_err(io_error)?;
            if file.metadata().await.map_err(io_error)?.len() < offset {
                return Err(ProviderError::new(ErrorKind::Corrupt));
            }
            file.set_len(offset).await.map_err(io_error)?;
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(io_error)?;
            Ok(Box::pin(LimitedWriter {
                file,
                remaining: max_length,
                position: offset,
                progress: self.progress.clone(),
                cancel: cancel.clone(),
            }) as Pin<Box<dyn AsyncWrite + Send>>)
        })
    }
    fn finish<'a>(&'a mut self, length: u64, sha256: &'a str) -> ProviderFuture<'a, ()> {
        Box::pin(async move {
            if self.verified || length > self.max_length {
                return Err(ProviderError::new(ErrorKind::Corrupt));
            }
            let path = self.path.clone();
            let sha256 = sha256.to_owned();
            tokio::task::spawn_blocking(move || {
                verify_file(&path, length, &sha256)?;
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .map_err(io_error)?
                    .sync_all()
                    .map_err(io_error)
            })
            .await
            .map_err(io_error)??;
            self.verified = true;
            Ok(())
        })
    }
}

#[cfg(test)]
#[test]
fn download_progress_reports_partial_writes_without_counting_retry_bytes_twice() {
    use tokio::io::AsyncWriteExt;
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let directory=tempfile::tempdir().unwrap();
        let delivered=std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let observed_bytes=delivered.clone();
        let phase=super::phase_progress::PhaseProgress::new(move |reading| {
            observed_bytes.fetch_max(reading.bytes,std::sync::atomic::Ordering::Relaxed);
        });
        phase.plan(1,8);
        let progress=super::phase_progress::DownloadProgress::new(&phase);
        let mut sink=SpoolSink::create(&directory.path().join("body"),8).unwrap();
        let observed=progress.clone();sink.observe(std::sync::Arc::new(move |bytes|observed.advance(bytes)));
        let cancel=Cancellation::default();
        let mut writer=sink.open(0,8,&cancel).await.unwrap();
        writer.write_all(b"1234").await.unwrap();
        assert_eq!((phase.read().items,phase.read().bytes),(0,4));
        assert_eq!(delivered.load(std::sync::atomic::Ordering::Relaxed),4);
        drop(writer);
        let mut writer=sink.open(0,8,&cancel).await.unwrap();
        writer.write_all(b"1234").await.unwrap();
        assert_eq!(phase.read().bytes,4);
        assert_eq!(delivered.load(std::sync::atomic::Ordering::Relaxed),4);
        writer.write_all(b"5678").await.unwrap();
        assert_eq!(phase.read().bytes,8);
        drop(writer);drop(sink);drop(progress);
        assert_eq!(phase.read().bytes,0);
        let retry=super::phase_progress::DownloadProgress::new(&phase);
        retry.advance(4);retry.commit(8);drop(retry);phase.completed(0);
        assert_eq!((phase.read().items,phase.read().bytes),(1,8));
        phase.flush();
        assert_eq!(delivered.load(std::sync::atomic::Ordering::Relaxed),8);
    });
}
