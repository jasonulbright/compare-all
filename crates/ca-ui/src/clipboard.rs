//! Formatting helpers for text copied between comparison views.

/// Quote a tab separated field when its text contains a field or row break.
#[must_use]
pub fn table_field(value: &str) -> String {
    if value.contains(['\t', '\r', '\n', '"']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::table_field;

    #[test]
    fn quotes_fields_that_would_split_a_tab_separated_copy() {
        assert_eq!(table_field("plain"), "plain");
        assert_eq!(table_field("a\tb"), "\"a\tb\"");
        assert_eq!(table_field("a\n\"b\""), "\"a\n\"\"b\"\"\"");
    }
}
