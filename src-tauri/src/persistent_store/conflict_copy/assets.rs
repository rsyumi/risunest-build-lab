use super::*;
use crate::asset_repository::owner_manifest_codec::{self, OwnerManifestEntry};
use crate::lossless_f0::{self, PortableFragment};
use super::super::AssetOwnerLocator;
use std::collections::BTreeMap;

#[derive(Clone)]
struct Occurrence { location: AssetLocation, kind: String, key: String, offset: Option<usize> }

fn bad() -> StoreError { conflict(CopyConflict::AssetBinding) }
fn escape(value: &str) -> String { value.replace('~', "~0").replace('/', "~1") }

// The fragment inventory uses JSONPath. Convert only its emitted property/index grammar.
fn pointer(path: &str) -> StoreResult<String> {
    let mut rest = path.strip_prefix('$').ok_or_else(bad)?;
    let mut out = String::new();
    while !rest.is_empty() {
        let (part, consumed) = if let Some(after) = rest.strip_prefix('.') {
            let end = after.find(['.', '[']).unwrap_or(after.len());
            if end == 0 { return Err(bad()); }
            (after[..end].to_string(), end + 1)
        } else if let Some(after) = rest.strip_prefix("[\"") {
            let mut escaped = false;
            let mut end = None;
            for (index, character) in after.char_indices() {
                if escaped { escaped = false; continue; }
                if character == '\\' { escaped = true; }
                else if character == '"' { end = Some(index); break; }
            }
            let end = end.ok_or_else(bad)?;
            if !after[end + 1..].starts_with(']') { return Err(bad()); }
            (serde_json::from_str::<String>(&rest[1..end + 3])?, end + 4)
        } else if let Some(after) = rest.strip_prefix('[') {
            let end = after.find(']').ok_or_else(bad)?;
            if end == 0 || !after[..end].bytes().all(|byte| byte.is_ascii_digit()) { return Err(bad()); }
            (after[..end].to_string(), end + 2)
        } else { return Err(bad()); };
        out.push('/'); out.push_str(&escape(&part)); rest = &rest[consumed..];
    }
    Ok(out)
}

enum Fragment { Character, Conversation, Message }

fn occurrences(value: &Value, fragment: Fragment) -> StoreResult<Vec<Occurrence>> {
    let message = matches!(fragment, Fragment::Message);
    let references = lossless_f0::scan_portable_fragment(match fragment {
        Fragment::Character => PortableFragment::Character { value, selected_chat: None, has_chats: false },
        Fragment::Conversation => PortableFragment::Conversation { value, character_id: "copy" },
        Fragment::Message => PortableFragment::Message { value, character_id: "copy", conversation_id: "copy", index: 0 },
    }).map_err(|_| bad())?;
    let mut result = Vec::new();
    for reference in references {
        if reference.target_kind == "cold" { return Err(conflict(CopyConflict::SourceHistory)); }
        if !matches!(reference.target_kind.as_str(), "asset" | "inlay") { continue; }
        let mut path = pointer(&reference.source_path)?;
        if message { path = path.strip_prefix("/message/0").ok_or_else(bad)?.to_owned(); }
        if value.pointer(&path).and_then(Value::as_str).is_none() { return Err(bad()); }
        result.push(Occurrence { location: AssetLocation { pointer: path,
            token_index: reference.metadata.get("tokenIndex").and_then(Value::as_u64).map(|value| value as usize) },
            kind: reference.target_kind, key: reference.target_key,
            offset: reference.metadata.get("offset").and_then(Value::as_u64).map(|value| value as usize) });
    }
    // This native root form is also recognized outside the named legacy image fields.
    fn paths(value: &Value, path: &str, result: &mut Vec<Occurrence>) {
        match value {
            Value::String(key) if key.starts_with("assets/") => {
                let location = AssetLocation { pointer: path.into(), token_index: None };
                if !result.iter().any(|entry| entry.location == location) {
                    result.push(Occurrence { location, kind: "asset".into(), key: key.clone(), offset: None });
                }
            },
            Value::Array(items) => for (index, item) in items.iter().enumerate() { paths(item, &format!("{path}/{index}"), result); },
            Value::Object(items) => for (key, item) in items { paths(item, &format!("{path}/{}", escape(key)), result); },
            _ => (),
        }
    }
    paths(value, "", &mut result);
    Ok(result)
}

