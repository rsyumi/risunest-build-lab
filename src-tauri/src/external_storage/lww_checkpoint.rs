use super::{contract::*, lww_segment as segment};
use risunest_sync_wire::{stamp::DecimalU64, unit::{UnitKey, UnitValue}};
use crate::persistent_store::lww::Change;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{collections::{BTreeMap, BTreeSet}, path::Path};

pub(crate) type Coverage = BTreeMap<String, DecimalU64>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Checkpoint {
    pub schema: String,
    pub snapshot_id: String,
    pub repository_id: String,
    pub library_id: String,
    pub covered_prefixes: Coverage,
    pub state_identity: String,
    #[serde(with="nested_metadata")]
    pub library: risunest_external_storage_format::snapshot::LibrarySnapshotRef,
    #[serde(with="nested_metadata")]
    pub asset_catalogs: Vec<risunest_external_storage_format::snapshot::StoredObject>,
    pub standalone_bodies: BTreeMap<String, segment::LargeBody>,
}
pub(super) mod nested_metadata {
    use serde::{Serialize,Deserialize,Serializer,Deserializer,de::{DeserializeOwned,Error}};
    use serde_json::Value;
    pub fn serialize<T:Serialize,S:Serializer>(value:&T,serializer:S)->Result<S::Ok,S::Error> {
        fn convert(value:&mut Value)->Result<(),String> {
            match value {
                Value::Number(_)=>return Err("numeric control integer".into()),
                Value::Array(values)=>for value in values {
                    if let Value::Number(number)=value {
                        let byte=number.as_u64().filter(|byte|*byte<=u8::MAX as u64)
                            .ok_or("invalid control hash byte")?;
                        *value=Value::String(byte.to_string());
                    } else {convert(value)?;}
                },
                Value::Object(values)=>for value in values.values_mut() {convert(value)?;},
                _=>{},
            }
            Ok(())
        }
        let mut value=serde_json::to_value(value).map_err(<S::Error as serde::ser::Error>::custom)?;
        convert(&mut value).map_err(<S::Error as serde::ser::Error>::custom)?;
        value.serialize(serializer)
    }
    pub fn deserialize<'de,T:DeserializeOwned,D:Deserializer<'de>>(deserializer:D)->Result<T,D::Error> {
        fn integer(value:&mut Value)->Result<(),String> {
            let text=value.as_str().ok_or("control integer must be a decimal string")?;
            if text.is_empty() || (text.len()>1&&text.starts_with('0')) || !text.bytes().all(|byte|byte.is_ascii_digit()) {return Err("noncanonical control integer".into());}
            *value=Value::Number(text.parse::<u8>().map_err(|_|"control hash byte overflow")?.into());Ok(())
        }
        fn convert(value:&mut Value)->Result<(),String> {
            match value {
                Value::Number(_)=>return Err("numeric control integer".into()),
                Value::Array(values)=>for value in values {if value.is_string() {integer(value)?;} else {convert(value)?;}},
                Value::Object(values)=>for value in values.values_mut() {convert(value)?;},
                _=>{},
            }
            Ok(())
        }
        let mut value=Value::deserialize(deserializer)?;convert(&mut value).map_err(D::Error::custom)?;
        serde_json::from_value(value).map_err(D::Error::custom)
    }
}impl Checkpoint {
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        if self.schema != "risunest.lww-snapshot/v1" || self.library_id.is_empty() {
            return Err(segment::corrupt());
        }
        risunest_sync_wire::validate_id(&self.snapshot_id).map_err(|_| segment::corrupt())?;
        risunest_sync_wire::validate_hash(&self.state_identity).map_err(|_| segment::corrupt())?;
        for writer in self.covered_prefixes.keys() {
            risunest_sync_wire::stamp::validate_writer_id(writer).map_err(|_| segment::corrupt())?;
        }
        self.library.validate(&self.repository_id).map_err(|_| segment::corrupt())?;
        for (hash,body) in &self.standalone_bodies {
            risunest_sync_wire::validate_hash(hash).map_err(sql)?;
            risunest_sync_wire::validate_hash(&body.sha256).map_err(sql)?;
            let id=uuid::Uuid::parse_str(&body.object_id).map_err(sql)?;
            if id.to_string()!=body.object_id || body.locator.as_ref().is_none_or(|locator|locator.object.is_empty())
                || risunest_external_storage_format::crypto::ciphertext_length(body.plaintext_byte_length.0).map_err(sql)?!=body.byte_length.0 {return Err(segment::corrupt());}
        }        for catalog in &self.asset_catalogs { catalog.validate().map_err(sql)?; if catalog.header.repository_id != self.repository_id { return Err(segment::corrupt()); } }
        let bytes=risunest_sync_wire::canonical::encode(self).map_err(|_|segment::corrupt())?;
        if bytes.len()>risunest_external_storage_format::snapshot::MAX_METADATA_BYTES {return Err(segment::corrupt());}
        Ok(bytes)
    }
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len()>risunest_external_storage_format::snapshot::MAX_METADATA_BYTES {return Err(segment::corrupt());}
        let value: Self = risunest_sync_wire::canonical::decode(bytes,bytes.len()).map_err(|_|segment::corrupt())?;
        if value.encode()? != bytes { return Err(segment::corrupt()); }
        Ok(value)
    }
}

