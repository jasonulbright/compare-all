//! Registry value types and their data.

use crate::bytes::hex_display;
use crate::error::Result;
use crate::limits::Limits;
use crate::record::RecordValue;
use serde::{Deserialize, Serialize};

/// Longest binary payload rendered byte by byte in a display form.
const HEX_DISPLAY_LIMIT: usize = 64;

/// The type code a registry value carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ValueKind {
    /// No data.
    None,
    /// Text.
    Sz,
    /// Text holding environment references.
    ExpandSz,
    /// Opaque bytes.
    Binary,
    /// 32 bit number, little endian in the file form.
    Dword,
    /// 32 bit number, big endian in the file form.
    DwordBigEndian,
    /// A symbolic link target.
    Link,
    /// A list of strings.
    MultiSz,
    /// A hardware resource list.
    ResourceList,
    /// A full hardware resource descriptor.
    FullResourceDescriptor,
    /// A hardware resource requirements list.
    ResourceRequirementsList,
    /// 64 bit number.
    Qword,
    /// A type code this build does not name.
    Other(u32),
}

impl ValueKind {
    /// The numeric code used in a `.reg` file and by the operating system.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Sz => 1,
            Self::ExpandSz => 2,
            Self::Binary => 3,
            Self::Dword => 4,
            Self::DwordBigEndian => 5,
            Self::Link => 6,
            Self::MultiSz => 7,
            Self::ResourceList => 8,
            Self::FullResourceDescriptor => 9,
            Self::ResourceRequirementsList => 10,
            Self::Qword => 11,
            Self::Other(code) => code,
        }
    }

    /// The type for a numeric code.
    #[must_use]
    pub const fn from_code(code: u32) -> Self {
        match code {
            0 => Self::None,
            1 => Self::Sz,
            2 => Self::ExpandSz,
            3 => Self::Binary,
            4 => Self::Dword,
            5 => Self::DwordBigEndian,
            6 => Self::Link,
            7 => Self::MultiSz,
            8 => Self::ResourceList,
            9 => Self::FullResourceDescriptor,
            10 => Self::ResourceRequirementsList,
            11 => Self::Qword,
            other => Self::Other(other),
        }
    }

    /// The name shown in a listing.
    #[must_use]
    pub fn name(self) -> String {
        match self {
            Self::None => "REG_NONE".to_owned(),
            Self::Sz => "REG_SZ".to_owned(),
            Self::ExpandSz => "REG_EXPAND_SZ".to_owned(),
            Self::Binary => "REG_BINARY".to_owned(),
            Self::Dword => "REG_DWORD".to_owned(),
            Self::DwordBigEndian => "REG_DWORD_BIG_ENDIAN".to_owned(),
            Self::Link => "REG_LINK".to_owned(),
            Self::MultiSz => "REG_MULTI_SZ".to_owned(),
            Self::ResourceList => "REG_RESOURCE_LIST".to_owned(),
            Self::FullResourceDescriptor => "REG_FULL_RESOURCE_DESCRIPTOR".to_owned(),
            Self::ResourceRequirementsList => "REG_RESOURCE_REQUIREMENTS_LIST".to_owned(),
            Self::Qword => "REG_QWORD".to_owned(),
            Self::Other(code) => format!("REG_UNKNOWN({code})"),
        }
    }
}

/// The data of one registry value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueData {
    /// No data.
    None(Vec<u8>),
    /// Text.
    Sz(String),
    /// Text holding environment references.
    ExpandSz(String),
    /// Opaque bytes.
    Binary(Vec<u8>),
    /// 32 bit number.
    Dword(u32),
    /// 32 bit number stored big endian in the file form.
    DwordBigEndian(u32),
    /// A symbolic link target, stored as bytes.
    Link(Vec<u8>),
    /// A list of strings.
    MultiSz(Vec<String>),
    /// 64 bit number.
    Qword(u64),
    /// Bytes of a type this build does not name.
    Other {
        /// Numeric type code.
        kind: u32,
        /// Raw payload.
        bytes: Vec<u8>,
    },
    /// A decoded value whose typed form cannot reproduce its original bytes.
    ///
    /// The decoded form is retained for display, while comparisons and writes
    /// use the original bytes so malformed or extended values are not lost.
    Preserved {
        /// Registry value type.
        kind: ValueKind,
        /// Registry-form payload used for comparison and live writes.
        bytes: Vec<u8>,
        /// Best-effort typed form used for display.
        decoded: Box<ValueData>,
        /// Original REGEDIT4 payload, when the value was read from a legacy file.
        legacy_v4_bytes: Option<Vec<u8>>,
    },
}

