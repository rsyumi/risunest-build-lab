//! Short authoritative operations called between native network stages.

use super::{
    external_storage_state as jobs, sync_selection, PersistentStore, StoreError, StoreResult,
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ExternalJob {
    pub id: String,
    pub connection_id: String,
    pub repository_id: String,
    pub capture_id: String,
    pub identity: sync_selection::CaptureIdentity,
    pub role: String,
    pub strategy: Option<String>,
    pub expected_head: Option<String>,
    pub commit_id: String,
    pub phase: String,
}

fn invalid(message: &str) -> StoreError {
    StoreError::Validation {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture(store: &mut PersistentStore) -> sync_selection::CaptureIdentity {
        let identity = store.external_identity().unwrap();
        let tx = store.connection.transaction().unwrap();
        jobs::register_capture(
            &tx,
            "capture",
            &identity,
            "scope",
            "logical-v1",
            "",
            &"a".repeat(64),
            "destination",
        )
        .unwrap();
        tx.commit().unwrap();
        identity
    }
    #[test]
    fn backup_completion_records_its_point_and_refuses_another_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let identity = capture(&mut store);
        store
            .external_prepare_backup("backup", "destination", "repository", "capture", "point")
            .unwrap();
        store
            .external_finish_backup("backup", "point", "backup-snapshot", "point-observation")
            .unwrap();
        let job = store.external_job("backup").unwrap().unwrap();
        assert_eq!(job.connection_id, "destination");
        assert_eq!(job.phase, "complete");
        assert!(store.external_job("missing").unwrap().is_none());
        assert_eq!(
            store.external_backup_result("backup").unwrap(),
            Some(("backup-snapshot".into(), identity))
        );
        assert!(store
            .external_finish_backup("backup", "point", "different-snapshot", "point-observation")
            .is_err());
    }
    #[test]
    fn cancelling_a_prepared_failure_releases_the_destination_and_capture_pin() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        capture(&mut store);
        store
            .external_prepare_backup("failed", "destination", "repository", "capture", "point")
            .unwrap();
        assert!(store
            .external_prepare_backup("next", "destination", "repository", "capture", "point2")
            .is_err());
        store.external_cancel_prepared("failed").unwrap();
        let pins: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM external_storage_capture_refs WHERE job_id='failed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pins, 0);
        store
            .external_prepare_backup("next", "destination", "repository", "capture", "point2")
            .unwrap();
    }
}

