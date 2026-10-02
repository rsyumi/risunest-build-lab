use crate::stamp::Stamp;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveRecord { pub id: String, pub creation_stamp: Stamp }

/// Folder projections retain their structure and apply this rule to references.
pub fn read_order(ordered_ids: &[String], live: &[LiveRecord], retired: &BTreeSet<String>) -> Vec<String> {
    let records: BTreeMap<_, _> = live.iter().filter(|r| !retired.contains(&r.id)).map(|r| (r.id.as_str(), r)).collect();
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for id in ordered_ids {
        if records.contains_key(id.as_str()) && seen.insert(id.as_str()) { result.push(id.clone()); }
    }
    let mut unlisted: Vec<_> = records.values().filter(|r| !seen.contains(r.id.as_str())).collect();
    unlisted.sort_by(|a, b| a.creation_stamp.cmp(&b.creation_stamp).then_with(|| a.id.as_bytes().cmp(b.id.as_bytes())));
    result.extend(unlisted.into_iter().map(|r| r.id.clone()));
    result
}
