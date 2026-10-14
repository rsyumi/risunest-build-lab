use crate::{masking, model::*};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Weak},
};

fn err(_: impl std::fmt::Debug) -> String {
    "network-log-storage-failed".into()
}
fn private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(err)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(err)?;
    }
    Ok(())
}
fn file(path: &Path, append: bool) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(err)
}
fn valid_id(id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| "invalid-network-log-id".into())
}
fn schema(retained: bool) -> &'static str {
    if retained {
        "main"
    } else {
        "recent"
    }
}

pub(crate) struct Store {
    db: Connection,
    recent: PathBuf,
    retained: PathBuf,
    pub(crate) bytes: [u64; 2],
    pub stop_active: bool,
    recent_count: u64,
    overhead: [u64; 2],
    writers: HashMap<(String, usize), File>,
    started: HashMap<String, std::time::Instant>,
    readers: HashMap<String, Weak<()>>,
}
pub struct Snapshot {
    pub record: Record,
    pub(crate) request: File,
    pub(crate) response: File,
    pub(crate) events: File,
    _lease: Arc<()>,
}
impl Store {
    pub fn open(recent: PathBuf, retained: PathBuf) -> Result<Self, String> {
        if recent == retained || recent.starts_with(&retained) || retained.starts_with(&recent) {
            return Err("invalid-network-log-roots".into());
        }
        if recent.exists() {
            fs::remove_dir_all(&recent).map_err(err)?;
        }
        private_dir(&recent)?;
        private_dir(&retained)?;
        let db = Connection::open(retained.join("index.sqlite")).map_err(err)?;
        db.execute(
            "ATTACH DATABASE ?1 AS recent",
            [recent.join("index.sqlite").to_string_lossy().as_ref()],
        )
        .map_err(err)?;
        for name in ["main", "recent"] {
            db.execute_batch(&format!("PRAGMA {name}.journal_mode=DELETE; PRAGMA {name}.synchronous=NORMAL; PRAGMA {name}.cache_size=-512;
                PRAGMA {name}.auto_vacuum=INCREMENTAL;
                CREATE TABLE IF NOT EXISTS {name}.records(id TEXT PRIMARY KEY, started INTEGER NOT NULL, source TEXT NOT NULL,
                    method TEXT NOT NULL, url TEXT NOT NULL, status INTEGER, state TEXT NOT NULL, bytes INTEGER NOT NULL, detail TEXT NOT NULL);
                CREATE INDEX IF NOT EXISTS {name}.records_time ON records(started DESC, id DESC);" )).map_err(err)?;
        }
        let mut store = Self {
            db,
            recent,
            retained,
            bytes: [0, 0],
            stop_active: false,
            recent_count: 0,
            overhead: [0, 0],
            writers: HashMap::new(),
            started: HashMap::new(),
            readers: HashMap::new(),
        };
        // Only promotion manifests can recover a move interrupted before its index commit.
        for entry in fs::read_dir(&store.retained).map_err(err)? {
            let entry = entry.map_err(err)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if valid_id(&name).is_err() || !entry.file_type().map_err(err)?.is_dir() {
                continue;
            }
            let manifest = entry.path().join("promotion.json");
            if manifest.exists() {
                let mut record: Record = serde_json::from_reader(
                    File::open(&manifest)
                        .map_err(err)?
                        .take((METADATA_BYTES * 4) as u64),
                )
                .map_err(err)?;
                if record.id != name {
                    return Err("invalid-network-log-promotion".into());
                }
                record.retained = true;
                store.update(&record)?;
                fs::remove_file(manifest).map_err(err)?;
            } else if !store
                .db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM main.records WHERE id=?1)",
                    [&name],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(err)?
            {
                fs::remove_dir_all(entry.path()).map_err(err)?;
            }
        }
        let unfinished = {
            let mut stmt = store
                .db
                .prepare("SELECT detail FROM main.records WHERE state='active'")
                .map_err(err)?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(err)?
        };
        for json in unfinished {
            let mut record: Record = serde_json::from_str(&json).map_err(err)?;
            record.state = "incomplete".into();
            record.capture_error = Some("interrupted".into());
            for side in [Side::Request, Side::Response] {
                let actual = fs::metadata(store.dir(&record).join(side.file()))
                    .map(|m| m.len())
                    .unwrap_or(0);
                record.body_mut(side).captured_bytes = actual.min(record.body(side).captured_bytes);
                record.body_mut(side).truncated = true;
                if let Ok(file) = OpenOptions::new()
                    .write(true)
                    .open(store.dir(&record).join(side.file()))
                {
                    file.set_len(record.body(side).captured_bytes)
                        .map_err(err)?;
                }
            }
            if let Ok(file) = OpenOptions::new()
                .write(true)
                .open(store.dir(&record).join("events"))
            {
                file.set_len(record.event_bytes).map_err(err)?;
            }
            store.update(&record)?;
        }
        store.bytes[1] = store
            .db
            .query_row("SELECT coalesce(sum(bytes),0) FROM main.records", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(err)? as u64;
        store.refresh_overhead(false);
        store.refresh_overhead(true);
        Ok(store)
    }
    pub fn settings(&self) -> Result<Settings, String> {
        let path = self.retained.join("settings.json");
        if !path.exists() {
            return Ok(Settings::default());
        }
        let settings: Settings =
            serde_json::from_reader(File::open(path).map_err(err)?.take(4096)).map_err(err)?;
        settings.validate()?;
        Ok(settings)
    }
    pub fn configure(&mut self, settings: &Settings) -> Result<(), String> {
        let tmp = self.retained.join("settings.tmp");
        let mut output = file(&tmp, false)?;
        serde_json::to_writer(&mut output, settings).map_err(err)?;
        output.sync_all().map_err(err)?;
        drop(output);
        fs::rename(tmp, self.retained.join("settings.json")).map_err(err)?;
        self.refresh_overhead(true);
        let _ = self.room(false, 0, settings, None);
        Ok(())
    }
    fn root(&self, retained: bool) -> &Path {
        if retained {
            &self.retained
        } else {
            &self.recent
        }
    }
    pub(crate) fn dir(&self, record: &Record) -> PathBuf {
        self.root(record.retained).join(&record.id)
    }
    fn refresh_overhead(&mut self, retained: bool) {
        self.overhead[usize::from(retained)] =
            ["index.sqlite", "index.sqlite-journal", "settings.json"]
                .iter()
                .map(|p| {
                    fs::metadata(self.root(retained).join(p))
                        .map(|m| m.len())
                        .unwrap_or(0)
                })
                .sum::<u64>();
    }
    fn used(&self, retained: bool) -> u64 {
        self.bytes[usize::from(retained)] + self.overhead[usize::from(retained)]
    }
    fn room(
        &mut self,
        retained: bool,
        extra: u64,
        settings: &Settings,
        inserting: Option<&str>,
    ) -> Result<(), String> {
        let limit = if retained {
            settings.retained_mib
        } else {
            settings.recent_mib
        } * MIB;
        loop {
            if self
                .used(retained)
                .saturating_add(extra)
                .saturating_add(2 * MIB)
                <= limit
                && (retained
                    || self.recent_count + u64::from(inserting.is_some())
                        <= settings.recent_count as u64)
            {
                return Ok(());
            }
            if retained {
                return Err("quota".into());
            }
            let oldest = {
                let mut stmt = self.db.prepare("SELECT id FROM recent.records WHERE state!='active' ORDER BY started,id LIMIT 5").map_err(err)?;
                let ids = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(err)?;
                ids.collect::<Result<Vec<_>, _>>()
                    .map_err(err)?
                    .into_iter()
                    .find(|id| !self.pinned(id))
            };
            let Some(id) = oldest else {
                return Err("quota".into());
            };
            self.delete(&id)?;
            self.db
                .execute_batch("PRAGMA recent.incremental_vacuum;")
                .map_err(err)?;
            self.refresh_overhead(false);
        }
    }
    pub fn insert(&mut self, record: &Record, settings: &Settings) -> Result<(), String> {
        let overhead = serde_json::to_vec(record).map_err(err)?.len() as u64 * 2 + 16 * 1024;
        self.room(record.retained, overhead, settings, Some(&record.id))?;
        let dir = self.dir(record);
        private_dir(&dir)?;
        for name in ["request", "response", "events"] {
            file(&dir.join(name), false)?;
        }
        if let Err(error) = self.update(record) {
            let _ = fs::remove_dir_all(&dir);
            return Err(error);
        }
        if !record.retained {
            self.recent_count += 1;
        }
        if record.state == "active" {
            self.started
                .insert(record.id.clone(), std::time::Instant::now());
        }
        Ok(())
    }
    pub fn admit_metadata(&mut self, record: &Record, settings: &Settings) -> Result<(), String> {
        self.room(
            record.retained,
            serde_json::to_vec(record).map_err(err)?.len() as u64 * 2 + 16 * 1024,
            settings,
            None,
        )
    }
    pub fn finish(&mut self, id: &str) {
        self.writers.retain(|(key, _), _| key != id);
        self.started.remove(id);
    }
    pub fn update(&mut self, record: &Record) -> Result<(), String> {
        self.db
            .execute(
                &format!(
                    "INSERT OR REPLACE INTO {}.records VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    schema(record.retained)
                ),
                params![
                    record.id,
                    record.started_at as i64,
                    record.source,
                    record.request.method,
                    record.request.url,
                    record.response.status,
                    record.state,
                    (record.request.body.captured_bytes
                        + record.response.body.captured_bytes
                        + record.event_bytes) as i64,
                    serde_json::to_string(record).map_err(err)?
                ],
            )
            .map_err(err)?;
        self.refresh_overhead(record.retained);
        Ok(())
    }
    pub fn append(
        &mut self,
        record: &mut Record,
        side: Side,
        bytes: &[u8],
        elapsed: u64,
        kind: Option<&str>,
        settings: &Settings,
    ) -> Result<(), String> {
        let observation = Observation {
            elapsed_ms: elapsed,
            side,
            offset: record.body(side).captured_bytes,
            length: bytes.len() as u64,
            kind: kind.unwrap_or("chunk").into(),
        };
        let event = if record.stream_kind.as_deref() == Some("websocket")
            || (record.stream_kind.as_deref() == Some("sse") && matches!(side, Side::Response))
        {
            let mut data = serde_json::to_vec(&observation).map_err(err)?;
            data.push(b'\n');
            data
        } else {
            vec![]
        };
        self.room(
            record.retained,
            bytes.len() as u64 + event.len() as u64,
            settings,
            None,
        )?;
        let path = self.dir(record);
        let key = (record.id.clone(), side.index());
        if !self.writers.contains_key(&key) {
            self.writers
                .insert(key.clone(), file(&path.join(side.file()), true)?);
        }
        let body = self.writers.get_mut(&key).unwrap();
        let mut remaining = bytes;
        while !remaining.is_empty() {
            match body.write(remaining) {
                Ok(0) => return Err("network-log-storage-failed".into()),
                Ok(written) => {
                    record.body_mut(side).captured_bytes += written as u64;
                    self.bytes[usize::from(record.retained)] += written as u64;
                    remaining = &remaining[written..];
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(err(error)),
            }
        }
        if !event.is_empty() {
            let key = (record.id.clone(), 2);
            if !self.writers.contains_key(&key) {
                self.writers
                    .insert(key.clone(), file(&path.join("events"), true)?);
            }
            let events = self.writers.get_mut(&key).unwrap();
            if events.write_all(&event).is_err() {
                events.set_len(record.event_bytes).map_err(err)?;
                return Err("network-log-storage-failed".into());
            }
            record.event_bytes += event.len() as u64;
            self.bytes[usize::from(record.retained)] += event.len() as u64;
        }
        Ok(())
    }
    pub fn record(&self, id: &str) -> Result<Record, String> {
        valid_id(id)?;
        let json: Option<String> = self.db.query_row("SELECT detail FROM main.records WHERE id=?1 UNION ALL SELECT detail FROM recent.records WHERE id=?1 LIMIT 1", [id], |r| r.get(0)).optional().map_err(err)?;
        let mut record: Record =
            serde_json::from_str(&json.ok_or_else(|| "network-log-expired".to_string())?)
                .map_err(err)?;
        self.elapsed(&mut record);
        Ok(record)
    }
    fn elapsed(&self, record: &mut Record) {
        if record.state == "active" {
            if let Some(start) = self.started.get(&record.id) {
                record.elapsed_ms = start.elapsed().as_millis() as u64;
            }
        }
    }
    pub fn list(&self, query: &Query) -> Result<Listing, String> {
        if query.search.len() > 1024 || query.offset > 1_000_000 {
            return Err("invalid-network-log-query".into());
        }
        let settings = self.settings()?;
        let tables = if query.retained {
            "SELECT * FROM main.records".into()
        } else {
            format!("SELECT * FROM (SELECT * FROM recent.records UNION ALL SELECT * FROM main.records) ORDER BY started DESC,id DESC LIMIT {}", settings.recent_count)
        };
        let status = match query.status.as_str() {
            "error" => "(status>=400 OR state='error' OR state='incomplete')",
            "active" => "state='active'",
            _ => "1",
        };
        let mut stmt = self.db.prepare(&format!("SELECT detail FROM ({tables}) WHERE instr(lower(url||' '||source),lower(?1))>0 AND {status} ORDER BY started DESC,id DESC LIMIT 51 OFFSET ?2")).map_err(err)?;
        let rows = stmt
            .query_map(params![query.search, query.offset], |r| {
                r.get::<_, String>(0)
            })
            .map_err(err)?;
        let mut items = Vec::new();
        for row in rows {
            let mut record: Record = serde_json::from_str(&row.map_err(err)?).map_err(err)?;
            self.elapsed(&mut record);
            items.push(ListItem::from(&record));
        }
        let has_more = items.len() > 50;
        items.truncate(50);
        Ok(Listing {
            items,
            has_more,
            recent_bytes: self.used(false),
            retained_bytes: self.used(true),
            warning: (self.used(false) > settings.recent_mib * MIB
                || self.used(true) > settings.retained_mib * MIB)
                .then(|| "quota".into()),
        })
    }
    fn pinned(&self, id: &str) -> bool {
        self.readers
            .get(id)
            .is_some_and(|reader| reader.strong_count() != 0)
    }
    pub fn snapshot(&mut self, id: &str) -> Result<Snapshot, String> {
        self.readers.retain(|_, reader| reader.strong_count() != 0);
        if self.readers.values().map(Weak::strong_count).sum::<usize>() >= 4 {
            return Err("network-log-readers-busy".into());
        }
        let lease = self
            .readers
            .get(id)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| Arc::new(()));
        self.readers.insert(id.into(), Arc::downgrade(&lease));
        let record = self.record(id)?;
        let dir = self.dir(&record);
        let open = |name| File::open(dir.join(name)).map_err(|_| "network-log-expired".to_string());
        Ok(Snapshot {
            record,
            _lease: lease,
            request: open("request")?,
            response: open("response")?,
            events: open("events")?,
        })
    }
    pub fn retain(&mut self, id: &str, settings: &Settings) -> Result<(), String> {
        let mut record = self.record(id)?;
        if record.retained {
            return Ok(());
        }
        let bytes = record.request.body.captured_bytes
            + record.response.body.captured_bytes
            + record.event_bytes;
        self.room(
            true,
            bytes + (METADATA_BYTES * 4) as u64,
            settings,
            Some(id),
        )?;
        self.writers.retain(|(key, _), _| key != id);
        let from = self.dir(&record);
        let to = self.retained.join(id);
        record.retained = true;
        let mut manifest = file(&from.join("promotion.json"), false)?;
        serde_json::to_writer(&mut manifest, &record).map_err(err)?;
        manifest.sync_all().map_err(err)?;
        drop(manifest);
        if fs::rename(&from, &to).is_err() {
            private_dir(&to)?;
            let copied = (|| {
                for name in ["request", "response", "events", "promotion.json"] {
                    let mut input = File::open(from.join(name)).map_err(err)?;
                    let mut output = file(&to.join(name), false)?;
                    std::io::copy(&mut input, &mut output).map_err(err)?;
                    output.sync_all().map_err(err)?;
                }
                Ok::<_, String>(())
            })();
            if let Err(error) = copied {
                let _ = fs::remove_dir_all(&to);
                return Err(error);
            }
        }
        self.db.execute_batch("BEGIN IMMEDIATE").map_err(err)?;
        let committed = self
            .update(&record)
            .and_then(|_| {
                self.db
                    .execute("DELETE FROM recent.records WHERE id=?1", [id])
                    .map(|_| ())
                    .map_err(err)
            })
            .and_then(|_| self.db.execute_batch("COMMIT").map_err(err));
        if let Err(error) = committed {
            let _ = self.db.execute_batch("ROLLBACK");
            if !from.exists() {
                let _ = fs::rename(&to, &from);
            } else {
                let _ = fs::remove_dir_all(&to);
            }
            return Err(error);
        }
        self.recent_count = self.recent_count.saturating_sub(1);
        self.refresh_overhead(false);
        self.refresh_overhead(true);
        self.bytes[0] = self.bytes[0].saturating_sub(bytes);
        self.bytes[1] += bytes;
        if from.exists() {
            fs::remove_dir_all(&from).map_err(err)?;
        }
        let _ = fs::remove_file(to.join("promotion.json"));
        Ok(())
    }
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        if self.pinned(id) {
            return Err("network-log-readers-busy".into());
        }
        let record = self.record(id)?;
        if record.state == "active" {
            return Err("network-log-request-active".into());
        }
        let dir = self.dir(&record);
        if dir.exists() {
            fs::remove_dir_all(dir).map_err(err)?;
        }
        self.db
            .execute(
                &format!(
                    "DELETE FROM {}.records WHERE id=?1",
                    schema(record.retained)
                ),
                [id],
            )
            .map_err(err)?;
        if !record.retained {
            self.recent_count = self.recent_count.saturating_sub(1);
        }
        self.refresh_overhead(record.retained);
        self.bytes[usize::from(record.retained)] = self.bytes[usize::from(record.retained)]
            .saturating_sub(
                record.request.body.captured_bytes
                    + record.response.body.captured_bytes
                    + record.event_bytes,
            );
        Ok(())
    }
    pub fn clear_recent(&mut self) -> Result<(), String> {
        self.writers.clear();
        self.db
            .execute_batch("DELETE FROM recent.records; DETACH DATABASE recent;")
            .map_err(err)?;
        fs::remove_dir_all(&self.recent).map_err(err)
    }
}
impl Snapshot {
    /// Maps SSE event end offsets to the observed chunk that delivered them. Reading starts at
    /// `from`, the `next` position returned for the preceding page, so each page reads forward
    /// over its own chunks only.
    pub fn arrival_times(&mut self, ends: &[u64], from: u64) -> Result<Arrivals, String> {
        let total = self.record.event_bytes;
        if ends.len() > 50 || ends.windows(2).any(|pair| pair[0] > pair[1]) || from > total {
            return Err("invalid-network-log-query".into());
        }
        let mut arrivals = Arrivals {
            times: vec![None; ends.len()],
            next: from,
        };
        if ends.is_empty() || self.record.stream_kind.as_deref() != Some("sse") {
            return Ok(arrivals);
        }
        if from > 0 {
            let mut previous = [0];
            self.events.seek(SeekFrom::Start(from - 1)).map_err(err)?;
            self.events.read_exact(&mut previous).map_err(err)?;
            if previous[0] != b'\n' {
                return Err("invalid-network-log-query".into());
            }
        } else {
            self.events.seek(SeekFrom::Start(0)).map_err(err)?;
        }
        let mut reader = BufReader::with_capacity(4096, (&mut self.events).take(total - from));
        let (mut index, mut position, mut line) = (0, from, String::new());
        while index < ends.len() {
            line.clear();
            let read = reader
                .by_ref()
                .take(1024)
                .read_line(&mut line)
                .map_err(err)?;
            if read == 0 {
                break;
            }
            let observation: Observation = serde_json::from_str(&line).map_err(err)?;
            if matches!(observation.side, Side::Response) {
                while index < ends.len() && ends[index] <= observation.offset + observation.length {
                    if ends[index] > observation.offset {
                        arrivals.times[index] = Some(observation.elapsed_ms);
                        // The next page's first event can end inside this same chunk.
                        arrivals.next = position;
                    }
                    index += 1;
                }
            }
            position += read as u64;
        }
        Ok(arrivals)
    }

