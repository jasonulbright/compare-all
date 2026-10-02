//! The syntax definition a text format carries.
//!
//! A grammar is one ordered list of items. Each item names the element it
//! contributes to, so several items can build up one element — a comment
//! element, for example, usually needs one item for the line form and one for
//! the block form. Order is precedence: an item earlier in the list wins over a
//! later one at the same position in a line.
//!
//! The model is deliberately flat. There is no nesting and no context, so a
//! language whose meaning depends on enclosing structure is approximated rather
//! than parsed.

use crate::compat::{is_empty_map, Extensible, UnknownFields};
use serde::{Deserialize, Serialize};

/// How the text of an item is interpreted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchOptions {
    /// The text matches character case exactly.
    #[serde(default = "crate::grammar::default_true")]
    pub match_character_case: bool,
    /// The text is a regular expression rather than a literal.
    #[serde(default)]
    pub regular_expression: bool,
    /// Unrecognized keys, preserved across a load and save.
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub unknown: UnknownFields,
}

impl MatchOptions {
    /// Case-sensitive literal text.
    pub fn literal() -> Self {
        Self {
            match_character_case: true,
            regular_expression: false,
            unknown: UnknownFields::new(),
        }
    }

    /// Case-insensitive literal text.
    pub fn literal_any_case() -> Self {
        Self {
            match_character_case: false,
            ..Self::literal()
        }
    }

    /// A case-sensitive regular expression.
    pub fn regex() -> Self {
        Self {
            match_character_case: true,
            regular_expression: true,
            unknown: UnknownFields::new(),
        }
    }

    /// A case-insensitive regular expression.
    pub fn regex_any_case() -> Self {
        Self {
            match_character_case: false,
            ..Self::regex()
        }
    }
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self::literal()
    }
}

/// Where a column range item stops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColumnEnd {
    /// The last column the item covers, counted from one and inclusive.
    Column(u32),
    /// The item runs to the end of the line.
    EndOfLine,
}

/// What an item matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "category", rename_all = "camelCase")]
pub enum ItemKind {
    /// One specific string.
    #[serde(rename_all = "camelCase")]
    Basic {
        /// The string to match.
        text: String,
        /// How the string is interpreted.
        #[serde(default)]
        options: MatchOptions,
        /// The match has to start and end on a word boundary.
        #[serde(default = "crate::grammar::default_true")]
        whole_word: bool,
    },
    /// Any one of a list of tokens.
    #[serde(rename_all = "camelCase")]
    List {
        /// The tokens, one per entry.
        tokens: Vec<String>,
        /// How the tokens are interpreted.
        #[serde(default)]
        options: MatchOptions,
        /// The match has to start and end on a word boundary.
        #[serde(default = "crate::grammar::default_true")]
        whole_word: bool,
    },
    /// Text between a starting and an ending delimiter.
    #[serde(rename_all = "camelCase")]
    Delimited {
        /// The delimiter that opens the element.
        start: String,
        /// The delimiter that closes it. Ignored when the item stops at the end
        /// of the line.
        #[serde(default)]
        stop: String,
        /// The element runs to the end of the line instead of to a closing
        /// delimiter.
        #[serde(default)]
        stop_at_end_of_line: bool,
        /// A character that lets the closing delimiter appear inside the
        /// element. The character and the one after it are both consumed.
        #[serde(default)]
        escape: Option<char>,
        /// The element may continue on following lines when its closing
        /// delimiter is not reached. Has no effect together with
        /// `stop_at_end_of_line`.
        #[serde(default = "crate::grammar::default_true")]
        line_spanning: bool,
        /// The element continues only when its escape character immediately
        /// precedes the line ending. Has no effect together with
        /// `stop_at_end_of_line` or `line_spanning`.
        #[serde(default)]
        continue_after_escaped_newline: bool,
        /// How the delimiters are interpreted.
        #[serde(default)]
        options: MatchOptions,
    },
    /// A fixed range of columns.
    #[serde(rename_all = "camelCase")]
    Columns {
        /// The first column the item covers, counted from one.
        start_column: u32,
        /// Where the item stops.
        end: ColumnEnd,
    },
    /// A repeating multi-line block, such as a page heading, recognized by the
    /// text at its top.
    #[serde(rename_all = "camelCase")]
    Lines {
        /// The text marking the top of each block.
        text: String,
        /// The first block starts at line one even without the marker text.
        #[serde(default)]
        or_line_1: bool,
        /// How many lines the block occupies, counting the one that carries the
        /// marker.
        line_count: u32,
        /// How the marker text is interpreted.
        #[serde(default)]
        options: MatchOptions,
    },
}

/// One entry in a grammar's ordered item list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrammarItem {
    /// The element this item contributes to. Several items may name the same
    /// element; the name is what the importance checklist and the color options
    /// list show.
    pub element: String,
    /// What the item matches.
    pub kind: Extensible<ItemKind>,
    /// Character case changes inside this element are important differences.
    #[serde(default)]
    pub case_sensitive: bool,
    /// Raises the alignment algorithm's preference for pairing lines that carry
    /// this element.
    #[serde(default)]
    pub line_weight: i32,
    /// Unrecognized keys, preserved across a load and save.
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub unknown: UnknownFields,
}

