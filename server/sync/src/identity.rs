use crate::{Error, Result};

pub fn new_uuid() -> Result<String> {
    let mut bytes = [0; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| Error::new("entropy-unavailable", 503))?;
    Ok(uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string())
}

pub fn is_uuid_v4(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| {
        id.get_version() == Some(uuid::Version::Random)
            && id.get_variant() == uuid::Variant::RFC4122
            && id.to_string() == value
    })
}

pub(crate) fn secret_token() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| Error::new("entropy-unavailable", 503))?;
    Ok(hex::encode(bytes))
}
