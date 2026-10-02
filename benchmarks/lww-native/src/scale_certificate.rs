use super::fixture::{TARGET_ASSETS, TARGET_DATABASE_BYTES};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag="tier", rename_all="kebab-case")]
pub enum ScaleRequirement {
    Target,
    AboveTarget,
    Correctness { minimum_database_bytes:u64, minimum_assets:u64 },
}

impl ScaleRequirement {
    pub fn minimums(self) -> (u64,u64) {
        match self {
            Self::Target => (TARGET_DATABASE_BYTES,TARGET_ASSETS),
            Self::AboveTarget => (TARGET_DATABASE_BYTES + TARGET_DATABASE_BYTES/4,TARGET_ASSETS+1),
            Self::Correctness {minimum_database_bytes,minimum_assets} => (minimum_database_bytes,minimum_assets),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DatabaseAllocation {
    pub page_size:u64,
    pub page_count:u64,
    pub freelist_count:u64,
    pub file_bytes:u64,
    pub wal_bytes:u64,
    pub regular_file:bool,
    pub sparse_file:bool,
    pub reparse_point:bool,
    pub checkpoint_complete:bool,
    pub reopened:bool,
    pub integrity_checked:bool,
    pub native_schema_only:bool,
    pub active_characters:u64,
    pub active_conversations:u64,
    pub active_messages:u64,
    pub active_record_bytes:u64,
    pub maximum_message_bytes:u64,
    pub maximum_character_detail_bytes:u64,
    pub maximum_root_bytes:u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CertifiedAsset {
    pub payload_hash:String,
    pub catalog_bytes:u64,
    pub file_bytes:u64,
    pub regular_file:bool,
    pub sparse_file:bool,
    pub reparse_point:bool,
    pub verified_file_hash:String,
    pub active_aliases:u64,
    pub verified_owner_references:u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScaleCertificate {
    pub schema:String,
    pub requirement:ScaleRequirement,
    pub database:DatabaseAllocation,
    pub catalog_rows:u64,
    pub verified_owner_heads:u64,
    pub measured_conversation_sha256:String,
    pub assets:Vec<CertifiedAsset>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CertifiedScale {
    pub requirement:ScaleRequirement,
    pub database_allocated_bytes:u64,
    pub database_active_page_bytes:u64,
    pub unique_materialized_owned_assets:u64,
    pub physical_asset_bytes:u64,
}

impl ScaleCertificate {
    pub fn validate(&self) -> Result<CertifiedScale,String> {
        if self.schema != "risunest.native-persisted-scale/v1" || self.measured_conversation_sha256.len()!=64
            || !self.measured_conversation_sha256.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err("scale certificate must describe observed native persistence".into());
        }
        let db = &self.database;
        let (minimum_bytes,minimum_assets) = self.requirement.minimums();
        if !db.regular_file || db.sparse_file || db.reparse_point || !db.checkpoint_complete || !db.reopened
            || !db.integrity_checked || !db.native_schema_only || db.wal_bytes != 0 {
            return Err("unverified, indirect, sparse or uncheckpointed native database".into());
        }
        if !db.page_size.is_power_of_two() || !(512..=65536).contains(&db.page_size)
            || db.freelist_count > db.page_count {
            return Err("invalid SQLite page allocation".into());
        }
        let allocated = db.page_count.checked_mul(db.page_size).ok_or("database byte overflow")?;
        let active = (db.page_count-db.freelist_count).checked_mul(db.page_size).ok_or("active byte overflow")?;
        if db.file_bytes != allocated || allocated < minimum_bytes || active < minimum_bytes {
            return Err("actual checkpointed live SQLite allocation is below target".into());
        }
        if db.active_characters < 2 || db.active_conversations < 2 || db.active_messages == 0
            || db.active_record_bytes == 0 || db.maximum_message_bytes == 0
            || db.maximum_message_bytes > 16384 || db.maximum_character_detail_bytes > 1024*1024
            || db.maximum_root_bytes > 1024*1024 {
            return Err("fixture lacks bounded meaningful native shards".into());
        }
        if !matches!(self.requirement,ScaleRequirement::Correctness {..}) && db.active_record_bytes<minimum_bytes {
            return Err("final fixture lacks target-sized actual active native record content".into());
        }
        if self.verified_owner_heads == 0 || self.catalog_rows < self.assets.len() as u64 {
            return Err("asset catalog/owner evidence is missing".into());
        }
        let mut unique = BTreeSet::new();
        let mut physical_bytes = 0u64;
        for asset in &self.assets {
            if asset.payload_hash.len()!=64 || !asset.payload_hash.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !unique.insert(&asset.payload_hash) {
                return Err("asset hashes must be unique canonical identities".into());
            }
            if !asset.regular_file || asset.sparse_file || asset.reparse_point || asset.catalog_bytes==0
                || asset.file_bytes!=asset.catalog_bytes || asset.verified_file_hash!=asset.payload_hash
                || asset.active_aliases==0 || asset.verified_owner_references==0 {
                return Err(format!("asset is not materialized, cataloged and owner-verified: {}",asset.payload_hash));
            }
            physical_bytes = physical_bytes.checked_add(asset.file_bytes).ok_or("asset byte overflow")?;
        }
        if (unique.len() as u64) < minimum_assets { return Err("unique materialized owned asset count is below target".into()); }
        Ok(CertifiedScale {requirement:self.requirement, database_allocated_bytes:allocated,
            database_active_page_bytes:active,unique_materialized_owned_assets:unique.len() as u64,
            physical_asset_bytes:physical_bytes})
    }
}

pub fn validate_above_target(target:&ScaleCertificate, above:&ScaleCertificate) -> Result<(),String> {
    if target.requirement!=ScaleRequirement::Target || above.requirement!=ScaleRequirement::AboveTarget {
        return Err("final scale comparison needs target and above-target native certificates".into());
    }
    let base=target.validate()?;
    let larger=above.validate()?;
    if larger.database_active_page_bytes<=base.database_active_page_bytes
        || larger.unique_materialized_owned_assets<=base.unique_materialized_owned_assets
        || above.database.active_characters<=target.database.active_characters
        || above.database.active_conversations<=target.database.active_conversations
        || above.database.active_messages<=target.database.active_messages
        || above.database.active_record_bytes<=target.database.active_record_bytes
        || above.measured_conversation_sha256!=target.measured_conversation_sha256 {
        return Err("above fixture must grow actual unrelated native data and preserve measured content".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tiny() -> ScaleCertificate {
        let hash = "ab".repeat(32);
        ScaleCertificate {schema:"risunest.native-persisted-scale/v1".into(),
            requirement:ScaleRequirement::Correctness {minimum_database_bytes:4096,minimum_assets:1},
            database:DatabaseAllocation {page_size:4096,page_count:2,freelist_count:0,file_bytes:8192,
                wal_bytes:0,regular_file:true,sparse_file:false,reparse_point:false,checkpoint_complete:true,reopened:true,
                integrity_checked:true,native_schema_only:true,active_characters:2,active_conversations:2,
                active_messages:2,active_record_bytes:200,maximum_message_bytes:100,maximum_character_detail_bytes:100,maximum_root_bytes:100},
            catalog_rows:1,verified_owner_heads:1,measured_conversation_sha256:"ef".repeat(32),assets:vec![CertifiedAsset {payload_hash:hash.clone(),
                catalog_bytes:1024,file_bytes:1024,regular_file:true,sparse_file:false,reparse_point:false,
                verified_file_hash:hash,active_aliases:1,verified_owner_references:1}]}
    }
    #[test]
    fn only_live_native_allocation_and_physical_owned_payloads_pass() {
        let certificate = tiny();
        assert!(certificate.validate().is_ok());
        let mutations:[fn(&mut ScaleCertificate);9] = [|c|c.database.freelist_count=2,
            |c:&mut ScaleCertificate|c.database.reopened=false,
            |c:&mut ScaleCertificate|c.database.native_schema_only=false,
            |c:&mut ScaleCertificate|c.database.file_bytes=1_073_741_824,
            |c:&mut ScaleCertificate|c.database.maximum_character_detail_bytes=1_073_741_824,
            |c:&mut ScaleCertificate|c.assets[0].regular_file=false,
            |c:&mut ScaleCertificate|c.assets[0].sparse_file=true,
            |c:&mut ScaleCertificate|c.assets[0].verified_owner_references=0,
            |c:&mut ScaleCertificate|c.assets[0].verified_file_hash="cd".repeat(32)];
        for mutate in mutations {
            let mut invalid = certificate.clone(); mutate(&mut invalid); assert!(invalid.validate().is_err());
        }
    }
    #[test]
    fn target_and_above_counts_cannot_be_replaced_by_catalog_claims_or_duplicate_rows() {
        let mut certificate = tiny();
        certificate.requirement=ScaleRequirement::Target;
        certificate.catalog_rows=100000;
        assert!(certificate.validate().is_err());
        certificate.database.page_count=TARGET_DATABASE_BYTES/certificate.database.page_size;
        certificate.database.file_bytes=TARGET_DATABASE_BYTES;
        assert_eq!(certificate.validate().unwrap_err(),"final fixture lacks target-sized actual active native record content");
        certificate=tiny(); certificate.assets.push(certificate.assets[0].clone()); certificate.catalog_rows=2;
        assert!(certificate.validate().is_err());
        assert_eq!(ScaleRequirement::AboveTarget.minimums(),(1_342_177_280,100001));
    }
}
