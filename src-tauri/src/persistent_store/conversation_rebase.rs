use super::{record_projection::codec_error, PersistentStore, ReadTarget, StoreError, StoreResult};
use risunest_external_storage_format::message_pages::MessageHash;
use risunest_sync_wire::payload_value;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

const MAX_SPANS: usize = 4096;
const MAX_ALIGNMENT_CELLS: usize = 262144;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    pub baseline_lease: String,
    pub current_lease: String,
    pub baseline_revision: i64,
    pub current_revision: i64,
    pub character_id: String,
    pub conversation_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Span {
    original_start: usize,
    current_start: usize,
    count: usize,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum Mapping {
    #[serde(rename_all = "camelCase")]
    Mapped { original_count: usize, current_count: usize, spans: Vec<Span> },
    Conflict { reason: &'static str },
}

struct History<'a> {
    connection: &'a Connection,
    target: &'a ReadTarget,
    character: &'a str,
    conversation: &'a str,
    count: usize,
}

impl PersistentStore {
    pub(crate) fn rebase_conversation_history(&self, request: &Request) -> StoreResult<Mapping> {
        self.rebase_conversation_history_bounded(request, MAX_SPANS, MAX_ALIGNMENT_CELLS, 8192)
    }

    #[cfg(test)]
    pub(super) fn rebase_conversation_history_with_budget(&self, request: &Request, max_spans: usize, max_cells: usize, max_keys: usize) -> StoreResult<Mapping> {
        self.rebase_conversation_history_bounded(request, max_spans, max_cells, max_keys)
    }

    fn rebase_conversation_history_bounded(&self, request: &Request, max_spans: usize, max_cells: usize, max_keys: usize) -> StoreResult<Mapping> {
        let (old_connection, old_target) = self.read_view(Some(&request.baseline_lease))?;
        let (new_connection, new_target) = self.read_view(Some(&request.current_lease))?;
        let old_identity = super::sync_selection::identity(old_connection)?;
        let new_identity = super::sync_selection::identity(new_connection)?;
        let active_identity = super::sync_selection::identity(&self.connection)?;
        if old_target.revision != request.baseline_revision || new_target.revision != request.current_revision ||
            old_target.revision > new_target.revision ||
            old_identity.store_id != new_identity.store_id || old_identity.library_epoch != new_identity.library_epoch ||
            old_identity.generation != new_identity.generation || old_identity.selection_epoch != new_identity.selection_epoch ||
            new_identity.store_id != active_identity.store_id || new_identity.library_epoch != active_identity.library_epoch ||
            new_identity.generation != active_identity.generation || new_identity.selection_epoch != active_identity.selection_epoch {
            return Err(StoreError::Validation { message: "conversation reconciliation lease identity changed".into() });
        }
        let Some(original) = history(old_connection, &old_target, request)? else { return Ok(conflict("missing-history")); };
        let Some(current) = history(new_connection, &new_target, request)? else { return Ok(conflict("missing-history")); };
        map_history(&original, &current, max_spans, max_cells, max_keys)
    }
}

fn history<'a>(connection: &'a Connection, target: &'a ReadTarget, request: &'a Request) -> StoreResult<Option<History<'a>>> {
    let count: Option<i64> = connection.query_row(
        "SELECT message_count FROM conversations WHERE generation=?1 AND character_id=?2 AND conversation_id=?3",
        params![target.generation, request.character_id, request.conversation_id], |row| row.get(0),
    ).optional()?;
    count.map(|count| Ok(History { connection, target, character: &request.character_id,
        conversation: &request.conversation_id, count: usize::try_from(count).map_err(codec_error)? })).transpose()
}

fn conflict(reason: &'static str) -> Mapping { Mapping::Conflict { reason } }
fn append(spans: &mut Vec<Span>, original_start: usize, current_start: usize) {
    if let Some(last) = spans.last_mut() {
        if last.original_start + last.count == original_start && last.current_start + last.count == current_start {
            last.count += 1;
            return;
        }
    }
    spans.push(Span { original_start, current_start, count: 1 });
}

fn sql(reverse: bool) -> &'static str {
    if reverse {
        "SELECT message_index,canonical_hash,canonical_size FROM messages WHERE generation=?1 AND character_id=?2 AND conversation_id=?3 ORDER BY message_index DESC"
    } else {
        "SELECT message_index,canonical_hash,canonical_size FROM messages WHERE generation=?1 AND character_id=?2 AND conversation_id=?3 ORDER BY message_index ASC"
    }
}

