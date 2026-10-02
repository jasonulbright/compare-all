//! ID3 version 1 and version 2 tags.

use crate::bytes::{slice, to_u64, utf16_be_string, utf16_le_string};
use crate::error::Result;
use crate::limits::{Limits, RecordBudget};
use crate::record::ByteRange;

/// Size of an ID3 version 1 tag.
const V1_SIZE: usize = 128;
/// Size of an ID3 version 2 header.
const V2_HEADER: usize = 10;

/// One tag field read from a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagField {
    /// Identifier as the file writes it.
    pub id: String,
    /// Readable name of the field.
    pub name: String,
    /// Text of the field, for a text field.
    pub text: Option<String>,
    /// Raw payload, for a binary field.
    pub binary: Option<Vec<u8>>,
    /// Range of the field in the file.
    pub source: ByteRange,
}

/// An ID3 version 1 tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Id3v1 {
    /// Fields in a fixed order.
    pub fields: Vec<TagField>,
    /// Range of the tag in the file.
    pub source: ByteRange,
}

/// An ID3 version 2 tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Id3v2 {
    /// Major version, 2 to 4.
    pub major: u8,
    /// Revision number.
    pub revision: u8,
    /// Frames in file order.
    pub frames: Vec<TagField>,
    /// Range of the whole tag in the file, header included.
    pub source: ByteRange,
}

/// Read an ID3 version 1 tag from the end of a file.
#[must_use]
pub fn read_v1(data: &[u8]) -> Option<Id3v1> {
    if data.len() < V1_SIZE {
        return None;
    }
    let start = data.len() - V1_SIZE;
    let tag = data.get(start..)?;
    if tag.get(..3)? != b"TAG" {
        return None;
    }
    let mut fields = Vec::new();
    let mut add = |id: &str, name: &str, at: usize, len: usize| {
        let raw = tag.get(at..at + len).unwrap_or_default();
        fields.push(TagField {
            id: id.to_owned(),
            name: name.to_owned(),
            text: Some(latin1(raw).trim_end_matches(['\0', ' ']).to_owned()),
            binary: None,
            source: ByteRange::new(to_u64(start + at), to_u64(len)),
        });
    };
    add("TIT2", "Title", 3, 30);
    add("TPE1", "Artist", 33, 30);
    add("TALB", "Album", 63, 30);
    add("TYER", "Year", 93, 4);
    // A version 1.1 tag ends the comment early and puts the track number in the
    // last byte, so a zero at that position marks the shorter comment.
    let has_track =
        tag.get(125).copied().unwrap_or(0) == 0 && tag.get(126).copied().unwrap_or(0) != 0;
    if has_track {
        add("COMM", "Comment", 97, 28);
        let number = tag.get(126).copied().unwrap_or(0);
        fields.push(TagField {
            id: "TRCK".to_owned(),
            name: "Track".to_owned(),
            text: Some(number.to_string()),
            binary: None,
            source: ByteRange::new(to_u64(start + 126), 1),
        });
    } else {
        add("COMM", "Comment", 97, 30);
    }
    let genre = tag.get(127).copied().unwrap_or(0);
    fields.push(TagField {
        id: "TCON".to_owned(),
        name: "Genre".to_owned(),
        text: Some(genre_name(genre)),
        binary: None,
        source: ByteRange::new(to_u64(start + 127), 1),
    });
    Some(Id3v1 {
        fields,
        source: ByteRange::new(to_u64(start), to_u64(V1_SIZE)),
    })
}

/// Length of the ID3 version 2 tag at the start of a file, header included.
#[must_use]
pub fn v2_span(data: &[u8]) -> Option<(usize, usize)> {
    let header = data.get(..V2_HEADER)?;
    if header.get(..3)? != b"ID3" {
        return None;
    }
    let size = syncsafe(header.get(6..10)?)?;
    let footer = if header.get(5).copied().unwrap_or(0) & 0x10 != 0 {
        10
    } else {
        0
    };
    Some((0, V2_HEADER + size + footer))
}

