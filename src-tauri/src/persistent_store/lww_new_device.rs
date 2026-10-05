use super::super::sync_selection::{BindingSelectionChange, SyncTarget};
use super::*;

impl PersistentStore {
    pub(crate) fn prepare_lww_new_device(
        &mut self,
        header: &Header,
        staging_id: &str,
    ) -> StoreResult<NewDevicePreparation> {
        validate_header(header)?;
        self.lww_recover_intents()?;
        verify(self.device_store()?.connection(), header.binding_authority)?;
        let (_, mut selection) = self.validated_new_device_stage(header, staging_id)?;
        let tx = self.device_store_mut()?.transaction()?;
        verify(&tx, header.binding_authority)?;
        let old_writer: String = tx.query_row(
            "SELECT writer_id FROM device_meta WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let prior:Option<(String,String,String,String,String,String)>=tx.query_row("SELECT authorization_id,authority,staging_id,old_writer_id,writer_id,selection_change FROM lww_new_device_authorizations WHERE request_id=?1",[&header.request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
        if let Some((id, authority, stage, original_writer, writer_id, change)) = prior {
            let original: BindingSelectionChange = serde_json::from_str(&change)?;
            selection.new_epoch = original.new_epoch;
            if authority != header.binding_authority.0.to_string()
                || stage != staging_id
                || original_writer != old_writer
                || serde_json::to_string(&selection)? != change
            {
                return Err(error("new-device-preparation-integrity"));
            }
            return Ok(NewDevicePreparation {
                authorization_id: id,
                writer_id,
            });
        }
        let id = Uuid::new_v4().to_string();
        let writer_id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO lww_new_device_authorizations VALUES(?1,?2,?3,?4,?5,?6,?7,0)",
            params![
                id,
                header.request_id,
                header.binding_authority.0.to_string(),
                staging_id,
                old_writer,
                writer_id,
                serde_json::to_string(&selection)?
            ],
        )?;
        tx.commit()?;
        Ok(NewDevicePreparation {
            authorization_id: id,
            writer_id,
        })
    }

