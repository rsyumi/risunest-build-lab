use super::{base_response, prepare_file_response, ResolvedBlob};
use crate::trust_boundary::{is_link_like, is_lower_hex_256, open_regular_source};
use image::{metadata::Orientation, DynamicImage, ImageDecoder, ImageReader};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, BufReader, Cursor, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex},
    time::{Duration, SystemTime},
};
use tauri::http::{Request, Response, StatusCode};

pub(super) const VARIANT: &str = "gallery-v1";
const DIMENSION: u32 = 512;
const DISK_BYTES: u64 = 128 * 1024 * 1024;
const DISK_ENTRIES: usize = 4096;
const DECODE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug)]
enum ThumbnailError {
    Fallback,
    Unavailable,
}

impl From<io::Error> for ThumbnailError {
    fn from(_: io::Error) -> Self { Self::Unavailable }
}

fn decode_error(error: image::ImageError) -> ThumbnailError {
    match error {
        image::ImageError::Limits(_) | image::ImageError::Unsupported(_) => ThumbnailError::Fallback,
        _ => ThumbnailError::Unavailable,
    }
}

struct Entry {
    bytes: u64,
    used: u64,
    touched: SystemTime,
}

#[derive(Default)]
struct Index {
    loaded: bool,
    entries: HashMap<String, Entry>,
    bytes: u64,
    clock: u64,
    fallbacks: HashMap<String, u64>,
}

enum Source {
    Native(crate::server_sync::residency::TransientBody),
    External { file: tempfile::NamedTempFile, _scratch: tempfile::TempDir },
}

impl Source {
    fn len(&self) -> io::Result<u64> {
        match self {
            Self::Native(body) => body.len(),
            Self::External { file, .. } => Ok(file.as_file().metadata()?.len()),
        }
    }
}

impl Read for Source {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Native(body) => body.read(buffer),
            Self::External { file, .. } => file.read(buffer),
        }
    }
}

pub(super) struct ThumbnailCache {
    directory: PathBuf,
    index: Mutex<Index>,
    generation: Mutex<()>,
    closed: Arc<AtomicBool>,
    max_bytes: u64,
    max_entries: usize,
    decode_bytes: u64,
}

fn unavailable() -> io::Error {
    io::Error::other("gallery-thumbnail-unavailable")
}

impl ThumbnailCache {
    pub(super) fn new(cache_root: PathBuf) -> Self {
        Self {
            directory: cache_root.join("native-media").join(VARIANT),
            index: Mutex::new(Index::default()),
            generation: Mutex::new(()),
            closed: Arc::new(AtomicBool::new(false)),
            max_bytes: DISK_BYTES,
            max_entries: DISK_ENTRIES,
            decode_bytes: DECODE_BYTES,
        }
    }

    pub(super) fn close(&self) {
        let _index = self.index.lock();
        self.closed.store(true, Ordering::Release);
    }

    fn check(&self) -> io::Result<()> {
        if self.closed.load(Ordering::Acquire) { Err(unavailable()) } else { Ok(()) }
    }

    fn path(&self, hash: &str) -> PathBuf {
        self.directory.join(format!("{hash}.webp"))
    }

