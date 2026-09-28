//! Where captured record and generated bodies live. Small ones share a
//! database the external root already keeps, so a capture makes a group of
//! them durable in one transaction instead of publishing and syncing one file
//! per object. Larger ones keep the shared immutable file layout.
use crate::{
    persistent_store::StoreError,
    trust_boundary::{is_link_like, sync_directory},
};
use risunest_external_storage_format::content_identity::{hash, hash_reader};
use risunest_small_object_store as small_object_store;
pub(crate) use risunest_small_object_store::Body;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, StoreError>;

fn invalid(message: &str) -> StoreError {
    StoreError::Validation {
        message: message.into(),
    }
}

/// Bodies at or below this go to the database. The threshold decides where a
/// new body is written; where an existing one lives is decided by asking.
pub(crate) const SMALL_OBJECT_BYTES: usize = 64 * 1024;
const GROUP_ROWS: usize = 256;
const GROUP_BYTES: usize = 8 * 1024 * 1024;
const CANCELLABLE_READ_BYTES: usize = 64 * 1024;

/// Reads at most one bounded chunk at a time and asks `cancelled` before each,
/// so a long body can be stopped between chunks rather than only after it.
pub(crate) struct CancellableRead<'a, R> {
    inner: R,
    cancelled: &'a dyn Fn() -> bool,
    refused: bool,
}

impl<'a, R> CancellableRead<'a, R> {
    pub(crate) fn new(inner: R, cancelled: &'a dyn Fn() -> bool) -> Self {
        Self {
            inner,
            cancelled,
            refused: false,
        }
    }

    /// Whether a read was refused because `cancelled` said so.
    pub(crate) fn refused(&self) -> bool {
        self.refused
    }
}

impl<R: io::Read> io::Read for CancellableRead<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            self.refused = true;
            return Err(io::Error::other("read cancelled"));
        }
        let limit = buffer.len().min(CANCELLABLE_READ_BYTES);
        self.inner.read(&mut buffer[..limit])
    }
}

/// Which store a caller expects to read an object from. Section spool parts are
/// files the caller produced itself and are read straight from disk; capture
/// bodies are resolved by identity, wherever this store put them.
#[derive(Clone, Debug)]
pub(crate) enum ObjectSource {
    File(PathBuf),
    Captured(String),
    /// A body the asset repository answers for under this content hash, held
    /// locally or through its custody. It is registered where it is rather
    /// than read out of a staging file, and only packaging resolves one.
    Library(String),
    /// A record the receiving library already holds under the same key and
    /// identity. A difference leaves it alone, so its body was never fetched
    /// and nothing may read it.
    Unchanged,
}

impl ObjectSource {
    /// The file this source names, when it names one at all.
    pub(crate) fn file(&self) -> Option<&Path> {
        match self {
            Self::File(path) => Some(path),
            Self::Captured(_) | Self::Library(_) | Self::Unchanged => None,
        }
    }
}

pub(crate) struct ContentStore {
    objects: PathBuf,
    db: rusqlite::Connection,
    /// Bodies a caller has an identity for that are not durable yet. A reader
    /// on this store sees them exactly as it sees written ones.
    staged: BTreeMap<String, Vec<u8>>,
    staged_bytes: usize,
    /// What the checkpoint after the last group left in the log.
    checkpoint: small_object_store::Checkpoint,
    wal_high_water: u64,
}

