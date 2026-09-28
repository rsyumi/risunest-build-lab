//! What a publication already knows about the remote objects it can name.
//!
//! The cache is rebuildable: every row restates something an authenticated
//! remote document already says, so losing it costs work rather than
//! correctness. It is not a second authority and never decides on its own that
//! a remote object still exists.
use super::{
    contract::{RepositoryHandle, Result},
    contract::ObjectRole,
    packaging::{corrupt, transient, EntryPlan, RemoteObject},
};
use risunest_external_storage_format::snapshot as wire;
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

/// One node of a published catalog, by the keys it covered. A later
/// publication assigns its entries to these ranges so that a node nothing
/// changed encodes to the bytes it encoded before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CatalogRange {
    pub(super) level: u16,
    pub(super) first_key: String,
    pub(super) last_key: String,
}

/// How many published graphs one connection keeps on hand, per catalog kind.
const RETAINED_GRAPHS: i64 = 64;

pub(super) fn kind_name(kind: wire::CatalogKind) -> &'static str {
    match kind {
        wire::CatalogKind::Records => "records",
        wire::CatalogKind::Assets => "assets",
        wire::CatalogKind::Section => "section",
    }
}

pub(super) struct PackageCache {
    db: Connection,
}
impl PackageCache {
    pub(super) fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root).map_err(transient)?;
        if crate::trust_boundary::is_link_like(&fs::symlink_metadata(root).map_err(transient)?) {
            return Err(corrupt("package cache is a link"));
        }
        let db_path = root.join("snapshot-cache.sqlite");
        if db_path.exists()
            && crate::trust_boundary::is_link_like(
                &fs::symlink_metadata(&db_path).map_err(transient)?,
            )
        {
            return Err(corrupt("package cache database is a link"));
        }
        let db = Connection::open(db_path).map_err(transient)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS remote_objects(
               repository_id TEXT NOT NULL, connection_identity TEXT NOT NULL,
               object_id TEXT NOT NULL, plaintext_sha256 TEXT NOT NULL,
               value TEXT NOT NULL,
               PRIMARY KEY(repository_id,connection_identity,object_id));
             CREATE INDEX IF NOT EXISTS remote_objects_plaintext
               ON remote_objects(repository_id,connection_identity,plaintext_sha256);
             CREATE TABLE IF NOT EXISTS entries(
               repository_id TEXT NOT NULL, connection_identity TEXT NOT NULL,
               catalog_kind TEXT NOT NULL, entry_key TEXT NOT NULL,
               content_sha256 TEXT NOT NULL, byte_length INTEGER NOT NULL,
               value TEXT NOT NULL,
               PRIMARY KEY(repository_id,connection_identity,catalog_kind,entry_key));
             CREATE TABLE IF NOT EXISTS catalogs(
               repository_id TEXT NOT NULL, connection_identity TEXT NOT NULL,
               catalog_kind TEXT NOT NULL, fingerprint TEXT NOT NULL,
               value TEXT NOT NULL,
               PRIMARY KEY(repository_id,connection_identity,catalog_kind,fingerprint));
             CREATE TABLE IF NOT EXISTS catalog_shape(
               repository_id TEXT NOT NULL, connection_identity TEXT NOT NULL,
               root_identity TEXT NOT NULL, level INTEGER NOT NULL,
               ordinal INTEGER NOT NULL, first_key TEXT NOT NULL, last_key TEXT NOT NULL,
               PRIMARY KEY(repository_id,connection_identity,root_identity,level,ordinal));
             CREATE TABLE IF NOT EXISTS graph_members(
               repository_id TEXT NOT NULL, connection_identity TEXT NOT NULL,
               catalog_kind TEXT NOT NULL,
               root_identity TEXT NOT NULL, member_identity TEXT NOT NULL,
               member_stored TEXT NOT NULL,
               PRIMARY KEY(repository_id,connection_identity,root_identity,member_identity));",
        )
        .map_err(transient)?;
        // Which publication an entry belonged to is no longer carried by the
        // row, and everything here is rebuildable, so a table that still
        // carries it starts again rather than being read two ways.
        let tagged: bool = db
            .prepare("SELECT 1 FROM pragma_table_info('entries') WHERE name='source'")
            .map_err(transient)?
            .exists([])
            .map_err(transient)?;
        // A catalog kind that changes every publication would otherwise crowd
        // out one that never changes, so the bound is per kind and a table
        // without it cannot be read the way this one is.
        let scoped: bool = db
            .prepare("SELECT 1 FROM pragma_table_info('graph_members') WHERE name='catalog_kind'")
            .map_err(transient)?
            .exists([])
            .map_err(transient)?;
        if !scoped {
            db.execute_batch(
                "DROP TABLE graph_members;
                 CREATE TABLE graph_members(
                   repository_id TEXT NOT NULL, connection_identity TEXT NOT NULL,
                   catalog_kind TEXT NOT NULL,
                   root_identity TEXT NOT NULL, member_identity TEXT NOT NULL,
                   member_stored TEXT NOT NULL,
                   PRIMARY KEY(repository_id,connection_identity,root_identity,member_identity));",
            )
            .map_err(transient)?;
        }
        if tagged {
            db.execute_batch(
                "DROP TABLE entries;
                 CREATE TABLE entries(
                   repository_id TEXT NOT NULL, connection_identity TEXT NOT NULL,
                   catalog_kind TEXT NOT NULL, entry_key TEXT NOT NULL,
                   content_sha256 TEXT NOT NULL, byte_length INTEGER NOT NULL,
                   value TEXT NOT NULL,
                   PRIMARY KEY(repository_id,connection_identity,catalog_kind,entry_key));",
            )
            .map_err(transient)?;
        }
        Ok(Self { db })
    }
    pub(super) fn object(
        &self,
        format_repository_id: &str,
        repository: &RepositoryHandle,
        id: &str,
        plaintext: &str,
    ) -> Result<Option<RemoteObject>> {
        let encoded: Option<String> = self.db.query_row(
            "SELECT value FROM remote_objects WHERE repository_id=?1 AND connection_identity=?2 AND object_id=?3 AND plaintext_sha256=?4",
            params![format_repository_id,repository.connection_identity,id,plaintext], |row| row.get(0),
        ).optional().map_err(transient)?;
        let Some(encoded) = encoded else {
            return Ok(None);
        };
        let value: RemoteObject = serde_json::from_str(&encoded).map_err(corrupt)?;
        value.stored(repository)?;
        if value.repository_id != format_repository_id || value.object_id != id
            || value.plaintext_sha256 != plaintext
        {
            return Err(corrupt("cached object identity"));
        }
        Ok(Some(value))
    }
    /// Every object this connection recorded with these exact bytes. A
    /// publication that encodes something already published asks this before
    /// uploading it again under an identity of its own.
    pub(super) fn objects_by_plaintext(
        &self,
        format_repository_id: &str,
        repository: &RepositoryHandle,
        plaintext: &str,
    ) -> Result<Vec<RemoteObject>> {
        let mut statement = self.db.prepare(
            "SELECT value FROM remote_objects WHERE repository_id=?1 AND connection_identity=?2 AND plaintext_sha256=?3",
        ).map_err(transient)?;
        let rows = statement.query_map(
            params![format_repository_id, repository.connection_identity, plaintext],
            |row| row.get::<_, String>(0),
        ).map_err(transient)?;
        let mut objects = Vec::new();
        for row in rows {
            let value: RemoteObject =
                serde_json::from_str(&row.map_err(transient)?).map_err(corrupt)?;
            value.stored(repository)?;
            if value.repository_id != format_repository_id || value.plaintext_sha256 != plaintext {
                return Err(corrupt("cached object identity"));
            }
            objects.push(value);
        }
        Ok(objects)
    }

    pub(super) fn forget_object(
        &self,
        format_repository_id: &str,
        repository: &RepositoryHandle,
        object_id: &str,
    ) -> Result<()> {
        let transaction = self.db.unchecked_transaction().map_err(transient)?;
        transaction.execute(
            "DELETE FROM remote_objects WHERE repository_id=?1 AND connection_identity=?2 AND object_id=?3",
            params![format_repository_id, repository.connection_identity, object_id],
        ).map_err(transient)?;
        transaction.execute(
            "DELETE FROM entries WHERE repository_id=?1 AND connection_identity=?2
             AND EXISTS(SELECT 1 FROM json_each(entries.value,'$.packs') AS pack
               WHERE json_extract(pack.value,'$.objectId')=?3)",
            params![format_repository_id, repository.connection_identity, object_id],
        ).map_err(transient)?;
        // A changed child can change every parent hash. These roots are only
        // an optimization; their source entries and sealed uploads stay intact.
        transaction.execute(
            "DELETE FROM catalogs WHERE repository_id=?1 AND connection_identity=?2",
            params![format_repository_id, repository.connection_identity],
        ).map_err(transient)?;
        transaction.commit().map_err(transient)
    }

    pub(super) fn put_object(&self, repository: &RepositoryHandle, value: &RemoteObject) -> Result<()> {
        value.stored(repository)?;
        self.db.execute(
            "INSERT INTO remote_objects VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(repository_id,connection_identity,object_id) DO UPDATE SET plaintext_sha256=excluded.plaintext_sha256,value=excluded.value",
            params![value.repository_id,repository.connection_identity,value.object_id,value.plaintext_sha256,serde_json::to_string(value).map_err(corrupt)?],
        ).map_err(transient)?;
        Ok(())
    }
    pub(super) fn entry(
        &self,
        format_repository_id: &str,
        repository: &RepositoryHandle,
        kind: wire::CatalogKind,
        key: &str,
        digest: &str,
        length: u64,
    ) -> Result<Option<EntryPlan>> {
        let encoded: Option<String> = self.db.query_row(
            "SELECT value FROM entries WHERE repository_id=?1 AND connection_identity=?2 AND catalog_kind=?3 AND entry_key=?4 AND content_sha256=?5 AND byte_length=?6",
            params![format_repository_id,repository.connection_identity,kind_name(kind),key,digest,i64::try_from(length).map_err(corrupt)?], |row| row.get(0),
        ).optional().map_err(transient)?;
        let Some(encoded) = encoded else {
            return Ok(None);
        };
        let value: EntryPlan = serde_json::from_str(&encoded).map_err(corrupt)?;
        value.validate(format_repository_id, repository)?;
        Ok(Some(value))
    }
    /// What this catalog last held under one key, whatever its content was.
    /// An entry that changed is repackaged, and what its previous version
    /// already placed is what the unchanged part of it can point at.
    pub(super) fn entry_by_key(
        &self,
        format_repository_id: &str,
        repository: &RepositoryHandle,
        kind: wire::CatalogKind,
        key: &str,
    ) -> Result<Option<EntryPlan>> {
        let encoded: Option<String> = self.db.query_row(
            "SELECT value FROM entries WHERE repository_id=?1 AND connection_identity=?2 AND catalog_kind=?3 AND entry_key=?4",
            params![format_repository_id, repository.connection_identity, kind_name(kind), key],
            |row| row.get(0),
        ).optional().map_err(transient)?;
        let Some(encoded) = encoded else {
            return Ok(None);
        };
        let value: EntryPlan = serde_json::from_str(&encoded).map_err(corrupt)?;
        value.validate(format_repository_id, repository)?;
        Ok(Some(value))
    }

    pub(super) fn put_entries(
        &self,
        format_repository_id: &str,
        repository: &RepositoryHandle,
        kind: wire::CatalogKind,
        values: &[EntryPlan],
    ) -> Result<()> {
        let transaction = self.db.unchecked_transaction().map_err(transient)?;
        for value in values {
            value.validate(format_repository_id, repository)?;
            transaction.execute(
                "INSERT INTO entries VALUES(?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(repository_id,connection_identity,catalog_kind,entry_key) DO UPDATE SET content_sha256=excluded.content_sha256,byte_length=excluded.byte_length,value=excluded.value",
                params![format_repository_id,repository.connection_identity,kind_name(kind),value.key,value.content_sha256,i64::try_from(value.byte_length).map_err(corrupt)?,serde_json::to_string(value).map_err(corrupt)?],
            ).map_err(transient)?;
        }
        transaction.commit().map_err(transient)?;
        Ok(())
    }

    /// What one published catalog root names, so a later publication can see
    /// that an object it holds belongs to the graph it selected as its parent
    /// without asking the provider about it. Recorded once the root exists.
    pub(super) fn record_graph(
        &self,
        format_repository_id: &str,
        repository: &RepositoryHandle,
        kind: wire::CatalogKind,
        root_identity: &str,
        members: &[(String, wire::StoredObject)],
        shape: &[CatalogRange],
    ) -> Result<()> {
        if root_identity.is_empty() {
            return Err(corrupt("catalog root has no identity"));
        }
        let transaction = self.db.unchecked_transaction().map_err(transient)?;
        {
            let mut statement = transaction
                .prepare("INSERT OR REPLACE INTO graph_members VALUES(?1,?2,?3,?4,?5,?6)")
                .map_err(transient)?;
            for (identity, member) in members {
                statement.execute(params![
                    format_repository_id, repository.connection_identity, kind_name(kind),
                    root_identity, identity, serde_json::to_string(member).map_err(corrupt)?,
                ]).map_err(transient)?;
            }
        }
        {
            let mut statement = transaction
                .prepare("INSERT OR REPLACE INTO catalog_shape VALUES(?1,?2,?3,?4,?5,?6,?7)")
                .map_err(transient)?;
            let mut ordinal: BTreeMap<u16, i64> = BTreeMap::new();
            for range in shape {
                let next = ordinal.entry(range.level).or_default();
                statement.execute(params![
                    format_repository_id, repository.connection_identity, root_identity,
                    i64::from(range.level), *next, range.first_key, range.last_key,
                ]).map_err(transient)?;
                *next += 1;
            }
        }
        // Only a recent root can still be selected as a parent, and a graph
        // nothing names is taking space for nothing. Counted within one kind,
        // because a catalog that changes every publication would otherwise
        // crowd out one that stands still.
        transaction.execute(
            "DELETE FROM graph_members WHERE repository_id=?1 AND connection_identity=?2
             AND catalog_kind=?3 AND root_identity NOT IN (
               SELECT root_identity FROM graph_members
               WHERE repository_id=?1 AND connection_identity=?2 AND catalog_kind=?3
               GROUP BY root_identity ORDER BY MAX(rowid) DESC LIMIT ?4)",
            params![
                format_repository_id, repository.connection_identity, kind_name(kind),
                RETAINED_GRAPHS,
            ],
        ).map_err(transient)?;
        transaction.execute(
            "DELETE FROM catalog_shape WHERE repository_id=?1 AND connection_identity=?2
             AND root_identity NOT IN (
               SELECT root_identity FROM graph_members
               WHERE repository_id=?1 AND connection_identity=?2)",
            params![format_repository_id, repository.connection_identity],
        ).map_err(transient)?;
        transaction.commit().map_err(transient)?;
        Ok(())
    }

    /// The key range each node of one published catalog covered, by level and
    /// in order. Empty means this cache never recorded that root, so the next
    /// catalog is built from scratch rather than assigned to its ranges.
    pub(super) fn shape_of(
        &self,
        format_repository_id: &str,
        repository: &RepositoryHandle,
        root_identity: &str,
    ) -> Result<Vec<CatalogRange>> {
        let mut statement = self.db.prepare(
            "SELECT level,first_key,last_key FROM catalog_shape WHERE repository_id=?1 AND connection_identity=?2 AND root_identity=?3 ORDER BY level,ordinal",
        ).map_err(transient)?;
        let rows = statement.query_map(
            params![format_repository_id, repository.connection_identity, root_identity],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
        ).map_err(transient)?;
        let mut ranges = Vec::new();
        for row in rows {
            let (level, first_key, last_key) = row.map_err(transient)?;
            ranges.push(CatalogRange {
                level: u16::try_from(level).map_err(corrupt)?,
                first_key,
                last_key,
            });
        }
        Ok(ranges)
    }

    /// Whether this cache already holds what a root names, without reading
    /// the closure back. A publication asks before rebuilding it from the
    /// provider, and a large graph answers in one row.
    pub(super) fn knows_graph(
        &self,
        format_repository_id: &str,
        repository: &RepositoryHandle,
        root_identity: &str,
    ) -> Result<bool> {
        let found: Option<i64> = self.db.query_row(
            "SELECT 1 FROM graph_members WHERE repository_id=?1 AND connection_identity=?2 AND root_identity=?3 LIMIT 1",
            params![format_repository_id, repository.connection_identity, root_identity],
            |row| row.get(0),
        ).optional().map_err(transient)?;
        Ok(found.is_some())
    }

    /// Everything one published catalog root names, as the objects a
    /// publication can reference again. Empty means this cache never recorded
    /// that root, not that the root names nothing.
    pub(super) fn graph_of(
        &self,
        format_repository_id: &str,
        repository: &RepositoryHandle,
        root_identity: &str,
    ) -> Result<Vec<RemoteObject>> {
        let mut statement = self.db.prepare(
            "SELECT member_stored FROM graph_members WHERE repository_id=?1 AND connection_identity=?2 AND root_identity=?3",
        ).map_err(transient)?;
        let rows = statement.query_map(
            params![format_repository_id, repository.connection_identity, root_identity],
            |row| row.get::<_, String>(0),
        ).map_err(transient)?;
        let mut objects = Vec::new();
        for row in rows {
            let encoded = row.map_err(transient)?;
            let stored: wire::StoredObject = serde_json::from_str(&encoded).map_err(corrupt)?;
            let object = RemoteObject::from_stored(&stored, repository)?;
            if object.repository_id != format_repository_id {
                return Err(corrupt("cached graph repository"));
            }
            objects.push(object);
        }
        Ok(objects)
    }

    /// Everything the selected parent graph holds, read once for a whole
    /// publication. An empty answer means this cache knows nothing about that
    /// parent, which admits nothing and asks about everything.
    pub(super) fn members_of(
        &self,
        format_repository_id: &str,
        repository: &RepositoryHandle,
        roots: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>> {
        let mut statement = self.db.prepare(
            "SELECT member_identity FROM graph_members WHERE repository_id=?1 AND connection_identity=?2 AND root_identity=?3",
        ).map_err(transient)?;
        let mut members = BTreeSet::new();
        for root in roots {
            let rows = statement.query_map(
                params![format_repository_id, repository.connection_identity, root],
                |row| row.get::<_, String>(0),
            ).map_err(transient)?;
            for row in rows {
                members.insert(row.map_err(transient)?);
            }
        }
        Ok(members)
    }

    /// The packs one catalog's current entries still point at, with the live
    /// entry count, live source bytes and first key of each. A pack named by no
    /// current entry is absent, so nothing here asks the provider anything or
    /// reaches a historical root.
    pub(super) fn catalog(
        &self,
        format_repository_id: &str,
        repository: &RepositoryHandle,
        kind: wire::CatalogKind,
        fingerprint: &str,
    ) -> Result<Option<RemoteObject>> {
        let encoded: Option<String> = self.db.query_row(
            "SELECT value FROM catalogs WHERE repository_id=?1 AND connection_identity=?2 AND catalog_kind=?3 AND fingerprint=?4",
            params![format_repository_id,repository.connection_identity,kind_name(kind),fingerprint], |row| row.get(0),
        ).optional().map_err(transient)?;
        let Some(encoded) = encoded else {
            return Ok(None);
        };
        let value: RemoteObject = serde_json::from_str(&encoded).map_err(corrupt)?;
        value.stored(repository)?;
        if value.role != ObjectRole::Catalog || value.repository_id != format_repository_id {
            return Err(corrupt("cached catalog role"));
        }
        Ok(Some(value))
    }
    pub(super) fn put_catalog(
        &self,
        repository: &RepositoryHandle,
        kind: wire::CatalogKind,
        fingerprint: &str,
        value: &RemoteObject,
    ) -> Result<()> {
        value.stored(repository)?;
        self.db
            .execute(
                "INSERT OR REPLACE INTO catalogs VALUES(?1,?2,?3,?4,?5)",
                params![
                    value.repository_id,
                    repository.connection_identity,
                    kind_name(kind),
                    fingerprint,
                    serde_json::to_string(value).map_err(corrupt)?
                ],
            )
            .map_err(transient)?;
        Ok(())
    }
}
/// The existing upload inventory bounds collection ownership. Reading it never
/// discovers foreign packs or treats historical receipts as current existence.
pub(crate) fn forget_remote_object(root: &Path, repository: &RepositoryHandle, object: &RemoteObject) -> Result<()> {
    PackageCache::open(root)?.forget_object(&object.repository_id, repository, &object.object_id)
}