    fn initialize(&self, index: &mut Index) -> io::Result<()> {
        self.check()?;
        if index.loaded { return Ok(()); }
        index.entries.clear();
        index.bytes = 0;
        index.clock = 0;
        ensure_directory(&self.directory)?;
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let metadata = fs::symlink_metadata(entry.path())?;
            if is_link_like(&metadata) { return Err(unavailable()); }
            if metadata.is_file() && name.starts_with(".gallery-") {
                fs::remove_file(entry.path())?;
            } else if metadata.is_dir() && name.starts_with("asset-transient-") {
                // This profile cache owns these crash-left download spools.
                // remove_dir_all removes nested links without following them.
                fs::remove_dir_all(entry.path())?;
            } else if metadata.is_file() {
                if let Some(hash) = name.strip_suffix(".webp").filter(|hash| is_lower_hex_256(hash)) {
                    let touched = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    let used = touched.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_millis().min(u128::from(u64::MAX)) as u64;
                    index.clock = index.clock.max(used);
                    index.bytes += metadata.len();
                    index.entries.insert(hash.to_owned(), Entry { bytes: metadata.len(), used, touched });
                    self.evict(index, 0, 0)?;
                }
            }
        }
        self.evict(index, 0, 0)?;
        index.loaded = true;
        Ok(())
    }

    fn evict(&self, index: &mut Index, incoming_bytes: u64, incoming_entries: usize) -> io::Result<()> {
        while index.bytes.saturating_add(incoming_bytes) > self.max_bytes
            || index.entries.len().saturating_add(incoming_entries) > self.max_entries
        {
            let hash = index.entries.iter().min_by_key(|(_, entry)| entry.used)
                .map(|(hash, _)| hash.clone()).ok_or_else(unavailable)?;
            match fs::remove_file(self.path(&hash)) {
                Ok(()) => {},
                Err(error) if error.kind() == io::ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
            if let Some(entry) = index.entries.remove(&hash) {
                index.bytes = index.bytes.saturating_sub(entry.bytes);
            }
        }
        Ok(())
    }

    fn cached(&self, hash: &str) -> io::Result<Option<(File, u64)>> {
        let mut index = self.index.lock().map_err(|_| unavailable())?;
        self.initialize(&mut index)?;
        let Some(entry) = index.entries.get(hash) else { return Ok(None); };
        let expected = entry.bytes;
        let opened = (|| {
            let mut file = open_regular_source(&self.path(hash))?;
            if file.metadata()?.len() != expected { return Err(unavailable()); }
            let (width, height) = ImageReader::with_format(BufReader::new(file.try_clone()?), image::ImageFormat::WebP)
                .into_dimensions().map_err(|_| unavailable())?;
            if width == 0 || height == 0 || width.max(height) > DIMENSION { return Err(unavailable()); }
            file.seek(SeekFrom::Start(0))?;
            Ok(file)
        })();
        let file = match opened {
            Ok(file) => file,
            Err(_) => {
                let _ = fs::remove_file(self.path(hash));
                if let Some(entry) = index.entries.remove(hash) {
                    index.bytes = index.bytes.saturating_sub(entry.bytes);
                }
                return Ok(None);
            },
        };
        index.clock += 1;
        let used = index.clock;
        if let Some(entry) = index.entries.get_mut(hash) {
            entry.used = used;
            let now = SystemTime::now();
            if now.duration_since(entry.touched).unwrap_or_default() > Duration::from_secs(60) {
                if file.set_modified(now).is_ok() { entry.touched = now; }
            }
        }
        Ok(Some((file, expected)))
    }

    fn get(&self, root: &Path, hash: &str, size: u64) -> Result<(File, u64), ThumbnailError> {
        if !is_lower_hex_256(hash) { return Err(ThumbnailError::Unavailable); }
        self.check()?;
        if let Some(file) = self.cached(hash)? { return Ok(file); }
        if self.is_fallback(hash, size)? { return Err(ThumbnailError::Fallback); }
        let check = || self.check().map_err(|_| crate::server_sync::SyncError::new("cancelled", 409));
        let mut source = match crate::server_sync::residency::open_transient_server_with_check(root, &self.directory, hash, &check)
            .map_err(|_| unavailable())?
        {
            Some(body) => Source::Native(body),
            None => {
                let scratch = tempfile::Builder::new().prefix("asset-transient-").tempdir_in(&self.directory)?;
                let cancel = crate::external_storage::contract::Cancellation::with_external_flag(self.closed.clone());
                let file = tauri::async_runtime::block_on(crate::external_storage::lww_residency::spool_verified_remote_body(
                    root, hash, scratch.path(), &cancel,
                )).map_err(|_| unavailable())?.ok_or_else(unavailable)?;
                Source::External { file, _scratch: scratch }
            },
        };
        if source.len()? != size { return Err(ThumbnailError::Unavailable); }
        // Remote spooling owns a bounded media-transfer slot, but must not hold
        // the decoder lock while unrelated local previews are ready to decode.
        let _generation = self.generation.lock().map_err(|_| unavailable())?;
        self.check()?;
        if let Some(file) = self.cached(hash)? { return Ok(file); }
        if self.is_fallback(hash, size)? { return Err(ThumbnailError::Fallback); }
        // This is a derivative working-memory budget, not an import limit.
        // Verify oversized sources with a fixed buffer before using their
        // original response path, just as normal-sized derivatives are verified.
        if size > self.decode_bytes / 4 {
            let mut digest = Sha256::new();
            let mut received = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                self.check()?;
                let count = source.read(&mut buffer)?;
                if count == 0 { break; }
                received += count as u64;
                if received > size { return Err(ThumbnailError::Unavailable); }
                digest.update(&buffer[..count]);
            }
            if received != size || hex::encode(digest.finalize()) != hash { return Err(ThumbnailError::Unavailable); }
            self.remember_fallback(hash, size)?;
            return Err(ThumbnailError::Fallback);
        }
        let mut bytes = Vec::with_capacity(size as usize);
        (&mut source).take(size + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != size || hex::encode(Sha256::digest(&bytes)) != hash { return Err(ThumbnailError::Unavailable); }
        self.check()?;
        let encoded = match encode_with_budget(&bytes, self.decode_bytes) {
            Err(ThumbnailError::Fallback) => {
                self.remember_fallback(hash, size)?;
                return Err(ThumbnailError::Fallback);
            }
            result => result?,
        };
        drop(source);
        drop(bytes);
        self.publish(hash, &encoded)?;
        self.cached(hash)?.ok_or(ThumbnailError::Unavailable)
    }

    fn is_fallback(&self, hash: &str, size: u64) -> io::Result<bool> {
        Ok(self.index.lock().map_err(|_| unavailable())?.fallbacks.get(hash) == Some(&size))
    }

    fn remember_fallback(&self, hash: &str, size: u64) -> io::Result<()> {
        let mut index = self.index.lock().map_err(|_| unavailable())?;
        self.check()?;
        if index.fallbacks.len() >= self.max_entries { index.fallbacks.clear(); }
        if self.max_entries != 0 { index.fallbacks.insert(hash.to_owned(), size); }
        Ok(())
    }

    fn publish(&self, hash: &str, bytes: &[u8]) -> io::Result<()> {
        self.check()?;
        let mut index = self.index.lock().map_err(|_| unavailable())?;
        self.check()?;
        self.evict(&mut index, bytes.len() as u64, 1)?;
        let mut pending = tempfile::Builder::new().prefix(".gallery-").tempfile_in(&self.directory)?;
        pending.write_all(bytes)?;
        self.check()?;
        pending.persist(self.path(hash)).map_err(|error| error.error)?;
        index.clock += 1;
        let used = index.clock;
        index.bytes += bytes.len() as u64;
        index.entries.insert(hash.to_owned(), Entry { bytes: bytes.len() as u64, used, touched: SystemTime::now() });
        Ok(())
    }

    pub(super) fn response(&self, root: &Path, request: Request<()>) -> Option<Response<Option<std::io::Take<File>>>> {
        if request.method() != tauri::http::Method::GET && request.method() != tauri::http::Method::HEAD {
            return Some(base_response(StatusCode::METHOD_NOT_ALLOWED).header("Allow", "GET, HEAD").body(None).unwrap());
        }
        let uri = request.uri().to_string();
        let descriptor = super::decode_physical_key(&uri).and_then(|key| super::cas_content_hash(&key))
            .zip(super::cas_descriptor(&uri));
        let Some((hash, (_, source_size))) = descriptor else { return Some(super::not_found()); };
        let available = self.check().and_then(|_| {
            let local = crate::asset_repository::PayloadCas::new(root)?.stat_object(&hash)?;
            let actual = match local {
                Some(size) => Some(size),
                None => crate::server_sync::residency::Residency::open(root)
                    .and_then(|store| store.object(&hash, None))
                    .map(|object| object.map(|object| object.size))
                    .map_err(|_| unavailable())?,
            };
            self.check()?;
            if actual == Some(source_size) { Ok(()) } else { Err(unavailable()) }
        });
        if available.is_err() { return Some(super::not_found()); }
        let (file, size) = match self.get(root, &hash, source_size) {
            Ok(value) => value,
            Err(ThumbnailError::Fallback) => return if self.check().is_ok() { None } else { Some(super::not_found()) },
            Err(ThumbnailError::Unavailable) => return Some(super::not_found()),
        };
        if self.check().is_err() { return Some(super::not_found()); }
        let blob = ResolvedBlob {
            payload_path: self.path(&hash),
            mime: "image/webp".into(), size,
            validator: format!("\"{hash}-{VARIANT}\""),
            cache_control: "no-cache",
        };
        Some(prepare_file_response(request, blob, file))
    }
}