fn is_external(key: &str) -> bool {
    lossless_f0::is_external_asset_key(key)
}

fn read_head(store: &PersistentStore, lease: &str, owner: &AssetOwnerLocator) -> StoreResult<Option<AssetOwnerHead>> {
    let (db, target) = store.read_view(Some(lease)).map_err(|_| bad())?;
    Ok(query::read_asset_owner_head(db, owner, &target)?.map(|head| head.value))
}

fn manifest_entries(cas: &PayloadCas, head: &AssetOwnerHead, value: Option<&Value>) -> StoreResult<Vec<OwnerManifestEntry>> {
    head.validate()?;
    if !head.present { return if value.is_none() { Ok(Vec::new()) } else { Err(bad()) }; }
    let hash = head.manifest_hash.as_deref().ok_or_else(bad)?;
    let bytes = cas.read_object(hash)?.ok_or_else(bad)?;
    if owner_manifest_codec::owner_manifest_identity(&bytes) != hash { return Err(bad()); }
    let entries = owner_manifest_codec::decode_owner_manifest(&bytes).map_err(|_| bad())?;
    let tuples = value.and_then(Value::as_array).ok_or_else(bad)?;
    if entries.len() as i64 != head.entry_count || tuples.len() != entries.len() || tuples.iter().zip(&entries)
        .any(|(tuple, entry)| tuple.as_array().is_none_or(|tuple| tuple.len() != 3
            || tuple.iter().zip(&entry.tuple).any(|(actual, expected)| actual.as_str() != Some(expected)))) { return Err(bad()); }
    Ok(entries)
}

struct Resolver<'a> {
    store: &'a PersistentStore,
    request: &'a ConflictCopyRequest,
    aliases: BTreeMap<(String, String), AssetAlias>,
    used: BTreeSet<usize>,
}

