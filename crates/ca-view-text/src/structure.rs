//! Structural rows retain full paths so text alignment cannot pair unrelated values.

use crate::{jobs, lines, model, prettify};
use ca_diff::{Cancel, ClassifiedHunk, Hunk, HunkKind, Importance};
use prettify::JsonValue;
use quick_xml::{events::Event, Reader};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

const MAX_INPUT: usize = 4 * 1024 * 1024;
const MAX_OUTPUT: usize = 12 * 1024 * 1024;
const MAX_STORAGE: usize = 64 * 1024 * 1024;
const MAX_PATH: usize = 16 * 1024;
const MAX_ROWS: usize = 100_000;
const MAX_DEPTH: usize = 128;
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// The path convention and parser selected for both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Decoded object keys and positional arrays.
    Json,
    /// Expanded element names and positional same-name siblings.
    Xml,
}

impl Format {
    /// The heading that distinguishes the projection from source text.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Json => "JSON Structure Compare",
            Self::Xml => "XML Structure Compare",
        }
    }
}

/// Counts refer to paths, including containers, rather than source lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// The parser and path convention used.
    pub format: Format,
    /// Paths found only on the right.
    pub added: usize,
    /// Paths found only on the left.
    pub removed: usize,
    /// Paired paths whose values differ.
    pub changed: usize,
}

impl Summary {
    /// A visible mode label that also survives into report headings.
    #[must_use]
    pub fn label(self) -> String {
        format!(
            "{}: {} added, {} removed, {} changed",
            self.format.title(),
            self.added,
            self.removed,
            self.changed
        )
    }
}

/// The pane buffers and aligned rows are constructed together on the worker.
pub struct Comparison {
    /// Read-only left path/value projection.
    pub left_text: String,
    /// Read-only right path/value projection.
    pub right_text: String,
    /// Direct path alignment, independent of text ignore rules.
    pub data: jobs::TextData,
    /// Full comparison counts before filtering.
    pub summary: Summary,
}

/// Only like-format pairs may enter the structural projection.
#[must_use]
pub fn pair_format(left: &Path, right: &Path) -> Option<Format> {
    match prettify::pair_format(left, right)? {
        prettify::StructuredFormat::Json => Some(Format::Json),
        prettify::StructuredFormat::Xml => Some(Format::Xml),
        _ => None,
    }
}