pub(super) fn parse_request(uri: &str) -> Result<(String, bool), ()> {
    let mut url = url::Url::parse(uri).map_err(|_| ())?;
    let mut preview = false;
    let mut retained = Vec::new();
    for (key, value) in url.query_pairs() {
        if key == "thumbnail" {
            if preview || value != VARIANT { return Err(()); }
            preview = true;
        } else {
            retained.push((key.into_owned(), value.into_owned()));
        }
    }
    if preview { url.query_pairs_mut().clear().extend_pairs(retained); }
    Ok((url.into(), preview))
}

fn ensure_directory(path: &Path) -> io::Result<()> {
    for parent in path.ancestors() {
        match fs::symlink_metadata(parent) {
            Ok(metadata) if !metadata.is_dir() || is_link_like(&metadata) => return Err(unavailable()),
            Ok(_) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }
    }
    fs::create_dir_all(path)
}

#[cfg(test)]
fn encode(bytes: &[u8]) -> Result<Vec<u8>, ThumbnailError> {
    encode_with_budget(bytes, DECODE_BYTES)
}

fn encode_with_budget(bytes: &[u8], decode_bytes: u64) -> Result<Vec<u8>, ThumbnailError> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|_| unavailable())?;
    if reader.format().is_none() { return Err(ThumbnailError::Unavailable); }
    let jpeg_working = if reader.format() == Some(image::ImageFormat::Jpeg) {
        let working = jpeg_working_bytes(bytes)?;
        if working > decode_bytes { return Err(ThumbnailError::Fallback); }
        Some(working)
    } else { None };
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(decode_bytes / 2);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(decode_error)?;
    let (width, height) = decoder.dimensions();
    // Reserve the decoded surface and a decoder-owned surface (including
    // high-bit-depth output). Sampling and orientation only allocate at the
    // preview dimensions; they never create a full-size float resize buffer.
    let preview_pixels = u64::from(width.min(DIMENSION)) * u64::from(height.min(DIMENSION));
    let working = jpeg_working.unwrap_or(decoder.total_bytes().checked_mul(2)
        .and_then(|pixels| pixels.checked_add(preview_pixels * 24))
        .and_then(|pixels| pixels.checked_add((bytes.len() as u64) * 2)).ok_or_else(unavailable)?);
    if width == 0 || height == 0 { return Err(ThumbnailError::Unavailable); }
    if working > decode_bytes { return Err(ThumbnailError::Fallback); }
    let profile = decoder.icc_profile().map_err(|_| unavailable())?;
    if profile.as_ref().is_some_and(|value| value.len() as u64 + working > decode_bytes) { return Err(ThumbnailError::Fallback); }
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    // ImageDecoder reads the first image, so animation frame counts do not
    // multiply the gallery's decode budget or produce animated derivatives.
    let mut decoded = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    if decoded.width().max(decoded.height()) > DIMENSION {
        decoded = decoded.thumbnail(DIMENSION, DIMENSION);
    }
    decoded.apply_orientation(orientation);
    let rgba = decoded.into_rgba8();
    let encoded = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height()).encode(75.0).to_vec();
    Ok(attach_profile(encoded, profile.as_deref(), rgba.width(), rgba.height())?)
}

