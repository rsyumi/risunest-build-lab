use super::{plugin_owner, StoreResult};
use rusqlite::{params, Connection, Transaction};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
use uuid::Uuid;

static PROCESS_ID: LazyLock<String> = LazyLock::new(|| Uuid::new_v4().to_string());

pub(super) fn capture(transaction: &Transaction<'_>, staging_id: &str) -> StoreResult<()> {
    close(transaction)?;
    let waiting: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM plugin_storage WHERE generation=?1
         AND owner=?2 AND import_batch_id=?1 AND assigned_at IS NULL)",
        params![staging_id, plugin_owner::UNOWNED_OWNER],
        |row| row.get(0),
    )?;
    if !waiting {
        return Ok(());
    }
    let root: String = transaction.query_row(
        "SELECT value FROM root WHERE generation=?1",
        [staging_id],
        |row| row.get(0),
    )?;
    let root: Value = serde_json::from_str(&root)?;
    let Some(plugins) = root.get("plugins").and_then(Value::as_array) else {
        return Ok(());
    };
    for plugin in plugins {
        if plugin.get("enabled").and_then(Value::as_bool) != Some(true)
            || plugin.get("version").and_then(Value::as_str) != Some("3.0")
        {
            continue;
        }
        let (Some(owner), Some(script)) = (
            plugin.get("name").and_then(Value::as_str),
            plugin.get("script").and_then(Value::as_str),
        ) else {
            continue;
        };
        if !plugin_owner::validate_owner(owner) || plugin_owner::is_unowned(owner) {
            continue;
        }
        let code_hash = hex::encode(Sha256::digest(script.as_bytes()));
        transaction.execute(
            "INSERT OR IGNORE INTO plugin_claim_eligibility
             (import_batch_id,owner,code_hash,process_id) VALUES (?1,?2,?3,?4)",
            params![staging_id, owner, code_hash, PROCESS_ID.as_str()],
        )?;
    }
    Ok(())
}

pub(super) fn consume(
    connection: &Connection,
    batch: &str,
    owner: &str,
    hash: &str,
) -> StoreResult<bool> {
    Ok(connection.execute(
        "DELETE FROM plugin_claim_eligibility WHERE
         import_batch_id=?1 AND owner=?2 AND code_hash=?3 AND process_id=?4",
        params![batch, owner, hash, PROCESS_ID.as_str()],
    )? == 1)
}

pub(super) fn close(connection: &Connection) -> StoreResult<()> {
    connection.execute("DELETE FROM plugin_claim_eligibility", [])?;
    Ok(())
}
