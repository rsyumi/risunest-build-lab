use super::{descriptors_index::PendingIndex, json, parse, Store};
use crate::{Error, Result};
use risunest_sync_wire::{
    canonical,
    descriptor::{RecordDescriptor, ReferencePage, MAX_DESCRIPTOR_REFERENCES, MAX_TREE_DEPTH},
    ChangeSet, Domain, RecordVersion, MAX_METADATA_BYTES,
};
use rusqlite::{params, OptionalExtension};
use std::collections::BTreeSet;

impl Store {
    /// Normalize immutable control pages before the commit writer is acquired.
    /// Each page is bounded, published once, and reused by subsequent
    /// descriptors. Validation reads the objects it is given; the index rows it
    /// implies are written at bounded boundaries, not one statement per record
    /// and one transaction per reference node.
    pub(super) fn prepare_descriptors(&self, changes: &ChangeSet) -> Result<()> {
        let mut pending = PendingIndex::default();
        for change in &changes.changes {
            if let RecordVersion::Live {
                object_hash,
                descriptor_hash: Some(digest),
            } = &change.after
            {
                let descriptor = self
                    .prepare_descriptor(digest, &mut pending)
                    .map_err(|error| error.for_key(&change.key))?;
                if &descriptor.object_hash != object_hash {
                    return Err(Error::new("descriptor-object-mismatch", 409).for_key(&change.key));
                }
                if pending.is_full() {
                    pending.flush(&mut *self.db()?)?;
                }
            }
        }
        pending.flush(&mut *self.db()?)
    }
    fn prepare_descriptor(
        &self,
        digest: &str,
        pending: &mut PendingIndex,
    ) -> Result<RecordDescriptor> {
        if let Some(body) = pending.descriptor(digest) {
            return parse(body);
        }
        if let Some(body) = self
            .reader()?
            .query_row(
                "SELECT body FROM descriptors WHERE hash=?1",
                [digest],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return parse(&body);
        }
        let descriptor: RecordDescriptor =
            canonical::decode(&self.get_object(digest)?, MAX_METADATA_BYTES)?;
        descriptor.validate()?;
        if self.object_size(&descriptor.object_hash)?.is_none() {
            return Err(Error::new("missing-dependency", 409));
        }
        for hash in &descriptor.dependencies {
            if self.object_size(hash)?.is_none() {
                return Err(Error::new("missing-dependency", 409));
            }
        }
        if let Some(root) = &descriptor.dependency_root {
            self.prepare_reference_tree(root, false, pending)?;
        }
        if let Some(root) = &descriptor.relation_root {
            self.prepare_reference_tree(root, true, pending)?;
        }
        pending.add_descriptor(
            digest.to_owned(),
            descriptor.object_hash.clone(),
            json(&descriptor)?,
        );
        Ok(descriptor)
    }
    fn prepare_reference_tree(
        &self,
        root: &str,
        relations: bool,
        pending: &mut PendingIndex,
    ) -> Result<()> {
        let kind = if relations { "relations" } else { "objects" };
        let mut queue = vec![(root.to_owned(), 0usize, false)];
        let mut seen = BTreeSet::new();
        while let Some((digest, depth, expanded)) = queue.pop() {
            let existing = match pending.node(&digest) {
                Some((stats, _, _)) => Some(stats.kind.to_owned()),
                None => self
                    .reader()?
                    .query_row(
                        "SELECT kind FROM reference_nodes WHERE hash=?1",
                        [&digest],
                        |r| r.get(0),
                    )
                    .optional()?,
            };
            if let Some(existing) = existing {
                if existing != kind {
                    return Err(Error::new("reference-kind-mismatch", 409));
                }
                continue;
            }
            if depth >= MAX_TREE_DEPTH
                || (!expanded
                    && (!seen.insert(digest.clone()) || seen.len() > MAX_DESCRIPTOR_REFERENCES))
            {
                return Err(Error::new("invalid-reference-tree", 400));
            }
            let page: ReferencePage =
                canonical::decode(&self.get_object(&digest)?, MAX_METADATA_BYTES)?;
            page.validate()?;
            if let ReferencePage::Branches { children } = &page {
                if !expanded {
                    queue.push((digest, depth, true));
                    for child in children.iter().rev() {
                        queue.push((child.clone(), depth + 1, false));
                    }
                    continue;
                }
            } else if matches!(page, ReferencePage::Relations { .. }) != relations {
                return Err(Error::new("reference-kind-mismatch", 409));
            }
            let (count, tree_depth, first, last) = if let ReferencePage::Branches { children } =
                &page
            {
                let mut count = 0i64;
                let mut deepest = 0i64;
                let mut first = None;
                let mut last: Option<String> = None;
                for child in children {
                    // A child prepared earlier in this page is not written yet,
                    // so it is read from the pending rows exactly as a written
                    // one would be read from the table.
                    let (child_count, child_depth, child_first, child_last): (
                        i64,
                        i64,
                        String,
                        String,
                    ) = match pending.node(child).filter(|(stats, _, _)| stats.kind == kind) {
                        Some((stats, first, last)) => {
                            (stats.count, stats.depth, first.to_owned(), last.to_owned())
                        }
                        None => self.reader()?.query_row("SELECT item_count,tree_depth,first_value,last_value FROM reference_nodes WHERE hash=?1 AND kind=?2",params![child,kind],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?,
                    };
                    if last
                        .as_ref()
                        .is_some_and(|previous| previous >= &child_first)
                    {
                        return Err(Error::new("unordered-references", 400));
                    }
                    first.get_or_insert(child_first);
                    last = Some(child_last);
                    count += child_count;
                    deepest = deepest.max(child_depth);
                }
                (count, deepest + 1, first.unwrap(), last.unwrap())
            } else {
                (
                    page.values().len() as i64,
                    1,
                    page.values()[0].clone(),
                    page.values().last().unwrap().clone(),
                )
            };
            if count > MAX_DESCRIPTOR_REFERENCES as i64 || tree_depth > MAX_TREE_DEPTH as i64 {
                return Err(Error::new("reference-tree-too-large", 413));
            }
            pending.add_node(digest.clone(), kind, count, tree_depth, first, last);
            let table = match &page {
                ReferencePage::Objects { .. } => "reference_objects",
                ReferencePage::Relations { .. } => "reference_relations",
                ReferencePage::Branches { .. } => "reference_children",
            };
            for value in page.values() {
                if matches!(page, ReferencePage::Objects { .. })
                    && self.object_size(value)?.is_none()
                {
                    return Err(Error::new("missing-dependency", 409));
                }
                pending.add_edge(table, digest.clone(), value.clone());
            }
        }
        Ok(())
    }
    pub(super) fn update_relations(
        db: &rusqlite::Connection,
        domain: Domain,
        key: &str,
        version: &RecordVersion,
    ) -> Result<()> {
        db.execute(
            "DELETE FROM record_relations WHERE domain=?1 AND source=?2",
            params![domain.as_str(), key],
        )?;
        if let RecordVersion::Live {
            descriptor_hash: Some(digest),
            ..
        } = version
        {
            let body: String = db.query_row(
                "SELECT body FROM descriptors WHERE hash=?1",
                [digest],
                |r| r.get(0),
            )?;
            let descriptor: RecordDescriptor = parse(&body)?;
            for target in &descriptor.relations {
                db.execute("INSERT INTO record_relations(domain,source,target) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING", params![domain.as_str(),key,target])?;
            }
            if let Some(root) = descriptor.relation_root {
                db.execute("WITH RECURSIVE nodes(hash) AS (SELECT ?1 UNION SELECT child FROM reference_children JOIN nodes ON root=nodes.hash) INSERT INTO record_relations(domain,source,target) SELECT ?2,?3,target FROM reference_relations WHERE root IN nodes ON CONFLICT DO NOTHING",params![root,domain.as_str(),key])?;
            }
        }
        Ok(())
    }
    pub(super) fn validate_relations(db: &rusqlite::Connection, stage: &str) -> Result<()> {
        Self::each_change(db, stage, |change| {
            if !matches!(change.after, RecordVersion::Live { .. }) {
                let used: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM record_relations WHERE domain=?1 AND target=?2)",
                    params![change.domain.as_str(), change.key],
                    |r| r.get(0),
                )?;
                if used {
                    return Err(Error::new("referenced-record-deleted", 409));
                }
            }
            let mut statement =
                db.prepare("SELECT target FROM record_relations WHERE domain=?1 AND source=?2")?;
            let mut targets = statement.query(params![change.domain.as_str(), change.key])?;
            while let Some(row) = targets.next()? {
                let key: String = row.get(0)?;
                if !matches!(
                    Self::read_version(db, change.domain, &key)?,
                    RecordVersion::Live { .. }
                ) {
                    return Err(Error::new("missing-related-record", 409));
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::descriptors_index::commits;
    use super::*;
    use risunest_sync_wire::{descriptor::RecordDescriptor, hash, RecordChange};

    /// A27. Index rows follow bounded pages. A staged page of many records
    /// must not cost one durable commit per record.
    #[test]
    fn staged_page_index_commits_follow_bounded_groups_not_records() {
        const RECORDS: usize = 300;
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init(directory.path()).unwrap();
        let registration = store.add_device().unwrap();
        let device = store
            .authenticate(&registration.library_id, &registration.token)
            .unwrap();
        let mut changes = Vec::new();
        for index in 0..RECORDS {
            let body = format!("synthetic record body {index:08}").into_bytes();
            let object_hash = hash(&body);
            store.put_object(&device, &object_hash, &body).unwrap();
            let descriptor = RecordDescriptor::content(object_hash.clone());
            let encoded = descriptor.bytes().unwrap();
            let descriptor_hash = hash(&encoded);
            store
                .put_object(&device, &descriptor_hash, &encoded)
                .unwrap();
            changes.push(RecordChange {
                domain: Domain::Library,
                key: format!("synthetic-key-{index:06}"),
                before: RecordVersion::Absent,
                after: RecordVersion::Live {
                    object_hash,
                    descriptor_hash: Some(descriptor_hash),
                },
            });
        }
        let page = ChangeSet {
            changes,
            read_fences: Vec::new(),
            scope_fences: Vec::new(),
        };
        commits::take();
        store.stage_changes(&device, &page).unwrap();
        let flushes = commits::take();
        assert!(
            flushes <= RECORDS / 100,
            "a page of {RECORDS} records cost {flushes} index commits"
        );
        assert!(flushes > 0, "the page's index rows were never written");
        let stored: i64 = store
            .db()
            .unwrap()
            .query_row("SELECT count(*) FROM descriptors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, RECORDS as i64);
    }

    /// The same page's pending rows must answer an existing-node check and a
    /// branch's child lookup exactly as written rows would, and the written
    /// order must satisfy the index's own references.
    #[test]
    fn a_pages_reference_trees_resolve_against_rows_it_has_not_written_yet() {
        const REFERENCES: usize = 400;
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init(directory.path()).unwrap();
        let registration = store.add_device().unwrap();
        let device = store
            .authenticate(&registration.library_id, &registration.token)
            .unwrap();
        let mut dependencies = Vec::new();
        for index in 0..REFERENCES {
            let body = format!("synthetic dependency {index:08}").into_bytes();
            let digest = hash(&body);
            store.put_object(&device, &digest, &body).unwrap();
            dependencies.push(digest);
        }
        dependencies.sort();
        let (root, pages) =
            risunest_sync_wire::descriptor::build_reference_tree(&dependencies, false).unwrap();
        assert!(pages.len() > 1, "the fixture needs a branch over leaves");
        for (digest, bytes) in &pages {
            store.put_object(&device, digest, bytes).unwrap();
        }
        // Both records name the same root, so the second one meets a node the
        // first prepared and has not written.
        let mut changes = Vec::new();
        for index in 0..2 {
            let body = format!("synthetic shared-root record {index}").into_bytes();
            let object_hash = hash(&body);
            store.put_object(&device, &object_hash, &body).unwrap();
            let descriptor = RecordDescriptor {
                object_hash: object_hash.clone(),
                dependency_root: root.clone(),
                relation_root: None,
                dependencies: Vec::new(),
                relations: Vec::new(),
                scopes: Vec::new(),
            };
            let encoded = descriptor.bytes().unwrap();
            let descriptor_hash = hash(&encoded);
            store
                .put_object(&device, &descriptor_hash, &encoded)
                .unwrap();
            changes.push(RecordChange {
                domain: Domain::Library,
                key: format!("synthetic-shared-{index}"),
                before: RecordVersion::Absent,
                after: RecordVersion::Live {
                    object_hash,
                    descriptor_hash: Some(descriptor_hash),
                },
            });
        }
        store
            .stage_changes(
                &device,
                &ChangeSet {
                    changes,
                    read_fences: Vec::new(),
                    scope_fences: Vec::new(),
                },
            )
            .unwrap();
        let db = store.db().unwrap();
        let count = |sql: &str| -> i64 { db.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(
            count("SELECT count(*) FROM reference_nodes"),
            pages.len() as i64
        );
        assert_eq!(
            count("SELECT count(*) FROM reference_objects"),
            REFERENCES as i64
        );
        assert_eq!(
            count("SELECT count(*) FROM reference_children"),
            pages.len() as i64 - 1
        );
        // The branch recorded the totals its children carry, which is what the
        // shadow lookup had to supply while they were still pending.
        assert_eq!(
            db.query_row(
                "SELECT item_count FROM reference_nodes WHERE hash=?1",
                [root.as_ref().unwrap()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            REFERENCES as i64
        );
    }

    /// Two records naming one small root stay in the same pending group, so the
    /// second meets a node the first prepared and has not written.
    #[test]
    fn a_shared_root_is_recognised_while_it_is_still_pending() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init(directory.path()).unwrap();
        let registration = store.add_device().unwrap();
        let device = store
            .authenticate(&registration.library_id, &registration.token)
            .unwrap();
        let mut dependencies = (0..3)
            .map(|index| {
                let body = format!("synthetic small dependency {index}").into_bytes();
                let digest = hash(&body);
                store.put_object(&device, &digest, &body).unwrap();
                digest
            })
            .collect::<Vec<_>>();
        dependencies.sort();
        let (root, pages) =
            risunest_sync_wire::descriptor::build_reference_tree(&dependencies, false).unwrap();
        assert_eq!(pages.len(), 1, "the fixture needs one small page");
        for (digest, bytes) in &pages {
            store.put_object(&device, digest, bytes).unwrap();
        }
        let changes = (0..2)
            .map(|index| {
                let body = format!("synthetic small-root record {index}").into_bytes();
                let object_hash = hash(&body);
                store.put_object(&device, &object_hash, &body).unwrap();
                let descriptor = RecordDescriptor {
                    object_hash: object_hash.clone(),
                    dependency_root: root.clone(),
                    relation_root: None,
                    dependencies: Vec::new(),
                    relations: Vec::new(),
                    scopes: Vec::new(),
                };
                let encoded = descriptor.bytes().unwrap();
                let descriptor_hash = hash(&encoded);
                store
                    .put_object(&device, &descriptor_hash, &encoded)
                    .unwrap();
                RecordChange {
                    domain: Domain::Library,
                    key: format!("synthetic-small-{index}"),
                    before: RecordVersion::Absent,
                    after: RecordVersion::Live {
                        object_hash,
                        descriptor_hash: Some(descriptor_hash),
                    },
                }
            })
            .collect::<Vec<_>>();
        commits::take();
        store
            .stage_changes(
                &device,
                &ChangeSet {
                    changes,
                    read_fences: Vec::new(),
                    scope_fences: Vec::new(),
                },
            )
            .unwrap();
        assert_eq!(
            commits::take(),
            1,
            "both records belong to one bounded group"
        );
        let db = store.db().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM reference_nodes", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1,
            "the shared root was prepared once"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM reference_objects", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            dependencies.len() as i64
        );
    }
}
