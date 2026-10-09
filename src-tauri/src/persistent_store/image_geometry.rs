use std::io::BufReader;
use std::path::Path;
use image::{ImageDecoder, ImageReader, metadata::Orientation};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use super::{StoreError, StoreResult};

const FILE_NAME: &str = "image-geometry.sqlite";
const BATCH_LIMIT: usize = 64;
const SCHEMA: &str = "CREATE TABLE image_geometry (
    hash TEXT PRIMARY KEY NOT NULL,
    width INTEGER NOT NULL CHECK(width > 0),
    height INTEGER NOT NULL CHECK(height > 0)
);";

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImageGeometry {
    pub content_hash: String,
    pub width: u32,
    pub height: u32,
}

fn invalid(message: &str) -> StoreError {
    StoreError::Validation { message: message.to_owned() }
}

fn validate_hash(hash: &str) -> StoreResult<()> {
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        return Err(invalid("invalid image content hash"));
    }
    Ok(())
}

fn open(root: &Path) -> StoreResult<Connection> {
    let mut db = crate::sqlite_open::open(root.join("persistent").join(FILE_NAME))?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
    let version: u32 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 0 {
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: u32 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if current == 0 {
            tx.execute_batch(SCHEMA)?;
            tx.execute_batch("PRAGMA user_version=1;")?;
        } else if current != 1 {
            return Err(StoreError::SchemaMismatch { message: "Unsupported image geometry schema".into() });
        }
        tx.commit()?;
    } else if version != 1 {
        return Err(StoreError::SchemaMismatch { message: "Unsupported image geometry schema".into() });
    }
    let schema: String = db.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='image_geometry'", [], |row| row.get(0))?;
    let normalize = |sql: &str| sql.chars().filter(|c| !c.is_whitespace() && *c != ';').collect::<String>();
    if normalize(&schema) != normalize(SCHEMA) {
        return Err(StoreError::SchemaMismatch { message: "Image geometry schema is incompatible".into() });
    }
    Ok(db)
}

pub(crate) fn read(root: &Path, hashes: &[String]) -> StoreResult<Vec<ImageGeometry>> {
    if hashes.len() > BATCH_LIMIT { return Err(invalid("image geometry batch is too large")); }
    for hash in hashes { validate_hash(hash)?; }
    let db = open(root)?;
    let mut query = db.prepare("SELECT width,height FROM image_geometry WHERE hash=?1")?;
    let mut values = Vec::new();
    for hash in hashes {
        if let Some((width, height)) = query.query_row([hash], |row| Ok((row.get(0)?, row.get(1)?))).optional()? {
            if width == 0 || height == 0 { return Err(invalid("stored image dimensions must be positive")); }
            values.push(ImageGeometry { content_hash: hash.clone(), width, height });
        }
    }
    Ok(values)
}

