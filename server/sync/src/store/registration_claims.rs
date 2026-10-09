use super::*;
use risunest_sync_wire::lww::{
    NewDeviceClaimReceipt, NewDeviceClaimRequest, NewDeviceClaimState, NewDeviceClaimStatus,
    WriterBindingRequest,
};

/// Whether a writer or operation already belongs to the device, which keeps it from claiming one.
fn registration_used(tx: &rusqlite::Transaction, device: &Device) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM writers WHERE device=?1) OR EXISTS(SELECT 1 FROM operations WHERE device=?1)",
        [&device.id],
        |row| row.get(0),
    )?)
}

impl Store {
    /// Binds the writer of a join that keeps its installation's writer. The device's own writer
    /// binds again; a device another writer or an operation used, or a writer another device
    /// holds, is refused.
    pub fn bind_device_writer(
        &self,
        device: &Device,
        request: &WriterBindingRequest,
    ) -> Result<()> {
        request.validate()?;
        let mut db = self.db()?;
        let tx = db.transaction()?;
        Self::require_device(&tx, device)?;
        let other_writer: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM writers WHERE device=?1 AND writer<>?2)",
            params![device.id, request.writer_id],
            |row| row.get(0),
        )?;
        if other_writer {
            return Err(Error::new("registration-used", 409));
        }
        let owner: Option<String> = tx
            .query_row(
                "SELECT device FROM writers WHERE writer=?1",
                [&request.writer_id],
                |row| row.get(0),
            )
            .optional()?;
        match owner {
            Some(owner) if owner == device.id => return Ok(()),
            Some(_) => return Err(Error::new("writer-collision", 409)),
            None => {}
        }
        if registration_used(&tx, device)? {
            return Err(Error::new("registration-used", 409));
        }
        tx.execute(
            "INSERT INTO writers(writer,device) VALUES(?1,?2)",
            params![request.writer_id, device.id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn new_device_writer_claim(&self, device: &Device) -> Result<NewDeviceClaimState> {
        let mut db = self.db()?;
        let tx = db.transaction()?;
        Self::require_device(&tx, device)?;
        let saved: Option<(String, String)> = tx
            .query_row(
                "SELECT digest,body FROM device_writer_claims WHERE device=?1",
                [&device.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let claim = saved
            .map(|(request_digest, body)| -> Result<_> {
                Ok(NewDeviceClaimStatus {
                    request_digest,
                    receipt: parse(&body)?,
                })
            })
            .transpose()?;
        Ok(NewDeviceClaimState {
            claim,
            used: registration_used(&tx, device)?,
        })
    }

    pub fn claim_new_device_writer(
        &self,
        device: &Device,
        request: &NewDeviceClaimRequest,
    ) -> Result<NewDeviceClaimReceipt> {
        request.validate()?;
        let digest = request.digest()?;
        let mut db = self.db()?;
        let tx = db.transaction()?;
        Self::require_device(&tx, device)?;
        let prior: Option<(String, String)> = tx
            .query_row(
                "SELECT digest,body FROM device_writer_claims WHERE device=?1",
                [&device.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((original_digest, body)) = prior {
            if original_digest != digest {
                return Err(Error::new("registration-integrity", 409));
            }
            return parse(&body);
        }
        if registration_used(&tx, device)? {
            return Err(Error::new("registration-used", 409));
        }
        let authorization_used: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM device_writer_claims WHERE authorization=?1)",
            [&request.authorization_id],
            |row| row.get(0),
        )?;
        if authorization_used {
            return Err(Error::new("registration-integrity", 409));
        }
        let writer_known: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM writers WHERE writer=?1) OR EXISTS(SELECT 1 FROM writer_versions WHERE writer=?1)",
            [&request.writer_id],
            |row| row.get(0),
        )?;
        if writer_known {
            return Err(Error::new("writer-collision", 409));
        }
        let former_verifier = request
            .former_token
            .as_ref()
            .map(|token| hash(token.as_bytes()));
        let former_device: Option<String> = match &former_verifier {
            Some(verifier) => tx
                .query_row(
                    "SELECT id FROM devices WHERE verifier=?1",
                    [verifier],
                    |row| row.get(0),
                )
                .optional()?,
            None => None,
        };
        if former_device.as_ref() == Some(&device.id) {
            return Err(Error::new("registration-not-new", 409));
        }
        let head = Self::read_head(&tx)?;
        let receipt = NewDeviceClaimReceipt {
            authorization_id: request.authorization_id.clone(),
            writer_id: request.writer_id.clone(),
            device_id: device.id.clone(),
            library_id: head.library_id,
            epoch: head.epoch,
            former_credential_inactive: former_verifier.is_some(),
        };
        tx.execute(
            "INSERT INTO writers(writer,device) VALUES(?1,?2)",
            params![request.writer_id, device.id],
        )?;
        tx.execute(
            "INSERT INTO device_writer_claims(device,authorization,writer,digest,body) VALUES(?1,?2,?3,?4,?5)",
            params![device.id,request.authorization_id,request.writer_id,digest,json(&receipt)?],
        )?;
        if let Some(verifier) = former_verifier {
            tx.execute("UPDATE devices SET revoked=1 WHERE verifier=?1", [verifier])?;
        }
        tx.commit()?;
        self.announce_head();
        Ok(receipt)
    }
}
