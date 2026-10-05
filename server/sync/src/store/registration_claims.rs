use super::*;
use risunest_sync_wire::lww::{NewDeviceClaimReceipt, NewDeviceClaimRequest, NewDeviceClaimStatus};

impl Store {
    pub fn new_device_writer_claim(&self, device: &Device) -> Result<Option<NewDeviceClaimStatus>> {
        let mut db = self.db()?;
        let tx = db.transaction()?;
        Self::require_device(&tx, device)?;
        let saved: Option<(String, String)> = tx.query_row(
            "SELECT digest,body FROM device_writer_claims WHERE device=?1",
            [&device.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        saved.map(|(request_digest, body)| Ok(NewDeviceClaimStatus {
            request_digest,
            receipt: parse(&body)?,
        })).transpose()
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
        let used: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM writers WHERE device=?1) OR EXISTS(SELECT 1 FROM operations WHERE device=?1)",
            [&device.id],
            |row| row.get(0),
        )?;
        if used {
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