impl Resolver<'_> {
    fn alias(&mut self, occurrence: &Occurrence, scope: Option<&AssetScope>, expected_hash: Option<&Option<[u8; 32]>>) -> StoreResult<Option<String>> {
        let proof = if let Some(scope) = scope {
            let matches = self.request.introduced_asset_bindings.iter().enumerate()
                .filter(|(_, binding)| &binding.scope == scope && binding.location == occurrence.location).collect::<Vec<_>>();
            if matches.len() > 1 { return Err(bad()); }
            matches.into_iter().next()
        } else { None };
        let (lease, supplied) = match proof {
            Some((index, proof)) => {
                self.used.insert(index);
                if proof.alias.kind != occurrence.kind || proof.alias.key != occurrence.key { return Err(bad()); }
                (&proof.witness_lease, Some(&proof.alias))
            },
            None if scope.is_some() && !is_external(&occurrence.key) => return Err(bad()),
            None => (&self.request.source.lease, None),
        };
        let (db, target) = self.store.read_view(Some(lease)).map_err(|_| bad())?;
        if !same_identity(&self.request.source.identity, &sync_selection::identity(db)?) { return Err(bad()); }
        let Some(alias) = query::read_asset_alias(db, &occurrence.kind, &occurrence.key, &target)?.map(|alias| alias.value) else {
            return if supplied.is_none() && is_external(&occurrence.key) { Ok(None) } else { Err(bad()) };
        };
        if supplied.is_some_and(|expected| *expected != alias) { return Err(bad()); }
        if scope.is_some() && supplied.is_none() { return Err(bad()); }
        // A legacy key without an immutable object binding cannot prove which bytes the lease
        // referred to after another writer changed that physical key.
        if alias.object_hash.is_none() { return Err(bad()); }
        if let Some(expected) = expected_hash {
            if alias.object_hash != expected.map(hex::encode) { return Err(bad()); }
        }
        // Rebinding every proven occurrence avoids overwriting a shared alias, even when its
        // source and introduced occurrences deliberately denote different payloads.
        let identity = serde_json::to_value((&self.request.header.request_id, &self.request.target_character_id,
            &self.request.target_conversation_id, &alias))?;
        let hash = exact_hash(&identity)?;
        let key = if alias.kind == "asset" { format!("assets/copy-{hash}") } else { format!("copy-{hash}") };
        let mut replacement = alias;
        replacement.key = key.clone();
        self.aliases.insert((replacement.kind.clone(), key.clone()), replacement);
        Ok(Some(key))
    }

    fn rewrite(&mut self, value: &mut Value, fragment: Fragment, scope: Option<&AssetScope>,
        owner_entries: Option<&[OwnerManifestEntry]>, field: Option<&str>, check: &dyn Fn() -> bool) -> StoreResult<()> {
        let mut references = if let Some(field) = field {
            let mut wrapper = serde_json::Map::new();
            wrapper.insert(field.to_owned(), value.clone());
            let prefix = format!("/{}", escape(field));
            occurrences(&Value::Object(wrapper), fragment)?.into_iter().filter_map(|mut item| {
                item.location.pointer.strip_prefix(&prefix).map(str::to_owned).map(|relative| { item.location.pointer = relative; item })
            }).collect::<Vec<_>>()
        } else { occurrences(value, fragment)? };
        // Replace tokens from the end, keeping the inventory's UTF-16 offsets valid.
        references.sort_by(|a, b| a.location.pointer.cmp(&b.location.pointer).then(b.offset.cmp(&a.offset)));
        for reference in references {
            cancelled(check)?;
            let tuple_index = if field == Some("additionalAssets") {
                reference.location.pointer.strip_prefix('/').and_then(|path| path.strip_suffix("/1")).and_then(|index| index.parse::<usize>().ok())
            } else { reference.location.pointer.strip_prefix("/additionalAssets/").and_then(|path| path.strip_suffix("/1"))
                .and_then(|index| index.parse::<usize>().ok()) };
            let expected = tuple_index.and_then(|index| owner_entries.and_then(|entries| entries.get(index))).map(|entry| &entry.payload_hash);
            let Some(key) = self.alias(&reference, scope, expected)? else { continue };
            let value = value.pointer_mut(&reference.location.pointer).ok_or_else(bad)?;
            if let Some(offset) = reference.offset {
                let text = value.as_str().ok_or_else(bad)?;
                let mut units = 0;
                let byte = text.char_indices().find_map(|(byte, ch)| { let found = units == offset; units += ch.len_utf16(); found.then_some(byte) }).ok_or_else(bad)?;
                let tail = &text[byte..];
                let prefix = ["{{inlay::", "{{inlayed::", "{{inlayeddata::"].into_iter().find(|prefix| tail.starts_with(prefix)).ok_or_else(bad)?;
                let start = byte + prefix.len();
                let end = start + reference.key.len();
                if text.get(start..end) != Some(reference.key.as_str()) || !text[end..].starts_with("}}") { return Err(bad()); }
                let mut replaced = text.to_owned(); replaced.replace_range(start..end, &key); *value = Value::String(replaced);
            } else {
                if value.as_str() != Some(reference.key.as_str()) { return Err(bad()); }
                *value = Value::String(key);
            }
        }
        Ok(())
    }
}

fn introduced_entries(store: &PersistentStore, request: &ConflictCopyRequest, fields: &[CopyField]) -> StoreResult<Option<Vec<OwnerManifestEntry>>> {
    let owner = AssetOwnerLocator::CharacterAdditionalAssets { character_id: request.source.character_id.clone() };
    if let Some(proof) = &request.introduced_owner_head {
        if proof.head.owner != owner || read_head(store, &proof.witness_lease, &owner)?.as_ref() != Some(&proof.head) { return Err(bad()); }
        let (db, _) = store.read_view(Some(&proof.witness_lease)).map_err(|_| bad())?;
        if !same_identity(&request.source.identity, &sync_selection::identity(db)?) { return Err(bad()); }
        let Some(CopyField::Set { field, value }) = fields.get(proof.mutation) else { return Err(bad()); };
        if field != "additionalAssets" { return Err(bad()); }
        Ok(Some(manifest_entries(&PayloadCas::new(&store.repository_root)?, &proof.head, Some(value))?))
    } else { Ok(None) }
}