fn key(history: &History<'_>, row: &Row<'_>, expected: usize) -> StoreResult<MessageHash> {
    let index: i64 = row.get(0)?;
    if usize::try_from(index).ok() != Some(expected) { return Err(codec_error("conversation history positions are incomplete")); }
    let hash: String = row.get(1)?;
    let byte_length = u64::try_from(row.get::<_, i64>(2)?).map_err(codec_error)?;
    if !hash.is_empty() { return Ok(MessageHash { hash, byte_length }); }
    // Imported rows may not yet have a canonical hash cache. The retained reader stays read-only.
    let body: String = history.connection.query_row(
        "SELECT value FROM messages WHERE generation=?1 AND character_id=?2 AND conversation_id=?3 AND message_index=?4",
        params![history.target.generation, history.character, history.conversation, index], |row| row.get(0),
    )?;
    let bytes = payload_value::canonicalize(body.as_bytes()).map_err(codec_error)?;
    Ok(MessageHash::from_bytes(&bytes))
}

fn equal(original: &History<'_>, current: &History<'_>) -> StoreResult<bool> {
    if original.count != current.count { return Ok(false); }
    let mut old_statement = original.connection.prepare(sql(false))?;
    let mut new_statement = current.connection.prepare(sql(false))?;
    let mut old_rows = old_statement.query(params![original.target.generation, original.character, original.conversation])?;
    let mut new_rows = new_statement.query(params![current.target.generation, current.character, current.conversation])?;
    for index in 0..original.count {
        let old = old_rows.next()?.ok_or_else(|| codec_error("missing original conversation row"))?;
        let new = new_rows.next()?.ok_or_else(|| codec_error("missing current conversation row"))?;
        if key(original, old, index)? != key(current, new, index)? { return Ok(false); }
    }
    Ok(old_rows.next()?.is_none() && new_rows.next()?.is_none())
}

fn subsequence(short: &History<'_>, long: &History<'_>, reverse: bool, max_spans: usize) -> StoreResult<Result<Option<Vec<Span>>, ()>> {
    let mut short_statement = short.connection.prepare(sql(reverse))?;
    let mut long_statement = long.connection.prepare(sql(reverse))?;
    let mut short_rows = short_statement.query(params![short.target.generation, short.character, short.conversation])?;
    let mut long_rows = long_statement.query(params![long.target.generation, long.character, long.conversation])?;
    let mut spans = Vec::new();
    let mut short_index = 0;
    let mut short_key = if short.count == 0 { None } else {
        let row = short_rows.next()?.ok_or_else(|| codec_error("missing original conversation row"))?;
        Some(key(short, row, if reverse { short.count - 1 } else { 0 })?)
    };
    for long_index in 0..long.count {
        if short_index == short.count { break; }
        let row = long_rows.next()?.ok_or_else(|| codec_error("missing current conversation row"))?;
        if short_key.as_ref() != Some(&key(long, row, if reverse { long.count - long_index - 1 } else { long_index })?) { continue; }
        append(&mut spans, short_index, long_index);
        if spans.len() > max_spans { return Ok(Err(())); }
        short_index += 1;
        short_key = if short_index == short.count { None } else {
            let row = short_rows.next()?.ok_or_else(|| codec_error("missing original conversation row"))?;
            Some(key(short, row, if reverse { short.count - short_index - 1 } else { short_index })?)
        };
    }
    if short_index != short.count { return Ok(Ok(None)); }
    if reverse {
        spans.reverse();
        for span in &mut spans {
            span.original_start = short.count - span.original_start - span.count;
            span.current_start = long.count - span.current_start - span.count;
        }
    }
    Ok(Ok(Some(spans)))
}

fn keys(history: &History<'_>, from: usize, to: usize) -> StoreResult<Vec<MessageHash>> {
    let mut statement = history.connection.prepare("SELECT message_index,canonical_hash,canonical_size FROM messages WHERE generation=?1 AND character_id=?2 AND conversation_id=?3 AND message_index>=?4 AND message_index<?5 ORDER BY message_index")?;
    let mut rows = statement.query(params![history.target.generation, history.character, history.conversation, i64::try_from(from).map_err(codec_error)?, i64::try_from(to).map_err(codec_error)?])?;
    let mut result = Vec::with_capacity(to - from);
    for index in from..to {
        let row = rows.next()?.ok_or_else(|| codec_error("missing conversation row"))?;
        result.push(key(history, row, index)?);
    }
    Ok(result)
}