pub(crate) fn write(root: &Path, values: &[ImageGeometry]) -> StoreResult<()> {
    if values.len() > BATCH_LIMIT { return Err(invalid("image geometry batch is too large")); }
    for value in values {
        validate_hash(&value.content_hash)?;
        if value.width == 0 || value.height == 0 { return Err(invalid("image dimensions must be positive")); }
    }
    let mut db = open(root)?;
    let tx = db.transaction()?;
    {
        let mut insert = tx.prepare("INSERT INTO image_geometry(hash,width,height) VALUES(?1,?2,?3) ON CONFLICT(hash) DO NOTHING")?;
        let mut query = tx.prepare("SELECT width,height FROM image_geometry WHERE hash=?1")?;
        for value in values {
            insert.execute(params![value.content_hash, value.width, value.height])?;
            let stored: (u32, u32) = query.query_row([&value.content_hash], |row| Ok((row.get(0)?, row.get(1)?)))?;
            if stored != (value.width, value.height) { return Err(invalid("conflicting dimensions for immutable image")); }
        }
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn compute(root: &Path, hash: &str) -> StoreResult<Option<ImageGeometry>> {
    validate_hash(hash)?;
    let cas = crate::asset_repository::PayloadCas::new(root)?;
    let Some(file) = cas.open_object(hash)? else { return Ok(None); };
    let reader = ImageReader::new(BufReader::new(file)).with_guessed_format()?;
    if reader.format().is_none() { return Ok(None); }
    let mut decoder = match reader.into_decoder() {
        Ok(decoder) => decoder,
        Err(image::ImageError::Unsupported(_)) => return Ok(None),
        Err(error) => return Err(invalid(&format!("image header read failed: {error}"))),
    };
    let (mut width, mut height) = decoder.dimensions();
    let orientation = decoder.orientation().map_err(|_| invalid("image orientation read failed"))?;
    if matches!(orientation, Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH) {
        std::mem::swap(&mut width, &mut height);
    }
    if width == 0 || height == 0 { return Ok(None); }
    Ok(Some(ImageGeometry { content_hash: hash.to_owned(), width, height }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_store::PersistentStore;
    use crate::asset_repository::PayloadCas;

    #[test]
    fn geometry_survives_reopen_and_does_not_write_library_or_device_revisions() {
        let directory = tempfile::tempdir().unwrap();
        let store = PersistentStore::open(directory.path()).unwrap();
        let revision = store.revision().unwrap();
        let device_revision = store.device_store().unwrap().revision().unwrap();
        let record = ImageGeometry { content_hash: "a".repeat(64), width: 640, height: 480 };
        write(directory.path(), &[record.clone(), record.clone()]).unwrap();
        assert_eq!(read(directory.path(), &[record.content_hash.clone()]).unwrap(), vec![record]);
        assert_eq!(store.revision().unwrap(), revision);
        assert_eq!(store.device_store().unwrap().revision().unwrap(), device_revision);
        assert!(read(directory.path(), &["bad".into()]).is_err());
        assert!(write(directory.path(), &[ImageGeometry { content_hash: "b".repeat(64), width: 0, height: 1 }]).is_err());
    }

    #[test]
    fn reads_local_headers_without_reencoding_and_skips_missing_or_unsupported_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let _store = PersistentStore::open(directory.path()).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(7, 3).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let object = cas.prepare_bytes(png.get_ref()).unwrap();
        let geometry = compute(directory.path(), &object.content_hash).unwrap().unwrap();
        assert_eq!((geometry.width, geometry.height), (7, 3));
        assert_eq!(cas.read_object(&object.content_hash).unwrap().unwrap(), *png.get_ref());
        assert!(compute(directory.path(), &"a".repeat(64)).unwrap().is_none());
        let unsupported = cas.prepare_bytes(b"synthetic unsupported image").unwrap();
        assert!(compute(directory.path(), &unsupported.content_hash).unwrap().is_none());
    }

    #[test]
    fn preserves_animation_bytes_and_applies_exif_orientation() {
        let directory = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            encoder.encode_frames((0..2).map(|_| image::Frame::new(image::RgbaImage::new(7, 3)))).unwrap();
        }
        let animated = cas.prepare_bytes(&gif).unwrap();
        assert_eq!(compute(directory.path(), &animated.content_hash).unwrap().map(|v| (v.width, v.height)), Some((7, 3)));
        assert_eq!(cas.read_object(&animated.content_hash).unwrap().unwrap(), gif);
        let mut jpeg = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(7, 3).write_to(&mut jpeg, image::ImageFormat::Jpeg).unwrap();
        let exif = b"\xff\xe1\x00\x22Exif\x00\x00II\x2a\x00\x08\x00\x00\x00\x01\x00\x12\x01\x03\x00\x01\x00\x00\x00\x06\x00\x00\x00\x00\x00\x00\x00";
        jpeg.get_mut().splice(2..2, exif.iter().copied());
        let rotated = cas.prepare_bytes(jpeg.get_ref()).unwrap();
        assert_eq!(compute(directory.path(), &rotated.content_hash).unwrap().map(|v| (v.width, v.height)), Some((3, 7)));
        let broken = cas.prepare_bytes(b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(compute(directory.path(), &broken.content_hash).is_err());
    }

    #[test]
    fn rejects_conflicts_atomically_and_does_not_repair_incompatible_schema() {
        let directory = tempfile::tempdir().unwrap();
        let _store = PersistentStore::open(directory.path()).unwrap();
        let value = ImageGeometry { content_hash: "a".repeat(64), width: 10, height: 20 };
        write(directory.path(), &[value.clone()]).unwrap();
        let other = ImageGeometry { content_hash: "b".repeat(64), ..value.clone() };
        assert!(write(directory.path(), &[other.clone(), ImageGeometry { width: 30, ..value.clone() }]).is_err());
        assert!(read(directory.path(), &[other.content_hash]).unwrap().is_empty());
        let db = open(directory.path()).unwrap();
        db.execute_batch("PRAGMA user_version=2").unwrap();
        assert!(matches!(read(directory.path(), &[value.content_hash]), Err(StoreError::SchemaMismatch { .. })));
        assert_eq!(db.query_row("PRAGMA user_version", [], |row| row.get::<_,u32>(0)).unwrap(), 2);
    }

    #[test]
    fn synthetic_library_index_measurement_and_profile_isolation() {
        let directory = tempfile::tempdir().unwrap();
        let _store = PersistentStore::open(directory.path()).unwrap();
        let records: Vec<_> = (0..10_000).map(|i| ImageGeometry { content_hash: format!("{i:064x}"), width: 640, height: 480 }).collect();
        let start = std::time::Instant::now();
        for batch in records.chunks(64) { write(directory.path(), batch).unwrap(); }
        let write_ms = start.elapsed().as_millis();
        let start = std::time::Instant::now();
        for batch in records.chunks(64) {
            assert_eq!(read(directory.path(), &batch.iter().map(|r| r.content_hash.clone()).collect::<Vec<_>>()).unwrap(), batch);
        }
        let read_ms = start.elapsed().as_millis();
        let other = tempfile::tempdir().unwrap();
        let _other_store = PersistentStore::open(other.path()).unwrap();
        assert!(read(other.path(), &[records[0].content_hash.clone()]).unwrap().is_empty());
        let bytes = std::fs::metadata(directory.path().join("persistent").join(FILE_NAME)).unwrap().len();
        println!("synthetic_image_geometry records=10000 bytes={bytes} write_ms={write_ms} read_ms={read_ms}");
    }

    #[test]
    fn same_profile_snapshot_activation_and_temporary_sweep_retain_geometry() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let snapshot = store.snapshot_create("synthetic geometry test").unwrap();
        let value = ImageGeometry { content_hash: "a".repeat(64), width: 10, height: 20 };
        write(directory.path(), &[value.clone()]).unwrap();
        let stage = store.snapshot_restore_stage(&snapshot.id, "synthetic-geometry-restore").unwrap();
        store.snapshot_restore_activate(&stage.staging_id, store.revision().unwrap(), store.lww_binding_authority().unwrap()).unwrap();
        drop(store);
        let _reopened = PersistentStore::open(directory.path()).unwrap();
        assert_eq!(read(directory.path(), &[value.content_hash.clone()]).unwrap(), vec![value]);
    }
}
