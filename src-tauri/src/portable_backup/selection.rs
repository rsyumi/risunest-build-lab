//! What an archive holds, grouped the way the import preview lists it.

use super::*;
use rusqlite::Connection;
use std::collections::BTreeMap;

/// One record an import can take or leave.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArchiveEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    /// Conversations under a character; zero for anything else.
    pub(crate) conversations: u64,
    /// Findings the diagnosis raised against this record.
    pub(crate) damaged: u64,
}

/// The records an archive holds, grouped the way the import screen offers them. The settings
/// record and the storage authorities are not here: an import always brings those.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArchiveInventory {
    pub(crate) characters: Vec<ArchiveEntry>,
    pub(crate) presets: Vec<ArchiveEntry>,
    pub(crate) plugins: Vec<ArchiveEntry>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginKey {
    pub(crate) owner: String,
    pub(crate) key: String,
}
impl PluginKey {
    pub(crate) fn identity(&self) -> String {
        serde_json::to_string(self).expect("plugin identity serialization")
    }
}
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArchiveExclusions {
    pub(crate) characters: Vec<String>,
    pub(crate) presets: Vec<String>,
    pub(crate) plugins: Vec<PluginKey>,
}

/// What the reader chose. An empty selection of a kind means none of that kind, so a partial
/// import must name what it wants rather than relying on a default.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArchiveSelection {
    pub(crate) characters: Vec<String>,
    pub(crate) presets: Vec<String>,
    pub(crate) plugins: Vec<PluginKey>,
    /// Records the reader left out on purpose even though a chosen record refers to them. Their
    /// references come in broken, which the preview says before anything is staged.
    pub(crate) excluded: ArchiveExclusions,
}

fn counted(db: &Connection, sql: &str) -> Result<Vec<ArchiveEntry>> {
    let mut statement = db.prepare(sql)?;
    let mut rows = statement.query([])?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next()? {
        entries.push(ArchiveEntry {
            id: row.get(0)?,
            conversations: sql_u64(row.get(1)?)?,
            name: row.get(2)?,
            damaged: 0,
        });
    }
    Ok(entries)
}

/// Reads what an archive holds. `damaged` counts the findings a diagnosis raised against each
/// record, so the screen can mark the ones that are already broken.
pub(crate) fn inventory(
    db: &Connection,
    findings: &[crate::data_health::Finding],
) -> Result<ArchiveInventory> {
    let mut inventory = ArchiveInventory {
        characters: counted(
            db,
            "SELECT character_id,conversation_count,name FROM characters ORDER BY configured_index",
        )?,
        presets: counted(db, "SELECT preset_id,0,name FROM bot_presets ORDER BY configured_index")?,
        plugins: {
            let mut statement = db.prepare("SELECT owner,storage_key FROM plugin_storage ORDER BY ordinal")?;
            let rows = statement.query_map([], |row| Ok(PluginKey { owner: row.get(0)?, key: row.get(1)? }))?;
            rows.map(|row| row.map(|key| ArchiveEntry { id: key.identity(), name: format!("{} / {}", key.owner, key.key), ..ArchiveEntry::default() })).collect::<std::result::Result<Vec<_>, _>>()?
        },
    };
    let mut damage: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    for finding in findings {
        // A conversation or message names its character first, which is the record the reader
        // chooses, so the damage is counted there.
        let owner = if matches!(finding.owner.kind.as_str(), "conversation" | "message") {
            finding.owner.id.split('/').next().unwrap_or_default()
        } else { finding.owner.id.as_str() };
        let kind = match finding.owner.kind.as_str() {
            "conversation" | "message" => "character",
            kind => kind,
        };
        *damage.entry((kind, owner)).or_default() += 1;
    }
    for (kind, entries) in [
        ("character", &mut inventory.characters),
        ("preset", &mut inventory.presets),
        ("plugin", &mut inventory.plugins),
    ] {
        for entry in entries.iter_mut() {
            entry.damaged = damage
                .get(&(kind, entry.id.as_str()))
                .copied()
                .unwrap_or_default();
        }
    }
    Ok(inventory)
}

#[cfg(test)]
mod tests;
