use super::{json, parse, random_id, Device, Store};
use crate::{Error, Result};
use risunest_sync_wire::{
    lww::{
        AckRequest, CancelOperationRequest, ChangesPage, JournalItem, OperationReceipt,
        PushReceipt, PushRequest, StatePage, StatePin, TimeSample, UnitChange,
    },
    stamp::{DecimalU64, MAX_CLOCK_SKEW_MS},
    unit::{compare_version, LwwDecision, UnitKey, UnitValue},
    validate_hash, validate_id, MAX_METADATA_BYTES,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn time_ms() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::new("clock-invalid", 503))?
        .as_millis()
        .try_into()
        .map_err(|_| Error::new("clock-overflow", 503))
}

pub(super) fn parents(key: &UnitKey) -> Result<Vec<UnitKey>> {
    let c = key.components();
    let mut result = Vec::new();
    match c[0].as_str() {
        "character" | "archive" => result.push(UnitKey::new(&["exists", "character", &c[1]])?),
        "preset" | "persona" => result.push(UnitKey::new(&["exists", &c[0], &c[1]])?),
        "conversation" | "messages" => {
            result.push(UnitKey::new(&["exists", "character", &c[1]])?);
            result.push(UnitKey::new(&["exists", "conversation", &c[1], &c[2]])?);
        }
        "exists" if c[1] == "conversation" => {
            result.push(UnitKey::new(&["exists", "character", &c[2]])?)
        }
        "record" if c[1] != "plugins" => result.push(UnitKey::new(&["exists", &c[1], &c[2]])?),
        "order" if c[1] == "conversations" => {
            result.push(UnitKey::new(&["exists", "character", &c[2]])?)
        }
        _ => {}
    }
    Ok(result)
}
fn is_retirement(change: &UnitChange) -> bool {
    change.key.components()[0] == "exists" && change.value == UnitValue::Deleted
}
fn read_unit(db: &Connection, key: &str) -> Result<Option<UnitChange>> {
    db.query_row("SELECT body FROM units WHERE key=?1", [key], |r| {
        r.get::<_, String>(0)
    })
    .optional()?
    .map(|body| parse(&body))
    .transpose()
}
fn operation(db: &Connection, device: &Device, id: &str) -> Result<Option<OperationReceipt>> {
    db.query_row(
        "SELECT body FROM operations WHERE device=?1 AND operation=?2",
        params![device.id, id],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(|body| parse(&body))
    .transpose()
}
/// A repeated operation answers exactly as it first did, rejection included.
fn replay(db: &Connection, device: &Device, id: &str, digest: &str) -> Result<Option<PushReceipt>> {
    let Some((body, status, key)) = db
        .query_row(
            "SELECT body,error_status,error_key FROM operations WHERE device=?1 AND operation=?2",
            params![device.id, id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<u16>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    let prior: OperationReceipt = parse(&body)?;
    if prior.body_digest() != digest {
        return Err(Error::new("operation-integrity", 409));
    }
    match prior {
        OperationReceipt::Accepted { receipt, .. } => Ok(Some(receipt)),
        OperationReceipt::Rejected { error, .. } => {
            let mut rejection = Error::new(
                rejection_code(&error),
                status.ok_or(Error::new("corrupt-metadata", 503))?,
            );
            rejection.key = key;
            Err(rejection)
        }
    }
}
/// Stored rejections carry codes the server itself produced, so this set stays small.
fn rejection_code(code: &str) -> &'static str {
    static CODES: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<&'static str>>> =
        std::sync::OnceLock::new();
    let mut codes = CODES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(code) = codes.get(code) {
        return code;
    }
    let code: &'static str = Box::leak(code.to_owned().into_boxed_str());
    codes.insert(code);
    code
}
fn save_operation(
    db: &Connection,
    device: &Device,
    id: &str,
    receipt: &OperationReceipt,
    rejection: Option<&Error>,
) -> Result<()> {
    db.execute(
        "INSERT INTO operations(device,operation,digest,body,error_status,error_key) VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            device.id,
            id,
            receipt.body_digest(),
            json(receipt)?,
            rejection.map(|error| error.status),
            rejection.and_then(|error| error.key.as_deref())
        ],
    )?;
    Ok(())
}
/// The body an `asset` or `inlay` unit names. Garbage collection keeps it while
/// the unit is retained, so a value that cannot be read is never accepted.
pub(super) fn alias_object(bytes: &str) -> Result<Option<String>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let invalid = || Error::new("invalid-alias", 400);
    let decoded = URL_SAFE_NO_PAD.decode(bytes).map_err(|_| invalid())?;
    let value: serde_json::Value = serde_json::from_slice(&decoded).map_err(|_| invalid())?;
    match value.as_object().ok_or_else(invalid)?.get("objectHash") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(hash)) => {
            validate_hash(hash).map_err(|_| invalid())?;
            Ok(Some(hash.clone()))
        }
        Some(_) => Err(invalid()),
    }
}
fn is_alias(key: &UnitKey) -> bool {
    matches!(key.components()[0].as_str(), "asset" | "inlay")
}