fn map_history(original: &History<'_>, current: &History<'_>, max_spans: usize, max_cells: usize, max_keys: usize) -> StoreResult<Mapping> {
    let mapped = |spans| Mapping::Mapped { original_count: original.count, current_count: current.count, spans };
    if equal(original, current)? {
        return Ok(mapped(if original.count == 0 { Vec::new() } else { vec![Span { original_start: 0, current_start: 0, count: original.count }] }));
    }
    let (short, long) = if original.count <= current.count { (original, current) } else { (current, original) };
    let forward = match subsequence(short, long, false, max_spans)? { Ok(value) => value, Err(()) => return Ok(conflict("history-budget")) };
    if let Some(forward) = forward {
        let backward = match subsequence(short, long, true, max_spans)? { Ok(Some(value)) => value, _ => return Ok(conflict("history-budget")) };
        let mut forced: Vec<Span> = Vec::new();
        let (mut left, mut right) = (0, 0);
        while left < forward.len() && right < backward.len() {
            let a = &forward[left];
            let b = &backward[right];
            let start = a.original_start.max(b.original_start);
            let end = (a.original_start + a.count).min(b.original_start + b.count);
            if end > start && a.current_start + start - a.original_start == b.current_start + start - b.original_start {
                let current_start = a.current_start + start - a.original_start;
                if let Some(last) = forced.last_mut().filter(|last| last.original_start + last.count == start && last.current_start + last.count == current_start) {
                    last.count += end - start;
                } else { forced.push(Span { original_start: start, current_start, count: end - start }); }
            }
            if a.original_start + a.count <= b.original_start + b.count { left += 1; } else { right += 1; }
        }
        if forced.len() > max_spans { return Ok(conflict("history-budget")); }
        if original.count > current.count { for span in &mut forced { std::mem::swap(&mut span.original_start, &mut span.current_start); } }
        return Ok(mapped(forced));
    }
    let (prefix, suffix) = fringes(original, current, max_keys)?;
    let old_end = original.count - suffix;
    let new_end = current.count - suffix;
    if (old_end - prefix).checked_add(new_end - prefix).is_none_or(|count| count > max_keys) { return Ok(conflict("history-budget")); }
    let old_keys = keys(original, prefix, old_end)?;
    let new_keys = keys(current, prefix, new_end)?;
    let old_keys: Vec<_> = old_keys.iter().map(|key| (key.hash.as_str(), key.byte_length)).collect();
    let new_keys: Vec<_> = new_keys.iter().map(|key| (key.hash.as_str(), key.byte_length)).collect();
    Ok(match align_windows(&old_keys, &new_keys, max_spans, max_cells) {
        Ok(middle) => {
            let mut spans = Vec::new();
            if prefix > 0 { spans.push(Span { original_start: 0, current_start: 0, count: prefix }); }
            for span in middle { append_run(&mut spans, Span { original_start: prefix + span.original_start, current_start: prefix + span.current_start, count: span.count }); }
            if suffix > 0 { append_run(&mut spans, Span { original_start: old_end, current_start: new_end, count: suffix }); }
            if spans.len() > max_spans { conflict("history-budget") } else { mapped(spans) }
        }, Err(reason) => conflict(reason),
    })
}

fn append_run(spans: &mut Vec<Span>, span: Span) {
    if let Some(last) = spans.last_mut().filter(|last| last.original_start + last.count == span.original_start && last.current_start + last.count == span.current_start) {
        last.count += span.count;
    } else { spans.push(span); }
}

type Fingerprint = (String, u64);
fn fingerprint(key: MessageHash) -> Fingerprint { (key.hash, key.byte_length) }

fn scan(history: &History<'_>, from: usize, to: usize, mut visit: impl FnMut(Fingerprint) -> bool) -> StoreResult<bool> {
    let mut statement = history.connection.prepare("SELECT message_index,canonical_hash,canonical_size FROM messages WHERE generation=?1 AND character_id=?2 AND conversation_id=?3 AND message_index>=?4 AND message_index<?5 ORDER BY message_index")?;
    let mut rows = statement.query(params![history.target.generation, history.character, history.conversation, i64::try_from(from).map_err(codec_error)?, i64::try_from(to).map_err(codec_error)?])?;
    for index in from..to {
        let row = rows.next()?.ok_or_else(|| codec_error("missing conversation row"))?;
        if !visit(fingerprint(key(history, row, index)?)) { return Ok(false); }
    }
    Ok(true)
}

