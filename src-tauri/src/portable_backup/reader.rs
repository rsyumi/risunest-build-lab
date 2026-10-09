use super::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Seek, SeekFrom},
    path::Path,
};
use zip::{CompressionMethod, ZipArchive};

struct IdentityFile {
    file: File,
    identity: crate::asset_repository::ExactFileIdentity,
    metadata: bool,
    #[cfg(test)]
    scope: super::source_io::Scope,
}
impl IdentityFile {
    fn new(file: File, identity: crate::asset_repository::ExactFileIdentity, metadata: bool) -> Self {
        Self { file, identity, metadata, #[cfg(test)] scope: super::source_io::capture_scope() }
    }
    fn try_clone(&self) -> io::Result<Self> { Ok(Self::new(self.file.try_clone()?,self.identity.clone(),self.metadata)) }
    fn metadata(&self) -> io::Result<std::fs::Metadata> { self.file.metadata() }
    fn check(&self) -> io::Result<()> {
        if crate::asset_repository::exact_file_identity(&self.file)? != self.identity {
            return Err(io::Error::new(io::ErrorKind::InvalidData,"portable source identity changed"));
        }
        Ok(())
    }
}
impl Read for IdentityFile {
    fn read(&mut self,bytes:&mut[u8])->io::Result<usize> {
        #[cfg(test)] let offset=self.file.stream_position()?;
        let result=(|| {self.check()?;let count=self.file.read(bytes)?;self.check()?;Ok(count)})();
        #[cfg(test)] if self.metadata {self.scope.metadata(offset,&result);}
        result
    }
}
impl Seek for IdentityFile { fn seek(&mut self,position:SeekFrom)->io::Result<u64> {self.check()?;self.file.seek(position)} }

pub(crate) struct ArchiveObjectReader {
    input: std::io::Take<IdentityFile>,
    hash: String,
    hasher: Sha256,
    size: u64,
    consumed: u64,
    verified: bool,
    #[cfg(test)] scope: super::source_io::Scope,
}
impl Read for ArchiveObjectReader {
    fn read(&mut self,bytes:&mut[u8])->io::Result<usize> {
        #[cfg(test)] self.scope.before_object_read(&self.hash);
        #[cfg(test)] let offset=self.input.get_mut().stream_position()?;
        let result=self.input.read(bytes);
        #[cfg(test)] self.scope.read(&self.hash,offset,&result);
        let count=result?;
        self.consumed+=count as u64;
        self.hasher.update(&bytes[..count]);
        #[cfg(test)] self.scope.hash_update(&self.hash,count as u64);
        if self.consumed==self.size && !self.verified {
            let valid=hex::encode(self.hasher.clone().finalize())==self.hash;
            #[cfg(test)] self.scope.hashed(&self.hash,self.consumed,valid);
            if !valid {return Err(io::Error::new(io::ErrorKind::InvalidData,"portable object hash mismatch"));}
            self.verified=true;
        }
        if count==0 && self.consumed!=self.size {return Err(io::Error::new(io::ErrorKind::UnexpectedEof,"portable object body truncated"));}
        Ok(count)
    }
}
#[cfg(test)]
impl Drop for ArchiveObjectReader {fn drop(&mut self) {self.scope.closed(&self.hash,self.verified);}}

struct Entry {
    start: u64,
    size: u64,
    method: CompressionMethod,
    expected_hash: Option<String>,
}
pub(crate) struct VerifiedArchive {
    pub(crate) db: Connection,
    pub(crate) manifest: Manifest,
    // The catalog is job-owned and lives exactly as long as this verified handle.
    _directory: tempfile::TempDir,
    input: IdentityFile,
    identity: crate::asset_repository::ExactFileIdentity,
    packs: BTreeMap<i64, Entry>,
}

impl VerifiedArchive {
    /// The input handle must already refer to an immutable, managed input. Verification does not
    /// turn an arbitrary mutable desktop path into an immutable restore source.
    pub(crate) fn open(input: File, job_directory: &Path, probe: &dyn CancellationProbe) -> Result<Self> {
        Self::open_inner(input, job_directory, probe, true)
    }

    pub(crate) fn open_for_restore(input: File, job_directory: &Path, probe: &dyn CancellationProbe) -> Result<Self> {
        Self::open_inner(input, job_directory, probe, false)
    }

