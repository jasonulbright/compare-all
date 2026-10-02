//! Declarative construction of a settings group and its override twin.

/// Declares a settings group as two types: a fully populated struct carrying a
/// value for every field, and an override struct whose fields are all optional.
///
/// Both types are generated from one field list so a field can never exist in
/// one and be forgotten in the other. Both also carry an `unknown` map holding
/// the fields a newer build wrote, so a save re-emits everything a load did not
/// understand.
///
/// The generated types rely on the container-level serde default: a field left
/// out of a document takes the value from the `Default` implementation, which
/// is the documented default rather than the field type's own zero value.
/// Per-field defaults would substitute the zero value and are therefore wrong
/// here; removing the container attribute changes the meaning of every
/// incomplete document.
macro_rules! settings_group {
    (
        $(#[$group_doc:meta])*
        $name:ident / $override_name:ident {
            $(
                $(#[$field_doc:meta])*
                $field:ident : $ty:ty = $default:expr
            ),* $(,)?
        }
    ) => {
        $(#[$group_doc])*
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "camelCase", default)]
        pub struct $name {
            $(
                $(#[$field_doc])*
                pub $field: $ty,
            )*
            /// Fields written by another build, preserved verbatim.
            #[serde(
                flatten,
                skip_serializing_if = "std::collections::BTreeMap::is_empty"
            )]
            pub unknown: std::collections::BTreeMap<String, serde_json::Value>,
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    $( $field: $default, )*
                    unknown: std::collections::BTreeMap::new(),
                }
            }
        }

        $(#[$group_doc])*
        ///
        /// Override form: a field left as `None` keeps the value from the layer
        /// below instead of writing one.
        #[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "camelCase", default)]
        pub struct $override_name {
            $(
                $(#[$field_doc])*
                #[serde(
                    skip_serializing_if = "Option::is_none",
                    deserialize_with = "crate::settings::macros::deserialize_present"
                )]
                pub $field: Option<$ty>,
            )*
            /// Fields written by another build, preserved verbatim.
            #[serde(
                flatten,
                skip_serializing_if = "std::collections::BTreeMap::is_empty"
            )]
            pub unknown: std::collections::BTreeMap<String, serde_json::Value>,
        }

        impl $override_name {
            /// Writes every field this override sets onto `base`.
            pub fn apply_to(&self, base: &mut $name) {
                $(
                    if let Some(value) = self.$field.clone() {
                        base.$field = value;
                    }
                )*
                base.unknown.extend(
                    self.unknown.iter().map(|(k, v)| (k.clone(), v.clone()))
                );
            }

            /// Builds an override that sets every field to the given values.
            #[must_use]
            pub fn from_full(full: &$name) -> Self {
                Self {
                    $( $field: Some(full.$field.clone()), )*
                    unknown: full.unknown.clone(),
                }
            }

            /// Folds `other` into this override; fields set by `other` win.
            pub fn merge(&mut self, other: &Self) {
                $(
                    if other.$field.is_some() {
                        self.$field = other.$field.clone();
                    }
                )*
                self.unknown.extend(
                    other.unknown.iter().map(|(k, v)| (k.clone(), v.clone()))
                );
            }

            /// True when the override sets nothing and carries nothing from
            /// another build.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.unknown.is_empty() $( && self.$field.is_none() )*
            }
        }

        impl crate::settings::macros::OverrideGroup for $override_name {
            fn is_empty(&self) -> bool {
                $override_name::is_empty(self)
            }
        }
    };
}

/// Declares a per-kind settings struct composed of settings groups, together
/// with the matching composition of their override types.
///
/// Both generated types carry an `unknown` map at their own level, so a group
/// a newer build added beside the known ones is preserved as well as an unknown
/// field inside a known group.
macro_rules! settings_composite {
    (
        $(#[$group_doc:meta])*
        $name:ident / $override_name:ident {
            $(
                $(#[$field_doc:meta])*
                $field:ident : $ty:ty => $override_ty:ty
            ),* $(,)?
        }
    ) => {
        $(#[$group_doc])*
        #[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "camelCase", default)]
        pub struct $name {
            $(
                $(#[$field_doc])*
                #[serde(default)]
                pub $field: $ty,
            )*
            /// Groups written by another build, preserved verbatim.
            #[serde(
                flatten,
                skip_serializing_if = "std::collections::BTreeMap::is_empty"
            )]
            pub unknown: std::collections::BTreeMap<String, serde_json::Value>,
        }

        $(#[$group_doc])*
        ///
        /// Override form: each group is itself an override whose unset fields
        /// fall through to the layer below.
        #[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "camelCase", default)]
        pub struct $override_name {
            $(
                $(#[$field_doc])*
                #[serde(
                    default,
                    skip_serializing_if = "crate::settings::macros::group_is_empty"
                )]
                pub $field: $override_ty,
            )*
            /// Groups written by another build, preserved verbatim.
            #[serde(
                flatten,
                skip_serializing_if = "std::collections::BTreeMap::is_empty"
            )]
            pub unknown: std::collections::BTreeMap<String, serde_json::Value>,
        }

        impl $override_name {
            /// Writes every field these overrides set onto `base`.
            pub fn apply_to(&self, base: &mut $name) {
                $( self.$field.apply_to(&mut base.$field); )*
                base.unknown.extend(
                    self.unknown.iter().map(|(k, v)| (k.clone(), v.clone()))
                );
            }

            /// Builds an override that sets every field to the given values.
            #[must_use]
            pub fn from_full(full: &$name) -> Self {
                Self {
                    $( $field: <$override_ty>::from_full(&full.$field), )*
                    unknown: full.unknown.clone(),
                }
            }

            /// Folds `other` into this override; fields set by `other` win.
            pub fn merge(&mut self, other: &Self) {
                $( self.$field.merge(&other.$field); )*
                self.unknown.extend(
                    other.unknown.iter().map(|(k, v)| (k.clone(), v.clone()))
                );
            }

            /// True when the override sets nothing and carries nothing from
            /// another build.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.unknown.is_empty() $( && self.$field.is_empty() )*
            }
        }

        impl crate::settings::macros::OverrideGroup for $override_name {
            fn is_empty(&self) -> bool {
                $override_name::is_empty(self)
            }
        }
    };
}

/// An override type that can report setting nothing.
///
/// A group setting nothing is left out of the document entirely, so a stored
/// override holds only what was actually overridden and a document round trips
/// through this build without gaining empty objects.
pub(crate) trait OverrideGroup {
    /// True when the override sets nothing.
    fn is_empty(&self) -> bool;
}

pub(crate) fn group_is_empty<T: OverrideGroup>(value: &T) -> bool {
    value.is_empty()
}

/// Reads a field that is present into `Some`, so an override whose value is
/// itself optional keeps the difference between a field left unset and a field
/// set to nothing.
pub(crate) fn deserialize_present<'de, T, D>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

pub(crate) use settings_composite;
pub(crate) use settings_group;