impl ValueData {
    /// The type of this data.
    #[must_use]
    pub const fn kind(&self) -> ValueKind {
        match self {
            Self::None(_) => ValueKind::None,
            Self::Sz(_) => ValueKind::Sz,
            Self::ExpandSz(_) => ValueKind::ExpandSz,
            Self::Binary(_) => ValueKind::Binary,
            Self::Dword(_) => ValueKind::Dword,
            Self::DwordBigEndian(_) => ValueKind::DwordBigEndian,
            Self::Link(_) => ValueKind::Link,
            Self::MultiSz(_) => ValueKind::MultiSz,
            Self::Qword(_) => ValueKind::Qword,
            Self::Other { kind, .. } => ValueKind::from_code(*kind),
            Self::Preserved { kind, .. } => *kind,
        }
    }

    /// Build data of `kind` from its raw registry bytes.
    ///
    /// Text types decode from little endian UTF-16. A payload that is not a
    /// whole number of UTF-16 units keeps its trailing byte out of the text.
    ///
    /// # Errors
    ///
    /// Returns [`RecordError::LimitExceeded`] when the payload is over
    /// [`Limits::max_value_bytes`].
    pub fn from_raw(kind: ValueKind, bytes: &[u8], limits: &Limits) -> Result<Self> {
        limits.check_value(crate::bytes::to_u64(bytes.len()))?;
        let decoded = match kind {
            ValueKind::None => Self::None(bytes.to_vec()),
            ValueKind::Sz => Self::Sz(decode_utf16(bytes)),
            ValueKind::ExpandSz => Self::ExpandSz(decode_utf16(bytes)),
            ValueKind::Link => Self::Link(bytes.to_vec()),
            ValueKind::MultiSz => Self::MultiSz(decode_multi(bytes)),
            ValueKind::Dword => Self::Dword(read_u32_le(bytes)),
            ValueKind::DwordBigEndian => Self::DwordBigEndian(read_u32_be(bytes)),
            ValueKind::Qword => Self::Qword(read_u64_le(bytes)),
            ValueKind::Binary => Self::Binary(bytes.to_vec()),
            ValueKind::ResourceList
            | ValueKind::FullResourceDescriptor
            | ValueKind::ResourceRequirementsList => Self::Other {
                kind: kind.code(),
                bytes: bytes.to_vec(),
            },
            ValueKind::Other(code) => Self::Other {
                kind: code,
                bytes: bytes.to_vec(),
            },
        };
        if decoded.to_raw() == bytes {
            Ok(decoded)
        } else {
            Ok(Self::Preserved {
                kind,
                bytes: bytes.to_vec(),
                decoded: Box::new(decoded),
                legacy_v4_bytes: None,
            })
        }
    }

    /// The raw registry bytes of this data.
    #[must_use]
    pub fn to_raw(&self) -> Vec<u8> {
        match self {
            Self::None(bytes)
            | Self::Binary(bytes)
            | Self::Link(bytes)
            | Self::Other { bytes, .. }
            | Self::Preserved { bytes, .. } => bytes.clone(),
            Self::Sz(text) | Self::ExpandSz(text) => encode_utf16_z(text),
            Self::MultiSz(items) => encode_multi(items),
            Self::Dword(value) => value.to_le_bytes().to_vec(),
            Self::DwordBigEndian(value) => value.to_be_bytes().to_vec(),
            Self::Qword(value) => value.to_le_bytes().to_vec(),
        }
    }