impl GrammarItem {
    /// An item contributing to `element`.
    pub fn new(element: impl Into<String>, kind: ItemKind) -> Self {
        Self {
            element: element.into(),
            kind: Extensible::Known(kind),
            case_sensitive: false,
            line_weight: 0,
            unknown: UnknownFields::new(),
        }
    }

    /// The same item, marked case sensitive for comparison purposes.
    #[must_use]
    pub fn case_sensitive(mut self) -> Self {
        self.case_sensitive = true;
        self
    }

    /// The same item with a line weight.
    #[must_use]
    pub fn with_line_weight(mut self, weight: i32) -> Self {
        self.line_weight = weight;
        self
    }
}

/// An ordered list of grammar items.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grammar {
    /// The items, highest precedence first.
    #[serde(default)]
    pub items: Vec<GrammarItem>,
    /// Unrecognized keys, preserved across a load and save.
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub unknown: UnknownFields,
}

impl Grammar {
    /// A grammar with no items, under which every line is unclaimed text.
    pub fn empty() -> Self {
        Self::default()
    }

    /// A grammar built from items in precedence order.
    pub fn from_items(items: Vec<GrammarItem>) -> Self {
        Self {
            items,
            unknown: UnknownFields::new(),
        }
    }

    /// Element names in first-appearance order, each named once.
    ///
    /// This is the order the importance checklist and the color options list
    /// present, so it has to follow the item list rather than sort.
    pub fn element_names(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for item in &self.items {
            if !seen.iter().any(|n: &String| n == &item.element) {
                seen.push(item.element.clone());
            }
        }
        seen
    }

    /// The largest line weight among items naming `element`.
    pub fn line_weight_of(&self, element: &str) -> i32 {
        self.items
            .iter()
            .filter(|i| i.element == element)
            .map(|i| i.line_weight)
            .max()
            .unwrap_or(0)
    }

    /// Whether any item naming `element` marks it case sensitive.
    pub fn is_case_sensitive(&self, element: &str) -> bool {
        self.items
            .iter()
            .any(|i| i.element == element && i.case_sensitive)
    }
}

pub(crate) const fn default_true() -> bool {
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn element_names_follow_item_order_and_appear_once() {
        let grammar = Grammar::from_items(vec![
            GrammarItem::new(
                "Comment",
                ItemKind::Delimited {
                    start: "//".into(),
                    stop: String::new(),
                    stop_at_end_of_line: true,
                    escape: None,
                    line_spanning: false,
                    continue_after_escaped_newline: false,
                    options: MatchOptions::literal(),
                },
            ),
            GrammarItem::new(
                "Keyword",
                ItemKind::List {
                    tokens: vec!["if".into()],
                    options: MatchOptions::literal(),
                    whole_word: true,
                },
            ),
            GrammarItem::new(
                "Comment",
                ItemKind::Delimited {
                    start: "/*".into(),
                    stop: "*/".into(),
                    stop_at_end_of_line: false,
                    escape: None,
                    line_spanning: true,
                    continue_after_escaped_newline: false,
                    options: MatchOptions::literal(),
                },
            ),
        ]);
        assert_eq!(grammar.element_names(), vec!["Comment", "Keyword"]);
    }

    #[test]
    fn line_weight_and_case_sensitivity_read_back_per_element() {
        let grammar = Grammar::from_items(vec![
            GrammarItem::new(
                "Keyword",
                ItemKind::List {
                    tokens: vec!["fn".into()],
                    options: MatchOptions::literal(),
                    whole_word: true,
                },
            )
            .with_line_weight(3)
            .case_sensitive(),
            GrammarItem::new(
                "Keyword",
                ItemKind::List {
                    tokens: vec!["let".into()],
                    options: MatchOptions::literal(),
                    whole_word: true,
                },
            )
            .with_line_weight(7),
        ]);
        assert_eq!(grammar.line_weight_of("Keyword"), 7);
        assert!(grammar.is_case_sensitive("Keyword"));
        assert_eq!(grammar.line_weight_of("Missing"), 0);
    }

    #[test]
    fn an_unknown_item_category_survives_a_round_trip() {
        let text = r#"{"items":[{"element":"Future","kind":{"category":"tomorrow","n":2}}]}"#;
        let grammar: Grammar = serde_json::from_str(text).unwrap();
        assert!(!grammar.items[0].kind.is_known());
        let written = serde_json::to_string(&grammar).unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(reparsed["items"][0]["kind"]["category"], "tomorrow");
        assert_eq!(reparsed["items"][0]["kind"]["n"], 2);
    }

    #[test]
    fn unknown_keys_on_an_item_survive_a_round_trip() {
        let text = r#"{"items":[{"element":"E","kind":{"category":"basic","text":"x"},"futureFlag":true}]}"#;
        let grammar: Grammar = serde_json::from_str(text).unwrap();
        let written = serde_json::to_string(&grammar).unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(reparsed["items"][0]["futureFlag"], true);
        assert_eq!(reparsed["items"][0]["kind"]["text"], "x");
    }
}
