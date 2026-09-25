//! Deferred index rows for one staged page. Descriptor and reference-tree
//! validation reads the objects it is given and records the rows its result
//! implies; the rows reach the database at bounded boundaries instead of one
//! autocommit statement per record and one transaction per reference node.
use crate::Result;
use rusqlite::{params, Connection};
use std::collections::BTreeMap;

/// Section 4's collection rule for a batch of small rows.
const MAX_PENDING_ROWS: usize = 256;
const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) struct NodeStats {
    pub kind: &'static str,
    pub count: i64,
    pub depth: i64,
}

/// Children must exist before the branch that names them, so the recorded
/// order is the order the rows are written in.
enum Row {
    Node {
        hash: String,
        kind: &'static str,
        count: i64,
        depth: i64,
        first: String,
        last: String,
    },
    Edge {
        table: &'static str,
        root: String,
        value: String,
    },
    Descriptor {
        hash: String,
        object: String,
        body: String,
    },
}

#[derive(Default)]
pub(super) struct PendingIndex {
    rows: Vec<Row>,
    bytes: usize,
    /// Nodes this page prepared but has not written yet, so a later parent in
    /// the same page sees them exactly as it would see committed rows.
    nodes: BTreeMap<String, (NodeStats, String, String)>,
    descriptors: BTreeMap<String, String>,
}

impl PendingIndex {
    pub fn node(&self, hash: &str) -> Option<(NodeStats, &str, &str)> {
        self.nodes
            .get(hash)
            .map(|(stats, first, last)| (*stats, first.as_str(), last.as_str()))
    }

    pub fn descriptor(&self, hash: &str) -> Option<&str> {
        self.descriptors.get(hash).map(String::as_str)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_node(
        &mut self,
        hash: String,
        kind: &'static str,
        count: i64,
        depth: i64,
        first: String,
        last: String,
    ) {
        self.nodes.insert(
            hash.clone(),
            (
                NodeStats { kind, count, depth },
                first.clone(),
                last.clone(),
            ),
        );
        self.bytes += hash.len() + first.len() + last.len();
        self.rows.push(Row::Node {
            hash,
            kind,
            count,
            depth,
            first,
            last,
        });
    }

    pub fn add_edge(&mut self, table: &'static str, root: String, value: String) {
        self.bytes += root.len() + value.len();
        self.rows.push(Row::Edge { table, root, value });
    }

    pub fn add_descriptor(&mut self, hash: String, object: String, body: String) {
        self.descriptors.insert(hash.clone(), body.clone());
        self.bytes += hash.len() + object.len() + body.len();
        self.rows.push(Row::Descriptor { hash, object, body });
    }

    pub fn is_full(&self) -> bool {
        self.rows.len() >= MAX_PENDING_ROWS || self.bytes >= MAX_PENDING_BYTES
    }

    /// Write one bounded group. The rows are content addressed and idempotent,
    /// so a flush that only part of a page reached is replayed by the next
    /// attempt rather than leaving a half-described tree behind.
    pub fn flush(&mut self, db: &mut Connection) -> Result<()> {
        if self.rows.is_empty() {
            self.clear_lookups();
            return Ok(());
        }
        #[cfg(test)]
        commits::record();
        let tx = db.transaction()?;
        for row in &self.rows {
            match row {
                Row::Node {
                    hash,
                    kind,
                    count,
                    depth,
                    first,
                    last,
                } => {
                    tx.execute("INSERT INTO reference_nodes VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(hash) DO NOTHING",params![hash,kind,count,depth,first,last])?;
                }
                Row::Edge { table, root, value } => {
                    tx.execute(
                        &format!("INSERT INTO {table} VALUES(?1,?2) ON CONFLICT DO NOTHING"),
                        params![root, value],
                    )?;
                }
                Row::Descriptor { hash, object, body } => {
                    tx.execute("INSERT INTO descriptors(hash,object,body) VALUES(?1,?2,?3) ON CONFLICT(hash) DO NOTHING",params![hash,object,body])?;
                }
            }
        }
        tx.commit()?;
        self.rows.clear();
        self.bytes = 0;
        self.clear_lookups();
        Ok(())
    }

    /// Written rows are found in the database, so the shadow lookups only have
    /// to cover what is still pending.
    fn clear_lookups(&mut self) {
        self.nodes.clear();
        self.descriptors.clear();
    }
}

#[cfg(test)]
pub(super) mod commits {
    std::thread_local! {
        static COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    pub fn record() {
        COUNT.with(|count| count.set(count.get() + 1));
    }
    pub fn take() -> usize {
        COUNT.with(|count| count.replace(0))
    }
}