fn jpeg_working_bytes(bytes: &[u8]) -> Result<u64, ThumbnailError> {
    let mut source = Cursor::new(bytes);
    let mut decoder = zune_jpeg::JpegDecoder::new(&mut source);
    decoder.set_options(decoder.options().set_strict_mode(false).set_max_width(usize::MAX).set_max_height(usize::MAX));
    decoder.decode_headers().map_err(|error| match error {
        zune_jpeg::errors::DecodeErrors::Unsupported(_) => ThumbnailError::Fallback,
        _ => ThumbnailError::Unavailable,
    })?;
    let info = decoder.info().ok_or(ThumbnailError::Unavailable)?;
    let width = u64::from(info.width);
    let height = u64::from(info.height);
    let components = decoder.input_colorspace().ok_or(ThumbnailError::Unavailable)?.num_components() as u64;
    drop(decoder);
    // Header decoding stops immediately after SOS. Only a uniquely matching
    // all-component scan can avoid the baseline decoder's full coefficient planes.
    let header_end = source.position() as usize;
    let mut scan_components = (1_u8..=4).filter(|count| {
        let length = 6 + 2 * usize::from(*count);
        header_end.checked_sub(length + 2).and_then(|start| bytes.get(start..start + 5))
            .is_some_and(|header| header == [0xff, 0xda, 0, length as u8, *count])
    });
    let all_components_in_first_scan = scan_components.next() == Some(info.components) && scan_components.next().is_none();
    let decoded = width * height * if components == 1 { 1 } else { 3 };
    let padded_width = width.div_ceil(32) * 32;
    let padded_height = height.div_ceil(32) * 32;
    // Progressive and baseline multi-scan JPEG keep full i16 coefficient planes.
    // MCU padding and four-component input also apply to CMYK/YCCK sources.
    let coefficients = if info.sof.is_progressive() || !all_components_in_first_scan {
        padded_width * padded_height * u64::from(info.components).max(components) * 2
    } else { 0 };
    let surfaces = (decoded * 2).max(decoded + coefficients);
    // Cover raw MCU rows, previous rows and generic 4x4 upsampling scratch,
    // plus compressed input/metadata copies retained by the image adapter.
    Ok(surfaces + padded_width * 4096 + width.min(u64::from(DIMENSION)) * height.min(u64::from(DIMENSION)) * 24 + bytes.len() as u64 * 4)
}

