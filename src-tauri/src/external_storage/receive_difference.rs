//! The key-level difference between a complete remote catalog and the local
//! library, computed before any write transaction is opened.
use std::collections::BTreeMap;

use super::contract::{ErrorKind, ProviderError, Result};

/// One record as a complete remote catalog names it.
#[derive(Clone, Debug)]
pub(crate) struct RemoteRecord {
    pub key: String,
    pub content_hash: String,
    pub byte_length: u64,
}

/// One record as the local library holds it at the revision the difference is
/// computed against.
#[derive(Clone, Debug)]
pub(crate) struct LocalRecord {
    pub key: String,
    pub content_hash: String,
}

/// What a normal receive would have to write, before the sources behind it are
/// resolved. `removed` is computed against the complete local view, so a
/// partial download can never produce one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReceiveDifference {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
    /// The catalog length of everything in `added` and `changed`.
    pub arriving_bytes: u64,
}

/// The size past which a receive is no longer a normal small one and belongs in
/// the staging and activation path instead. `records` and `bytes` bound the
/// keys and their envelopes; `dependent_bytes` and `work` bound what those
/// envelopes bring with them: the message pages and owner manifests behind
/// them, and the rows written, rows deleted and bodies confirmed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DifferenceBudget {
    pub records: usize,
    pub bytes: u64,
    pub dependent_bytes: u64,
    pub work: u64,
}

impl DifferenceBudget {
    pub(crate) fn admits_dependents(&self, dependent_bytes: u64, work: u64) -> bool {
        dependent_bytes <= self.dependent_bytes && work <= self.work
    }
}

impl ReceiveDifference {
    pub(crate) fn written_records(&self) -> usize {
        self.added.len() + self.changed.len() + self.removed.len()
    }

    pub(crate) fn within(&self, budget: DifferenceBudget) -> bool {
        self.written_records() <= budget.records && self.arriving_bytes <= budget.bytes
    }
}

fn corrupt(_: &str) -> ProviderError {
    ProviderError::new(ErrorKind::Corrupt)
}

/// Both sides are complete views of the same scope. A key named twice by either
/// one has no single content to compare, so it is rejected rather than resolved.
pub(crate) fn difference<R, L>(remote: R, local: L) -> Result<ReceiveDifference>
where
    R: IntoIterator<Item = RemoteRecord>,
    L: IntoIterator<Item = LocalRecord>,
{
    let mut held = BTreeMap::new();
    for record in local {
        if held.insert(record.key, record.content_hash).is_some() {
            return Err(corrupt("local view names a record twice"));
        }
    }
    let mut difference = ReceiveDifference::default();
    let mut seen = BTreeMap::new();
    for record in remote {
        let previous = held.remove(&record.key);
        if seen.insert(record.key.clone(), ()).is_some() {
            return Err(corrupt("remote catalog names a record twice"));
        }
        match previous {
            Some(hash) if hash == record.content_hash => difference.unchanged += 1,
            Some(_) => {
                difference.arriving_bytes = difference
                    .arriving_bytes
                    .checked_add(record.byte_length)
                    .ok_or_else(|| corrupt("receive length overflow"))?;
                difference.changed.push(record.key);
            }
            None => {
                difference.arriving_bytes = difference
                    .arriving_bytes
                    .checked_add(record.byte_length)
                    .ok_or_else(|| corrupt("receive length overflow"))?;
                difference.added.push(record.key);
            }
        }
    }
    difference.removed = held.into_keys().collect();
    Ok(difference)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(entries: &[(&str, &str, u64)]) -> Vec<RemoteRecord> {
        entries
            .iter()
            .map(|(key, hash, bytes)| RemoteRecord {
                key: (*key).to_owned(),
                content_hash: (*hash).to_owned(),
                byte_length: *bytes,
            })
            .collect()
    }

    fn local(entries: &[(&str, &str)]) -> Vec<LocalRecord> {
        entries
            .iter()
            .map(|(key, hash)| LocalRecord {
                key: (*key).to_owned(),
                content_hash: (*hash).to_owned(),
            })
            .collect()
    }

    #[test]
    fn an_identical_catalog_leaves_nothing_to_write() {
        let result = difference(
            remote(&[("root", "aa", 10), ("character/1", "bb", 20)]),
            local(&[("root", "aa"), ("character/1", "bb")]),
        )
        .unwrap();
        assert_eq!(result.written_records(), 0);
        assert_eq!(result.unchanged, 2);
        assert_eq!(result.arriving_bytes, 0);
    }

    #[test]
    fn a_key_is_added_changed_removed_or_left_alone() {
        let result = difference(
            remote(&[("root", "aa", 10), ("character/1", "cc", 20), ("character/2", "dd", 30)]),
            local(&[("root", "aa"), ("character/1", "bb"), ("character/3", "ee")]),
        )
        .unwrap();
        assert_eq!(result.added, ["character/2"]);
        assert_eq!(result.changed, ["character/1"]);
        assert_eq!(result.removed, ["character/3"]);
        assert_eq!(result.unchanged, 1);
    }

    #[test]
    fn arriving_bytes_counts_only_what_has_to_be_written() {
        let result = difference(
            remote(&[("root", "aa", 10), ("character/1", "cc", 20), ("character/2", "dd", 30)]),
            local(&[("root", "aa"), ("character/1", "bb"), ("character/3", "ee")]),
        )
        .unwrap();
        // The unchanged root and the removed record carry no body to write.
        assert_eq!(result.arriving_bytes, 50);
    }

    #[test]
    fn a_removal_is_measured_against_every_key_the_library_holds() {
        let result = difference(
            remote(&[("root", "aa", 10)]),
            local(&[("root", "aa"), ("character/1", "bb"), ("character/2", "cc")]),
        )
        .unwrap();
        assert_eq!(result.removed, ["character/1", "character/2"]);
    }

    #[test]
    fn a_key_named_twice_by_either_side_is_refused() {
        assert!(difference(
            remote(&[("root", "aa", 10), ("root", "bb", 10)]),
            local(&[]),
        )
        .is_err());
        assert!(difference(remote(&[]), local(&[("root", "aa"), ("root", "bb")])).is_err());
    }

    #[test]
    fn the_budget_counts_removals_and_leaves_unchanged_records_out() {
        let result = difference(
            remote(&[("root", "aa", 10), ("character/1", "cc", 20)]),
            local(&[("root", "aa"), ("character/2", "bb"), ("character/3", "dd")]),
        )
        .unwrap();
        // One added and two removed, against one record left alone.
        assert_eq!(result.written_records(), 3);
        let budget = |records, bytes| DifferenceBudget {
            records, bytes, dependent_bytes: 0, work: 0,
        };
        assert!(result.within(budget(3, 20)));
        assert!(!result.within(budget(2, 20)));
        assert!(!result.within(budget(3, 19)));
    }
}
