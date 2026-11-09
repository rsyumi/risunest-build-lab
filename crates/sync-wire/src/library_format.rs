//! The library format level: one integer per library naming which content it
//! may hold. A library without the level unit is level 1.
use crate::{unit::{UnitKey, UnitValue}, Result, WireError};

/// Requests carry the highest level the client understands; responses carry
/// the library's level.
pub const HEADER: &str = "x-risunest-library-format";
/// The highest level the server accepts, on every response that carries `HEADER`.
pub const MAX_HEADER: &str = "x-risunest-library-format-max";
/// Marks a level push that applies only while the library holds no other unit.
pub const PRECONDITION_HEADER: &str = "x-risunest-library-precondition";
pub const EMPTY_LIBRARY: &str = "empty";

pub const KEY: &str = r#"["library-format"]"#;
/// The largest level a JavaScript number names exactly.
const MAX_LEVEL: u64 = (1 << 53) - 1;

pub fn key() -> UnitKey { UnitKey::try_from(KEY.to_owned()).expect("library format key is valid") }
pub fn is_key(key: &UnitKey) -> bool { key.as_str() == KEY }

/// The level a level unit names. Fields a later level adds are ignored, so a
/// value this build cannot fully read still reports its level.
pub fn level(value: &UnitValue) -> Option<u64> {
    let (_, value) = value.validated_inline().ok()??;
    let level = value.get("level")?.as_f64()?;
    (level.fract() == 0.0 && level >= 2.0 && level <= MAX_LEVEL as f64).then_some(level as u64)
}

/// The level unit value for `level`.
pub fn value(level: u64) -> Result<UnitValue> {
    if !(2..=MAX_LEVEL).contains(&level) { return Err(WireError("invalid-library-format")); }
    UnitValue::inline(format!(r#"{{"level":{level}}}"#).as_bytes())
}

/// Whether `value` is exactly the level unit this build writes for its level.
pub fn is_canonical(value: &UnitValue) -> bool {
    level(value).is_some_and(|level| self::value(level).is_ok_and(|expected| expected == *value))
}

/// Reads a level header value.
pub fn parse_header(value: &str) -> Result<u64> {
    if value.is_empty() || value.len() > 16 || !value.bytes().all(|b| b.is_ascii_digit()) || value.starts_with('0') {
        return Err(WireError("invalid-library-format"));
    }
    value.parse().ok().filter(|level| *level <= MAX_LEVEL).ok_or(WireError("invalid-library-format"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_read_tolerantly_and_written_canonically() {
        assert!(is_key(&key()));
        assert_eq!(level(&value(2).unwrap()), Some(2));
        assert!(is_canonical(&value(3).unwrap()));
        let later = UnitValue::inline(br#"{"level":3,"next":true}"#).unwrap();
        assert_eq!((level(&later), is_canonical(&later)), (Some(3), false));
        for invalid in [br#"{"level":1}"#.as_slice(), br#"{"level":2.5}"#, br#"{"level":"2"}"#, br#"{}"#, b"2"] {
            assert_eq!(level(&UnitValue::inline(invalid).unwrap()), None);
        }
        assert_eq!(level(&UnitValue::Deleted), None);
        assert!(value(1).is_err());
        assert_eq!(parse_header("2"), Ok(2));
        for invalid in ["", "0", "02", "-1", "2 ", "x", "99999999999999999"] {
            assert!(parse_header(invalid).is_err(), "{invalid}");
        }
    }
}
