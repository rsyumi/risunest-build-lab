use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::Path;

pub const TARGET_DATABASE_BYTES: u64 = 1_073_741_824;
pub const TARGET_ASSETS: u64 = 100_000;
pub const BODY_BYTES: usize = 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FixtureScale {
    pub minimum_database_bytes: u64,
    pub assets: u64,
    pub seed: u64,
    pub long_conversation_messages: u64,
}

impl FixtureScale {
    pub fn small() -> Self {
        Self { minimum_database_bytes: 262_144, assets: 64, seed: 1, long_conversation_messages: 4096 }
    }

    pub fn target() -> Self {
        Self { minimum_database_bytes: TARGET_DATABASE_BYTES, assets: TARGET_ASSETS, ..Self::small() }
    }

    pub fn above_target() -> Self {
        Self { minimum_database_bytes: TARGET_DATABASE_BYTES + TARGET_DATABASE_BYTES / 4, assets: TARGET_ASSETS + 1, ..Self::small() }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FixtureReceipt {
    pub schema: String,
    pub scale: FixtureScale,
    pub database_bytes: u64,
    pub database_sha256: String,
    pub characters: u64,
    pub messages: u64,
    pub catalog_bytes: u64,
    pub catalog_sha256: String,
    pub asset_bodies_materialized: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetDescriptor {
    pub index: u64,
    pub logical_key: String,
    pub payload_hash: String,
    pub byte_length: u64,
}

pub fn asset_body(seed: u64, index: u64) -> Vec<u8> {
    let mut output = Vec::with_capacity(BODY_BYTES);
    for block in 0..(BODY_BYTES / 32) {
        let mut hash = Sha256::new();
        hash.update(seed.to_le_bytes());
        hash.update(index.to_le_bytes());
        hash.update((block as u64).to_le_bytes());
        output.extend_from_slice(&hash.finalize());
    }
    output
}

pub fn asset_descriptor(seed: u64, index: u64) -> AssetDescriptor {
    AssetDescriptor {
        index,
        logical_key: format!("assets/synthetic-{index}.bin"),
        payload_hash: hex::encode(Sha256::digest(asset_body(seed, index))),
        byte_length: BODY_BYTES as u64,
    }
}

struct CountingWriter<W> {
    inner: W,
    bytes: u64,
    hash: Sha256,
}

impl<W: Write> CountingWriter<W> {
    fn new(inner: W) -> Self { Self { inner, bytes: 0, hash: Sha256::new() } }
    fn finish(mut self) -> io::Result<(u64, String)> {
        self.inner.flush()?;
        Ok((self.bytes, hex::encode(self.hash.finalize())))
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.bytes += count as u64;
        self.hash.update(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> { self.inner.flush() }
}

fn message(seed: u64, character: u64, index: u64) -> serde_json::Value {
    let text = format!("synthetic-{seed}-{character}-{index}:{}", "abcdefgh01234567".repeat(64));
    json!({"role":"user","data":text,"chatId":format!("message-{character}-{index}")})
}

fn write_character<W: Write>(writer: &mut W, seed: u64, index: u64, messages: u64) -> io::Result<()> {
    write!(writer, "{{\"chaId\":\"synthetic-character-{index}\",\"type\":\"character\",\"name\":\"Synthetic {index}\",\"chatPage\":0,\"chats\":[{{\"id\":\"synthetic-conversation-{index}\",\"name\":\"Synthetic\",\"message\":[")?;
    for m in 0..messages {
        if m != 0 { writer.write_all(b",")?; }
        serde_json::to_writer(&mut *writer, &message(seed, index, m))?;
    }
    writer.write_all(b"]}]}")
}

/// Writes only to a newly created directory. Memory use is independent of library size.
pub fn generate(directory: &Path, scale: FixtureScale, materialize_bodies: bool) -> io::Result<FixtureReceipt> {
    fs::create_dir(directory)?;
    let mut database = CountingWriter::new(BufWriter::new(File::create(directory.join("database.json"))?));
    database.write_all(b"{\"botPresets\":[{\"id\":\"synthetic-preset-0\",\"name\":\"Synthetic 0\"},{\"id\":\"synthetic-preset-1\",\"name\":\"Synthetic 1\"}],\"personas\":[{\"id\":\"synthetic-persona-0\",\"name\":\"Synthetic 0\"},{\"id\":\"synthetic-persona-1\",\"name\":\"Synthetic 1\"}],\"temperature\":1,\"characters\":[")?;
    write_character(&mut database, scale.seed, 0, scale.long_conversation_messages)?;
    let mut characters = 1;
    let mut messages = scale.long_conversation_messages;
    while database.bytes < scale.minimum_database_bytes {
        database.write_all(b",")?;
        write_character(&mut database, scale.seed, characters, 64)?;
        characters += 1;
        messages += 64;
    }
    database.write_all(b"]}")?;
    let (database_bytes, database_sha256) = database.finish()?;
    let mut catalog = CountingWriter::new(BufWriter::new(File::create(directory.join("assets.jsonl"))?));
    if materialize_bodies { fs::create_dir(directory.join("bodies"))?; }
    for index in 0..scale.assets {
        let descriptor = asset_descriptor(scale.seed, index);
        serde_json::to_writer(&mut catalog, &descriptor)?;
        catalog.write_all(b"\n")?;
        if materialize_bodies {
            fs::write(directory.join("bodies").join(&descriptor.payload_hash), asset_body(scale.seed, index))?;
        }
    }
    let (catalog_bytes, catalog_sha256) = catalog.finish()?;
    let receipt = FixtureReceipt {
        schema: "risunest.synthetic-lww-fixture/v1".into(), scale,
        database_bytes, database_sha256, characters, messages, catalog_bytes, catalog_sha256,
        asset_bodies_materialized: materialize_bodies,
    };
    serde_json::to_writer_pretty(File::create(directory.join("fixture.json"))?, &receipt)?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generation_is_deterministic_valid_and_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let scale = FixtureScale { minimum_database_bytes: 10_000, assets: 3, seed: 12, long_conversation_messages: 2 };
        let a = generate(&root.path().join("a"), scale.clone(), true).unwrap();
        let b = generate(&root.path().join("b"), scale.clone(), false).unwrap();
        assert_eq!(a.database_sha256, b.database_sha256);
        assert_eq!(a.catalog_sha256, b.catalog_sha256);
        assert!(a.database_bytes >= scale.minimum_database_bytes);
        let db: serde_json::Value = serde_json::from_reader(File::open(root.path().join("a/database.json")).unwrap()).unwrap();
        assert_eq!(db["characters"][0]["chats"][0]["message"].as_array().unwrap().len(), 2);
        for i in 0..3 {
            let d = asset_descriptor(12, i);
            let body = fs::read(root.path().join("a/bodies").join(d.payload_hash)).unwrap();
            assert_eq!(body, asset_body(12, i));
        }
        assert_eq!(generate(&root.path().join("a"), scale, false).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    }
    #[test]
    fn scales_are_targets_not_limits() {
        assert!(FixtureScale::above_target().assets > TARGET_ASSETS);
        assert!(FixtureScale::above_target().minimum_database_bytes > TARGET_DATABASE_BYTES);
        assert_ne!(asset_descriptor(1, TARGET_ASSETS).payload_hash, asset_descriptor(1, TARGET_ASSETS + 1).payload_hash);
    }
}
