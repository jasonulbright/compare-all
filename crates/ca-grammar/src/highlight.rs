//! Element names mapped onto a small set of color roles.
//!
//! The crate names roles, not colors. A view owns the palette and the bold and
//! italic choices; this layer only says which role a run of text plays, so the
//! grammar layer stays free of any toolkit dependency.

use crate::compat::{is_empty_map, Extensible, UnknownFields};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A color role a run of text can play.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StyleSlot {
    /// Text no element claims.
    Plain,
    /// Whitespace, which a view may choose to reveal.
    Whitespace,
    /// A comment in any of its forms.
    Comment,
    /// A string or character literal.
    Literal,
    /// A numeric literal.
    Number,
    /// A reserved word of the language.
    Keyword,
    /// A user-chosen name.
    Identifier,
    /// A preprocessor or build-time directive.
    Directive,
    /// Punctuation that carries meaning, such as an operator.
    Operator,
    /// The name of a markup tag.
    Tag,
    /// The name of a markup or configuration attribute or key.
    Attribute,
    /// A named division of a file, such as a configuration section heading.
    Section,
    /// A repeating multi-line block such as a page heading.
    Block,
    /// An element the palette has no specific role for.
    Other,
}

impl StyleSlot {
    /// A role guessed from an element name.
    ///
    /// The guess exists so a user-authored element gets a sensible color the
    /// moment it is created, without an entry having to be added anywhere.
    /// Matching ignores character case and any separators in the name.
    pub fn guess_from_element(element: &str) -> Self {
        let folded: String = element
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        let has = |needle: &str| folded.contains(needle);
        if has("comment") || has("remark") || has("docstring") {
            Self::Comment
        } else if has("number") || has("numeric") || has("float") || has("integer") {
            Self::Number
        } else if has("color")
            || has("string")
            || has("literal")
            || has("char")
            || has("code")
            || has("link")
            || has("url")
        {
            Self::Literal
        } else if has("keyword") || has("reserved") {
            Self::Keyword
        } else if has("preprocessor") || has("directive") || has("pragma") {
            Self::Directive
        } else if has("identifier") || has("name") || has("symbol") || has("variable") {
            Self::Identifier
        } else if has("operator") || has("punctuation") {
            Self::Operator
        } else if has("tag") || has("element") {
            Self::Tag
        } else if has("attribute") || has("property") || has("key") {
            Self::Attribute
        } else if has("section") || has("heading") || has("header") {
            Self::Section
        } else if has("pageblock") || has("block") {
            Self::Block
        } else {
            Self::Other
        }
    }
}

/// Element name to color role assignments for one format.
///
/// Only the assignments that differ from the guess have to be stored, so a
/// format that names its elements conventionally needs no entries at all.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleMap {
    /// Explicit assignments, keyed by element name.
    #[serde(default)]
    pub slots: BTreeMap<String, Extensible<StyleSlot>>,
    /// Unrecognized keys, preserved across a load and save.
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub unknown: UnknownFields,
}

impl StyleMap {
    /// An empty map, under which every element falls back to the guess.
    pub fn new() -> Self {
        Self::default()
    }

    /// Assign `slot` to `element`.
    pub fn set(&mut self, element: impl Into<String>, slot: StyleSlot) {
        self.slots.insert(element.into(), Extensible::Known(slot));
    }

    /// The role for `element`.
    pub fn slot_for(&self, element: &str) -> StyleSlot {
        self.slots
            .get(element)
            .and_then(Extensible::known)
            .copied()
            .unwrap_or_else(|| StyleSlot::guess_from_element(element))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn conventional_names_guess_their_role() {
        assert_eq!(
            StyleSlot::guess_from_element("Line Comment"),
            StyleSlot::Comment
        );
        assert_eq!(
            StyleSlot::guess_from_element("literal_string"),
            StyleSlot::Literal
        );
        assert_eq!(StyleSlot::guess_from_element("Keyword"), StyleSlot::Keyword);
        assert_eq!(StyleSlot::guess_from_element("Number"), StyleSlot::Number);
        assert_eq!(
            StyleSlot::guess_from_element("Preprocessor"),
            StyleSlot::Directive
        );
        assert_eq!(StyleSlot::guess_from_element("Wombat"), StyleSlot::Other);
    }

    #[test]
    fn an_explicit_assignment_beats_the_guess() {
        let mut map = StyleMap::new();
        map.set("Wombat", StyleSlot::Keyword);
        assert_eq!(map.slot_for("Wombat"), StyleSlot::Keyword);
        assert_eq!(map.slot_for("Comment"), StyleSlot::Comment);
    }

    #[test]
    fn an_unknown_role_falls_back_to_the_guess_and_is_written_back() {
        let map: StyleMap = serde_json::from_str(r#"{"slots":{"Comment":"fuchsia"}}"#).unwrap();
        assert_eq!(map.slot_for("Comment"), StyleSlot::Comment);
        let text = serde_json::to_string(&map).unwrap();
        assert!(text.contains("fuchsia"));
    }
}