/// Align decoded paths and classify values without applying text rules.
/// A leading U+FEFF is content; byte-order marks must be removed by decoding.
///
/// # Errors
/// Refuses malformed or ambiguous input, resource limits and cancellation.
#[allow(clippy::too_many_lines)]
pub fn compare(
    left: &str,
    right: &str,
    format: Format,
    cancel: &dyn Cancel,
) -> Result<Comparison, String> {
    check_cancel(cancel)?;
    for (side, text) in [("left", left), ("right", right)] {
        if text.len() > MAX_INPUT {
            return Err(format!(
                "{side} side: input exceeds the 4 MiB structural comparison limit"
            ));
        }
    }
    let mut left = parse(left, format, cancel).map_err(|error| format!("left side: {error}"))?;
    let mut right = parse(right, format, cancel).map_err(|error| format!("right side: {error}"))?;
    let mut paths = BTreeSet::new();
    for path in left.entries.keys().chain(right.entries.keys()) {
        check_cancel(cancel)?;
        if paths.len() == MAX_ROWS && !paths.contains(path) {
            return Err("comparison exceeds the 100,000 path limit".into());
        }
        paths.insert(path.clone());
    }
    let mut summary = Summary {
        format,
        added: 0,
        removed: 0,
        changed: 0,
    };
    let mut left_lines = Vec::new();
    let mut right_lines = Vec::new();
    let mut left_text = String::new();
    let mut right_text = String::new();
    let mut hunks = Vec::new();
    for path in paths {
        check_cancel(cancel)?;
        let a = left.entries.remove(&path);
        let b = right.entries.remove(&path);
        let kind = match (&a, &b) {
            (Some(a), Some(b)) if a == b => HunkKind::Same,
            (Some(_), Some(_)) => {
                summary.changed += 1;
                HunkKind::Changed
            }
            (Some(_), None) => {
                summary.removed += 1;
                HunkKind::LeftOnly
            }
            (None, Some(_)) => {
                summary.added += 1;
                HunkKind::RightOnly
            }
            (None, None) => continue,
        };
        let status = match kind {
            HunkKind::Same => "Same",
            HunkKind::Changed => "Changed",
            HunkKind::LeftOnly => "Removed",
            HunkKind::RightOnly => "Added",
        };
        let a_start = u32::try_from(left_lines.len()).map_err(|error| error.to_string())?;
        let b_start = u32::try_from(right_lines.len()).map_err(|error| error.to_string())?;
        if let Some(value) = a {
            append_line(&mut left_lines, &mut left_text, status, &path, &value)?;
        }
        if let Some(value) = b {
            append_line(&mut right_lines, &mut right_text, status, &path, &value)?;
        }
        let importance = (kind != HunkKind::Same).then_some(Importance::Important);
        hunks.push(ClassifiedHunk {
            hunk: Hunk {
                kind,
                left: a_start
                    ..u32::try_from(left_lines.len()).map_err(|error| error.to_string())?,
                right: b_start
                    ..u32::try_from(right_lines.len()).map_err(|error| error.to_string())?,
            },
            importance,
            left_lines: Vec::new(),
            right_lines: Vec::new(),
        });
    }
    let left_metrics: Vec<_> = left_lines.iter().map(|line| lines::measure(line)).collect();
    let right_metrics: Vec<_> = right_lines
        .iter()
        .map(|line| lines::measure(line))
        .collect();
    let widest = left_metrics
        .iter()
        .chain(&right_metrics)
        .map(|item| item.columns)
        .max()
        .unwrap_or(0);
    check_cancel(cancel)?;
    Ok(Comparison {
        left_text,
        right_text,
        summary,
        data: jobs::TextData {
            left: jobs::SidePayload {
                lines: Arc::new(left_lines),
                metrics: left_metrics,
                ..Default::default()
            },
            right: jobs::SidePayload {
                lines: Arc::new(right_lines),
                metrics: right_metrics,
                ..Default::default()
            },
            model: model::build(&hunks),
            widest,
            ..Default::default()
        },
    })
}

fn append_line(
    lines: &mut Vec<String>,
    text: &mut String,
    status: &str,
    path: &str,
    value: &str,
) -> Result<(), String> {
    let size = status.len() + path.len() + value.len() + 8;
    if text.len().saturating_add(size) > MAX_OUTPUT {
        return Err("projection exceeds the 12 MiB output limit".into());
    }
    let line = format!("{status}  {path} = {value}");
    text.push_str(&line);
    text.push('\n');
    lines.push(line);
    Ok(())
}

struct Document {
    entries: BTreeMap<String, String>,
    storage: usize,
}

impl Document {
    fn check_entry(&self, path: usize, value: usize) -> Result<(), String> {
        check_path(path)?;
        if self.storage.saturating_add(entry_cost(path, value)) > MAX_STORAGE {
            return Err("structure exceeds the 64 MiB storage estimate".into());
        }
        if self.entries.len() >= MAX_ROWS {
            return Err("structure exceeds the 100,000 path limit".into());
        }
        Ok(())
    }

    fn consume(&mut self, cost: usize) -> Result<(), String> {
        if self.storage.saturating_add(cost) > MAX_STORAGE {
            return Err("structure exceeds the 64 MiB storage estimate".into());
        }
        self.storage += cost;
        Ok(())
    }

    fn insert_quoted(&mut self, path: String, value: &str) -> Result<(), String> {
        self.check_entry(path.len(), quoted_len(value))?;
        self.insert(path, quote(value)?)
    }

    fn insert(&mut self, path: String, value: String) -> Result<(), String> {
        self.check_entry(path.len(), value.len())?;
        self.storage += entry_cost(path.len(), value.len());
        if self.entries.insert(path, value).is_some() {
            return Err("duplicate path in structured input".into());
        }
        Ok(())
    }
}