pub(super) fn prepare(store: &PersistentStore, request: &ConflictCopyRequest, character: &mut Value, conversation: &mut Value,
    fields: &mut [CopyField], chat_fields: &mut [CopyField], ranges: &mut [CopyRange], check: &dyn Fn() -> bool) -> StoreResult<Vec<AssetAlias>> {
    let cas = PayloadCas::new(&store.repository_root)?;
    let owner = AssetOwnerLocator::CharacterAdditionalAssets { character_id: request.source.character_id.clone() };
    let source_head = read_head(store, &request.source.lease, &owner)?;
    let owner_changed = fields.iter().any(|field| matches!(field, CopyField::Set { field, .. } | CopyField::Delete { field } if field == "additionalAssets"));
    let source_entries = if !owner_changed { source_head.as_ref().map(|head| manifest_entries(&cas, head, character.get("additionalAssets"))).transpose()? } else { None };
    let introduced_entries = introduced_entries(store, request, fields)?;
    let mut resolver = Resolver { store, request, aliases: BTreeMap::new(), used: BTreeSet::new() };
    // Superseded fields and deleted source rows are not part of the copy's asset roots.
    for field in fields.iter() {
        let name = match field { CopyField::Set { field, .. } | CopyField::Delete { field } => field };
        character.as_object_mut().ok_or_else(bad)?.remove(name);
    }
    for field in chat_fields.iter() {
        let name = match field { CopyField::Set { field, .. } | CopyField::Delete { field } => field };
        conversation.as_object_mut().ok_or_else(bad)?.remove(name);
    }
    let old_messages = conversation.as_object_mut().ok_or_else(bad)?.remove("message").and_then(|value| value.as_array().cloned()).ok_or_else(bad)?;
    if old_messages.len() as i64 != request.source.message_count { return Err(conflict(CopyConflict::SourceHistory)); }
    let mut messages = old_messages.into_iter().map(|value| (value, None)).collect::<Vec<_>>();
    for (range_index, range) in ranges.iter().enumerate() {
        if range.start < 0 || range.delete_count < 0 || range.start > messages.len() as i64
            || range.delete_count > messages.len() as i64 - range.start || range.messages.iter().any(|value| !value.is_object()) { return Err(conflict(CopyConflict::Range)); }
        messages.splice(range.start as usize..(range.start + range.delete_count) as usize, range.messages.iter().enumerate()
            .map(|(message, value)| (value.clone(), Some(AssetScope::Message { range: range_index, message }))));
    }
    resolver.rewrite(character, Fragment::Character, None, source_entries.as_deref(), None, check)?;
    resolver.rewrite(conversation, Fragment::Conversation, None, None, None, check)?;
    for (index, field) in fields.iter_mut().enumerate() {
        if let CopyField::Set { field, value } = field {
            resolver.rewrite(value, Fragment::Character, Some(&AssetScope::CharacterField { mutation: index }),
                introduced_entries.as_deref(), Some(field), check)?;
        }
    }
    for (index, field) in chat_fields.iter_mut().enumerate() {
        if let CopyField::Set { field, value } = field {
            resolver.rewrite(value, Fragment::Conversation, Some(&AssetScope::ConversationField { mutation: index }), None, Some(field), check)?;
        }
    }
    for (message, scope) in &mut messages { resolver.rewrite(message, Fragment::Message, scope.as_ref(), None, None, check)?; }
    // Validate the whole submitted prefix, including rows a later local range removes, without
    // publishing aliases that the final copied history does not reference.
    let kept = messages.iter().filter_map(|(_, scope)| scope.clone()).collect::<BTreeSet<_>>();
    for (range, entry) in ranges.iter().enumerate() {
        for (message, value) in entry.messages.iter().enumerate() {
            let scope = AssetScope::Message { range, message };
            if kept.contains(&scope) { continue; }
            let mut discarded = value.clone();
            let retained = resolver.aliases.clone();
            resolver.rewrite(&mut discarded, Fragment::Message, Some(&scope), None, None, check)?;
            resolver.aliases = retained;
        }
    }
    if resolver.used.len() != request.introduced_asset_bindings.len() { return Err(bad()); }
    conversation.as_object_mut().unwrap().insert("message".into(), Value::Array(messages.into_iter().map(|(value, _)| value).collect()));
    Ok(resolver.aliases.into_values().collect())
}