    pub fn body_page(&mut self, side: Side, offset: u64) -> Result<BodyPage, String> {
        self.body_range(side, offset, PAGE_BYTES, None)
    }
    pub fn body_range(
        &mut self,
        side: Side,
        offset: u64,
        length: usize,
        force_hex: Option<bool>,
    ) -> Result<BodyPage, String> {
        let body = self.record.body(side);
        let total = body.captured_bytes;
        let binary = force_hex.unwrap_or(body.binary);
        let input = match side {
            Side::Request => &mut self.request,
            Side::Response => &mut self.response,
        };
        let offset = offset.min(total);
        input.seek(SeekFrom::Start(offset)).map_err(err)?;
        let mut bytes = Vec::new();
        input
            .take((total - offset).min(length.min(PAGE_BYTES) as u64))
            .read_to_end(&mut bytes)
            .map_err(err)?;
        // Keep a split UTF-8 codepoint for the following page.
        if !binary && offset + (bytes.len() as u64) < total {
            if let Err(error) = std::str::from_utf8(&bytes) {
                if error.error_len().is_none() {
                    bytes.truncate(error.valid_up_to());
                }
            }
        }
        let next_offset = offset + bytes.len() as u64;
        let text = if binary {
            masking::hex(&bytes)
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        Ok(BodyPage {
            text,
            offset,
            next_offset,
            total,
            binary,
        })
    }
    pub fn observations(&mut self, offset: u64) -> Result<BodyPage, String> {
        let total = self.record.event_bytes;
        let offset = offset.min(total);
        self.events.seek(SeekFrom::Start(offset)).map_err(err)?;
        let mut bytes = Vec::new();
        (&mut self.events)
            .take((total - offset).min(PAGE_BYTES as u64))
            .read_to_end(&mut bytes)
            .map_err(err)?;
        if let Some((end, _)) = bytes
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .nth(49)
        {
            bytes.truncate(end + 1);
        } else if offset + (bytes.len() as u64) < total {
            if let Some(end) = bytes.iter().rposition(|b| *b == b'\n') {
                bytes.truncate(end + 1);
            }
        }
        Ok(BodyPage {
            next_offset: offset + bytes.len() as u64,
            text: String::from_utf8_lossy(&bytes).into_owned(),
            offset,
            total,
            binary: false,
        })
    }
    pub fn copy_body(&mut self, side: Side) -> Result<String, String> {
        let body = self.record.body(side);
        if body.captured_bytes > 16 * MIB {
            return Err("network-log-clipboard-too-large".into());
        }
        if body.binary {
            return Err("network-log-binary-body".into());
        }
        let input = match side {
            Side::Request => &mut self.request,
            Side::Response => &mut self.response,
        };
        input.seek(SeekFrom::Start(0)).map_err(err)?;
        let mut bytes = Vec::new();
        input
            .take(body.captured_bytes)
            .read_to_end(&mut bytes)
            .map_err(err)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}