fn entry_cost(path: usize, value: usize) -> usize {
    512usize.saturating_add(path.saturating_add(value).saturating_mul(4))
}

fn check_path(length: usize) -> Result<(), String> {
    if length > MAX_PATH {
        return Err("path exceeds the 16 KiB limit".into());
    }
    Ok(())
}

fn join_path(parent: &str, segment: &str) -> Result<String, String> {
    check_path(parent.len().saturating_add(segment.len()).saturating_add(1))?;
    Ok(format!("{parent}/{segment}"))
}

fn parse(text: &str, format: Format, cancel: &dyn Cancel) -> Result<Document, String> {
    check_cancel(cancel)?;
    if text.len() > MAX_INPUT {
        return Err("input exceeds the 4 MiB structural comparison limit".into());
    }
    if text.starts_with('\u{feff}') {
        return Err("U+FEFF precedes the structured content".into());
    }
    let mut document = Document {
        entries: BTreeMap::new(),
        storage: text.len().saturating_mul(12),
    };
    match format {
        Format::Json => json_entries(
            prettify::parse_json(text, cancel)?,
            text,
            "$",
            0,
            &mut document,
            cancel,
        )?,
        Format::Xml => xml_entries(text, &mut document, cancel)?,
    }
    Ok(document)
}

fn quote(value: &str) -> Result<String, String> {
    use std::fmt::Write;
    let mut output = String::with_capacity(quoted_len(value));
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{8}' => output.push_str("\\b"),
            '\u{c}' => output.push_str("\\f"),
            character if character <= '\u{1f}' || bidi_control(character) => {
                write!(output, "\\u{:04x}", u32::from(character))
                    .map_err(|error| error.to_string())?;
            }
            character => output.push(character),
        }
    }
    output.push('"');
    Ok(output)
}

fn visible(quoted: &str) -> Result<String, String> {
    use std::fmt::Write;
    let mut output = String::with_capacity(quoted.len());
    for character in quoted.chars() {
        if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
            write!(output, "\\u{:04x}", u32::from(character)).map_err(|error| error.to_string())?;
        } else {
            output.push(character);
        }
    }
    Ok(output)
}

fn quoted_len(value: &str) -> usize {
    value.chars().fold(2usize, |length, character| {
        length.saturating_add(match character {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            character if character <= '\u{1f}' || bidi_control(character) => 6,
            character => character.len_utf8(),
        })
    })
}

