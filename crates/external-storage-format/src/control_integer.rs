use risunest_sync_wire::stamp::DecimalU64;
use serde::{Deserialize, Serialize};

pub(crate) fn serialize<T: Copy + Into<u64>, S: serde::Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    DecimalU64((*value).into()).serialize(serializer)
}

pub(crate) fn deserialize<'de, T: TryFrom<u64>, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    T::try_from(DecimalU64::deserialize(deserializer)?.0)
        .map_err(|_| serde::de::Error::custom("control-integer-overflow"))
}

pub(crate) mod optional {
    use super::*;

    pub(crate) fn serialize<T: Copy + Into<u64>, S: serde::Serializer>(
        value: &Option<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .map(|value| DecimalU64(value.into()))
            .serialize(serializer)
    }

    pub(crate) fn deserialize<'de, T: TryFrom<u64>, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        Option::<DecimalU64>::deserialize(deserializer)?
            .map(|value| {
                T::try_from(value.0)
                    .map_err(|_| serde::de::Error::custom("control-integer-overflow"))
            })
            .transpose()
    }
}
