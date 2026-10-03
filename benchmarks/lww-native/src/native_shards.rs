use super::fixture::{asset_descriptor, AssetDescriptor};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const FIRST_CONVERSATION_MESSAGES: u64 = 4096;
pub const MAX_SHARD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShardPlan {
    pub seed: u64,
    pub assets: u64,
    pub ordinary_messages: u16,
    pub assets_per_owner: u16,
}

impl ShardPlan {
    pub fn new(seed:u64, assets:u64) -> Self {
        Self {seed, assets, ordinary_messages:256, assets_per_owner:64}
    }
    fn validate(&self) -> Result<(), String> {
        if !(1..=512).contains(&self.ordinary_messages) || !(1..=256).contains(&self.assets_per_owner) {
            return Err("shard plan exceeds bounded message/owner batch size".into());
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnerEntryInput {
    pub tuple: [String;3],
    pub payload_hash: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct NativeShard {
    pub index: u64,
    pub character: Value,
    pub assets: Vec<AssetDescriptor>,
    pub owner_entries: Vec<OwnerEntryInput>,
    pub serialized_character_bytes: u64,
    pub character_sha256: String,
}

pub struct NativeShards {
    plan: ShardPlan,
    next_character: u64,
    next_asset: u64,
    exhausted: bool,
}

impl NativeShards {
    pub fn new(plan:ShardPlan) -> Result<Self, String> {
        plan.validate()?;
        Ok(Self {plan, next_character:0, next_asset:0, exhausted:false})
    }
    pub fn assigned_assets(&self) -> u64 { self.next_asset }
    pub fn generated_characters(&self) -> u64 { self.next_character }
}

impl Iterator for NativeShards {
    type Item = Result<NativeShard, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.exhausted { return None; }
        let index = self.next_character;
        let Some(next_character) = index.checked_add(1) else {
            self.exhausted = true;
            return Some(Err("character ordinal overflow".into()));
        };
        let count = if index == 0 { FIRST_CONVERSATION_MESSAGES } else { u64::from(self.plan.ordinary_messages) };
        let messages = (0..count).map(|message| json!({"role":"user",
            "data":format!("synthetic-{}-{index}-{message}:{}", self.plan.seed, "abcdefgh01234567".repeat(64)),
            "chatId":format!("message-{index}-{message}")})).collect::<Vec<_>>();
        let remaining = self.plan.assets - self.next_asset;
        let owned = remaining.min(u64::from(self.plan.assets_per_owner));
        let assets = (self.next_asset..self.next_asset + owned)
            .map(|asset| asset_descriptor(self.plan.seed, asset)).collect::<Vec<_>>();
        let owner_entries = assets.iter().map(|asset| OwnerEntryInput {
            tuple:[format!("Synthetic asset {}", asset.index), asset.logical_key.clone(), "bin".into()],
            payload_hash:asset.payload_hash.clone(),
        }).collect::<Vec<_>>();
        let character = json!({"chaId":format!("synthetic-character-{index}"), "type":"character",
            "name":format!("Synthetic {index}"), "chatPage":0,
            "notes":"", "bias":[], "emotionImages":[], "globalLore":[],
            "sdData":[["always","solo, 1girl"],["negative",""],["|character's appearance",""],
                ["current situation",""],["$character's pose",""],["$character's emotion",""],["current location",""]],
            "utilityBot":false, "exampleMessage":"", "creatorNotes":"", "systemPrompt":"",
            "postHistoryInstructions":"", "alternateGreetings":[], "tags":[], "creator":"",
            "characterVersion":"", "personality":"", "scenario":"", "firstMsgIndex":-1,
            "replaceGlobalNote":"", "additionalText":"",
            "additionalAssets":owner_entries.iter().map(|entry| entry.tuple.clone()).collect::<Vec<_>>(),
            "chats":[{"id":format!("synthetic-conversation-{index}"), "name":"Synthetic",
                "note":"", "localLore":[], "message":messages}]});
        let bytes = serde_json::to_vec(&character).expect("synthetic JSON is serializable");
        if bytes.len() > MAX_SHARD_BYTES {
            self.exhausted = true;
            return Some(Err("generated character exceeds bounded shard bytes".into()));
        }
        self.next_character = next_character;
        self.next_asset += owned;
        Some(Ok(NativeShard {index, character, assets, owner_entries,
            serialized_character_bytes:bytes.len() as u64,
            character_sha256:hex::encode(Sha256::digest(&bytes))}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shards_are_lazy_bounded_deterministic_and_continue_after_asset_assignment() {
        let plan = ShardPlan {ordinary_messages:2, assets_per_owner:2, ..ShardPlan::new(7,3)};
        let mut a = NativeShards::new(plan.clone()).unwrap();
        let mut b = NativeShards::new(plan).unwrap();
        assert_eq!(a.generated_characters(),0);
        for index in 0..3 {
            let left = a.next().unwrap().unwrap();
            assert_eq!(left,b.next().unwrap().unwrap());
            assert_eq!(left.index,index);
            assert!(left.serialized_character_bytes <= MAX_SHARD_BYTES as u64);
            assert_eq!(left.assets.len(), [2,1,0][index as usize]);
            assert_eq!(left.owner_entries.len(),left.assets.len());
            for key in ["notes","exampleMessage","creatorNotes","systemPrompt","postHistoryInstructions",
                "creator","characterVersion","personality","scenario","replaceGlobalNote","additionalText"] {
                assert_eq!(left.character[key],json!(""));
            }
            for key in ["bias","emotionImages","globalLore","alternateGreetings","tags"] {
                assert_eq!(left.character[key],json!([]));
            }
            assert_eq!(left.character["sdData"],json!([["always","solo, 1girl"],["negative",""],
                ["|character's appearance",""],["current situation",""],["$character's pose",""],
                ["$character's emotion",""],["current location",""]]));
            assert_eq!(left.character["utilityBot"],json!(false));
            assert_eq!(left.character["firstMsgIndex"],json!(-1));
            assert_eq!(left.character["chats"][0]["note"],json!(""));
            assert_eq!(left.character["chats"][0]["localLore"],json!([]));
            let messages = left.character["chats"][0]["message"].as_array().unwrap();
            assert_eq!(messages.len(),if index==0 {4096} else {2});
            assert_eq!(messages.iter().map(|m|m["chatId"].as_str().unwrap())
                .collect::<std::collections::BTreeSet<_>>().len(),messages.len());
        }
        assert_eq!(a.assigned_assets(),3);
    }
    #[test]
    fn unbounded_plan_is_rejected_before_generation() {
        let mut plan = ShardPlan::new(1,100001);
        plan.ordinary_messages = 513;
        assert!(NativeShards::new(plan).is_err());
    }
}