const fn bidi_control(character: char) -> bool {
    matches!(character, '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

fn ordered_value(
    kind: &str,
    order: &[String],
    path: &str,
    document: &Document,
) -> Result<String, String> {
    let size = kind.len()
        + 3
        + order
            .iter()
            .map(|value| quoted_len(value) + 1)
            .sum::<usize>();
    document.check_entry(path.len(), size)?;
    let mut value = String::with_capacity(size);
    value.push_str(kind);
    value.push_str(" [");
    for (index, segment) in order.iter().enumerate() {
        if index > 0 {
            value.push(',');
        }
        value.push_str(&quote(segment)?);
    }
    value.push(']');
    Ok(value)
}

fn json_entries(
    value: prettify::JsonValue,
    text: &str,
    path: &str,
    depth: usize,
    document: &mut Document,
    cancel: &dyn Cancel,
) -> Result<(), String> {
    check_cancel(cancel)?;
    if depth > MAX_DEPTH {
        return Err("structure exceeds the 128 level nesting limit".into());
    }
    if path.len() > MAX_PATH {
        return Err("path exceeds the 16 KiB limit".into());
    }
    let scalar = match value {
        JsonValue::Null => "null".into(),
        JsonValue::Boolean(value) => value.to_string(),
        JsonValue::Number(value) => decimal(&value)?,
        JsonValue::String(value) => return document.insert_quoted(path.into(), &value),
        JsonValue::Array(values) => {
            document.insert(path.into(), "[]".into())?;
            for (index, value) in values.into_iter().enumerate() {
                json_entries(
                    value,
                    text,
                    &format!("{path}[{index}]"),
                    depth + 1,
                    document,
                    cancel,
                )?;
            }
            return Ok(());
        }
        JsonValue::Object(entries) => {
            document.insert(path.into(), "{}".into())?;
            let mut keys = BTreeSet::new();
            for (key, key_offset, value) in entries {
                let length = path
                    .len()
                    .saturating_add(quoted_len(&key))
                    .saturating_add(2);
                document.check_entry(length, 0)?;
                if !keys.insert(key.clone()) {
                    let preview: String = key.chars().take(80).collect();
                    let suffix = if preview.len() < key.len() {
                        " (truncated)"
                    } else {
                        ""
                    };
                    return Err(prettify::source_error(
                        text,
                        key_offset,
                        &format!("duplicate JSON key {}{suffix}", visible(&quote(&preview)?)?),
                    ));
                }
                json_entries(
                    value,
                    text,
                    &format!("{path}[{}]", quote(&key)?),
                    depth + 1,
                    document,
                    cancel,
                )?;
            }
            return Ok(());
        }
    };
    document.insert(path.into(), scalar)
}

fn decimal(raw: &str) -> Result<String, String> {
    let (negative, raw) = raw
        .strip_prefix('-')
        .map_or((false, raw), |raw| (true, raw));
    let (mantissa, exponent) = raw.split_once(['e', 'E']).unwrap_or((raw, "0"));
    let mut exponent = exponent
        .parse::<i64>()
        .map_err(|_| "JSON exponent exceeds the structural comparison range")?;
    let fraction = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    exponent = exponent
        .checked_sub(i64::try_from(fraction).map_err(|error| error.to_string())?)
        .ok_or("JSON exponent exceeds the structural comparison range")?;
    let digits: String = mantissa
        .chars()
        .filter(|character| *character != '.')
        .collect();
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Ok("0".into());
    }
    let significant = digits.trim_end_matches('0');
    exponent = exponent
        .checked_add(
            i64::try_from(digits.len() - significant.len()).map_err(|error| error.to_string())?,
        )
        .ok_or("JSON exponent exceeds the structural comparison range")?;
    let sign = if negative { "-" } else { "" };
    if (0..=32).contains(&exponent) {
        Ok(format!(
            "{sign}{significant}{}",
            "0".repeat(usize::try_from(exponent).map_err(|error| error.to_string())?)
        ))
    } else if (-32..0).contains(&exponent) {
        let places = usize::try_from(-exponent).map_err(|error| error.to_string())?;
        if places < significant.len() {
            let split = significant.len() - places;
            Ok(format!(
                "{sign}{}.{}",
                &significant[..split],
                &significant[split..]
            ))
        } else {
            Ok(format!(
                "{sign}0.{}{significant}",
                "0".repeat(places - significant.len())
            ))
        }
    } else {
        Ok(format!("{sign}{significant}e{exponent}"))
    }
}

fn check_cancel(cancel: &dyn Cancel) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("structural comparison cancelled".into())
    } else {
        Ok(())
    }
}

#[derive(Default)]
struct XmlFrame {
    path: String,
    preserve: bool,
    siblings: BTreeMap<String, usize>,
    content: Vec<Content>,
    bindings: Vec<String>,
}

enum Content {
    Text(String),
    Node(String),
}

struct Namespaces {
    bindings: BTreeMap<String, Vec<String>>,
}

impl Namespaces {
    fn new() -> Self {
        Self {
            bindings: BTreeMap::from([("xml".into(), vec![XML_NAMESPACE.into()])]),
        }
    }

    fn bind(&mut self, prefix: &str, value: String) -> Result<(), String> {
        if prefix == "xmlns"
            || value == XMLNS_NAMESPACE
            || (prefix != "xml" && value == XML_NAMESPACE)
            || (prefix == "xml" && value != XML_NAMESPACE)
            || (!prefix.is_empty() && value.is_empty())
        {
            return Err("invalid XML: reserved or empty namespace binding".into());
        }
        self.bindings.entry(prefix.into()).or_default().push(value);
        Ok(())
    }

    fn pop(&mut self, prefixes: &[String]) {
        for prefix in prefixes.iter().rev() {
            if let Some(values) = self.bindings.get_mut(prefix) {
                values.pop();
                if values.is_empty() {
                    self.bindings.remove(prefix);
                }
            }
        }
    }

