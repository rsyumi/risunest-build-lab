use super::{Device, Store};
use crate::{Error, Result};
use risunest_sync_wire::{
    delta, hash,
    transfer::{self, Frame},
    validate_hash,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferRequest {
    pub target: String,
    pub bases: Vec<String>,
}
impl Store {
    pub fn receive_frames(&self, device: &Device, bytes: &[u8]) -> Result<Vec<String>> {
        #[cfg(test)]
        let measured = std::time::Instant::now();
        let frames = transfer::decode(bytes)?;
        #[cfg(test)]
        super::objects::frame_metrics::record(0, measured);
        if frames
            .iter()
            .any(|f| matches!(f, Frame::FullRequired { .. }))
        {
            return Err(Error::new("invalid-upload-frame", 400));
        }
        let mut verified = Vec::new();
        let mut objects: Vec<(String, Vec<u8>)> = Vec::new();
        let mut materialized = 0usize;
        for frame in frames {
            #[cfg(test)]
            let measured = std::time::Instant::now();
            let bytes = match frame {
                Frame::Full(bytes) => bytes,
                Frame::Delta(recipe) => {
                    let hashes = recipe
                        .bases
                        .iter()
                        .map(|b| b.hash.clone())
                        .collect::<Vec<_>>();
                    let remote = hashes
                        .iter()
                        .filter(|hash| !objects.iter().any(|(h, _)| h == *hash))
                        .cloned()
                        .collect::<Vec<_>>();
                    self.pin_objects(device, &remote)?;
                    let bases = hashes
                        .iter()
                        .map(|h| match objects.iter().find(|(hash, _)| hash == h) {
                            Some((_, bytes)) => Ok(bytes.clone()),
                            None => self.get_object(h),
                        })
                        .collect::<Result<Vec<_>>>()?;
                    recipe.apply(&bases.iter().map(Vec::as_slice).collect::<Vec<_>>())?
                }
                Frame::FullRequired { .. } => unreachable!(),
            };
            let digest = hash(&bytes);
            #[cfg(test)]
            super::objects::frame_metrics::record(1, measured);
            materialized = materialized
                .checked_add(bytes.len())
                .filter(|size| *size <= 32 * 1024 * 1024)
                .ok_or(Error::new("batch-too-large", 413))?;
            objects.push((digest.clone(), bytes));
            verified.push(digest);
        }
        self.put_objects(device, &objects)?;
        Ok(verified)
    }
    pub fn transfer_objects(
        &self,
        device: &Device,
        requests: &[TransferRequest],
    ) -> Result<Vec<u8>> {
        if requests.len() > 1024 {
            return Err(Error::new("too-many-candidates", 400));
        }
        for request in requests {
            validate_hash(&request.target)?;
            if request.bases.len() > delta::MAX_BASES {
                return Err(Error::new("too-many-bases", 400));
            }
            let mut seen = std::collections::BTreeSet::new();
            for base in &request.bases {
                validate_hash(base)?;
                if !seen.insert(base) {
                    return Err(Error::new("duplicate-base", 400));
                }
            }
        }
        // Sizes come from object metadata, so the reply is planned without
        // opening a body. A target that does not exist is the request's error;
        // a base that does not exist only removes a delta option.
        let mut sizes = std::collections::BTreeMap::new();
        for request in requests {
            if !sizes.contains_key(&request.target) {
                let size = self
                    .object_size(&request.target)?
                    .ok_or(Error::new("object-not-found", 404))?;
                sizes.insert(request.target.clone(), size);
            }
            for base in &request.bases {
                if !sizes.contains_key(base) {
                    if let Some(size) = self.object_size(base)? {
                        sizes.insert(base.clone(), size);
                    }
                }
            }
        }
        // One lease transaction per bounded group of the deduplicated union,
        // not one transaction per object inside the reply loop.
        let union = sizes.keys().cloned().collect::<Vec<_>>();
        for group in union.chunks(1024) {
            self.pin_objects(device, group)?;
        }
        let mut frames = Vec::new();
        let mut used = 8usize;
        let mut materialized = 0u64;
        for request in requests {
            let size = sizes[&request.target];
            let mut frame = Frame::FullRequired {
                hash: request.target.clone(),
                size,
            };
            // Decide from metadata whether this target can still be answered.
            // A filled reply must not make every remaining candidate be read
            // and discarded.
            let mut selected = Vec::new();
            let mut base_bytes = 0;
            if size > 64 {
                for digest in &request.bases {
                    if let Some(base) = sizes.get(digest).copied() {
                        if base <= delta::MAX_TARGET_BYTES as u64
                            && base_bytes + base <= delta::MAX_BASE_BYTES as u64
                        {
                            selected.push(digest);
                            base_bytes += base;
                        }
                    }
                }
            }
            let inlinable =
                size <= delta::MAX_TARGET_BYTES as u64 && materialized + size <= 32 * 1024 * 1024;
            let fits = used + size as usize + 45 <= transfer::PREFERRED_BATCH_BYTES;
            if inlinable && (fits || !selected.is_empty()) {
                let target = self.get_object(&request.target)?;
                let bases = selected
                    .into_iter()
                    .map(|digest| self.get_object(digest))
                    .collect::<Result<Vec<_>>>()?;
                if !bases.is_empty() {
                    let recipe = delta::create(
                        &bases.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                        &target,
                    )
                    .ok();
                    if let Some(recipe) = recipe {
                        if let Ok(bytes) = recipe.encode() {
                            if bytes.len() + 64 < target.len()
                                && used + bytes.len() + 5 <= 8 * 1024 * 1024
                            {
                                frame = Frame::Delta(recipe);
                            }
                        }
                    }
                }
                if matches!(frame, Frame::FullRequired { .. }) && fits {
                    frame = Frame::Full(target);
                }
            }
            let encoded = transfer::encode(std::slice::from_ref(&frame))?;
            if used + encoded.len() - 8 > 8 * 1024 * 1024 {
                return Err(Error::new("batch-too-large", 413));
            }
            used += encoded.len() - 8;
            if !matches!(frame, Frame::FullRequired { .. }) {
                materialized += size;
            }
            frames.push(frame);
        }
        Ok(transfer::encode(&frames)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_sync_wire::transfer::PREFERRED_BATCH_BYTES;

    fn body(index: usize, size: usize) -> Vec<u8> {
        let mut bytes = format!("synthetic transfer target {index:08} ").into_bytes();
        bytes.resize(size, b'.');
        bytes
    }

    /// A27. Once the reply is full the remaining targets are declined from
    /// their recorded size, so no further body is read and discarded.
    #[test]
    fn a_filled_reply_opens_no_body_it_cannot_carry() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init(directory.path()).unwrap();
        let registration = store.add_device().unwrap();
        let device = store
            .authenticate(&registration.library_id, &registration.token)
            .unwrap();
        // Half the reply target each, so the reply fills before the last one.
        let size = PREFERRED_BATCH_BYTES / 2;
        let requests = (0..4)
            .map(|index| {
                let bytes = body(index, size);
                let target = hash(&bytes);
                store.put_object(&device, &target, &bytes).unwrap();
                TransferRequest {
                    target,
                    bases: Vec::new(),
                }
            })
            .collect::<Vec<_>>();
        super::super::objects::frame_metrics::take_opens();
        let encoded = store.transfer_objects(&device, &requests).unwrap();
        let opens = super::super::objects::frame_metrics::take_opens();
        let frames = transfer::decode(&encoded).unwrap();
        let carried = frames
            .iter()
            .filter(|frame| !matches!(frame, Frame::FullRequired { .. }))
            .count();
        let declined = frames.len() - carried;
        assert!(
            carried > 0 && declined > 0,
            "{carried} carried {declined} declined"
        );
        assert_eq!(
            opens, carried,
            "a declined target must not be opened, saw {opens} opens for {carried} carried frames"
        );
    }

    /// The deduplicated target and base union is leased once per bounded
    /// group, not once per object inside the reply loop.
    #[test]
    fn a_repeated_target_is_leased_once_and_a_missing_base_keeps_the_target() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init(directory.path()).unwrap();
        let registration = store.add_device().unwrap();
        let device = store
            .authenticate(&registration.library_id, &registration.token)
            .unwrap();
        let bytes = body(0, 4096);
        let target = hash(&bytes);
        store.put_object(&device, &target, &bytes).unwrap();
        let absent = hash(b"synthetic base that was never published");
        let requests = vec![
            TransferRequest {
                target: target.clone(),
                bases: vec![absent.clone()],
            },
            TransferRequest {
                target: target.clone(),
                bases: vec![absent],
            },
        ];
        let frames =
            transfer::decode(&store.transfer_objects(&device, &requests).unwrap()).unwrap();
        assert_eq!(frames.len(), 2);
        assert!(frames
            .iter()
            .all(|frame| matches!(frame, Frame::Full(carried) if *carried == bytes)));
        let leases: i64 = store
            .db()
            .unwrap()
            .query_row("SELECT count(*) FROM object_leases", [], |r| r.get(0))
            .unwrap();
        assert_eq!(leases, 1);
    }
}