fn attach_profile(encoded: Vec<u8>, profile: Option<&[u8]>, width: u32, height: u32) -> io::Result<Vec<u8>> {
    let Some(profile) = profile else { return Ok(encoded); };
    if encoded.get(..4) != Some(b"RIFF") || encoded.get(8..12) != Some(b"WEBP") { return Err(unavailable()); }
    let mut extended = vec![0x20, 0, 0, 0];
    extended.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
    extended.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
    let mut chunks = &encoded[12..];
    if chunks.get(..4) == Some(b"VP8X") {
        if chunks.get(4..8) != Some(&10_u32.to_le_bytes()) || chunks.len() < 18 { return Err(unavailable()); }
        extended.copy_from_slice(&chunks[8..18]);
        extended[0] |= 0x20;
        chunks = &chunks[18..];
    }
    let mut output = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
    output.extend_from_slice(&10_u32.to_le_bytes());
    output.extend_from_slice(&extended);
    output.extend_from_slice(b"ICCP");
    output.extend_from_slice(&u32::try_from(profile.len()).map_err(|_| unavailable())?.to_le_bytes());
    output.extend_from_slice(profile);
    if profile.len() % 2 != 0 { output.push(0); }
    output.extend_from_slice(chunks);
    let length = u32::try_from(output.len() - 8).map_err(|_| unavailable())?;
    output[4..8].copy_from_slice(&length.to_le_bytes());
    Ok(output)
}

#[cfg(test)]
mod tests;
