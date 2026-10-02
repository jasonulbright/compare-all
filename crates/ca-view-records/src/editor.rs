//! The value editor: the text form of a registry value and its parse back.
//!
//! Text types edit as text, a string list as one item per line, the number
//! types as a decimal or `0x` hexadecimal number, and every other type as
//! hexadecimal bytes. A parse that fails returns a message for the editor to
//! show; nothing here panics on what the user typed.

use ca_records::registry::{ValueData, ValueKind, ValueName};

/// The types the editor offers for a new value, in the order it lists them.
pub const OFFERED: [ValueKind; 7] = [
    ValueKind::Sz,
    ValueKind::ExpandSz,
    ValueKind::MultiSz,
    ValueKind::Dword,
    ValueKind::Qword,
    ValueKind::Binary,
    ValueKind::None,
];

/// What the editor holds while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueForm {
    /// Name of the value.
    pub name: String,
    /// True when the name cannot change here: Modify edits type and data.
    pub name_fixed: bool,
    /// The type chosen.
    pub kind: ValueKind,
    /// The data in its text form.
    pub text: String,
}

impl ValueForm {
    /// An empty text value named `name`.
    #[must_use]
    pub fn new_value(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            name_fixed: false,
            kind: ValueKind::Sz,
            text: String::new(),
        }
    }

    /// The form of an existing value.
    #[must_use]
    pub fn existing(name: &ValueName, data: &ValueData) -> Self {
        Self {
            name: name.display().to_owned(),
            name_fixed: true,
            kind: data.kind(),
            text: editable_text(data),
        }
    }

    /// The types the type control lists: the offered ones, and the value's
    /// own type when it is not among them.
    #[must_use]
    pub fn kinds(&self) -> Vec<ValueKind> {
        let mut kinds = OFFERED.to_vec();
        if !kinds.contains(&self.kind) {
            kinds.push(self.kind);
        }
        kinds
    }

    /// The name the form states, as the registry stores it.
    ///
    /// # Errors
    ///
    /// Returns a message for an empty name on a new value.
    pub fn value_name(&self) -> Result<ValueName, String> {
        if self.name_fixed {
            return Ok(from_display(&self.name));
        }
        if self.name.is_empty() {
            return Err("A new value needs a name.".to_owned());
        }
        Ok(ValueName::from_raw(&self.name))
    }

    /// The data the form states.
    ///
    /// # Errors
    ///
    /// Returns a message that names what is wrong with the text.
    pub fn data(&self) -> Result<ValueData, String> {
        parse_data(self.kind, &self.text)
    }
}

/// The value name a listing row shows, as the registry stores it.
#[must_use]
pub fn from_display(name: &str) -> ValueName {
    if name == ValueName::Default.display() {
        ValueName::Default
    } else {
        ValueName::from_raw(name)
    }
}

/// The text form of `data` the editor starts from.
#[must_use]
pub fn editable_text(data: &ValueData) -> String {
    match data {
        ValueData::Preserved { decoded, .. } => editable_text(decoded),
        ValueData::Sz(text) | ValueData::ExpandSz(text) => text.clone(),
        ValueData::MultiSz(items) => items.join("\n"),
        ValueData::Dword(value) | ValueData::DwordBigEndian(value) => value.to_string(),
        ValueData::Qword(value) => value.to_string(),
        ValueData::None(bytes)
        | ValueData::Binary(bytes)
        | ValueData::Link(bytes)
        | ValueData::Other { bytes, .. } => hex_text(bytes),
    }
}

/// Bytes as space separated pairs of hexadecimal digits.
#[must_use]
pub fn hex_text(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse the text of the editor as data of `kind`.
///
/// # Errors
///
/// Returns a message for a number that does not parse or does not fit, for
/// malformed hexadecimal text, and for an empty item in a string list.
pub fn parse_data(kind: ValueKind, text: &str) -> Result<ValueData, String> {
    Ok(match kind {
        ValueKind::Sz => ValueData::Sz(text.to_owned()),
        ValueKind::ExpandSz => ValueData::ExpandSz(text.to_owned()),
        ValueKind::MultiSz => ValueData::MultiSz(parse_lines(text)?),
        ValueKind::Dword => ValueData::Dword(parse_u32(text)?),
        ValueKind::DwordBigEndian => ValueData::DwordBigEndian(parse_u32(text)?),
        ValueKind::Qword => ValueData::Qword(parse_number(text, u64::MAX)?),
        ValueKind::None => ValueData::None(parse_hex(text)?),
        ValueKind::Binary => ValueData::Binary(parse_hex(text)?),
        ValueKind::Link => ValueData::Link(parse_hex(text)?),
        ValueKind::ResourceList
        | ValueKind::FullResourceDescriptor
        | ValueKind::ResourceRequirementsList
        | ValueKind::Other(_) => ValueData::Other {
            kind: kind.code(),
            bytes: parse_hex(text)?,
        },
    })
}

fn parse_lines(text: &str) -> Result<Vec<String>, String> {
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if let Some(index) = lines.iter().position(String::is_empty) {
        return Err(format!(
            "Line {} is empty. A string list cannot hold an empty item.",
            index + 1
        ));
    }
    Ok(lines)
}

fn parse_u32(text: &str) -> Result<u32, String> {
    let value = parse_number(text, u64::from(u32::MAX))?;
    u32::try_from(value).map_err(|_| format!("{value} does not fit in 32 bits."))
}

/// A decimal number, or a hexadecimal one after `0x`, at most `max`.
fn parse_number(text: &str, max: u64) -> Result<u64, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Type a number.".to_owned());
    }
    let parsed = match trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        Some(digits) => u64::from_str_radix(digits, 16),
        None => trimmed.parse::<u64>(),
    };
    let value = parsed.map_err(|_| {
        format!("{trimmed} is not a number. Type a decimal number or 0x and hexadecimal digits.")
    })?;
    if value > max {
        return Err(format!("{value} is larger than {max}."));
    }
    Ok(value)
}

