//! Property values as JSON: the JSON form of a message shows them as they are, a binary form
//! (postcard) carries them as compact JSON text, because the drawing's types use serde
//! representations (tagged enums, defaults) that only self-describing formats read back.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Error;

#[derive(Clone, Debug, PartialEq)]
pub struct Json(pub serde_json::Value);

impl Json {
    /// The JSON value of `v`.
    pub fn of<T: Serialize + ?Sized>(v: &T) -> Result<Json, Error> {
        serde_json::to_value(v).map(Json).map_err(|e| Error::Encode(e.to_string()))
    }

    /// Read the value as a `T`; `what` names it in the error.
    pub fn read<'a, T: Deserialize<'a>>(&'a self, what: &str) -> Result<T, Error> {
        T::deserialize(&self.0).map_err(|e| Error::Invalid(format!("{what}: {e}")))
    }
}

impl Serialize for Json {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            self.0.serialize(s)
        } else {
            let text = serde_json::to_string(&self.0).map_err(serde::ser::Error::custom)?;
            s.serialize_str(&text)
        }
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Text;
        impl serde::de::Visitor<'_> for Text {
            type Value = Json;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON text")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Json, E> {
                // serde_json caps nesting depth, so hostile input can't overflow the stack.
                serde_json::from_str(v).map(Json).map_err(E::custom)
            }
        }
        if d.is_human_readable() { serde_json::Value::deserialize(d).map(Json) } else { d.deserialize_str(Text) }
    }
}