pub(crate) fn covers(left: &Coverage, right: &Coverage) -> bool {
    right.iter().all(|(writer, prefix)| left.get(writer).copied().unwrap_or(DecimalU64(0)) >= *prefix)
}
pub(crate) fn retained(checkpoints: &[Checkpoint]) -> Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    for candidate in checkpoints {
        let mut dominated = false;
        for other in checkpoints {
            if candidate.snapshot_id == other.snapshot_id { continue; }
            if !covers(&other.covered_prefixes, &candidate.covered_prefixes) { continue; }
            if covers(&candidate.covered_prefixes, &other.covered_prefixes) {
                if candidate.state_identity != other.state_identity { return Err(segment::corrupt()); }
                if other.snapshot_id > candidate.snapshot_id { continue; }
            }
            dominated = true;
        }
        if !dominated { result.insert(candidate.snapshot_id.clone()); }
    }
    Ok(result)
}

pub(crate) struct PublishedCatalog {
    db: Connection,
    pub coverage: Coverage,
    pub visited: u64,
}
fn sql(error: impl std::fmt::Display) -> ProviderError { segment::corrupt().caused(&error) }
impl PublishedCatalog {
    pub(crate) fn create(path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(sql)?;
        file.sync_all().map_err(sql)?;
        let db = Connection::open(path).map_err(sql)?;
        // A catalog is scratch rebuilt from the remote state and never reopened, so its writes skip durability.
        db.execute_batch("PRAGMA journal_mode=MEMORY; PRAGMA synchronous=OFF;
            CREATE TABLE units(key TEXT PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE versions(key TEXT NOT NULL,stamp TEXT NOT NULL,identity TEXT NOT NULL,PRIMARY KEY(key,stamp));
            CREATE TABLE retirements(key TEXT PRIMARY KEY,body TEXT NOT NULL);").map_err(sql)?;
        Ok(Self { db, coverage: Coverage::new(), visited: 0 })
    }
    pub(crate) fn merge(&mut self, change: &Change) -> Result<()> {
        self.visited = self.visited.checked_add(1).ok_or_else(segment::corrupt)?;
        change.stamp.validate().map_err(sql)?;
        let identity = change.value.identity();
        #[cfg(test)]
        super::lww_segment::observe_value_identity("native_checkpoint_descriptor_identity","native_checkpoint_value_identity",&change.value,identity.is_ok());
        let identity = identity.map_err(sql)?;
        let stamp = serde_json::to_string(&change.stamp).map_err(sql)?;
        let old: Option<String> = self.db.query_row("SELECT identity FROM versions WHERE key=?1 AND stamp=?2", params![change.key.as_str(), stamp], |r| r.get(0)).optional().map_err(sql)?;
        if old.as_ref().is_some_and(|old| old != &identity) { return Err(segment::corrupt()); }
        self.db.execute("INSERT OR IGNORE INTO versions VALUES(?1,?2,?3)",params![change.key.as_str(), stamp, identity]).map_err(sql)?;
        let previous: Option<String> = self.db.query_row("SELECT body FROM units WHERE key=?1", [change.key.as_str()], |r| r.get(0)).optional().map_err(sql)?;
        let mut winner = BTreeMap::new();
        if let Some(previous) = previous { winner.insert(change.key.as_str().into(), serde_json::from_str::<Change>(&previous).map_err(sql)?); }
        segment::merge(&mut winner, change.clone())?;
        self.db.execute("INSERT INTO units VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET body=excluded.body", params![change.key.as_str(), serde_json::to_string(winner.get(change.key.as_str()).ok_or_else(segment::corrupt)?).map_err(sql)?]).map_err(sql)?;
        if change.key.components()[0] == "exists" && matches!(change.value, UnitValue::Deleted) {
            let old:Option<String>=self.db.query_row("SELECT body FROM retirements WHERE key=?1",[change.key.as_str()],|row|row.get(0)).optional().map_err(sql)?;
            let mut deletion=BTreeMap::new();
            if let Some(old)=old {deletion.insert(change.key.as_str().into(),serde_json::from_str::<Change>(&old).map_err(sql)?);}
            segment::merge(&mut deletion,change.clone())?;
            self.db.execute("INSERT INTO retirements VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET body=excluded.body",params![change.key.as_str(),serde_json::to_string(deletion.get(change.key.as_str()).ok_or_else(segment::corrupt)?).map_err(sql)?]).map_err(sql)?;
        }
        Ok(())
    }
    pub(crate) fn changes(&self) -> Result<Vec<Change>> {
        let mut changes = Vec::new();
        self.visit_changes(&mut |change| {changes.push(change);Ok(())})?;
        Ok(changes)
    }
    pub(crate) fn visit_changes(&self,visitor:&mut dyn FnMut(Change)->Result<()>)->Result<()> {
        let mut query = self.db.prepare("SELECT units.key,COALESCE(retirements.body,units.body),retirements.key IS NOT NULL FROM units LEFT JOIN retirements ON retirements.key=units.key ORDER BY units.key").map_err(sql)?;
        let rows = query.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,bool>(2)?))).map_err(sql)?;
        for row in rows {
            let (key,body,retired) = row.map_err(sql)?;
            let change: Change = serde_json::from_str(&body).map_err(sql)?;
            if change.key.as_str()!=key {return Err(segment::corrupt());}
            if !retired {
                let mut suppressed=false;
                for parent in parents(&change.key)? {
                    if self.db.query_row("SELECT EXISTS(SELECT 1 FROM retirements WHERE key=?1)",[parent.as_str()],|row|row.get::<_,bool>(0)).map_err(sql)? {suppressed=true;break;}
                }
                if suppressed {continue;}
            }
            visitor(change)?;
        }
        Ok(())
    }
    pub(crate) fn identity(&self) -> Result<String> {
        let mut digest = sha2::Sha256::new();
        use sha2::Digest;
        #[cfg(test)] crate::persistent_store::hash_work::begin("checkpoint-state.identity");
        let visited=self.visit_changes(&mut |change| {
            let bytes = risunest_sync_wire::canonical::encode(&serde_json::to_value(change).map_err(sql)?).map_err(sql)?;
            digest.update((bytes.len() as u64).to_le_bytes()); digest.update(&bytes);
            #[cfg(test)] crate::persistent_store::hash_work::update("checkpoint-state.identity",8+bytes.len());
            Ok(())
        });
        if let Err(error)=visited {
            #[cfg(test)] crate::persistent_store::hash_work::incomplete("checkpoint-state.identity");
            return Err(error);
        }
        Ok(hex::encode(digest.finalize()))
    }
    pub(crate) fn include_prefixes(&mut self, coverage: &Coverage) {
        for (writer,prefix) in coverage { let current=self.coverage.entry(writer.clone()).or_insert(DecimalU64(0)); *current=(*current).max(*prefix); }
    }
    pub(crate) fn include_segment(&mut self, segment: &segment::Segment) -> Result<bool> {
        let prefix=self.coverage.get(&segment.writer_id).copied().unwrap_or(DecimalU64(0));
        if segment.seq <= prefix { return Ok(false); }
        if prefix.0.checked_add(1) != Some(segment.seq.0) { return Ok(false); }
        for change in &segment.changes { self.merge(change)?; }
        self.coverage.insert(segment.writer_id.clone(),segment.seq);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn change(key:&[&str],clock:u64,value:UnitValue)->Change {Change{key:UnitKey::new(key).unwrap(),stamp:risunest_sync_wire::stamp::Stamp{physical_ms:DecimalU64(clock),logical:0,writer_id:"00000000-0000-4000-8000-000000000001".into()},value}}
    #[test]
    fn permanent_retirement_suppresses_newer_existence_and_children_in_either_order() {
        let deletion=change(&["exists","character","retired"],1,UnitValue::Deleted);
        let existence=change(&["exists","character","retired"],3,UnitValue::inline(b"true").unwrap());
        let child=change(&["character","retired","name"],4,UnitValue::inline(b"\"stale\"").unwrap());
        for input in [vec![deletion.clone(),existence.clone(),child.clone()],vec![child.clone(),existence.clone(),deletion.clone()]] {
            let directory=tempfile::tempdir().unwrap(); let mut catalog=PublishedCatalog::create(&directory.path().join("units.sqlite")).unwrap();
            for change in input {catalog.merge(&change).unwrap();}
            let changes=catalog.changes().unwrap(); assert_eq!(changes.len(),1); assert!(matches!(changes[0].value,UnitValue::Deleted)); assert_eq!(changes[0].stamp,deletion.stamp);
        }
    }
    #[test]
    fn two_deletion_stamps_have_the_same_retired_state_in_every_order() {
        let older=change(&["exists","character","c"],1,UnitValue::Deleted);
        let newer=change(&["exists","character","c"],2,UnitValue::Deleted);
        let revived=change(&["exists","character","c"],3,UnitValue::inline(b"true").unwrap());
        let inputs=[older.clone(),newer.clone(),revived.clone()]; let mut identities=BTreeSet::new();
        for indices in [[0,1,2],[0,2,1],[1,0,2],[1,2,0],[2,0,1],[2,1,0]] {
            let root=tempfile::tempdir().unwrap();let mut catalog=PublishedCatalog::create(&root.path().join("units")).unwrap();
            for index in indices {catalog.merge(&inputs[index]).unwrap();}
            assert_eq!(catalog.changes().unwrap()[0].stamp,newer.stamp);
            identities.insert(catalog.identity().unwrap());
        }
        assert_eq!(identities.len(),1);
    }
    #[test]
    fn historical_equal_stamp_conflict_is_detected_even_after_a_greater_winner() {
        let directory=tempfile::tempdir().unwrap(); let mut catalog=PublishedCatalog::create(&directory.path().join("units.sqlite")).unwrap();
        catalog.merge(&change(&["root","language"],1,UnitValue::inline(b"\"ko\"").unwrap())).unwrap();
        catalog.merge(&change(&["root","language"],2,UnitValue::inline(b"\"en\"").unwrap())).unwrap();
        assert!(catalog.merge(&change(&["root","language"],1,UnitValue::inline(b"\"de\"").unwrap())).is_err());
    }
    #[test]
    fn listing_gap_does_not_advance_the_prefix_and_opaque_values_survive() {
        let directory=tempfile::tempdir().unwrap(); let mut catalog=PublishedCatalog::create(&directory.path().join("units.sqlite")).unwrap();
        let mut segment=segment::Segment::new("library","00000000-0000-4000-8000-000000000001",2);
        segment.changes.push(change(&["future-extension","opaque"],1,UnitValue::inline(b"null").unwrap()));
        assert!(!catalog.include_segment(&segment).unwrap()); assert!(catalog.coverage.is_empty());
        segment.seq=DecimalU64(1); assert!(catalog.include_segment(&segment).unwrap());
        assert_eq!(catalog.changes().unwrap()[0].key,segment.changes[0].key); assert!(!catalog.include_segment(&segment).unwrap());
    }
}
fn parents(key: &UnitKey) -> Result<Vec<UnitKey>> {
    let p=key.components();
    let parts: Vec<Vec<&str>> = match p[0].as_str() {
        "character" | "group-members" | "archive" if p.len()>=2 => vec![vec!["exists","character",&p[1]]],
        "conversation" | "messages" if p.len()>=3 => vec![vec!["exists","character",&p[1]],vec!["exists","conversation",&p[1],&p[2]]],
        "exists" if p.len()>=3 && p[1]=="conversation" => vec![vec!["exists","character",&p[2]]],
        "preset" | "persona" if p.len()>=2 => vec![vec!["exists",&p[0],&p[1]]],
        "record" if p.len()>=3 && p[1]!="plugins" => vec![vec!["exists",&p[1],&p[2]]],
        "order" if p.len()>=3 && p[1]=="conversations" => vec![vec!["exists","character",&p[2]]],
        _ => vec![],
    };
    parts.iter().map(|p| UnitKey::new(p).map_err(sql)).collect()
}