    fn expanded(&self, bytes: &[u8], attribute: bool) -> Result<String, String> {
        use std::fmt::Write;
        valid_qname(bytes)?;
        let name = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
        let (prefix, local) = name.split_once(':').unwrap_or(("", name));
        let namespace = if prefix.is_empty() && attribute {
            None
        } else {
            self.bindings
                .get(prefix)
                .and_then(|values| values.last())
                .filter(|value| !value.is_empty())
        };
        if (!prefix.is_empty() && namespace.is_none()) || prefix == "xmlns" {
            return Err("undeclared or reserved XML namespace prefix".into());
        }
        let local_size = local
            .chars()
            .map(|character| {
                if bidi_control(character) {
                    6
                } else {
                    character.len_utf8()
                }
            })
            .sum::<usize>();
        let length = local_size + namespace.map_or(0, |uri| quoted_len(uri) + 3);
        check_path(length)?;
        let mut result = String::with_capacity(length);
        if let Some(uri) = namespace {
            result.push_str("Q{");
            result.push_str(&quote(uri)?);
            result.push('}');
        }
        for character in local.chars() {
            if bidi_control(character) {
                write!(result, "\\u{:04x}", u32::from(character))
                    .map_err(|error| error.to_string())?;
            } else {
                result.push(character);
            }
        }
        Ok(result)
    }
}

fn xml_entries(text: &str, document: &mut Document, cancel: &dyn Cancel) -> Result<(), String> {
    let mut error_offset = 0;
    xml_entries_at(text, document, cancel, &mut error_offset)
        .map_err(|error| prettify::source_error(text, error_offset, &error))
}