// Only submitted values are rewritten. Existing target detail and messages retain their aliases.
pub(super) fn prepare_scoped(store: &PersistentStore, request: &ConflictCopyRequest,
    fields: &mut [CopyField], chat_fields: &mut [CopyField], ranges: &mut [CopyRange], check: &dyn Fn() -> bool) -> StoreResult<Vec<AssetAlias>> {
    let entries = introduced_entries(store, request, fields)?;
    let mut resolver = Resolver { store, request, aliases: BTreeMap::new(), used: BTreeSet::new() };
    for (mutation, field) in fields.iter_mut().enumerate() {
        if let CopyField::Set { field, value } = field {
            resolver.rewrite(value, Fragment::Character, Some(&AssetScope::CharacterField { mutation }), entries.as_deref(), Some(field), check)?;
        }
    }
    for (mutation, field) in chat_fields.iter_mut().enumerate() {
        if let CopyField::Set { field, value } = field {
            resolver.rewrite(value, Fragment::Conversation, Some(&AssetScope::ConversationField { mutation }), None, Some(field), check)?;
        }
    }
    for (range, entry) in ranges.iter_mut().enumerate() {
        for (message, value) in entry.messages.iter_mut().enumerate() {
            resolver.rewrite(value, Fragment::Message, Some(&AssetScope::Message { range, message }), None, None, check)?;
        }
    }
    if resolver.used.len() != request.introduced_asset_bindings.len() { return Err(bad()); }
    Ok(resolver.aliases.into_values().collect())
}

pub(super) fn retain_referenced(character: &Value, conversation: &Value, messages: &[Value], aliases: &[AssetAlias]) -> StoreResult<Vec<AssetAlias>> {
    let mut references = occurrences(character, Fragment::Character)?;
    references.extend(occurrences(conversation, Fragment::Conversation)?);
    for message in messages { references.extend(occurrences(message, Fragment::Message)?); }
    let known = aliases.iter().map(|alias| ((alias.kind.clone(), alias.key.clone()), alias)).collect::<BTreeMap<_, _>>();
    let mut retained = BTreeMap::new();
    for reference in references {
        let identity = (reference.kind, reference.key);
        if let Some(alias) = known.get(&identity) { retained.insert(identity, (**alias).clone()); }
        else if !is_external(&identity.1) { return Err(bad()); }
    }
    Ok(retained.into_values().collect())
}