    fn open_inner(
        input: File,
        job_directory: &Path,
        probe: &dyn CancellationProbe,
        verify_bodies: bool,
    ) -> Result<Self> {
        check(probe)?;
        let identity = crate::asset_repository::exact_file_identity(&input)?;
        let mut input = IdentityFile::new(input,identity.clone(),true);
        let length = input.metadata()?.len();
        if length < 22 {
            return Err(Error::Invalid("truncated ZIP end record"));
        }
        input.seek(SeekFrom::End(-22))?;
        let mut end = [0; 22];
        input.read_exact(&mut end)?;
        if &end[..4] != b"PK\x05\x06" || end[20..] != [0, 0] {
            return Err(Error::Invalid("missing canonical ZIP end record"));
        }
        input.seek(SeekFrom::Start(0))?;
        let mut zip = ZipArchive::new(input.try_clone()?)?;
        if zip.offset() != 0 || zip.len() < 3 {
            return Err(Error::Invalid("invalid portable ZIP layout"));
        }
        let mut entries = BTreeMap::new();
        let mut ranges = Vec::new();
        let mut central_start = length;
        for index in 0..zip.len() {
            check(probe)?;
            let entry = zip.by_index(index)?;
            let name = entry.name().to_string();
            if entry.name_raw() != name.as_bytes()
                || entry.is_dir()
                || name.contains('\\')
                || name
                    .split('/')
                    .any(|s| s.is_empty() || s == ".." || s == ".")
                || name.starts_with('/')
            {
                return Err(Error::Invalid("unsafe ZIP entry name"));
            }
            let method = entry.compression();
            if method != CompressionMethod::Stored
                && !(name == "archive.sqlite" && method == CompressionMethod::Deflated)
            {
                return Err(Error::Invalid("unexpected ZIP compression method"));
            }
            if method == CompressionMethod::Stored && entry.size() != entry.compressed_size() {
                return Err(Error::Invalid("stored ZIP lengths disagree"));
            }
            let end = entry
                .data_start()
                .checked_add(entry.compressed_size())
                .ok_or(Error::Invalid("ZIP range overflow"))?;
            if entry.header_start() >= entry.data_start() || end > length {
                return Err(Error::Invalid("invalid ZIP payload range"));
            }
            ranges.push((entry.header_start(), end));
            central_start = central_start.min(entry.central_header_start());
            if entries
                .insert(
                    name,
                    Entry {
                        start: entry.data_start(),
                        size: entry.size(),
                        method,
                        expected_hash: None,
                    },
                )
                .is_some()
            {
                return Err(Error::Invalid("duplicate ZIP entry"));
            }
        }
        ranges.sort_unstable();
        if ranges[0].0 != 0
            || ranges.windows(2).any(|w| w[0].1 != w[1].0)
            || ranges.last().unwrap().1 != central_start
        {
            return Err(Error::Invalid("overlapping ZIP ranges"));
        }
        let format: Format = small_json(&mut zip, "format.json")?;
        if format.magic != FORMAT
            || format.format_version != VERSION
            || format.sqlite_schema_version != VERSION
            || format.source_app_build.len() > 256
            || uuid::Uuid::parse_str(&format.capture_id).is_err()
        {
            return Err(Error::Invalid("unsupported portable backup format"));
        }
        decimal(&format.created_at)?;
        let manifest: Manifest = small_json(&mut zip, "manifest.json")?;
        if manifest.capture_id != format.capture_id || !hash_valid(&manifest.catalog_sha256) {
            return Err(Error::Invalid("archive manifest identity mismatch"));
        }
        let catalog_length = decimal(&manifest.catalog_bytes)?;
        let pack_count = decimal(&manifest.pack_count)?;
        if pack_count.checked_add(3) != Some(zip.len() as u64) {
            return Err(Error::Invalid("unexpected ZIP entry inventory"));
        }
        let directory = tempfile::Builder::new()
            .prefix("portable-read-")
            .tempdir_in(job_directory)?;
        let path = directory.path().join("archive.sqlite");
        {
            let mut catalog = zip.by_name("archive.sqlite")?;
            if catalog.size() != catalog_length {
                return Err(Error::Invalid("catalog size differs from manifest"));
            }
            let mut output = File::create(&path)?;
            let copied=copy_hash_observed(&mut catalog,&mut output,catalog_length,probe,|_bytes| {
                #[cfg(test)] super::source_io::capture_scope().catalog_hash(_bytes as u64);
            });
            #[cfg(test)] super::source_io::capture_scope().catalog_checked(copied.as_ref().is_ok_and(|hash|hash==&manifest.catalog_sha256));
            if copied? != manifest.catalog_sha256 {
                return Err(Error::Invalid("catalog hash mismatch"));
            }
            if catalog.read(&mut [0])? != 0 {
                return Err(Error::Invalid("catalog has undeclared bytes"));
            }
            output.sync_all()?;
        }
        let db = crate::sqlite_open::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON; PRAGMA cache_size=-16384; PRAGMA temp_store=FILE;")?;
        catalog::verify_schema(&db)?;
        let capture: Option<String> = db
            .query_row(
                "SELECT value FROM backup_info WHERE key='captureId'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if capture.as_deref() != Some(&manifest.capture_id) {
            return Err(Error::Invalid("catalog capture identity mismatch"));
        }
        let profile: String = db.query_row(
            "SELECT value FROM backup_info WHERE key='profile'",
            [],
            |r| r.get(0),
        )?;
        let library: String = db.query_row(
            "SELECT value FROM backup_info WHERE key='libraryIncluded'",
            [],
            |r| r.get(0),
        )?;
        let device: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM device_sections WHERE included=1)",
            [],
            |r| r.get(0),
        )?;
        if profile
            != match manifest.profile {
                Profile::Portable => "portable",
                Profile::SourceSqlite => "source-sqlite",
            }
            || !matches!(library.as_str(), "true" | "false")
            || (library == "true") != manifest.library_included
            || device != manifest.device_included
        {
            return Err(Error::Invalid("catalog profile differs from manifest"));
        }
        let diagnostics: bool =
            db.query_row("SELECT EXISTS(SELECT 1 FROM diagnostics)", [], |r| r.get(0))?;
        if diagnostics && !manifest.repair_required {
            return Err(Error::Invalid("archive diagnostics require repair"));
        }
        if !manifest.library_included {
            for table in crate::persistent_store::portable::TABLES {
                let present: bool = db.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {})", table.name),
                    [],
                    |r| r.get(0),
                )?;
                if present {
                    return Err(Error::Invalid("excluded library contains raw rows"));
                }
            }
        }
        if manifest.profile == Profile::SourceSqlite {
            let snapshot:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM files WHERE kind='preserved' AND logical_key='source.sqlite' AND state='present')",[],|r|r.get(0))?;
            if !manifest.library_included || !manifest.repair_required || !snapshot {
                return Err(Error::Invalid("invalid physical SQLite salvage profile"));
            }
        }
        for (table, expected) in [
            ("packs", &manifest.pack_count),
            ("objects", &manifest.object_count),
            ("files", &manifest.file_count),
        ] {
            let count: i64 =
                db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            if sql_u64(count)? != decimal(expected)? {
                return Err(Error::Invalid("manifest count mismatch"));
            }
        }
        entries.remove("format.json");
        entries.remove("manifest.json");
        entries.remove("archive.sqlite");
        let mut packs = BTreeMap::new();
        {
            let mut statement =
                db.prepare("SELECT id,entry,byte_length,sha256 FROM packs ORDER BY id")?;
            let mut rows = statement.query([])?;
            let mut expected_id = 0_i64;
            while let Some(row) = rows.next()? {
                check(probe)?;
                let id: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                let size = sql_u64(row.get(2)?)?;
                let hash: String = row.get(3)?;
                if id != expected_id
                    || name != format!("payloads/{id:06}.bin")
                    || size == 0
                    || !hash_valid(&hash)
                {
                    return Err(Error::Invalid("invalid payload pack descriptor"));
                }
                expected_id += 1;
                let mut entry = entries
                    .remove(&name)
                    .ok_or(Error::Invalid("missing payload pack"))?;
                if entry.size != size || entry.method != CompressionMethod::Stored {
                    return Err(Error::Invalid("pack ZIP descriptor mismatch"));
                }
                entry.expected_hash = Some(hash);
                packs.insert(id, entry);
            }
        }
        if !entries.is_empty() {
            return Err(Error::Invalid("undeclared ZIP entries"));
        }
        let mut verified = Self {
            db,
            manifest,
            _directory: directory,
            input,
            identity,
            packs,
        };
        verified.verify_objects(probe, verify_bodies)?;
        verified.verify_file_mappings(probe)?;
        crate::device_backup::validate_archive_catalog(&verified.db, probe).map_err(|failure| {
            if failure.code == "device-cancelled" {
                Error::Cancelled
            } else {
                Error::Invalid("invalid device archive catalog")
            }
        })?;
        verified.check_identity()?;
        Ok(verified)
    }

    pub(crate) fn check_identity(&self) -> Result<()> {
        if crate::asset_repository::exact_file_identity(&self.input.file)? != self.identity {
            return Err(Error::Invalid("portable source identity changed"));
        }
        Ok(())
    }

    pub(crate) fn source_identity_guard(&self) -> Result<(File, crate::asset_repository::ExactFileIdentity)> {
        self.check_identity()?;
        Ok((self.input.file.try_clone()?, self.identity.clone()))
    }

    fn verify_objects(&mut self, probe: &dyn CancellationProbe, verify_bodies: bool) -> Result<()> {
        let mut statement = self.db.prepare(
            "SELECT sha256,pack_id,offset,byte_length FROM objects ORDER BY pack_id,offset,sha256",
        )?;
        let mut rows = statement.query([])?;
        let mut coverage: BTreeMap<i64, u64> = self.packs.keys().map(|id| (*id, 0)).collect();
        let mut pack_hashes: BTreeMap<i64, Sha256> =
            self.packs.keys().map(|id| (*id, Sha256::new())).collect();
        while let Some(row) = rows.next()? {
            check(probe)?;
            let hash = hex::encode(row.get::<_, Vec<u8>>(0)?);
            let pack: Option<i64> = row.get(1)?;
            let offset = sql_u64(row.get(2)?)?;
            let size = sql_u64(row.get(3)?)?;
            if !hash_valid(&hash) {
                return Err(Error::Invalid("invalid object hash"));
            }
            if size == 0 {
                if pack.is_some() || offset != 0 || hash != EMPTY_HASH {
                    return Err(Error::Invalid("invalid empty object descriptor"));
                }
                continue;
            }
            let id = pack.ok_or(Error::Invalid("nonempty object has no pack"))?;
            let entry = self
                .packs
                .get(&id)
                .ok_or(Error::Invalid("object references unknown pack"))?;
            let end = offset
                .checked_add(size)
                .ok_or(Error::Invalid("object range overflow"))?;
            if end > entry.size || coverage.get(&id) != Some(&offset) {
                return Err(Error::Invalid("overlapping or incomplete object ranges"));
            }
            coverage.insert(id, end);
            #[cfg(test)] super::source_io::capture_scope().declare(&hash,entry.start+offset,size);
            if !verify_bodies { continue; }
            let (mut input,_) = self.open_object(&hash)?;
            let mut sink = PackHashSink(
                pack_hashes
                    .get_mut(&id)
                    .ok_or(Error::Invalid("unknown pack hash state"))?,
            );
            copy_verified_object(&mut input, &mut sink, size, probe)?;
        }
        if self
            .packs
            .iter()
            .any(|(id, entry)| coverage.get(id) != Some(&entry.size))
        {
            return Err(Error::Invalid("unclaimed pack payload"));
        }
        if !verify_bodies { return Ok(()); }
        for (id, hash) in pack_hashes {
            if self.packs[&id].expected_hash.as_deref()
                != Some(hex::encode(hash.finalize()).as_str())
            {
                return Err(Error::Invalid("pack hash mismatch"));
            }
        }
        Ok(())
    }

    fn verify_file_mappings(&self, probe: &dyn CancellationProbe) -> Result<()> {
        let mut payloads=self.db.prepare("SELECT hash,byte_length FROM backup_payloads")?;
        let mut payload_lookup=self.db.prepare("SELECT byte_length FROM objects WHERE sha256=?1")?;
        let mut rows=payloads.query([])?;
        while let Some(row)=rows.next()? {
            check(probe)?;
            let hash:String=row.get(0)?;
            let expected:i64=row.get(1)?;
            let binary=hex::decode(&hash).map_err(|_|Error::Invalid("invalid original payload hash"))?;
            let actual:Option<i64>=payload_lookup.query_row([binary],|row|row.get(0)).optional()?;
            if !hash_valid(&hash) || expected<0 || actual!=Some(expected) {return Err(Error::Invalid("original payload catalog mismatch"));}
        }
        drop(rows);
        drop(payloads);
        let mut statement=self.db.prepare("SELECT f.kind,f.logical_key,f.object_hash,f.expected_hash,f.state,o.sha256 FROM files f LEFT JOIN objects o ON f.object_hash=o.sha256")?;
        let mut rows = statement.query([])?;
        let mut missing = false;
        while let Some(row) = rows.next()? {
            check(probe)?;
            let kind: String = row.get(0)?;
            let key: String = row.get(1)?;
            let object: Option<Vec<u8>> = row.get(2)?;
            let expected: Option<String> = row.get(3)?;
            let state: String = row.get(4)?;
            let found: Option<Vec<u8>> = row.get(5)?;
            if !matches!(
                kind.as_str(),
                "asset" | "inlay" | "owner" | "preserved" | "device" | "unit"
            ) || key.is_empty()
                || key.contains('\0')
                || expected.as_deref().is_some_and(|v| !hash_valid(v))
            {
                return Err(Error::Invalid("invalid logical file descriptor"));
            }
            match state.as_str() {
                "present"
                    if object.is_some()
                        && found == object
                        && expected.as_ref().is_none_or(|v| {
                            object.as_ref().is_some_and(|o| hex::encode(o) == *v)
                        }) =>
                {
                    ()
                }
                "missing" if object.is_none() => missing = true,
                "damaged" if object.is_some() && found == object && expected.is_some() => {
                    missing = true
                }
                _ => return Err(Error::Invalid("invalid logical file state")),
            }
        }
        if missing && !self.manifest.repair_required {
            return Err(Error::Invalid("incomplete inventory reported as complete"));
        }
        Ok(())
    }

    /// Streams a verified object to the caller's staging sink, never to an archive-provided path.
    pub(crate) fn copy_object(
        &self,
        hash: &str,
        output: &mut (impl Write + ?Sized),
        probe: &dyn CancellationProbe,
    ) -> Result<u64> {
        let (mut input, size) = self.open_object(hash)?;
        copy_verified_object(&mut input, output, size, probe)?;
        Ok(size)
    }

    /// Consume this bounded reader completely before opening another object. File clones share
    /// a seek cursor; the SQLite handle already confines this archive to one consuming thread.
    pub(crate) fn open_object(&self, hash: &str) -> Result<(ArchiveObjectReader, u64)> {
        self.check_identity()?;
        let (pack, offset, size): (Option<i64>, i64, i64) = self.db.query_row(
            "SELECT pack_id,offset,byte_length FROM objects WHERE sha256=?1",
            [hex::decode(hash).map_err(|_| Error::Invalid("invalid requested object hash"))?],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let size = sql_u64(size)?;
        let mut input = self.input.try_clone()?;
        input.metadata=false;
        if let Some(pack) = pack {
            let entry = self
                .packs
                .get(&pack)
                .ok_or(Error::Invalid("object references unknown pack"))?;
            input.seek(SeekFrom::Start(
                entry
                    .start
                    .checked_add(sql_u64(offset)?)
                    .ok_or(Error::Invalid("object file offset overflow"))?,
            ))?;
        }
        #[cfg(test)] let scope=super::source_io::capture_scope();
        #[cfg(test)] scope.opened(hash);
        let verified=size==0;
        if verified {
            let valid=hex::encode(Sha256::digest([]))==hash;
            #[cfg(test)] scope.hashed(hash,0,valid);
            if !valid {return Err(Error::Invalid("empty object hash mismatch"));}
        }
        Ok((ArchiveObjectReader {input:input.take(size),hash:hash.into(),hasher:Sha256::new(),size,consumed:0,verified,#[cfg(test)] scope}, size))
    }
}

struct PackHashSink<'a>(&'a mut Sha256);
impl Write for PackHashSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        #[cfg(test)] super::source_io::capture_scope().pack_hash(bytes.len() as u64);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn copy_verified_object(input:&mut ArchiveObjectReader, output:&mut (impl Write + ?Sized), size:u64, probe:&dyn CancellationProbe)->Result<()> {
    let mut copied=0u64;
    let mut bytes=[0u8;64*1024];
    loop {
        check(probe)?;
        let count=input.read(&mut bytes)?;
        if count==0 {break;}
        output.write_all(&bytes[..count])?;
        copied=copied.checked_add(count as u64).ok_or(Error::Invalid("object size overflow"))?;
    }
    if copied!=size {return Err(Error::Invalid("truncated object"));}
    Ok(())
}

fn small_json<T: serde::de::DeserializeOwned>(zip: &mut ZipArchive<IdentityFile>, name: &str) -> Result<T> {
    let mut entry = zip.by_name(name)?;
    if entry.size() > SMALL_DOCUMENT_LIMIT || entry.compression() != CompressionMethod::Stored {
        return Err(Error::Invalid("oversized or compressed format document"));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