#[allow(clippy::too_many_lines)]
fn xml_entries_at(
    text: &str,
    document: &mut Document,
    cancel: &dyn Cancel,
    error_offset: &mut usize,
) -> Result<(), String> {
    if let Some((offset, _)) = text
        .char_indices()
        .find(|(_, character)| !xml_char(*character))
    {
        *error_offset = offset;
        return Err("invalid XML 1.0 character".into());
    }
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_comments = true;
    reader.config_mut().check_end_names = true;
    let mut stack = vec![XmlFrame::default()];
    let mut roots = 0;
    let mut declaration = false;
    let mut namespaces = Namespaces::new();
    let space = format!("Q{{{}}}space", quote(XML_NAMESPACE)?);
    loop {
        *error_offset = usize::try_from(reader.buffer_position()).unwrap_or(text.len());
        check_cancel(cancel)?;
        let event = reader
            .read_event()
            .map_err(|error| format!("invalid XML: {}", prettify::diagnostic_reason(error)))?;
        document.consume(512)?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Eof => break,
            Event::Start(start) | Event::Empty(start) => {
                validate_attributes(&start, cancel)?;
                if stack.len() > MAX_DEPTH {
                    return Err("structure exceeds the 128 level nesting limit".into());
                }
                if stack.len() == 1 {
                    roots += 1;
                    if roots > 1 {
                        return Err("invalid XML: multiple document elements".into());
                    }
                }
                let mut bindings = Vec::new();
                let mut raw_keys = BTreeSet::new();
                for attribute in start.attributes().with_checks(false) {
                    check_cancel(cancel)?;
                    let attribute =
                        attribute.map_err(|error| format!("invalid XML attribute: {error}"))?;
                    valid_qname(attribute.key.as_ref())?;
                    if !raw_keys.insert(attribute.key.as_ref().to_vec()) {
                        return Err("duplicate XML attribute".into());
                    }
                    let key = std::str::from_utf8(attribute.key.as_ref())
                        .map_err(crate::prettify::xml_reason)?;
                    let prefix = if key == "xmlns" {
                        Some("")
                    } else {
                        key.strip_prefix("xmlns:")
                    };
                    if let Some(prefix) = prefix {
                        document.consume(entry_cost(prefix.len(), attribute.value.len()))?;
                        namespaces.bind(prefix, attribute_value(attribute.value.as_ref())?)?;
                        bindings.push(prefix.to_owned());
                    }
                }
                let name = namespaces.expanded(start.name().as_ref(), false)?;
                let parent = stack.last_mut().ok_or("invalid XML stack")?;
                let occurrence = parent.siblings.entry(name.clone()).or_default();
                *occurrence += 1;
                let suffix = format!("[{occurrence}]");
                check_path(parent.path.len() + name.len() + suffix.len() + 1)?;
                let segment = format!("{name}{suffix}");
                let path = join_path(&parent.path, &segment)?;
                parent.content.push(Content::Node(segment));
                let mut frame = XmlFrame {
                    path,
                    preserve: parent.preserve,
                    bindings,
                    ..Default::default()
                };
                let mut seen = BTreeSet::new();
                for attribute in start.attributes().with_checks(false) {
                    check_cancel(cancel)?;
                    let attribute =
                        attribute.map_err(|error| format!("invalid XML attribute: {error}"))?;
                    valid_qname(attribute.key.as_ref())?;
                    if attribute.key.as_ref() == b"xmlns"
                        || attribute.key.as_ref().starts_with(b"xmlns:")
                    {
                        continue;
                    }
                    let name = namespaces.expanded(attribute.key.as_ref(), true)?;
                    if !seen.insert(name.clone()) {
                        return Err("duplicate expanded XML attribute".into());
                    }
                    let value = attribute_value(attribute.value.as_ref())?;
                    if name == space {
                        frame.preserve = match value.as_str() {
                            "preserve" => true,
                            "default" => false,
                            _ => return Err("invalid xml:space value".into()),
                        };
                    }
                    let path = join_path(&frame.path, &format!("@{name}"))?;
                    document.insert_quoted(path, &value)?;
                }
                if empty {
                    namespaces.pop(&frame.bindings);
                    finish_xml(frame, document)?;
                } else {
                    stack.push(frame);
                }
            }
            Event::End(_) => {
                if stack.len() <= 1 {
                    return Err("invalid XML: closing tag without an element".into());
                }
                let frame = stack.pop().ok_or("invalid XML stack")?;
                namespaces.pop(&frame.bindings);
                finish_xml(frame, document)?;
            }
            Event::Text(value) => {
                if value.windows(3).any(|bytes| bytes == b"]]>") {
                    return Err("invalid XML: CDATA terminator in character data".into());
                }
                append_xml_text(
                    &mut stack,
                    &value.xml10_content().map_err(crate::prettify::xml_reason)?,
                )?;
            }
            Event::CData(value) => {
                if stack.len() == 1 {
                    return Err("invalid XML: CDATA outside document element".into());
                }
                append_xml_text(
                    &mut stack,
                    &value.xml10_content().map_err(crate::prettify::xml_reason)?,
                )?;
            }
            Event::GeneralRef(reference) => {
                if stack.len() == 1 {
                    return Err("invalid XML: reference outside document element".into());
                }
                let value = if let Some(character) = reference
                    .resolve_char_ref()
                    .map_err(crate::prettify::xml_reason)?
                {
                    if !xml_char(character) {
                        return Err("invalid XML character reference".into());
                    }
                    character.to_string()
                } else {
                    let name = reference.decode().map_err(crate::prettify::xml_reason)?;
                    quick_xml::escape::unescape(&format!("&{name};"))
                        .map_err(crate::prettify::xml_reason)?
                        .into_owned()
                };
                append_xml_text(&mut stack, &value)?;
            }
            Event::Comment(value) => {
                let value = value.xml10_content().map_err(crate::prettify::xml_reason)?;
                xml_misc(&mut stack, document, "comment()", &value)?;
            }
            Event::PI(value) => {
                valid_qname(value.target())?;
                if value.target().eq_ignore_ascii_case(b"xml") || value.target().contains(&b':') {
                    return Err("invalid XML processing instruction target".into());
                }
                let target =
                    std::str::from_utf8(value.target()).map_err(crate::prettify::xml_reason)?;
                let data = std::str::from_utf8(value.content())
                    .map_err(crate::prettify::xml_reason)?
                    .trim_start_matches([' ', '\t', '\r', '\n']);
                let value = if data.is_empty() {
                    target.to_owned()
                } else {
                    format!("{target} {data}")
                }
                .replace("\r\n", "\n")
                .replace('\r', "\n");
                xml_misc(&mut stack, document, "processing-instruction()", &value)?;
            }
            Event::Decl(value) => {
                if declaration
                    || roots > 0
                    || reader.buffer_position()
                        != u64::try_from(value.len() + 4).map_err(crate::prettify::xml_reason)?
                {
                    return Err("invalid XML declaration position".into());
                }
                declaration = true;
                if value
                    .version()
                    .map_err(crate::prettify::xml_reason)?
                    .as_ref()
                    != b"1.0"
                {
                    return Err("only XML 1.0 is supported".into());
                }
                let start = quick_xml::events::BytesStart::from_content(
                    std::str::from_utf8(value.as_ref()).map_err(crate::prettify::xml_reason)?,
                    3,
                );
                validate_attributes(&start, cancel)?;
                let mut names = Vec::new();
                let mut raw_keys = BTreeSet::new();
                for attribute in start.attributes().with_checks(false) {
                    let attribute = attribute.map_err(crate::prettify::xml_reason)?;
                    let key = attribute.key.as_ref();
                    if !raw_keys.insert(key.to_vec()) {
                        return Err("duplicate XML declaration attribute".into());
                    }
                    match key {
                        b"version" if names.is_empty() => {}
                        b"encoding" if names.len() == 1 => {
                            let value = attribute.value.as_ref();
                            if !value.first().is_some_and(u8::is_ascii_alphabetic)
                                || !value.iter().all(|byte| {
                                    byte.is_ascii_alphanumeric()
                                        || matches!(byte, b'.' | b'_' | b'-')
                                })
                            {
                                return Err("invalid XML encoding declaration".into());
                            }
                        }
                        b"standalone"
                            if !names
                                .iter()
                                .any(|name: &Vec<u8>| name.as_slice() == b"standalone")
                                && matches!(attribute.value.as_ref(), b"yes" | b"no") => {}
                        _ => return Err("invalid XML declaration attribute".into()),
                    }
                    names.push(key.to_vec());
                }
            }
            Event::DocType(_) => {
                return Err(
                    "XML document types and custom entities require the line comparison".into(),
                )
            }
        }
    }
    if roots != 1 || stack.len() != 1 {
        return Err("invalid XML: expected one complete document element".into());
    }
    let document_frame = stack.pop().ok_or("invalid XML stack")?;
    let order: Vec<_> = document_frame
        .content
        .into_iter()
        .filter_map(|item| match item {
            Content::Node(segment) => Some(segment),
            Content::Text(_) => None,
        })
        .collect();
    if order.len() > 1 {
        let value = ordered_value("document", &order, "/", document)?;
        document.insert("/".into(), value)?;
    }
    Ok(())
}