fn matching_edge(original: &History<'_>, current: &History<'_>, reverse: bool, limit: usize) -> StoreResult<usize> {
    let mut old_statement = original.connection.prepare(sql(reverse))?;
    let mut new_statement = current.connection.prepare(sql(reverse))?;
    let mut old_rows = old_statement.query(params![original.target.generation, original.character, original.conversation])?;
    let mut new_rows = new_statement.query(params![current.target.generation, current.character, current.conversation])?;
    for offset in 0..limit {
        let old = old_rows.next()?.ok_or_else(|| codec_error("missing original conversation row"))?;
        let new = new_rows.next()?.ok_or_else(|| codec_error("missing current conversation row"))?;
        if key(original, old, if reverse { original.count - offset - 1 } else { offset })? !=
            key(current, new, if reverse { current.count - offset - 1 } else { offset })? { return Ok(offset); }
    }
    Ok(limit)
}

fn no_crossing(original: &History<'_>, current: &History<'_>, from: usize, to: usize, old_from: usize, old_to: usize, new_from: usize, new_to: usize, max_keys: usize) -> StoreResult<bool> {
    let mut values = HashSet::new();
    if !scan(original, from, to, |key| { values.insert(key); values.len() <= max_keys })? { return Ok(false); }
    if !scan(original, old_from, old_to, |key| !values.contains(&key))? { return Ok(false); }
    scan(current, new_from, new_to, |key| !values.contains(&key))
}

fn fringes(original: &History<'_>, current: &History<'_>, max_keys: usize) -> StoreResult<(usize, usize)> {
    let common = original.count.min(current.count);
    let prefix = matching_edge(original, current, false, common)?;
    let suffix = matching_edge(original, current, true, common - prefix)?;
    let mut candidates = Vec::new();
    for (offset, key) in keys(original, prefix.saturating_sub(64), prefix)?.into_iter().enumerate() {
        candidates.push((fingerprint(key), prefix.saturating_sub(64) + offset, true));
    }
    for (offset, key) in keys(original, original.count - suffix, (original.count - suffix + 64).min(original.count))?.into_iter().enumerate() {
        candidates.push((fingerprint(key), original.count - suffix + offset, false));
    }
    let mut counts: HashMap<Fingerprint, [u8; 2]> = candidates.iter().map(|(key, _, _)| (key.clone(), [0, 0])).collect();
    for (side, history) in [original, current].into_iter().enumerate() {
        scan(history, 0, history.count, |key| {
            if let Some(count) = counts.get_mut(&key) { count[side] = (count[side] + 1).min(2); }
            true
        })?;
    }
    let (mut proved_prefix, mut proved_suffix) = (0, 0);
    for (key, index, is_prefix) in candidates {
        if counts[&key] != [1, 1] { continue; }
        if is_prefix { proved_prefix = proved_prefix.max(index + 1); }
        else { proved_suffix = proved_suffix.max(original.count - index); }
    }
    if proved_prefix < prefix && no_crossing(original, current, 0, prefix, prefix, original.count, prefix, current.count, max_keys)? { proved_prefix = prefix; }
    if proved_suffix < suffix && no_crossing(original, current, original.count - suffix, original.count, 0, original.count - suffix, 0, current.count - suffix, max_keys)? { proved_suffix = suffix; }
    Ok((proved_prefix, proved_suffix))
}