pub(crate) fn known_remote_objects(
    root: &Path,
    format_repository_id: &str,
    repository: &RepositoryHandle,
) -> Result<Vec<RemoteObject>> {
    let path = root.join("snapshot-cache.sqlite");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(transient(error)),
        Ok(_) => {}
    }
    if crate::trust_boundary::is_link_like(&fs::symlink_metadata(root).map_err(transient)?) {
        return Err(corrupt("package cache root is a link"));
    }
    crate::trust_boundary::open_regular_source(&path).map_err(transient)?;
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(transient)?;
    let mut query = db.prepare(
        "SELECT object_id,plaintext_sha256,value FROM remote_objects
         WHERE repository_id=?1 AND connection_identity=?2 ORDER BY object_id",
    ).map_err(transient)?;
    let rows = query.query_map(params![format_repository_id, repository.connection_identity], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    }).map_err(transient)?;
    let mut objects = Vec::new();
    for row in rows {
        let (id, digest, encoded) = row.map_err(transient)?;
        if encoded.len() > 128 * 1024 { return Err(corrupt("inventory object exceeds limit")); }
        let object: RemoteObject = serde_json::from_str(&encoded).map_err(corrupt)?;
        object.stored(repository)?;
        if object.repository_id != format_repository_id || object.object_id != id
            || object.plaintext_sha256 != digest
        {
            return Err(corrupt("inventory object identity differs"));
        }
        objects.push(object);
    }
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_storage::{contract::ErrorKind, fake};

    /// A catalog kind republished every time must not evict one that never
    /// changes, because the unchanging one is still what a parent names.
    #[test]
    fn a_kind_that_keeps_changing_does_not_crowd_out_one_that_stands_still() {
        let root = tempfile::tempdir().unwrap();
        let repository = fake::repository();
        let cache = PackageCache::open(root.path()).unwrap();
        let member = |id: &str| {
            let object = crate::external_storage::reachability::tests::object(id, ObjectRole::Pack);
            vec![(id.to_owned(), object.stored(&repository).unwrap())]
        };
        cache.record_graph(
            "format-repository", &repository, wire::CatalogKind::Assets, "asset-root",
            &member("pack-assets"), &[],
        ).unwrap();
        for index in 0..RETAINED_GRAPHS + 8 {
            cache.record_graph(
                "format-repository", &repository, wire::CatalogKind::Records,
                &format!("record-root-{index}"), &member(&format!("pack-records-{index}")), &[],
            ).unwrap();
        }
        assert_eq!(
            cache.graph_of("format-repository", &repository, "asset-root").unwrap().len(), 1,
        );
        assert!(cache.graph_of("format-repository", &repository, "record-root-0").unwrap().is_empty());
        assert_eq!(
            cache.graph_of(
                "format-repository", &repository, &format!("record-root-{}", RETAINED_GRAPHS + 7),
            ).unwrap().len(),
            1,
        );
        let held: i64 = cache.db.query_row(
            "SELECT count(DISTINCT root_identity) FROM graph_members WHERE catalog_kind='records'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(held, RETAINED_GRAPHS);
    }

    /// Nothing here is authoritative, so a cache that still names a row's
    /// publication on the row starts again instead of being read two ways.
    #[test]
    fn entries_naming_their_publication_are_discarded_on_open() {
        let root = tempfile::tempdir().unwrap();
        let repository = fake::repository();
        {
            let db = Connection::open(root.path().join("snapshot-cache.sqlite")).unwrap();
            db.execute_batch(
                "CREATE TABLE entries(
                   repository_id TEXT NOT NULL, connection_identity TEXT NOT NULL,
                   catalog_kind TEXT NOT NULL, entry_key TEXT NOT NULL,
                   content_sha256 TEXT NOT NULL, byte_length INTEGER NOT NULL,
                   value TEXT NOT NULL, source TEXT NOT NULL,
                   PRIMARY KEY(repository_id,connection_identity,catalog_kind,entry_key));
                 INSERT INTO entries VALUES('format-repository','synthetic-identity','assets','a','b',1,'{}','root');",
            ).unwrap();
        }
        let cache = PackageCache::open(root.path()).unwrap();
        let held: i64 = cache.db.query_row("SELECT count(*) FROM entries", [], |row| row.get(0)).unwrap();
        assert_eq!(held, 0);
        assert!(cache
            .entry("format-repository", &repository, wire::CatalogKind::Assets, "a", &"b".repeat(64), 1)
            .unwrap()
            .is_none());
        assert_eq!(
            cache.record_graph("format-repository", &repository, wire::CatalogKind::Assets, "", &[], &[])
                .unwrap_err().kind,
            ErrorKind::Corrupt,
        );
    }
}