fn attribute_value(bytes: &[u8]) -> Result<String, String> {
    let raw = std::str::from_utf8(bytes).map_err(crate::prettify::xml_reason)?;
    if raw.contains('<') {
        return Err("invalid XML attribute character".into());
    }
    let raw = raw.replace("\r\n", " ").replace(['\r', '\n', '\t'], " ");
    let value = quick_xml::escape::unescape(&raw)
        .map_err(crate::prettify::xml_reason)?
        .into_owned();
    if value.chars().any(|character| !xml_char(character)) {
        return Err("invalid XML attribute character reference".into());
    }
    Ok(value)
}

fn validate_attributes(
    start: &quick_xml::events::BytesStart<'_>,
    cancel: &dyn Cancel,
) -> Result<(), String> {
    let bytes: &[u8] = start.as_ref();
    let mut cursor = start.name().as_ref().len();
    while cursor < bytes.len() {
        check_cancel(cancel)?;
        if !matches!(bytes[cursor], b' ' | b'\t' | b'\r' | b'\n') {
            return Err("invalid XML: attributes require separating whitespace".into());
        }
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        let key = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
        {
            cursor += 1;
        }
        valid_qname(&bytes[key..cursor])?;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            return Err("invalid XML attribute assignment".into());
        }
        cursor += 1;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        let quote = *bytes.get(cursor).ok_or("invalid XML attribute value")?;
        if !matches!(quote, b'\'' | b'"') {
            return Err("XML attributes require quoted values".into());
        }
        cursor += 1;
        while bytes.get(cursor).is_some_and(|byte| *byte != quote) {
            cursor += 1;
            if cursor.is_multiple_of(1024) {
                check_cancel(cancel)?;
            }
        }
        if cursor == bytes.len() {
            return Err("unclosed XML attribute value".into());
        }
        cursor += 1;
    }
    Ok(())
}