fn align_windows<T: Eq + Hash>(original: &[T], current: &[T], max_spans: usize, mut remaining: usize) -> Result<Vec<Span>, &'static str> {
    fn positions<T: Eq + Hash>(keys: &[T]) -> HashMap<&T, (usize, usize, usize)> {
        let mut result = HashMap::new();
        for (index, key) in keys.iter().enumerate() {
            let known = result.entry(key).or_insert((index, index, 0));
            known.1 = index;
            known.2 += 1;
        }
        result
    }
    let old_positions = positions(original);
    let new_positions = positions(current);
    let mut suffix_min = vec![current.len(); original.len() + 1];
    for index in (0..original.len()).rev() {
        suffix_min[index] = suffix_min[index + 1].min(new_positions.get(&original[index]).map(|value| value.0).unwrap_or(current.len()));
    }
    let mut anchors = Vec::new();
    let mut prefix_max: Option<usize> = None;
    for (index, key) in original.iter().enumerate() {
        if let Some(&(first, last, count)) = new_positions.get(key) {
            if count == 1 && old_positions[key].2 == 1 && prefix_max.is_none_or(|max| max < first) && suffix_min[index + 1] > first {
                anchors.push((index, first));
            }
            prefix_max = Some(prefix_max.map_or(last, |max| max.max(last)));
        }
    }
    anchors.push((original.len(), current.len()));
    let mut spans = Vec::new();
    let (mut old_start, mut new_start) = (0, 0);
    for (old_end, new_end) in anchors {
        let old_window = &original[old_start..old_end];
        let new_window = &current[new_start..new_end];
        if old_window == new_window {
            for index in 0..old_window.len() { append(&mut spans, old_start + index, new_start + index); }
        } else {
            let cells = (old_window.len() + 1).checked_mul(new_window.len() + 1).ok_or("history-budget")?;
            if cells > remaining { return Err("history-budget"); }
            remaining -= cells;
            for span in align(old_window, new_window, max_spans)? {
                for index in 0..span.count { append(&mut spans, old_start + span.original_start + index, new_start + span.current_start + index); }
            }
        }
        if old_end < original.len() { append(&mut spans, old_end, new_end); }
        if spans.len() > max_spans { return Err("history-budget"); }
        old_start = old_end + 1;
        new_start = new_end + 1;
    }
    Ok(spans)
}

fn align<T: Eq>(original: &[T], current: &[T], max_spans: usize) -> Result<Vec<Span>, &'static str> {
    let width = current.len() + 1;
    let cells = (original.len() + 1) * width;
    let mut lengths = vec![0_u32; cells];
    let mut prefix = vec![0_u32; cells];
    for old in (0..original.len()).rev() {
        for new in (0..current.len()).rev() {
            let at = old * width + new;
            lengths[at] = lengths[at + width].max(lengths[at + 1])
                .max(if original[old] == current[new] { lengths[at + width + 1] + 1 } else { 0 });
        }
    }
    for old in 0..original.len() {
        for new in 0..current.len() {
            let at = (old + 1) * width + new + 1;
            prefix[at] = prefix[at - width].max(prefix[at - 1])
                .max(if original[old] == current[new] { prefix[at - width - 1] + 1 } else { 0 });
        }
    }
    let mut edges: Vec<Option<(usize, usize)>> = vec![None; lengths[0] as usize + 1];
    let mut ambiguous = vec![false; edges.len()];
    for old in 0..original.len() {
        for new in 0..current.len() {
            let at = old * width + new;
            if original[old] != current[new] || prefix[at] + 1 + lengths[at + width + 1] != lengths[0] { continue; }
            let rank = prefix[at] as usize + 1;
            if edges[rank].is_some() { ambiguous[rank] = true; }
            edges[rank] = Some((old, new));
        }
    }
    let mut spans = Vec::new();
    for (rank, edge) in edges.into_iter().enumerate() {
        if let Some((old, new)) = edge.filter(|_| !ambiguous[rank]) {
            append(&mut spans, old, new);
            if spans.len() > max_spans { return Err("history-budget"); }
        }
    }
    Ok(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_alignment_preserves_unrelated_changes_and_rejects_ambiguous_occurrences() {
        assert_eq!(align(&['a', 'b', 'c'], &['x', 'a', 'y', 'c'], 8).unwrap(), vec![
            Span { original_start: 0, current_start: 1, count: 1 }, Span { original_start: 2, current_start: 3, count: 1 },
        ]);
        assert!(align(&['a', 'a'], &['a', 'a', 'a'], 8).unwrap().is_empty());
        assert!(align(&['a', 'b'], &['b', 'a'], 8).unwrap().is_empty());
        assert_eq!(align(&['a', 'a', 'b'], &['a', 'a', 'a', 'b'], 8).unwrap(),
            vec![Span { original_start: 2, current_start: 3, count: 1 }]);
        assert_eq!(align(&['a', 'b', 'c'], &['a', 'x', 'c'], 1), Err("history-budget"));
    }
}
