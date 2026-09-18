//! Explicit Serde mappings for the HTLK value model.
//!
//! These implementations describe values; canonical ingress and resource limits
//! are enforced by the codec entry points, not by arbitrary Serde serializers.

use super::{FiniteFloat, Map, Value};
use ::serde::{Serialize, Serializer, ser::SerializeMap};

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Integer(value) => serializer.serialize_i64(*value),
            Self::Float(value) => value.serialize(serializer),
            Self::Text(value) => serializer.serialize_str(value),
            Self::Bytes(value) => serializer.serialize_bytes(value),
            Self::Array(values) => values.serialize(serializer),
            Self::Map(value) => value.serialize(serializer),
        }
    }
}

impl Serialize for FiniteFloat {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.get())
    }
}

impl Serialize for Map {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (key, value) in self.iter() {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}