fn append_xml_text(stack: &mut [XmlFrame], value: &str) -> Result<(), String> {
    if stack.len() == 1 {
        if value
            .chars()
            .all(|character| matches!(character, ' ' | '\t' | '\r' | '\n'))
        {
            return Ok(());
        }
        return Err("invalid XML: text outside document element".into());
    }
    if value.is_empty() {
        return Ok(());
    }
    let frame = stack.last_mut().ok_or("invalid XML stack")?;
    if let Some(Content::Text(previous)) = frame.content.last_mut() {
        previous.push_str(value);
    } else {
        frame.content.push(Content::Text(value.into()));
    }
    Ok(())
}

fn xml_misc(
    stack: &mut [XmlFrame],
    document: &mut Document,
    kind: &str,
    value: &str,
) -> Result<(), String> {
    let frame = stack.last_mut().ok_or("invalid XML stack")?;
    let occurrence = frame.siblings.entry(kind.into()).or_default();
    *occurrence += 1;
    let segment = format!("{kind}[{occurrence}]");
    document.insert_quoted(join_path(&frame.path, &segment)?, value)?;
    frame.content.push(Content::Node(segment));
    Ok(())
}

fn finish_xml(frame: XmlFrame, document: &mut Document) -> Result<(), String> {
    let has_nodes = frame
        .content
        .iter()
        .any(|item| matches!(item, Content::Node(segment) if !segment.starts_with("comment()") && !segment.starts_with("processing-instruction()")));
    let mixed = frame.content.iter().any(|item| matches!(item, Content::Text(text) if !text.chars().all(|character| matches!(character, ' ' | '\n' | '\r' | '\t'))));
    let mut order = Vec::new();
    let mut index = 0;
    for item in frame.content {
        match item {
            Content::Node(segment) => order.push(segment),
            Content::Text(text) if frame.preserve || mixed || !has_nodes => {
                index += 1;
                let segment = format!("text()[{index}]");
                document.insert_quoted(join_path(&frame.path, &segment)?, &text)?;
                order.push(segment);
            }
            Content::Text(_) => {}
        }
    }
    let value = if order.is_empty() {
        "element".into()
    } else {
        ordered_value("element", &order, &frame.path, document)?
    };
    document.insert(frame.path, value)
}

fn valid_qname(bytes: &[u8]) -> Result<(), String> {
    let name = std::str::from_utf8(bytes).map_err(crate::prettify::xml_reason)?;
    let mut count = 0;
    for part in name.split(':') {
        count += 1;
        let mut chars = part.chars();
        if !chars.next().is_some_and(name_start) || !chars.all(name_continue) || count > 2 {
            return Err("invalid XML qualified name".into());
        }
    }
    Ok(())
}

const fn name_start(character: char) -> bool {
    matches!(character, 'A'..='Z' | '_' | 'a'..='z' | '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{f6}' | '\u{f8}'..='\u{2ff}' | '\u{370}'..='\u{37d}' | '\u{37f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}' | '\u{2070}'..='\u{218f}' | '\u{2c00}'..='\u{2fef}' | '\u{3001}'..='\u{d7ff}' | '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}')
}

const fn name_continue(character: char) -> bool {
    name_start(character)
        || matches!(character, '-' | '.' | '0'..='9' | '\u{b7}' | '\u{300}'..='\u{36f}' | '\u{203f}'..='\u{2040}')
}

pub(super) const fn xml_char(character: char) -> bool {
    matches!(character, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}
