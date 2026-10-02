//! Building blocks that let persisted settings survive a newer writer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Keys a struct did not recognize, preserved verbatim across a load and save.
///
/// Fields of this type are `#[serde(flatten)]`ed into their owner, so the keys
/// sit at the owner's level in the file rather than under a nested object.
pub type UnknownFields = BTreeMap<String, serde_json::Value>;

/// A value of an enumerated setting that tolerates spellings this build does
/// not know.
///
/// The representation is untagged: a known value serializes exactly as `T`
/// does, and anything `T` rejects is kept as raw JSON and written back
/// unchanged. Order matters — `Known` is tried first, so a spelling this build
/// understands never degrades into `Unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Extensible<T> {
    /// A value this build understands.
    Known(T),
    /// A value this build does not understand, kept for the next writer.
    Unknown(serde_json::Value),
}

impl<T> Extensible<T> {
    /// The value when this build understands it.
    pub fn known(&self) -> Option<&T> {
        match self {
            Self::Known(v) => Some(v),
            Self::Unknown(_) => None,
        }
    }

    /// Whether this build understands the stored value.
    pub fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }
}

impl<T> Extensible<T>
where
    T: Copy,
{
    /// The value, or `fallback` when this build does not understand it.
    pub fn or(&self, fallback: T) -> T {
        match self {
            Self::Known(v) => *v,
            Self::Unknown(_) => fallback,
        }
    }
}

impl<T> From<T> for Extensible<T> {
    fn from(value: T) -> Self {
        Self::Known(value)
    }
}

impl<T> Default for Extensible<T>
where
    T: Default,
{
    fn default() -> Self {
        Self::Known(T::default())
    }
}

/// True when a map has no entries. Used as a `skip_serializing_if` predicate so
/// an empty unknown-field map adds nothing to the output.
pub(crate) fn is_empty_map(map: &UnknownFields) -> bool {
    map.is_empty()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    enum Sample {
        First,
        Second,
    }

    #[test]
    fn known_spelling_round_trips_as_a_plain_value() {
        let value: Extensible<Sample> = Sample::First.into();
        let text = serde_json::to_string(&value).unwrap();
        assert_eq!(text, "\"first\"");
        let back: Extensible<Sample> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn unknown_spelling_survives_a_round_trip() {
        let back: Extensible<Sample> = serde_json::from_str("\"third\"").unwrap();
        assert!(!back.is_known());
        assert_eq!(serde_json::to_string(&back).unwrap(), "\"third\"");
    }

    #[test]
    fn unknown_object_shaped_value_survives() {
        let back: Extensible<Sample> = serde_json::from_str(r#"{"kind":"future","n":3}"#).unwrap();
        assert_eq!(back.known(), None);
        let text = serde_json::to_string(&back).unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(reparsed["n"], serde_json::json!(3));
    }
}