pub(super) fn hydrate_scoped(store: &PersistentStore, request: &ConflictCopyRequest, check: &dyn Fn() -> bool,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> StoreResult<()> {
    let Some(proof) = &request.introduced_owner_head else { return Ok(()); };
    let owner = AssetOwnerLocator::CharacterAdditionalAssets { character_id: request.source.character_id.clone() };
    if proof.head.owner != owner || read_head(store, &proof.witness_lease, &owner)?.as_ref() != Some(&proof.head) { return Err(bad()); }
    let (db, _) = store.read_view(Some(&proof.witness_lease))?;
    if !same_identity(&request.source.identity, &sync_selection::identity(db)?) { return Err(bad()); }
    if !matches!(request.character_fields.get(proof.mutation), Some(CopyField::Set { field, .. }) if field == "additionalAssets") { return Err(bad()); }
    proof.head.validate()?;
    let Some(hash) = &proof.head.manifest_hash else { return Ok(()); };
    if PayloadCas::new(&store.repository_root)?.stat_object(hash)?.is_some() { return Ok(()); }
    cancelled(check)?;
    let probe = || if check() { Err(crate::server_sync::SyncError::new("cancelled", 499)) } else { Ok(()) };
    match crate::server_sync::residency::HydrationSession::new(&store.repository_root, cancellation)
        .and_then(|mut session| session.hydrate_many(std::slice::from_ref(hash), &probe)) {
        Ok(missing) if missing.is_empty() => Ok(()),
        _ if check() => Err(conflict(CopyConflict::Cancelled)),
        _ => Err(bad()),
    }
}

pub(super) fn manifest(character: &Value, character_id: &str, aliases: &[AssetAlias]) -> StoreResult<(AssetOwnerHead, Option<Vec<u8>>)> {
    let owner = AssetOwnerLocator::CharacterAdditionalAssets { character_id: character_id.into() };
    let Some(tuples) = character.get("additionalAssets") else {
        return Ok((AssetOwnerHead { owner, present: false, manifest_hash: None, entry_count: 0 }, None));
    };
    let entries = tuples.as_array().ok_or_else(bad)?.iter().map(|tuple| {
        let tuple: [String; 3] = serde_json::from_value(tuple.clone()).map_err(|_| bad())?;
        let alias = aliases.iter().find(|alias| alias.kind == "asset" && alias.key == tuple[1]);
        let payload_hash = alias.and_then(|alias| alias.object_hash.as_deref()).map(|hash| hex::decode(hash)
            .ok().and_then(|bytes| bytes.try_into().ok()).ok_or_else(bad)).transpose()?;
        if alias.is_none() && !is_external(&tuple[1]) { return Err(bad()); }
        Ok(OwnerManifestEntry { tuple, payload_hash })
    }).collect::<StoreResult<Vec<_>>>()?;
    let bytes = owner_manifest_codec::encode_owner_manifest(&entries).map_err(|_| bad())?;
    Ok((AssetOwnerHead { owner, present: true, manifest_hash: Some(owner_manifest_codec::owner_manifest_identity(&bytes)),
        entry_count: entries.len() as i64 }, Some(bytes)))
}

impl PersistentStore {
    // Invoke before pausing frontend persistence. The atomic copy only reads local CAS objects.
    pub(crate) fn hydrate_conflict_copy(&self, request: &ConflictCopyRequest, check: &dyn Fn() -> bool,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> StoreResult<()> {
        if self.read_conflict_copy_source(&request.source.lease, &request.source.character_id, &request.source.conversation_id)? != request.source {
            return Err(conflict(CopyConflict::SourceHistory));
        }
        let mut hashes = BTreeSet::new();
        let owner = AssetOwnerLocator::CharacterAdditionalAssets { character_id: request.source.character_id.clone() };
        if let Some(hash) = read_head(self, &request.source.lease, &owner)?.and_then(|head| head.manifest_hash) { hashes.insert(hash); }
        if let Some(proof) = &request.introduced_owner_head {
            if proof.head.owner != owner || read_head(self, &proof.witness_lease, &owner)?.as_ref() != Some(&proof.head) { return Err(bad()); }
            let (db, _) = self.read_view(Some(&proof.witness_lease)).map_err(|_| bad())?;
            if !same_identity(&request.source.identity, &sync_selection::identity(db)?) { return Err(bad()); }
            if let Some(hash) = &proof.head.manifest_hash { hashes.insert(hash.clone()); }
        }
        let cas = PayloadCas::new(&self.repository_root)?;
        let missing = hashes.into_iter().filter_map(|hash| match cas.stat_object(&hash) {
            Ok(None) => Some(Ok(hash)), Ok(Some(_)) => None, Err(error) => Some(Err(StoreError::from(error))),
        }).collect::<StoreResult<Vec<_>>>()?;
        if missing.is_empty() { return Ok(()); }
        cancelled(check)?;
        let probe = || if check() { Err(crate::server_sync::SyncError::new("cancelled", 499)) } else { Ok(()) };
        match crate::server_sync::residency::HydrationSession::new(&self.repository_root, cancellation)
            .and_then(|mut session| session.hydrate_many(&missing, &probe)) {
            Ok(missing) if missing.is_empty() => Ok(()),
            _ if check() => Err(conflict(CopyConflict::Cancelled)),
            _ => Err(bad()),
        }
    }
}