    /// The single line rendering shown in a listing.
    #[must_use]
    pub fn to_display(&self) -> String {
        match self {
            Self::Sz(text) | Self::ExpandSz(text) => text.clone(),
            Self::MultiSz(items) => items.join(" | "),
            Self::Dword(value) | Self::DwordBigEndian(value) => {
                format!("0x{value:08x} ({value})")
            }
            Self::Qword(value) => format!("0x{value:016x} ({value})"),
            Self::Preserved {
                kind,
                bytes,
                decoded,
                ..
            } => format!(
                "{} [raw {}: {}]",
                decoded.to_display(),
                kind.name(),
                hex_display(bytes, HEX_DISPLAY_LIMIT)
            ),
            Self::None(bytes)
            | Self::Binary(bytes)
            | Self::Link(bytes)
            | Self::Other { bytes, .. } => hex_display(bytes, HEX_DISPLAY_LIMIT),
        }
    }

    /// The typed value a record carries.
    #[must_use]
    pub fn to_record_value(&self) -> RecordValue {
        match self {
            Self::Sz(text) | Self::ExpandSz(text) => RecordValue::Text(text.clone()),
            Self::MultiSz(items) => RecordValue::TextList(items.clone()),
            Self::Dword(value) | Self::DwordBigEndian(value) => {
                RecordValue::Integer(i128::from(*value))
            }
            Self::Qword(value) => RecordValue::Integer(i128::from(*value)),
            Self::None(bytes)
            | Self::Binary(bytes)
            | Self::Link(bytes)
            | Self::Other { bytes, .. }
            | Self::Preserved { bytes, .. } => RecordValue::Bytes(bytes.clone()),
        }
    }
}

/// How a registry value name is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum ValueName {
    /// The unnamed value of a key, written `@` in a file.
    Default,
    /// A named value.
    Named {
        /// The name as written.
        name: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: crate::limits::Unknown,
    },
}

impl ValueName {
    /// The name shown in a listing. The default value shows as `(Default)`.
    #[must_use]
    pub fn display(&self) -> &str {
        match self {
            Self::Default => "(Default)",
            Self::Named { name, .. } => name,
        }
    }

    /// The name the operating system uses. The default value is the empty name.
    #[must_use]
    pub fn raw(&self) -> &str {
        match self {
            Self::Default => "",
            Self::Named { name, .. } => name,
        }
    }

    /// Build a name from the operating system form.
    #[must_use]
    pub fn from_raw(name: &str) -> Self {
        if name.is_empty() {
            Self::Default
        } else {
            Self::Named {
                name: name.to_owned(),
                unknown: crate::limits::Unknown::new(),
            }
        }
    }
}

fn decode_utf16(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0usize;
    while index + 1 < bytes.len() {
        let unit = u16::from_le_bytes([bytes[index], bytes[index + 1]]);
        index += 2;
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    String::from_utf16_lossy(&units)
}

fn decode_multi(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut units = Vec::new();
    let mut index = 0usize;
    while index + 1 < bytes.len() {
        let unit = u16::from_le_bytes([bytes[index], bytes[index + 1]]);
        index += 2;
        if unit == 0 {
            if units.is_empty() {
                break;
            }
            out.push(String::from_utf16_lossy(&units));
            units.clear();
            continue;
        }
        units.push(unit);
    }
    if !units.is_empty() {
        out.push(String::from_utf16_lossy(&units));
    }
    out
}

fn encode_utf16_z(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 2 + 2);
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out
}

fn encode_multi(items: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for item in items {
        for unit in item.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    let mut raw = [0u8; 4];
    for (slot, byte) in raw.iter_mut().zip(bytes.iter()) {
        *slot = *byte;
    }
    u32::from_le_bytes(raw)
}

fn read_u32_be(bytes: &[u8]) -> u32 {
    let mut raw = [0u8; 4];
    for (slot, byte) in raw.iter_mut().zip(bytes.iter()) {
        *slot = *byte;
    }
    u32::from_be_bytes(raw)
}

fn read_u64_le(bytes: &[u8]) -> u64 {
    let mut raw = [0u8; 8];
    for (slot, byte) in raw.iter_mut().zip(bytes.iter()) {
        *slot = *byte;
    }
    u64::from_le_bytes(raw)
}