/// Bytes written as hexadecimal digits.
///
/// Spaces and commas separate groups. A group holds pairs of digits, so an
/// odd group is refused rather than silently read as half a byte.
///
/// # Errors
///
/// Returns a message that names the first character or group that is wrong.
pub fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for (index, group) in text
        .split(|character: char| character.is_whitespace() || character == ',')
        .filter(|group| !group.is_empty())
        .enumerate()
    {
        if let Some(bad) = group
            .chars()
            .find(|character| !character.is_ascii_hexdigit())
        {
            return Err(format!(
                "{bad:?} is not a hexadecimal digit (group {}).",
                index + 1
            ));
        }
        if group.len() % 2 != 0 {
            return Err(format!(
                "Group {} ({group}) has an odd number of digits. Write each byte as two digits.",
                index + 1
            ));
        }
        for pair in group.as_bytes().chunks(2) {
            let digits = std::str::from_utf8(pair).unwrap_or_default();
            let byte = u8::from_str_radix(digits, 16)
                .map_err(|_| format!("{digits} is not a byte (group {}).", index + 1))?;
            out.push(byte);
        }
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{editable_text, from_display, parse_data, parse_hex, ValueForm};
    use ca_records::registry::{ValueData, ValueKind, ValueName};

    #[test]
    fn each_type_round_trips_through_its_text_form() {
        for data in [
            ValueData::Sz("text".into()),
            ValueData::ExpandSz("%PATH%".into()),
            ValueData::MultiSz(vec!["a".into(), "b".into()]),
            ValueData::Dword(42),
            ValueData::DwordBigEndian(7),
            ValueData::Qword(u64::MAX),
            ValueData::Binary(vec![0, 0xab, 0xff]),
            ValueData::None(Vec::new()),
            ValueData::Other {
                kind: 99,
                bytes: vec![1],
            },
        ] {
            let text = editable_text(&data);
            assert_eq!(parse_data(data.kind(), &text).unwrap(), data, "{text}");
        }
    }

    #[test]
    fn malformed_hex_is_refused_with_a_message() {
        assert_eq!(parse_hex("01,0a ff 0102").unwrap(), [1, 10, 255, 1, 2]);
        assert!(parse_hex("").unwrap().is_empty());
        assert!(parse_hex("0g").unwrap_err().contains("'g'"));
        assert!(parse_hex("1 02").unwrap_err().contains("odd"));
        assert!(parse_hex("é1").is_err());
    }

    #[test]
    fn a_number_that_does_not_fit_or_parse_is_refused() {
        assert_eq!(
            parse_data(ValueKind::Dword, "0x10").unwrap(),
            ValueData::Dword(16)
        );
        assert!(parse_data(ValueKind::Dword, "4294967296").is_err());
        assert!(parse_data(ValueKind::Dword, "-1").is_err());
        assert!(parse_data(ValueKind::Qword, "").is_err());
        assert!(parse_data(ValueKind::Qword, "0xZZ").is_err());
    }

    #[test]
    fn a_string_list_takes_one_item_per_line_and_refuses_an_empty_one() {
        assert_eq!(
            parse_data(ValueKind::MultiSz, "a\r\nb\n").unwrap(),
            ValueData::MultiSz(vec!["a".into(), "b".into()])
        );
        assert!(parse_data(ValueKind::MultiSz, "a\n\nb").is_err());
    }

    #[test]
    fn the_default_value_keeps_its_empty_name() {
        assert_eq!(from_display("(Default)"), ValueName::Default);
        let form = ValueForm::existing(&ValueName::Default, &ValueData::Sz("x".into()));
        assert_eq!(form.value_name().unwrap(), ValueName::Default);
        let unnamed = ValueForm::new_value("");
        assert!(unnamed.value_name().is_err());
        let odd = ValueForm::existing(&ValueName::from_raw("L"), &ValueData::Link(vec![1]));
        assert!(odd.kinds().contains(&ValueKind::Link));
    }
}