/// Read an ID3 version 2 tag from the start of a file.
///
/// # Errors
///
/// Returns [`RecordError::Truncated`] when the tag claims more bytes than the
/// file holds, and [`RecordError::LimitExceeded`] when a frame is over a limit.
pub fn read_v2(data: &[u8], limits: &Limits) -> Result<Option<Id3v2>> {
    let Some(header) = data.get(..V2_HEADER) else {
        return Ok(None);
    };
    if header.get(..3) != Some(b"ID3".as_slice()) {
        return Ok(None);
    }
    let major = header[3];
    let revision = header[4];
    let flags = header[5];
    let Some(size) = syncsafe(&header[6..10]) else {
        return Ok(None);
    };
    limits.check_value(to_u64(size))?;
    let body = slice(data, V2_HEADER, size, "id3v2 tag")?;
    // An unsynchronised tag hides frame sync patterns behind a zero byte; the
    // frame walk needs the original bytes back.
    let owned;
    let mut body = body;
    if flags & 0x80 != 0 {
        owned = unsynchronise(body);
        body = &owned;
    }
    let mut cursor = 0usize;
    if flags & 0x40 != 0 && major >= 3 {
        let extended = if major == 3 {
            body.get(..4)
                .map(|raw| u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize + 4)
        } else {
            body.get(..4).and_then(syncsafe)
        };
        cursor = extended.unwrap_or(0).min(body.len());
    }

    let mut budget = RecordBudget::new(limits);
    let mut frames = Vec::new();
    let id_len = if major <= 2 { 3 } else { 4 };
    let header_len = if major <= 2 { 6 } else { 10 };
    while cursor + header_len <= body.len() {
        let Some(raw) = body.get(cursor..cursor + header_len) else {
            break;
        };
        if raw.iter().take(id_len).all(|byte| *byte == 0) {
            break;
        }
        let id = latin1(raw.get(..id_len).unwrap_or_default());
        if !id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            break;
        }
        let size = if major <= 2 {
            usize::from(raw[3]) << 16 | usize::from(raw[4]) << 8 | usize::from(raw[5])
        } else if major >= 4 {
            match syncsafe(raw.get(4..8).unwrap_or_default()) {
                Some(value) => value,
                None => break,
            }
        } else {
            let part = raw.get(4..8).unwrap_or_default();
            if part.len() < 4 {
                break;
            }
            u32::from_be_bytes([part[0], part[1], part[2], part[3]]) as usize
        };
        limits.check_value(to_u64(size))?;
        let payload_at = cursor + header_len;
        let Some(payload) = body.get(payload_at..payload_at.saturating_add(size)) else {
            break;
        };
        budget.spend()?;
        frames.push(decode_frame(
            &id,
            payload,
            ByteRange::new(to_u64(V2_HEADER + cursor), to_u64(header_len + size)),
        ));
        // A frame of zero length would loop forever, so the step is forced
        // forward by at least one frame header.
        cursor = cursor
            .saturating_add(header_len)
            .saturating_add(size.max(1));
    }
    Ok(Some(Id3v2 {
        major,
        revision,
        frames,
        source: ByteRange::new(0, to_u64(V2_HEADER + size)),
    }))
}

fn decode_frame(id: &str, payload: &[u8], source: ByteRange) -> TagField {
    let mut name = frame_name(id).to_owned();
    if id.starts_with('T') || id == "COMM" || id == "COM" || id == "WXXX" || id == "USLT" {
        let (description, text) = decode_text_frame(id, payload);
        if let Some(description) = description.filter(|value| !value.is_empty()) {
            name = format!("{name}: {description}");
        }
        return TagField {
            id: id.to_owned(),
            name,
            text: Some(text),
            binary: None,
            source,
        };
    }
    TagField {
        id: id.to_owned(),
        name,
        text: None,
        binary: Some(payload.to_vec()),
        source,
    }
}

fn decode_text_frame(id: &str, payload: &[u8]) -> (Option<String>, String) {
    let Some((encoding, rest)) = payload.split_first() else {
        return (None, String::new());
    };
    let mut body = rest;
    if id == "TXXX" || id == "WXXX" {
        let mut parts = split_encoded_strings(body, *encoding).into_iter();
        let description = parts.next().map(|part| decode_string(*encoding, part));
        let values = parts
            .map(|part| {
                if id == "WXXX" {
                    latin1(part)
                } else {
                    decode_string(*encoding, part)
                }
            })
            .collect::<Vec<_>>();
        return (description, values.join("; "));
    }
    if id == "COMM" || id == "COM" || id == "USLT" {
        // The comment frame carries a language code and a description before
        // the text; both are skipped so the listing shows the text itself.
        body = body.get(3..).unwrap_or_default();
        body = skip_terminated(body, *encoding);
    }
    let value = split_encoded_strings(body, *encoding)
        .into_iter()
        .map(|part| decode_string(*encoding, part))
        .collect::<Vec<_>>()
        .join("; ");
    (None, value)
}