impl PersistentStore {
    pub(crate) fn external_pin_history_record(
        &self,
        job: &str,
    ) -> StoreResult<Option<jobs::PinHistoryRecord>> {
        jobs::pin_history_record(&self.connection, job)
    }
    pub(crate) fn external_prepare_pin_history(
        &mut self,
        intent: &jobs::PinHistoryIntent<'_>,
    ) -> StoreResult<()> {
        let tx = self.connection.transaction()?;
        jobs::prepare_pin_history(&tx, intent)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn external_finish_pin_history(
        &mut self,
        job: &str,
        observation: &str,
    ) -> StoreResult<()> {
        let tx = self.connection.transaction()?;
        jobs::finish_pin_history(&tx, job, observation)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn external_backup_result(
        &self,
        job: &str,
    ) -> StoreResult<Option<(String, sync_selection::CaptureIdentity)>> {
        let row:Option<(String,String)>=self.connection.query_row("SELECT p.snapshot_id,p.identity FROM external_storage_backup_points p JOIN external_storage_jobs j ON j.id=p.job_id WHERE p.job_id=?1 AND j.phase='complete'",[job],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        row.map(|(snapshot, identity)| Ok((snapshot, serde_json::from_str(&identity)?)))
            .transpose()
    }
    pub(crate) fn external_identity(&self) -> StoreResult<sync_selection::CaptureIdentity> {
        sync_selection::identity(&self.connection)
    }
    pub(crate) fn external_selection(&self) -> StoreResult<sync_selection::Selection> {
        sync_selection::read(&self.connection)
    }
    pub(crate) fn external_select(
        &mut self,
        epoch: &str,
        target: &sync_selection::SyncTarget,
    ) -> StoreResult<sync_selection::Selection> {
        let tx = self.connection.transaction()?;
        let selected = sync_selection::select(&tx, epoch, target)?;
        tx.commit()?;
        Ok(selected)
    }
    pub(crate) fn external_set_paused(&mut self, epoch: &str, paused: bool) -> StoreResult<sync_selection::Selection> {
        let tx = self.connection.transaction()?;
        let selected = sync_selection::set_paused(&tx, epoch, paused)?;
        tx.commit()?;
        Ok(selected)
    }

    pub(crate) fn external_job_has_no_capture_owner(&self, job: &str) -> StoreResult<bool> {
        Ok(!self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM external_storage_capture_refs WHERE job_id=?1)
             OR EXISTS(SELECT 1 FROM external_storage_jobs WHERE id=?1 AND phase NOT IN ('complete','cancelled'))",
            [job], |row| row.get::<_, bool>(0),
        )?)
    }


    pub(crate) fn external_job(&self, id: &str) -> StoreResult<Option<ExternalJob>> {
        self.connection
            .query_row(
                "SELECT connection_id,repository_id,capture_id,identity,role,strategy,expected_head,commit_id,phase
                 FROM external_storage_jobs WHERE id=?1",
                [id],
                |row| {
                    let identity = row.get::<_, String>(3)?;
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        identity,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(
                    connection_id,
                    repository_id,
                    capture_id,
                    identity,
                    role,
                    strategy,
                    expected_head,
                    commit_id,
                    phase,
                )| {
                    Ok(ExternalJob {
                        id: id.into(),
                        connection_id,
                        repository_id,
                        capture_id,
                        identity: serde_json::from_str(&identity)?,
                        role,
                        strategy,
                        expected_head,
                        commit_id,
                        phase,
                    })
                },
            )
            .transpose()
    }
    pub(crate) fn external_prepare_backup(
        &mut self,
        id: &str,
        connection: &str,
        repository: &str,
        capture: &str,
        point: &str,
    ) -> StoreResult<()> {
        let tx = self.connection.transaction()?;
        jobs::prepare_backup(&tx, id, connection, repository, capture, point)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn external_cancel_prepared(&mut self, job: &str) -> StoreResult<()> {
        let tx = self.connection.transaction()?;
        jobs::cancel_prepared(&tx, job)?;
        tx.commit()?;
        Ok(())
    }
    /// Called only while the file(true) admission permit is held and after the
    /// native runtime job for this connection has stopped.
    pub(crate) fn external_prepare_connection_removal(
        &mut self,
        connection: &str,
    ) -> StoreResult<()> {
        if connection.is_empty() {
            return Err(invalid("Missing external connection identity"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let unsettled:bool=tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM external_storage_jobs WHERE connection_id=?1 AND phase='applying')",
            [connection], |row| row.get(0))?;
        if unsettled {
            return Err(invalid(
                "Unsettled external operation blocks connection removal",
            ));
        }
        tx.execute(
            "UPDATE external_storage_jobs SET phase='publicationUnknown' WHERE connection_id=?1 AND phase='publishing'",
            [connection],
        )?;
        tx.execute(
            "UPDATE external_storage_jobs SET phase='cancelled' WHERE connection_id=?1 AND phase NOT IN ('complete','cancelled','publicationUnknown')",
            [connection],
        )?;
        tx.execute(
            "DELETE FROM external_storage_capture_refs WHERE job_id IN (SELECT id FROM external_storage_jobs WHERE connection_id=?1 AND phase!='publicationUnknown')",
            [connection],
        )?;
        let selection = sync_selection::read(&tx)?;
        if selection.target == sync_selection::SyncTarget::External(connection.into()) {
            sync_selection::select(&tx, &selection.epoch, &sync_selection::SyncTarget::None)?;
        }
        super::content_change_index::remove_connection_consumer(&tx, connection)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn external_finish_backup(
        &mut self,
        job: &str,
        point: &str,
        snapshot: &str,
        observation: &str,
    ) -> StoreResult<()> {
        if point.is_empty() || snapshot.is_empty() || observation.is_empty() {
            return Err(invalid("Missing confirmed backup point"));
        }
        let tx = self.connection.transaction()?;
        let (connection,repository,identity,expected,phase): (String,String,String,String,String) = tx.query_row(
            "SELECT connection_id,repository_id,identity,commit_id,phase FROM external_storage_jobs WHERE id=?1 AND role='backup'",[job],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?;
        if expected != point || phase != "ready" {
            return Err(invalid("Backup completion does not match its intent"));
        }
        tx.execute(
            "INSERT INTO external_storage_backup_points VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                job,
                connection,
                repository,
                snapshot,
                point,
                observation,
                identity
            ],
        )?;
        tx.execute(
            "UPDATE external_storage_jobs SET phase='complete' WHERE id=?1",
            [job],
        )?;
        tx.execute(
            "DELETE FROM external_storage_capture_refs WHERE job_id=?1",
            [job],
        )?;
        tx.commit()?;
        Ok(())
    }


}