impl ContentStore {
    /// `root` is the external storage root. The database sits beside the object
    /// directory rather than inside it, so a directory walk over objects never
    /// has to know about it.
    pub(crate) fn open(root: &Path) -> Result<Self> {
        let objects = root.join("objects");
        for directory in [root, &objects] {
            fs::create_dir_all(directory)?;
            if is_link_like(&fs::symlink_metadata(directory)?) {
                return Err(invalid("Capture directory must not be a link"));
            }
        }
        let db = rusqlite::Connection::open(root.join("content.sqlite"))?;
        // Incremental, so collection can return freed pages to the filesystem
        // without rewriting the database. It must precede the first table.
        db.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000; PRAGMA journal_size_limit=67108864;",
        )?;
        small_object_store::initialize(&db)
            .map_err(|_| invalid("Capture content store is unavailable"))?;
        Ok(Self {
            objects,
            db,
            staged: BTreeMap::new(),
            staged_bytes: 0,
            checkpoint: small_object_store::Checkpoint::UNKNOWN,
            wal_high_water: small_object_store::WAL_HIGH_WATER_BYTES,
        })
    }

    /// Attach to a store that already exists, creating nothing. A consumer
    /// that only reads uses this so it never leaves an external directory
    /// behind in a scratch repository.
    pub(crate) fn open_existing(root: &Path) -> Result<Option<Self>> {
        if !root.join("content.sqlite").is_file() {
            return Ok(None);
        }
        Self::open(root).map(Some)
    }

    pub(crate) fn object_directory(&self) -> &Path {
        &self.objects
    }

    /// Stage or publish one body under the identity the caller states. An
    /// identity already present keeps the body it already has; different bytes
    /// under it are corruption, never a replacement.
    pub(crate) fn put(&mut self, expected: &str, bytes: &[u8]) -> Result<()> {
        if hex::encode(hash(bytes)) != expected {
            return Err(invalid("Capture object identity differs"));
        }
        if bytes.len() > SMALL_OBJECT_BYTES {
            return self.write_file(expected, bytes);
        }
        if self.stat(expected)?.is_some() {
            // An identity already here keeps the body it already has, and that
            // body is checked rather than assumed, exactly as the file path
            // checks one it finds already published.
            if self.read_all(expected)? != bytes {
                return Err(invalid("Existing capture object differs"));
            }
            return Ok(());
        }
        self.staged_bytes += bytes.len();
        self.staged.insert(expected.to_owned(), bytes.to_vec());
        // A group that reaches the bound is written now. A durable body with no
        // reference to it is collectable; the reverse would be a dangling one.
        if self.staged.len() >= GROUP_ROWS || self.staged_bytes >= GROUP_BYTES {
            self.commit()?;
        }
        Ok(())
    }

    /// Make the staged group durable. Callers do this before recording the
    /// references that name it. A log readers are holding or a volume without
    /// room refuses the group and keeps it staged; groups already written stay
    /// written.
    pub(crate) fn commit(&mut self) -> Result<()> {
        if self.staged.is_empty() {
            return Ok(());
        }
        let available = fs2::available_space(&self.objects)?;
        match small_object_store::admit(
            &self.db,
            &mut self.checkpoint,
            self.wal_high_water,
            self.staged_bytes as u64,
            available,
        )
        .map_err(|_| invalid("Capture content store is unavailable"))?
        {
            None => (),
            Some(small_object_store::Pressure::Log) => {
                return Err(invalid("Captured content could not be saved now"))
            }
            Some(small_object_store::Pressure::Space) => {
                return Err(invalid("Not enough free space to save captured content"))
            }
        }
        let group = std::mem::take(&mut self.staged);
        self.staged_bytes = 0;
        let tx = self.db.transaction()?;
        small_object_store::insert_batch(
            &tx,
            &group
                .iter()
                .map(|(digest, bytes)| (digest.as_str(), bytes.as_slice()))
                .collect::<Vec<_>>(),
        )
        .map_err(|_| invalid("Existing capture object differs"))?;
        tx.commit()?;
        // Between groups, never inside one. A reader on another connection can
        // leave the log where it is; this group is already durable, and what
        // the checkpoint leaves decides whether the next one is admitted.
        self.checkpoint = small_object_store::checkpoint(&self.db)
            .unwrap_or(small_object_store::Checkpoint::UNKNOWN);
        Ok(())
    }

    /// Drop what a cancelled or restarted capture staged, so nothing records a
    /// reference to a body this store never wrote.
    pub(crate) fn discard(&mut self) {
        self.staged.clear();
        self.staged_bytes = 0;
    }

    pub(crate) fn stat(&self, digest: &str) -> Result<Option<u64>> {
        if let Some(bytes) = self.staged.get(digest) {
            return Ok(Some(bytes.len() as u64));
        }
        if let Some(size) = small_object_store::size(&self.db, digest)
            .map_err(|_| invalid("Capture object identity is invalid"))?
        {
            return Ok(Some(size));
        }
        let path = self.objects.join(digest);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
            Ok(metadata) if is_link_like(&metadata) || !metadata.is_file() => {
                Err(invalid("Capture object must not be a link"))
            }
            Ok(metadata) => Ok(Some(metadata.len())),
        }
    }

    pub(crate) fn open_body(&self, digest: &str) -> Result<Body> {
        if let Some(bytes) = self.staged.get(digest) {
            return Ok(Body::bytes(bytes.clone()));
        }
        let stored = small_object_store::read(&self.db, digest, SMALL_OBJECT_BYTES)
            .map_err(|_| invalid("Capture object body differs from its identity"))?;
        if let Some(bytes) = stored {
            return Ok(Body::bytes(bytes));
        }
        Ok(Body::File(crate::trust_boundary::open_regular_source(
            &self.objects.join(digest),
        )?))
    }

    /// The file holding this body, when a file is where it lives. A body the
    /// database holds has no path, and a caller protecting physical inputs
    /// names it by identity instead.
    pub(crate) fn file_path(&self, digest: &str) -> Result<Option<PathBuf>> {
        if small_object_store::size(&self.db, digest)
            .map_err(|_| invalid("Capture object identity is invalid"))?
            .is_some()
        {
            return Ok(None);
        }
        let path = self.objects.join(digest);
        Ok(path.is_file().then_some(path))
    }

    /// The whole body, for callers that need it in memory anyway.
    pub(crate) fn read_all(&self, digest: &str) -> Result<Vec<u8>> {
        use std::io::Read as _;
        let mut body = self.open_body(digest)?;
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub(crate) fn open_source(&self, source: &ObjectSource) -> Result<Body> {
        match source {
            ObjectSource::File(path) => Ok(Body::File(
                crate::trust_boundary::open_regular_source(path)?,
            )),
            ObjectSource::Captured(digest) => self.open_body(digest),
            // A library body belongs to the asset repository, which is the only
            // store that may hand one out.
            ObjectSource::Library(_) => Err(invalid("Capture source is not a capture body")),
            ObjectSource::Unchanged => Err(invalid("An unchanged record has no fetched body")),
        }
    }

    /// Confirm one object is present at the stated length, and when asked, that
    /// what is stored still matches the identity it is filed under.
    pub(crate) fn validate(&self, digest: &str, bytes: i64, contents: bool) -> Result<()> {
        self.validate_checked(digest, bytes, contents, &|| false)
    }

    /// `validate`, asking `cancelled` before every read of the body.
    pub(crate) fn validate_checked(
        &self,
        digest: &str,
        bytes: i64,
        contents: bool,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<()> {
        let expected =
            u64::try_from(bytes).map_err(|_| invalid("Capture object length is invalid"))?;
        match self.stat(digest)? {
            Some(size) if size == expected => (),
            Some(_) => return Err(invalid("Capture object length differs")),
            None => return Err(invalid("Capture object is missing")),
        }
        if !contents {
            return Ok(());
        }
        let mut body = CancellableRead::new(self.open_body(digest)?, cancelled);
        let actual = hash_reader(&mut body, expected).map_err(|_| {
            if body.refused() {
                invalid("Capture object check cancelled")
            } else {
                invalid("Capture object body differs from its identity")
            }
        })?;
        if hex::encode(actual) != digest {
            return Err(invalid("Capture object body differs from its identity"));
        }
        Ok(())
    }

    /// Identities and sizes of the database bodies, in identity order. Files
    /// keep their own enumeration through the object directory.
    pub(crate) fn page(&self, after: &str, limit: usize) -> Result<Vec<(String, u64)>> {
        small_object_store::page(&self.db, after, limit)
            .map_err(|_| invalid("Capture content store is unavailable"))
    }

    /// Remove a group the caller has established is unreferenced.
    pub(crate) fn delete(&mut self, digests: &[&str]) -> Result<usize> {
        let tx = self.db.transaction()?;
        let removed = small_object_store::delete_batch(&tx, digests)
            .map_err(|_| invalid("Capture object identity is invalid"))?;
        tx.commit()?;
        if removed > 0 {
            self.db
                .execute_batch("PRAGMA incremental_vacuum; PRAGMA wal_checkpoint(TRUNCATE);")?;
        }
        Ok(removed)
    }

    #[cfg(test)]
    pub(crate) fn set_wal_high_water(&mut self, bytes: u64) {
        self.wal_high_water = bytes;
    }

    /// Publish through the file layout whatever the size, so a measurement can
    /// compare the two placements over the same bodies.
    #[cfg(test)]
    pub(crate) fn put_as_file(&self, expected: &str, bytes: &[u8]) -> Result<()> {
        self.write_file(expected, bytes)
    }

    /// The shared immutable file path, kept for bodies above the threshold.
    fn write_file(&self, expected: &str, bytes: &[u8]) -> Result<()> {
        use std::io::Write as _;
        let path = self.objects.join(expected);
        let staging_path = self
            .objects
            .join(format!(".capture-{}.partial", uuid::Uuid::new_v4()));
        let mut staging_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging_path)?;
        let mut staging = super::capture::OwnedCaptureFile::new(staging_path.clone(), &self.objects);
        staging_file.write_all(bytes)?;
        staging_file.sync_all()?;
        drop(staging_file);
        let _ = sync_directory(&self.objects)?;
        #[cfg(target_os = "android")]
        let publication = crate::trust_boundary::rename_without_replace(&staging_path, &path);
        #[cfg(not(target_os = "android"))]
        let publication = fs::hard_link(&staging_path, &path);
        match publication {
            Ok(()) => {
                let _ = sync_directory(&self.objects)?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if is_link_like(&fs::symlink_metadata(&path)?) {
                    return Err(invalid("Capture object must not be a link"));
                }
                let mut file = File::open(&path)?;
                let actual = hash_reader(&mut file, bytes.len() as u64)
                    .map_err(|_| invalid("Existing capture object differs"))?;
                if hex::encode(actual) != expected {
                    return Err(invalid("Existing capture object differs"));
                }
            }
            Err(error) => return Err(error.into()),
        }
        let _ = staging.remove_and_sync()?;
        Ok(())
    }

    /// Flush the object directory after a capture publishes its files.
    pub(crate) fn sync_objects(&self) -> Result<()> {
        sync_directory(&self.objects)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn body(seed: usize, len: usize) -> Vec<u8> {
        let mut bytes = format!("synthetic capture body {seed:08} ").into_bytes();
        bytes.resize(len, (seed % 251) as u8);
        bytes
    }

    fn digest(bytes: &[u8]) -> String {
        hex::encode(hash(bytes))
    }

    #[test]
    fn a_body_goes_to_the_store_its_size_belongs_to_and_reads_back_either_way() {
        let root = tempfile::tempdir().unwrap();
        let mut store = ContentStore::open(root.path()).unwrap();
        let small = body(1, SMALL_OBJECT_BYTES);
        let large = body(2, SMALL_OBJECT_BYTES + 1);
        for bytes in [&small, &large] {
            store.put(&digest(bytes), bytes).unwrap();
        }
        store.commit().unwrap();
        assert!(store.file_path(&digest(&small)).unwrap().is_none());
        assert!(store.file_path(&digest(&large)).unwrap().is_some());
        for bytes in [&small, &large] {
            let hash = digest(bytes);
            assert_eq!(store.stat(&hash).unwrap(), Some(bytes.len() as u64));
            assert_eq!(&store.read_all(&hash).unwrap(), bytes);
            store.validate(&hash, bytes.len() as i64, true).unwrap();
            assert!(store.validate(&hash, bytes.len() as i64 - 1, false).is_err());
        }
        // Offering one identity's bytes under another is never a placement.
        assert!(store.put(&digest(&large), &small).is_err());
        assert!(store.stat(&digest(&body(3, 8))).unwrap().is_none());
    }

    /// A reader holding the log stops a capture before its log passes the
    /// mark by more than one group, keeps the refused group staged, and lets
    /// the same group land once the reader is gone.
    #[test]
    fn a_held_reader_refuses_the_next_group_and_its_release_admits_it() {
        let root = tempfile::tempdir().unwrap();
        let mut store = ContentStore::open(root.path()).unwrap();
        store.set_wal_high_water(512 * 1024);
        let first = body(1, 4096);
        store.put(&digest(&first), &first).unwrap();
        store.commit().unwrap();
        let reader = rusqlite::Connection::open(root.path().join("content.sqlite")).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader
            .query_row("SELECT count(*) FROM small_objects", [], |r| r.get(0))
            .unwrap();
        let mut written = vec![first];
        let refused = loop {
            let group = (0..32)
                .map(|index| body(1000 + written.len() * 32 + index, 4096))
                .collect::<Vec<_>>();
            for bytes in &group {
                store.put(&digest(bytes), bytes).unwrap();
            }
            match store.commit() {
                Ok(()) => written.extend(group),
                Err(error) => {
                    assert!(error.to_string().contains("could not be saved now"), "{error}");
                    break group;
                }
            }
            assert!(written.len() < 32 * 64, "the held reader never stopped the capture");
        };
        let log = std::fs::metadata(root.path().join("content.sqlite-wal")).unwrap().len();
        assert!(log < 2 * 1024 * 1024, "the log reached {log} bytes");
        // The refused group is still what the capture holds.
        for bytes in &refused {
            assert_eq!(&store.read_all(&digest(bytes)).unwrap(), bytes);
        }
        reader.execute_batch("COMMIT").unwrap();
        drop(reader);
        store.commit().unwrap();
        drop(store);
        let store = ContentStore::open(root.path()).unwrap();
        for bytes in written.iter().chain(&refused) {
            assert_eq!(&store.read_all(&digest(bytes)).unwrap(), bytes);
        }
    }

    #[test]
    fn a_staged_group_reads_back_before_it_lands_and_an_abandoned_one_leaves_nothing() {
        let root = tempfile::tempdir().unwrap();
        let mut store = ContentStore::open(root.path()).unwrap();
        let kept = body(10, 1024);
        let abandoned = body(11, 1024);
        store.put(&digest(&kept), &kept).unwrap();
        // A caller that was handed an identity can use it before the group
        // becomes durable.
        assert_eq!(store.read_all(&digest(&kept)).unwrap(), kept);
        store.commit().unwrap();
        store.put(&digest(&abandoned), &abandoned).unwrap();
        store.discard();
        assert!(store.stat(&digest(&abandoned)).unwrap().is_none());

        // A group that reaches the bound is written before the caller asks,
        // so an abandoned capture leaves collectable bodies, not lost memory.
        let staged = (0..GROUP_ROWS + 1)
            .map(|seed| {
                let bytes = body(100 + seed, 1024);
                let hash = digest(&bytes);
                store.put(&hash, &bytes).unwrap();
                hash
            })
            .collect::<Vec<_>>();
        store.discard();
        let durable = staged
            .iter()
            .filter(|hash| store.stat(hash).unwrap().is_some())
            .count();
        assert_eq!(durable, GROUP_ROWS);

        let reopened = ContentStore::open(root.path()).unwrap();
        assert_eq!(reopened.read_all(&digest(&kept)).unwrap(), kept);
        assert!(reopened.stat(&digest(&abandoned)).unwrap().is_none());
    }

    #[test]
    fn a_damaged_body_is_reported_and_collection_returns_its_pages() {
        let root = tempfile::tempdir().unwrap();
        let mut store = ContentStore::open(root.path()).unwrap();
        let bytes = body(20, 4096);
        let hash = digest(&bytes);
        store.put(&hash, &bytes).unwrap();
        store.commit().unwrap();
        store
            .db
            .execute(
                "UPDATE small_objects SET body=?1 WHERE hash=?2",
                rusqlite::params![b"damaged".to_vec(), hash],
            )
            .unwrap();
        assert!(store.read_all(&hash).is_err());
        assert!(store.validate(&hash, bytes.len() as i64, true).is_err());

        let filler = (0..64)
            .map(|seed| {
                let bytes = body(200 + seed, SMALL_OBJECT_BYTES / 2);
                let hash = digest(&bytes);
                store.put(&hash, &bytes).unwrap();
                hash
            })
            .collect::<Vec<_>>();
        store.commit().unwrap();
        let before = fs::metadata(root.path().join("content.sqlite")).unwrap().len();
        assert_eq!(
            store
                .delete(&filler.iter().map(String::as_str).collect::<Vec<_>>())
                .unwrap(),
            filler.len()
        );
        assert!(fs::metadata(root.path().join("content.sqlite")).unwrap().len() < before);
        assert_eq!(store.page("", 100).unwrap().len(), 1);
    }

    #[test]
    fn attaching_reads_an_existing_store_and_creates_nothing() {
        let root = tempfile::tempdir().unwrap();
        assert!(ContentStore::open_existing(&root.path().join("absent"))
            .unwrap()
            .is_none());
        assert!(!root.path().join("absent").exists());
        let bytes = body(30, 512);
        let mut store = ContentStore::open(root.path()).unwrap();
        store.put(&digest(&bytes), &bytes).unwrap();
        store.commit().unwrap();
        let attached = ContentStore::open_existing(root.path()).unwrap().unwrap();
        assert_eq!(attached.read_all(&digest(&bytes)).unwrap(), bytes);
        let mut opened = attached.open_source(&ObjectSource::Captured(digest(&bytes))).unwrap();
        let mut read = Vec::new();
        opened.read_to_end(&mut read).unwrap();
        assert_eq!(read, bytes);
    }

    #[test]
    #[ignore = "Explicit synthetic capture durability measurement"]
    fn capture_publication_cost_measurement() {
        const BODIES: usize = 1000;
        let root = tempfile::tempdir().unwrap();
        let bodies = (0..BODIES).map(|seed| body(seed, 2048)).collect::<Vec<_>>();
        for (pass, as_files) in [(0, true), (1, false)] {
            let directory = root.path().join(format!("pass-{pass}"));
            let mut store = ContentStore::open(&directory).unwrap();
            let started = std::time::Instant::now();
            for bytes in &bodies {
                let hash = digest(bytes);
                if as_files {
                    store.put_as_file(&hash, bytes).unwrap();
                } else {
                    store.put(&hash, bytes).unwrap();
                }
            }
            store.commit().unwrap();
            let files = fs::read_dir(directory.join("objects")).unwrap().count();
            eprintln!(
                "pass={pass} as_files={as_files} bodies={BODIES} object_files={files} elapsed_ms={}",
                started.elapsed().as_millis()
            );
        }
    }
}