fn split_encoded_strings(data: &[u8], encoding: u8) -> Vec<&[u8]> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let step = if encoding == 1 || encoding == 2 { 2 } else { 1 };
    let mut index = 0usize;
    while index + step <= data.len() {
        let terminated = if step == 2 {
            data.get(index..index + 2) == Some(&[0, 0])
        } else {
            data.get(index) == Some(&0)
        };
        if terminated {
            parts.push(data.get(start..index).unwrap_or_default());
            index += step;
            start = index;
        } else {
            index += step;
        }
    }
    if start < data.len() || parts.is_empty() {
        parts.push(data.get(start..).unwrap_or_default());
    }
    parts
}

fn skip_terminated(data: &[u8], encoding: u8) -> &[u8] {
    if encoding == 1 || encoding == 2 {
        let mut index = 0usize;
        while index + 1 < data.len() {
            if data[index] == 0 && data[index + 1] == 0 {
                return data.get(index + 2..).unwrap_or_default();
            }
            index += 2;
        }
        return &[];
    }
    match data.iter().position(|byte| *byte == 0) {
        Some(index) => data.get(index + 1..).unwrap_or_default(),
        None => &[],
    }
}

fn decode_string(encoding: u8, data: &[u8]) -> String {
    let text = match encoding {
        1 => {
            if data.get(..2) == Some(&[0xFF, 0xFE]) {
                utf16_le_string(data.get(2..).unwrap_or_default()).0
            } else if data.get(..2) == Some(&[0xFE, 0xFF]) {
                utf16_be_string(data.get(2..).unwrap_or_default())
            } else {
                utf16_le_string(data).0
            }
        }
        2 => utf16_be_string(data),
        3 => String::from_utf8_lossy(data).into_owned(),
        _ => latin1(data),
    };
    text.trim_end_matches('\0').to_owned()
}

fn latin1(data: &[u8]) -> String {
    data.iter().map(|byte| char::from(*byte)).collect()
}

fn syncsafe(raw: &[u8]) -> Option<usize> {
    if raw.len() < 4 || raw.iter().any(|byte| *byte & 0x80 != 0) {
        return None;
    }
    let value = (u32::from(raw[0]) << 21)
        | (u32::from(raw[1]) << 14)
        | (u32::from(raw[2]) << 7)
        | u32::from(raw[3]);
    usize::try_from(value).ok()
}

fn unsynchronise(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut index = 0usize;
    while index < data.len() {
        let byte = data[index];
        out.push(byte);
        index += 1;
        if byte == 0xFF && data.get(index) == Some(&0) {
            index += 1;
        }
    }
    out
}

/// The readable name of a frame identifier.
#[must_use]
pub fn frame_name(id: &str) -> &'static str {
    match id {
        "TIT2" | "TT2" => "Title",
        "TPE1" | "TP1" => "Artist",
        "TPE2" | "TP2" => "Album artist",
        "TALB" | "TAL" => "Album",
        "TYER" | "TYE" | "TDRC" => "Year",
        "TRCK" | "TRK" => "Track",
        "TPOS" => "Disc",
        "TCON" | "TCO" => "Genre",
        "TCOM" | "TCM" => "Composer",
        "TENC" => "Encoded by",
        "TSSE" => "Encoder settings",
        "TBPM" => "Beats per minute",
        "TPUB" => "Publisher",
        "TCOP" => "Copyright",
        "TLEN" => "Length",
        "COMM" | "COM" => "Comment",
        "USLT" => "Lyrics",
        "APIC" | "PIC" => "Picture",
        "PRIV" => "Private data",
        "GEOB" => "Encapsulated object",
        "MCDI" => "Compact disc identifier",
        "UFID" => "Unique file identifier",
        "TXXX" => "User text",
        "WXXX" => "Web link",
        _ => "Frame",
    }
}

/// The name of an ID3 version 1 genre code.
#[must_use]
pub fn genre_name(code: u8) -> String {
    const NAMES: [&str; 26] = [
        "Blues",
        "Classic Rock",
        "Country",
        "Dance",
        "Disco",
        "Funk",
        "Grunge",
        "Hip-Hop",
        "Jazz",
        "Metal",
        "New Age",
        "Oldies",
        "Other",
        "Pop",
        "Rhythm and Blues",
        "Rap",
        "Reggae",
        "Rock",
        "Techno",
        "Industrial",
        "Alternative",
        "Ska",
        "Death Metal",
        "Pranks",
        "Soundtrack",
        "Euro-Techno",
    ];
    NAMES
        .get(usize::from(code))
        .map_or_else(|| code.to_string(), |name| (*name).to_owned())
}