impl Store {
    pub fn time_sample(&self) -> Result<TimeSample> {
        Ok(TimeSample {
            server_time_ms: time_ms()?.into(),
            precision_ms: 1.into(),
        })
    }

    pub fn push(&self, device: &Device, request: &PushRequest) -> Result<PushReceipt> {
        self.push_at(device, request, time_ms)
    }
    pub(super) fn push_at(
        &self,
        device: &Device,
        request: &PushRequest,
        clock: impl FnOnce() -> Result<u64>,
    ) -> Result<PushReceipt> {
        validate_id(&request.operation_id)?;
        let digest = request.digest()?;
        let _gate = self
            .objects_gate
            .lock()
            .map_err(|_| Error::new("storage-unavailable", 503))?;
        // Admission time is read once the gate is held, so waiting behind
        // maintenance cannot judge stamps against a stale clock.
        let now = clock()?;
        {
            let db = self.reader()?;
            Self::require_device(&db, device)?;
            if request.library_id != Self::read_head(&db)?.library_id {
                return Err(Error::new("library-mismatch", 403));
            }
            if let Some(receipt) = replay(&db, device, &request.operation_id, &digest)? {
                return Ok(receipt);
            }
        }
        // Immutable descriptor indexes may be prepared before the state transaction.
        // The object gate keeps validation and reference publication together with GC.
        let preparation = request
            .validate()
            .map_err(Error::from)
            .and_then(|()| self.prepare_descriptors(&request.changes));
        let mut db = self.db()?;
        let tx = db.transaction()?;
        Self::require_device(&tx, device)?;
        let mut head = Self::read_head(&tx)?;
        if request.library_id != head.library_id {
            return Err(Error::new("library-mismatch", 403));
        }
        if let Some(receipt) = replay(&tx, device, &request.operation_id, &digest)? {
            return Ok(receipt);
        }
        let validation = (|| -> Result<Vec<UnitChange>> {
            preparation?;
            let claimed_writer: Option<String> = tx
                .query_row(
                    "SELECT writer FROM device_writer_claims WHERE device=?1",
                    [&device.id],
                    |row| row.get(0),
                )
                .optional()?;
            if claimed_writer
                .as_ref()
                .is_some_and(|writer| writer != &request.writer_id)
            {
                return Err(Error::new("writer-collision", 409));
            }
            let writer: Option<String> = tx
                .query_row(
                    "SELECT device FROM writers WHERE writer=?1",
                    [&request.writer_id],
                    |r| r.get(0),
                )
                .optional()?;
            if writer.as_ref().is_some_and(|owner| owner != &device.id) {
                return Err(Error::new("writer-collision", 409));
            }
            let retiring: std::collections::BTreeSet<&str> = request
                .changes
                .iter()
                .filter(|c| is_retirement(c))
                .map(|c| c.key.as_str())
                .collect();
            let mut winners = Vec::new();
            for change in &request.changes {
                if change.stamp.physical_ms.0
                    > now
                        .checked_add(MAX_CLOCK_SKEW_MS)
                        .ok_or(Error::new("clock-overflow", 503))?
                {
                    return Err(Error::new("clock-skew", 409).for_key(change.key.as_str()));
                }
                let identity = change.value.identity()?;
                if let UnitValue::Inline { bytes } = &change.value {
                    if is_alias(&change.key) {
                        alias_object(bytes).map_err(|error| error.for_key(change.key.as_str()))?;
                    }
                }
                let prior: Option<String> = tx.query_row("SELECT identity FROM writer_versions WHERE writer=?1 AND key=?2 AND physical=?3 AND logical=?4",
                    params![change.stamp.writer_id,change.key.as_str(),change.stamp.physical_ms.0.to_string(),change.stamp.logical.to_string()], |r| r.get(0)).optional()?;
                if prior.is_some_and(|old| old != identity) {
                    return Err(
                        Error::new("equal-stamp-integrity", 409).for_key(change.key.as_str())
                    );
                }
                let local = read_unit(&tx, change.key.as_str())?;
                let decision = match &local {
                    Some(local) => {
                        compare_version(&local.stamp, &local.value, &change.stamp, &change.value)?
                    }
                    None => LwwDecision::ApplyRemote,
                };
                let retired: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM retired WHERE key=?1)",
                    [change.key.as_str()],
                    |r| r.get(0),
                )?;
                let mut suppressed = retired && !is_retirement(change);
                for parent in parents(&change.key)? {
                    suppressed |= !is_retirement(change)
                        && (retiring.contains(parent.as_str())
                            || tx.query_row(
                                "SELECT EXISTS(SELECT 1 FROM retired WHERE key=?1)",
                                [parent.as_str()],
                                |r| r.get::<_, bool>(0),
                            )?);
                }
                if !suppressed
                    && (decision == LwwDecision::ApplyRemote || (is_retirement(change) && !retired))
                {
                    winners.push(change.clone());
                }
            }
            Ok(winners)
        })();
        let winners = match validation {
            Ok(winners) => winners,
            Err(error) if error.status < 500 => {
                save_operation(
                    &tx,
                    device,
                    &request.operation_id,
                    &OperationReceipt::Rejected {
                        operation_id: request.operation_id.clone(),
                        body_digest: digest,
                        error: error.code.into(),
                        server_time_ms: now.into(),
                    },
                    Some(&error),
                )?;
                tx.commit()?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        tx.execute(
            "INSERT OR IGNORE INTO writers VALUES(?1,?2)",
            params![request.writer_id, device.id],
        )?;
        for change in &request.changes {
            tx.execute(
                "INSERT OR IGNORE INTO writer_versions(writer,key,physical,logical,identity) VALUES(?1,?2,?3,?4,?5)",
                params![
                    change.stamp.writer_id,
                    change.key.as_str(),
                    change.stamp.physical_ms.0.to_string(),
                    change.stamp.logical.to_string(),
                    change.value.identity()?
                ],
            )?;
        }
        let mut seq: u64 = head
            .seq
            .as_str()
            .parse()
            .map_err(|_| Error::new("corrupt-metadata", 503))?;
        let mut accepted_keys = Vec::new();
        for change in winners {
            seq = seq
                .checked_add(1)
                .ok_or(Error::new("sequence-overflow", 503))?;
            let body = json(&change)?;
            if is_retirement(&change) {
                tx.execute("INSERT INTO retired VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET body=excluded.body", params![change.key.as_str(),body])?;
                tx.execute("DELETE FROM journal WHERE key IN (SELECT child FROM unit_parents WHERE parent=?1) AND key NOT IN (SELECT key FROM retired)", [change.key.as_str()])?;
                tx.execute("DELETE FROM units WHERE key IN (SELECT child FROM unit_parents WHERE parent=?1) AND key NOT IN (SELECT key FROM retired)", [change.key.as_str()])?;
            }
            tx.execute(
                "INSERT INTO units VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET body=excluded.body",
                params![change.key.as_str(), body],
            )?;
            tx.execute(
                "DELETE FROM unit_parents WHERE child=?1",
                [change.key.as_str()],
            )?;
            for parent in parents(&change.key)? {
                tx.execute(
                    "INSERT INTO unit_parents VALUES(?1,?2)",
                    params![parent.as_str(), change.key.as_str()],
                )?;
            }
            tx.execute("DELETE FROM journal WHERE key=?1", [change.key.as_str()])?;
            let item = JournalItem {
                seq: seq.into(),
                key: change.key.clone(),
                stamp: change.stamp,
                value: change.value,
            };
            tx.execute(
                "INSERT INTO journal(seq,key,body) VALUES(?1,?2,?3)",
                params![seq.to_string(), item.key.as_str(), json(&item)?],
            )?;
            accepted_keys.push(item.key);
        }
        if !accepted_keys.is_empty() {
            head.head_id =
                risunest_sync_wire::hash(format!("{}:{digest}:{seq}", head.head_id).as_bytes());
        }
        head.seq = seq.into();
        tx.execute("UPDATE library SET head=?1", [json(&head)?])?;
        let receipt = PushReceipt {
            operation_id: request.operation_id.clone(),
            seq: seq.into(),
            accepted_keys,
            server_time_ms: now.into(),
        };
        save_operation(
            &tx,
            device,
            &request.operation_id,
            &OperationReceipt::Accepted {
                body_digest: digest,
                receipt: receipt.clone(),
            },
            None,
        )?;
        tx.commit()?;
        drop(db);
        self.announce_head();
        Ok(receipt)
    }

    pub fn operation(&self, device: &Device, id: &str) -> Result<OperationReceipt> {
        validate_id(id)?;
        let db = self.reader()?;
        Self::require_device(&db, device)?;
        operation(&db, device, id)?.ok_or(Error::new("operation-not-found", 404))
    }

    pub fn cancel_operation(
        &self,
        device: &Device,
        id: &str,
        request: &CancelOperationRequest,
    ) -> Result<OperationReceipt> {
        validate_id(id)?;
        request.validate()?;
        let mut db = self.db()?;
        let tx = db.transaction()?;
        Self::require_device(&tx, device)?;
        if let Some(prior) = operation(&tx, device, id)? {
            if prior.body_digest() != request.body_digest {
                return Err(Error::new("operation-integrity", 409));
            }
            return Ok(prior);
        }
        let cancelled = Error::new("operation-cancelled", 409);
        let receipt = OperationReceipt::Rejected {
            operation_id: id.into(),
            body_digest: request.body_digest.clone(),
            error: cancelled.code.into(),
            server_time_ms: time_ms()?.into(),
        };
        save_operation(&tx, device, id, &receipt, Some(&cancelled))?;
        tx.commit()?;
        Ok(receipt)
    }

    pub fn changes(&self, device: &Device, after: DecimalU64, limit: usize) -> Result<ChangesPage> {
        if limit == 0 || limit > 1024 {
            return Err(Error::new("invalid-limit", 400));
        }
        let mut db = self.reader()?;
        let tx = db.transaction()?;
        Self::require_device(&tx, device)?;
        let head = Self::read_head(&tx)?;
        let through: DecimalU64 = head.seq.as_str().to_owned().try_into()?;
        let floor: DecimalU64 = head.min_retained_seq.as_str().to_owned().try_into()?;
        if after < floor {
            return Err(Error::new("journal-floor", 410).with_floor(floor, through));
        }
        if after > through {
            return Err(Error::new("journal-floor", 410).with_floor(floor, through));
        }
        let mut stmt = tx.prepare("SELECT body FROM journal WHERE (length(seq),seq)>(?1,?2) AND (length(seq),seq)<=(?3,?4) ORDER BY length(seq),seq LIMIT ?5")?;
        let rows = stmt.query_map(
            params![
                after.0.to_string().len() as i64,
                after.0.to_string(),
                through.0.to_string().len() as i64,
                through.0.to_string(),
                (limit + 1) as i64
            ],
            |r| r.get::<_, String>(0),
        )?;
        let mut items: Vec<JournalItem> = Vec::new();
        let mut bytes = 512;
        let mut more = false;
        for body in rows {
            let body = body?;
            if items.len() == limit || bytes + body.len() > MAX_METADATA_BYTES - 1024 {
                more = true;
                break;
            }
            bytes += body.len();
            items.push(parse(&body)?);
        }
        if more && items.is_empty() {
            return Err(Error::new("metadata-too-large", 413));
        }
        let next_after = if more {
            items.last().map(|item| item.seq).unwrap_or(after)
        } else {
            through
        };
        Ok(ChangesPage {
            through_seq: through,
            journal_floor: floor,
            items,
            next_after,
        })
    }

    pub fn create_state_pin(&self, device: &Device) -> Result<StatePin> {
        let mut db = self.db()?;
        let tx = db.transaction()?;
        Self::require_device(&tx, device)?;
        // A device reads one state at a time, so pins beyond its newest three were left behind
        // by a read that never finished. They give way to the new read instead of refusing it.
        tx.execute(
            "DELETE FROM state_pins WHERE id IN (SELECT id FROM state_pins WHERE device=?1 AND expires>unixepoch() ORDER BY rowid DESC LIMIT -1 OFFSET 3)",
            [&device.id],
        )?;
        let head = Self::read_head(&tx)?;
        let expires = super::uploads::now()?
            .checked_add(3600)
            .ok_or(Error::new("clock-overflow", 503))?;
        let id = random_id()?;
        tx.execute(
            "INSERT INTO state_pins VALUES(?1,?2,?3,?4)",
            params![id, device.id, head.seq.as_str(), expires],
        )?;
        let units = tx.execute(
            "INSERT INTO state_pin_units SELECT ?1,key,body FROM units",
            [&id],
        )?;
        tx.commit()?;
        Ok(StatePin {
            pin_id: id,
            start_seq: head.seq.as_str().to_owned().try_into()?,
            expires_at_ms: (expires as u64 * 1000).into(),
            unit_count: (units as u64).into(),
        })
    }

    pub fn state_page(
        &self,
        device: &Device,
        pin: &str,
        after: Option<&UnitKey>,
        limit: usize,
    ) -> Result<StatePage> {
        validate_id(pin)?;
        if limit == 0 || limit > 1024 {
            return Err(Error::new("invalid-limit", 400));
        }
        let mut db = self.reader()?;
        let tx = db.transaction()?;
        Self::require_device(&tx, device)?;
        let seq: Option<String> = tx.query_row("SELECT start_seq FROM state_pins WHERE id=?1 AND device=?2 AND expires>unixepoch()", params![pin,device.id], |r| r.get(0)).optional()?;
        let seq = seq.ok_or(Error::new("state-pin-expired", 410))?;
        let mut stmt = tx.prepare("SELECT body FROM state_pin_units WHERE pin=?1 AND key>?2 COLLATE BINARY ORDER BY key COLLATE BINARY LIMIT ?3")?;
        let rows = stmt.query_map(
            params![
                pin,
                after.map(UnitKey::as_str).unwrap_or(""),
                (limit + 1) as i64
            ],
            |r| r.get::<_, String>(0),
        )?;
        let mut items: Vec<UnitChange> = Vec::new();
        let mut bytes = 512;
        let mut more = false;
        for body in rows {
            let body = body?;
            if items.len() == limit || bytes + body.len() > MAX_METADATA_BYTES - 1024 {
                more = true;
                break;
            }
            bytes += body.len();
            items.push(parse(&body)?);
        }
        if more && items.is_empty() {
            return Err(Error::new("metadata-too-large", 413));
        }
        let next_key = if more {
            items.last().map(|item| item.key.clone())
        } else {
            None
        };
        Ok(StatePage {
            pin_id: pin.into(),
            start_seq: seq.try_into()?,
            items,
            next_key,
        })
    }

    pub fn release_state_pin(&self, device: &Device, pin: &str) -> Result<()> {
        validate_id(pin)?;
        let db = self.db()?;
        Self::require_device(&db, device)?;
        db.execute(
            "DELETE FROM state_pins WHERE id=?1 AND device=?2",
            params![pin, device.id],
        )?;
        Ok(())
    }

    pub fn acknowledge(&self, device: &Device, request: &AckRequest) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction()?;
        Self::require_device(&tx, device)?;
        let head = Self::read_head(&tx)?;
        let prior: String =
            tx.query_row("SELECT ack FROM devices WHERE id=?1", [&device.id], |r| {
                r.get(0)
            })?;
        if request.seq < DecimalU64::try_from(prior)?
            || request.seq > DecimalU64::try_from(head.seq.as_str().to_owned())?
        {
            return Err(Error::new("invalid-ack", 409));
        }
        tx.execute(
            "UPDATE devices SET ack=?1,last_ack=unixepoch() WHERE id=?2",
            params![request.seq.0.to_string(), device.id],
        )?;
        tx.commit()?;
        Ok(())
    }
}
