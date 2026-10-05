use super::*;

impl PersistentStore {
    fn validate_device_completion_input(&self, key: &UnitKey, value: &UnitValue) -> StoreResult<()> {
        wire(value.validate())?;
        if projection::is_device(key) {
            if let UnitValue::Object { descriptor, .. } = value {
                let device = self.device_store()?.connection();
                let source = if super::super::message_pages::retained_object_present(device, &descriptor.object_hash)? { device } else { &self.connection };
                validate_large_unit(source, value)?;
            }
        }
        Ok(())
    }

    pub(super) fn validate_quarantined_completion(&self, request_id: &str) -> StoreResult<()> {
        let device = self.device_store()?.connection();
        let (expected, stamp, body, digest): (String, String, String, String) = device.query_row(
            "SELECT authority,stamp,body,digest FROM lww_intents WHERE request_id=?1 AND complete=0", [request_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        if risunest_sync_wire::hash(body.as_bytes()) != digest { return Err(error("request-id-integrity")); }
        let proof: Option<String> = device.query_row("SELECT identity FROM lww_intent_proofs WHERE request_id=?1", [request_id], |row| row.get(0)).optional()?;
        if proof.as_deref() != Some(intent_identity(&expected, &stamp, &digest)?.as_str()) { return Err(error("request-id-integrity")); }
        let expected = wire(expected.try_into())?;
        verify(device, expected)?;
        let stamp: Stamp = serde_json::from_str(&stamp)?;
        wire(stamp.validate())?;
        let intent: Intent = serde_json::from_str(&body)?;
        self.verify_intent_device_revision(request_id)?;
        let receipt: Option<(String, i64, Option<String>)> = self.connection.query_row(
            "SELECT digest,revision,activated_generation FROM lww_requests WHERE request_id=?1", [request_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        if let Intent::Repair { entries } = &intent {
            if receipt.is_some() || entries.is_empty() { return Err(error("intent-completion-not-available")); }
            let mut applied = false;
            for entry in entries {
                wire(entry.stamp.validate())?;
                wire(entry.value.validate())?;
                let db = if projection::is_device(&entry.key) { device } else { &self.connection };
                let current: Option<(String, String, String, String)> = db.query_row(
                    "SELECT version,stamp,value,authority FROM lww_outbox WHERE key=?1", [entry.key.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                ).optional()?;
                let Some((version, current_stamp, value, authority)) = current else { return Err(error("unpublished-version-changed")); };
                let unit: Option<(String, String, String)> = db.query_row(
                    "SELECT version,stamp,value FROM lww_units WHERE key=?1", [entry.key.as_str()],
                    |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
                ).optional()?;
                if authority != expected.0.to_string() || entry.target_authority != expected || unit != Some((version.clone(), current_stamp.clone(), value.clone())) {
                    return Err(error("unpublished-version-changed"));
                }
                let settled = version == request_id && current_stamp == serde_json::to_string(&stamp)?;
                if (!settled && (version != entry.version || current_stamp != serde_json::to_string(&entry.stamp)?))
                    || serde_json::from_str::<UnitValue>(&value)? != entry.value {
                    return Err(error("unpublished-version-changed"));
                }
                applied |= settled;
            }
            return if applied { Ok(()) } else { Err(error("intent-completion-not-available")) };
        }
        let (committed, revision, generation) = receipt.ok_or_else(|| error("request-receipt-missing"))?;
        if committed != digest || revision > current_revision(&self.connection)? { return Err(error("request-id-integrity")); }
        match &intent {
            Intent::Target { staging_id, changes, .. } => {
                if generation.as_deref() != Some(staging_id.as_str()) || active_generation(&self.connection)? != *staging_id { return Err(error("request-id-integrity")); }
                intent_rows::read_target(device, request_id, changes)?.visit(false, |key, stamp, value, _| {
                    wire(stamp.ok_or_else(|| error("request-id-integrity"))?.validate())?;
                    self.validate_device_completion_input(&key, &value)
                })?;
            }
            Intent::Replacement { staging_id, base_revision, changes, device_sections: Some(_), device_changes, .. } => {
                if generation.as_deref() != Some(staging_id.as_str()) || base_revision.checked_add(1) != Some(revision) || active_generation(&self.connection)? != *staging_id {
                    return Err(error("request-id-integrity"));
                }
                intent_rows::read_replacement(device, request_id, changes)?;
                for (key, value) in device_changes.iter() { self.validate_device_completion_input(key, value)?; }
            }
            Intent::NewDevice { staging_id, changes, old_writer_id, writer_id, new_authority, authorization_id, selection_change, .. } => {
                if generation.as_deref() != Some(staging_id.as_str()) || active_generation(&self.connection)? != *staging_id || expected.0.checked_add(1) != Some(new_authority.0)
                    || self.lww_clock_state()?.writer_id != *old_writer_id {
                    return Err(error("request-id-integrity"));
                }
                let authorized: bool = device.query_row(
                    "SELECT EXISTS(SELECT 1 FROM lww_new_device_authorizations WHERE authorization_id=?1 AND request_id=?2 AND authority=?3 AND staging_id=?4 AND old_writer_id=?5 AND writer_id=?6 AND selection_change=?7 AND authorized=1)",
                    params![authorization_id,request_id,expected.0.to_string(),staging_id,old_writer_id,writer_id,serde_json::to_string(selection_change)?], |row| row.get(0),
                )?;
                if !authorized { return Err(error("new-device-authorization-missing")); }
                self.validate_completed_selection(selection_change)?;
                for change in changes { wire(change.stamp.validate())?; self.validate_device_completion_input(&change.key, &change.value)?; }
            }
            Intent::Switch { change, new_authority, .. } => {
                if generation.is_some() || expected.0.checked_add(1) != Some(new_authority.0) { return Err(error("request-id-integrity")); }
                self.validate_completed_selection(change)?;
            }
            _ => return Err(error("intent-completion-not-available")),
        }
        Ok(())
    }

    fn validate_completed_selection(&self, change: &super::super::sync_selection::BindingSelectionChange) -> StoreResult<()> {
        let state = self.lww_binding_state()?;
        if state.selection_epoch != change.new_epoch || state.target != change.target {
            return Err(error("binding-selection-changed"));
        }
        let (library, target): (Option<String>, Option<String>) = self.connection.query_row(
            "SELECT library_id,target_id FROM lww_binding_identity WHERE singleton=1", [], |row| Ok((row.get(0)?,row.get(1)?)),
        )?;
        if !matches!(change.target, super::super::sync_selection::SyncTarget::None)
            && (library != change.library_id || target != change.target_id) { return Err(error("binding-selection-changed")); }
        Ok(())
    }

    pub(crate) fn lww_complete_quarantined_intent(&mut self, request_id: &str, expected_token: &str, expected_revision: i64) -> StoreResult<bool> {
        let actual = current_revision(&self.connection)?;
        if actual != expected_revision { return Err(StoreError::RevisionConflict { expected: expected_revision, actual }); }
        let selected = self.lww_quarantined_intents()?.into_iter().find(|intent| intent.request_id == request_id);
        let Some(selected) = selected else {
            let complete: Option<bool> = self.device_store()?.connection().query_row("SELECT complete FROM lww_intents WHERE request_id=?1", [request_id], |row| row.get(0)).optional()?;
            return if complete == Some(true) { Ok(false) } else { Err(error("stale-quarantined-intent")) };
        };
        if selected.token != expected_token { return Err(error("stale-quarantined-intent")); }
        self.validate_quarantined_completion(request_id)?;
        let (expected, stamp, body, digest): (String, String, String, String) = self.device_store()?.connection().query_row(
            "SELECT authority,stamp,body,digest FROM lww_intents WHERE request_id=?1", [request_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        self.recover_intent(self.lww_binding_authority()?, request_id, expected, &stamp, &body, &digest)?;
        self.invalidate_message_object_roots();
        Ok(true)
    }
}