    // The native transport authorizes only after settling old publications and registering the reserved writer.
    pub(crate) fn authorize_lww_new_device(
        &mut self,
        authorization_id: &str,
    ) -> StoreResult<NewDevicePreparation> {
        let (request_id,expected,staging_id,old_writer,writer_id,authorized):(String,String,String,String,String,bool)=self.device_store()?.connection().query_row("SELECT request_id,authority,staging_id,old_writer_id,writer_id,authorized FROM lww_new_device_authorizations WHERE authorization_id=?1",[authorization_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
        if !authorized {
            self.lww_recover_intents()?;
            let header = Header {
                binding_authority: wire(expected.try_into())?,
                request_id,
            };
            verify(self.device_store()?.connection(), header.binding_authority)?;
            self.validated_new_device_stage(&header, &staging_id)?;
            let tx = self.device_store_mut()?.transaction()?;
            verify(&tx, header.binding_authority)?;
            let current: String = tx.query_row(
                "SELECT writer_id FROM device_meta WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            if current != old_writer {
                return Err(error("new-device-preparation-writer-changed"));
            }
            tx.execute(
                "UPDATE lww_new_device_authorizations SET authorized=1 WHERE authorization_id=?1",
                [authorization_id],
            )?;
            tx.commit()?;
        }
        Ok(NewDevicePreparation {
            authorization_id: authorization_id.into(),
            writer_id,
        })
    }

    /// The authorized new-device registration that reserved `writer_id`.
    pub(crate) fn lww_new_device_authorization(&self, writer_id: &str) -> StoreResult<Option<String>> {
        Ok(self
            .device_store()?
            .connection()
            .query_row(
                "SELECT authorization_id FROM lww_new_device_authorizations WHERE writer_id=?1 AND authorized=1",
                [writer_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Takes over the writer a changed registration claimed. The clock, unsent versions,
    /// publications and receive progress stay, so retained versions keep their stamps.
    pub(crate) fn lww_adopt_fresh_writer(
        &mut self,
        authority: DecimalU64,
        old_writer_id: &str,
        writer_id: &str,
    ) -> StoreResult<()> {
        self.lww_recover_intents()?;
        let tx = self.device_store_mut()?.transaction()?;
        verify(&tx, authority)?;
        let current: String = tx.query_row(
            "SELECT writer_id FROM device_meta WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if current == writer_id {
            return Ok(());
        }
        if current != old_writer_id {
            return Err(error("fresh-writer-changed"));
        }
        tx.execute(
            "UPDATE device_meta SET writer_id=?1,revision=revision+1 WHERE singleton=1",
            [writer_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn lww_replace_target_as_new_device(
        &mut self,
        header: &Header,
        staging_id: &str,
        authorization_id: &str,
    ) -> StoreResult<NewDeviceResult> {
        validate_header(header)?;
        let existing: Option<(String, String, String, String, bool)> = self
            .device_store()?
            .connection()
            .query_row(
                "SELECT authority,body,stamp,digest,complete FROM lww_intents WHERE request_id=?1",
                [&header.request_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        if let Some((expected, body, stamp, digest, complete)) = existing {
            let intent: Intent = serde_json::from_str(&body)?;
            let Intent::NewDevice {
                authorization_id: original_authorization,
                staging_id: original,
                changes,
                old_writer_id,
                writer_id,
                new_authority,
                selection_change,
            } = intent
            else {
                return Err(error("request-id-integrity"));
            };
            if original_authorization != authorization_id
                || expected != header.binding_authority.0.to_string()
                || original != staging_id
                || {
                    let hash_input = body.as_bytes();
                    #[cfg(test)]
                    crate::persistent_store::hash_work::observe("native_new_device_intent", hash_input.len());
                    risunest_sync_wire::hash(hash_input)
                } != digest
            {
                return Err(error("request-id-integrity"));
            }
            if !complete {
                self.finish_lww_new_device(
                    header,
                    staging_id,
                    &changes,
                    &old_writer_id,
                    &writer_id,
                    new_authority,
                    &serde_json::from_str(&stamp)?,
                    &digest,
                    &selection_change,
                )?;
            }
            return self.new_device_result(header, &writer_id, new_authority);
        }
        self.lww_recover_intents()?;
        verify(self.device_store()?.connection(), header.binding_authority)?;
        let state = self.lww_clock_state()?;
        let row:Option<(String,String,String,String,String,String,bool)>=self.device_store()?.connection().query_row("SELECT request_id,authority,staging_id,old_writer_id,writer_id,selection_change,authorized FROM lww_new_device_authorizations WHERE authorization_id=?1",[authorization_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
        let Some((request, expected, stage, old_writer, writer_id, selection, authorized)) = row
        else {
            return Err(error("missing-new-device-authorization"));
        };
        if !authorized
            || request != header.request_id
            || expected != header.binding_authority.0.to_string()
            || stage != staging_id
            || old_writer != state.writer_id
        {
            return Err(error("missing-or-stale-new-device-authorization"));
        }
        let (changes, mut proposed) = self.validated_new_device_stage(header, staging_id)?;
        let selection_change: BindingSelectionChange = serde_json::from_str(&selection)?;
        proposed.new_epoch = selection_change.new_epoch.clone();
        if serde_json::to_string(&proposed)? != selection {
            return Err(error("new-device-selection-integrity"));
        }
        let new_authority = DecimalU64(
            header
                .binding_authority
                .0
                .checked_add(1)
                .ok_or_else(|| error("binding-authority-exhausted"))?,
        );
        let intent = Intent::NewDevice {
            authorization_id: authorization_id.into(),
            staging_id: staging_id.into(),
            changes: changes.clone(),
            old_writer_id: state.writer_id.clone(),
            writer_id: writer_id.clone(),
            new_authority,
            selection_change: selection_change.clone(),
        };
        let body = serde_json::to_string(&intent)?;
        let digest = {
            let hash_input = body.as_bytes();
            #[cfg(test)]
            crate::persistent_store::hash_work::observe("native_new_device_intent", hash_input.len());
            risunest_sync_wire::hash(hash_input)
        };
        let stamp = match state.issued.or(state.accepted) {
            Some(stamp) => stamp,
            None => wire(issue_stamp(
                device_store::now_ms()?.try_into().map_err(error)?,
                &state.writer_id,
                None,
                None,
            ))?,
        };
        let tx = self.device_store_mut()?.transaction()?;
        verify(&tx, header.binding_authority)?;
        tx.execute(
            "INSERT INTO lww_intents VALUES(?1,?2,?3,?4,?5,0)",
            params![
                header.request_id,
                header.binding_authority.0.to_string(),
                serde_json::to_string(&stamp)?,
                body,
                digest
            ],
        )?;
        tx.commit()?;
        self.finish_lww_new_device(
            header,
            staging_id,
            &changes,
            &state.writer_id,
            &writer_id,
            new_authority,
            &stamp,
            &digest,
            &selection_change,
        )?;
        self.new_device_result(header, &writer_id, new_authority)
    }

    fn validated_new_device_stage(
        &self,
        header: &Header,
        staging_id: &str,
    ) -> StoreResult<(Vec<Change>, BindingSelectionChange)> {
        let (receive,encoded,activation,library,target,connection,source_authority,source_epoch,inspection):(String,String,Option<String>,String,String,String,String,String,String)=self.connection.query_row("SELECT s.receive_id,s.changes,s.activation_epoch,i.library_id,i.target_id,i.target,i.source_authority,i.source_epoch,s.inspection_id FROM lww_binding_stages s JOIN lww_binding_inspections i USING(inspection_id) WHERE s.staging_id=?1",[staging_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)))?;
        let selection = super::super::sync_selection::read(&self.connection)?;
        let target_connection: SyncTarget = serde_json::from_str(&connection)?;
        if receive != header.request_id
            || source_authority != header.binding_authority.0.to_string()
            || source_epoch != selection.epoch
            || activation.is_some()
            || matches!(target_connection, SyncTarget::None)
        {
            return Err(error("stale-or-wrong-target-new-device-stage"));
        }
        super::super::sync_selection::validate_binding_stage_content(&self.connection, staging_id)?;
        let changes: Vec<Change> = serde_json::from_str(&encoded)?;
        validate_binding_source(&self.connection, staging_id, header, &changes)?;
        let mut keys = BTreeSet::new();
        for change in &changes {
            if !keys.insert(change.key.clone()) {
                return Err(error("duplicate-unit-in-new-device-stage"));
            }
            wire(change.stamp.validate())?;
            wire({
                let result = change.value.validate();
                #[cfg(test)]
                crate::persistent_store::hash_work::validation(&change.value);
                result
            })?;
            projection::validate_received(&self.connection, &change.key, &change.value)?;
        }
        Ok((
            changes,
            BindingSelectionChange {
                initial_publication: false,
                expected_epoch: source_epoch,
                new_epoch: Uuid::new_v4().to_string(),
                target: target_connection,
                library_id: Some(library),
                target_id: Some(target),
                inspection_id: Some(inspection),
            },
        ))
    }

    fn new_device_result(
        &self,
        header: &Header,
        writer_id: &str,
        binding_authority: DecimalU64,
    ) -> StoreResult<NewDeviceResult> {
        let revision = self.connection.query_row(
            "SELECT revision FROM lww_requests WHERE request_id=?1",
            [&header.request_id],
            |r| r.get(0),
        )?;
        Ok(NewDeviceResult {
            revision,
            writer_id: writer_id.into(),
            binding_authority,
        })
    }
    pub(super) fn finish_lww_new_device(
        &mut self,
        header: &Header,
        staging_id: &str,
        changes: &[Change],
        old_writer_id: &str,
        writer_id: &str,
        new_authority: DecimalU64,
        stamp: &Stamp,
        digest: &str,
        selection_change: &BindingSelectionChange,
    ) -> StoreResult<()> {
        let state = self.lww_clock_state()?;
        if state.binding_authority != header.binding_authority || state.writer_id != old_writer_id {
            return Err(error("new-device-intent-authority-changed"));
        }
        commit::replace_commit_lww(
            &mut self.connection,
            staging_id,
            header,
            stamp,
            digest,
            &[],
            true,
            changes,
            Some(new_authority),
            Some(selection_change),
            None,
        )?;
        crate::server_sync::carry_operation_log(&self.repository_root, header.binding_authority, new_authority, false)
            .map_err(|failure| error(failure.code))?;
        self.copy_device_unit_bodies(changes)?;
        self.external_lww_release_writer_jobs(old_writer_id)?;
        let tx = self.device_store_mut()?.transaction()?;
        verify(&tx, header.binding_authority)?;
        let current_writer: String = tx.query_row(
            "SELECT writer_id FROM device_meta WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if current_writer != old_writer_id {
            return Err(error("new-device-intent-writer-changed"));
        }
        reset_target_device(&tx, header, changes, new_authority)?;
        tx.execute("DELETE FROM lww_progress", [])?;
        // The retired writer never sends again, so its unsettled segments and
        // sequences are dropped instead of waiting for an answer forever.
        tx.execute("DELETE FROM external_lww_segments WHERE writer=?1", [old_writer_id])?;
        tx.execute("DELETE FROM external_lww_sequences WHERE writer=?1", [old_writer_id])?;
        tx.execute(
            "UPDATE device_meta SET writer_id=?1,revision=revision+1 WHERE singleton=1",
            [writer_id],
        )?;
        tx.execute(
            "UPDATE lww_clock SET issued=NULL,binding_authority=?1 WHERE singleton=1",
            [new_authority.0.to_string()],
        )?;
        tx.execute(
            "UPDATE lww_intents SET complete=1 WHERE request_id=?1",
            [&header.request_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}
