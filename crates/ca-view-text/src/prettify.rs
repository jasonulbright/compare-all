//! Read-only formatting used by structured text comparisons.
//!
//! The formatter produces a temporary comparison projection. It never writes
//! its output to either source file.

mod toml;
mod yaml;

use ca_diff::{Cancel, NeverCancel};
use quick_xml::events::Event;
use std::fmt::Write as _;
use std::path::Path;

/// Largest formatted side retained for a comparison.
const MAX_FORMATTED_BYTES: usize = 24 * 1024 * 1024;
/// Largest input parsed into a temporary formatting tree.
const MAX_FORMAT_INPUT_BYTES: usize = 16 * 1024 * 1024;
/// Estimated temporary storage for one structured side, excluding the source
/// text already held by its comparison pane.
const MAX_FORMAT_TEMP_BYTES: usize = 64 * 1024 * 1024;
/// Conservative amortized storage per JSON value/key or XML event.
const ESTIMATED_BYTES_PER_LAYOUT_ITEM: usize = 96;

/// A recognized structured format supported by the comparison formatter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredFormat {
    /// JavaScript Object Notation.
    Json,
    /// Extensible Markup Language.
    Xml,
    /// YAML, including multi-document streams.
    Yaml,
    /// Tom's Obvious Minimal Language.
    Toml,
}

/// Whether both paths name the same structured format supported here.
#[must_use]
pub fn pair_format(left: &Path, right: &Path) -> Option<StructuredFormat> {
    let left = format_for(left)?;
    (format_for(right) == Some(left)).then_some(left)
}

/// Which structured format a path names.
#[must_use]
pub fn format_for(path: &Path) -> Option<StructuredFormat> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "json" => Some(StructuredFormat::Json),
        "xml" | "xsd" | "xsl" | "xslt" | "svg" | "rss" | "config" | "csproj" | "plist" => {
            Some(StructuredFormat::Xml)
        }
        "yaml" | "yml" => Some(StructuredFormat::Yaml),
        "toml" => Some(StructuredFormat::Toml),
        _ => None,
    }
}

/// Format JSON, XML, YAML or TOML for a temporary comparison view.
///
/// # Errors
///
/// Returns an error when the input is malformed, exceeds the nesting limit or
/// could expand beyond the formatted-output bound.
pub fn format(text: &str, format: StructuredFormat) -> Result<String, String> {
    format_cancellable(text, format, &NeverCancel)
}

/// Format structured text, polling `cancel` while parsing and writing.
///
/// # Errors
///
/// Returns an error when the input is malformed, exceeds a supported bound or
/// cancellation was requested.
pub fn format_cancellable(
    text: &str,
    format: StructuredFormat,
    cancel: &dyn Cancel,
) -> Result<String, String> {
    check_cancel(cancel)?;
    if text.len() > MAX_FORMAT_INPUT_BYTES {
        return Err(format!(
            "structured input exceeds the {} MiB formatting limit",
            MAX_FORMAT_INPUT_BYTES / (1024 * 1024)
        ));
    }
    // A byte order mark is an encoding signature, not structured content.
    let parsed = text.strip_prefix('\u{feff}').unwrap_or(text);
    let result = match format {
        StructuredFormat::Json => format_json(parsed, cancel),
        StructuredFormat::Xml => format_xml(parsed, cancel),
        StructuredFormat::Yaml => yaml::format(parsed, cancel),
        StructuredFormat::Toml => toml::format(parsed, cancel),
    };
    result.map_err(|error| {
        if parsed.len() != text.len() {
            if let Some((location, reason)) = error.split_once(": ") {
                if location.starts_with("line ") {
                    if let Some(offset) = location
                        .rsplit_once("byte ")
                        .and_then(|(_, byte)| byte.parse::<usize>().ok())
                    {
                        return source_error(text, offset + text.len() - parsed.len(), reason);
                    }
                }
            }
        }
        error
    })
}

pub(super) fn diagnostic_reason(reason: impl std::fmt::Display) -> String {
    struct Bounded(String);
    impl std::fmt::Write for Bounded {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            for character in text.chars() {
                let escaped = character.is_control()
                    || matches!(character, '\u{ad}' | '\u{0600}'..='\u{0605}' | '\u{061c}' | '\u{06dd}' | '\u{070f}' | '\u{0890}'..='\u{0891}' | '\u{08e2}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{110bd}' | '\u{110cd}' | '\u{13430}'..='\u{1343f}' | '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0001}' | '\u{e0020}'..='\u{e007f}');
                let cost = if escaped { 10 } else { character.len_utf8() };
                if self.0.len() + cost > 1024 {
                    return Err(std::fmt::Error);
                }
                if escaped {
                    write!(self.0, "\\u{{{:x}}}", u32::from(character))?;
                } else {
                    self.0.push(character);
                }
            }
            Ok(())
        }
    }
    let mut output = Bounded(String::new());
    if write!(output, "{reason}").is_err() {
        output.0.push_str(" (truncated)");
    }
    output.0
}

pub(super) fn source_error(text: &str, offset: usize, reason: &str) -> String {
    if matches!(
        reason,
        "formatting cancelled" | "structural comparison cancelled"
    ) {
        return reason.to_owned();
    }
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let mut line = 1usize;
    let mut column = 1usize;
    let mut carriage_return = false;
    for character in text[..offset].chars() {
        match character {
            '\r' => {
                line += 1;
                column = 1;
            }
            '\n' => {
                line += usize::from(!carriage_return);
                column = 1;
            }
            _ => column += 1,
        }
        carriage_return = character == '\r';
    }
    format!(
        "line {line}, column {column}, byte {offset}: {}",
        diagnostic_reason(reason)
    )
}

fn format_json(text: &str, cancel: &dyn Cancel) -> Result<String, String> {
    let mut parser = JsonParser::new(text, cancel);
    let value = parser.document()?;
    check_output_bound(text.len(), parser.layout_items, parser.max_depth)?;
    let mut output = String::with_capacity(text.len().saturating_add(32));
    value.write_pretty(&mut output, 0, cancel)?;
    Ok(output)
}

/// A lossless-enough JSON syntax tree: member order, duplicate members and
/// number spellings are retained while only insignificant whitespace changes.
pub(super) enum JsonValue {
    Null,
    Boolean(bool),
    Number(String),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, usize, Self)>),
}

impl JsonValue {
    fn write_pretty(
        &self,
        output: &mut String,
        depth: usize,
        cancel: &dyn Cancel,
    ) -> Result<(), String> {
        check_cancel(cancel)?;
        match self {
            Self::Null => output.push_str("null"),
            Self::Boolean(value) => output.push_str(if *value { "true" } else { "false" }),
            Self::Number(value) => output.push_str(value),
            Self::String(value) => quote_json(value, output, cancel)?,
            Self::Array(values) => {
                write_container(
                    output,
                    depth,
                    values,
                    |value, output, depth, cancel| value.write_pretty(output, depth, cancel),
                    '[',
                    ']',
                    cancel,
                )?;
            }
            Self::Object(entries) => {
                if entries.is_empty() {
                    output.push_str("{}");
                    return Ok(());
                }
                output.push('{');
                for (index, (key, _, value)) in entries.iter().enumerate() {
                    check_cancel(cancel)?;
                    output.push('\n');
                    indent(output, depth + 1);
                    quote_json(key, output, cancel)?;
                    output.push_str(": ");
                    value.write_pretty(output, depth + 1, cancel)?;
                    if index + 1 < entries.len() {
                        output.push(',');
                    }
                }
                output.push('\n');
                indent(output, depth);
                output.push('}');
            }
        }
        Ok(())
    }
}

fn write_container<T>(
    output: &mut String,
    depth: usize,
    values: &[T],
    mut write_value: impl FnMut(&T, &mut String, usize, &dyn Cancel) -> Result<(), String>,
    open: char,
    close: char,
    cancel: &dyn Cancel,
) -> Result<(), String> {
    if values.is_empty() {
        output.push(open);
        output.push(close);
        return Ok(());
    }
    output.push(open);
    for (index, value) in values.iter().enumerate() {
        check_cancel(cancel)?;
        output.push('\n');
        indent(output, depth + 1);
        write_value(value, output, depth + 1, cancel)?;
        if index + 1 < values.len() {
            output.push(',');
        }
    }
    output.push('\n');
    indent(output, depth);
    output.push(close);
    Ok(())
}

fn indent(output: &mut String, depth: usize) {
    for _ in 0..depth.saturating_mul(2) {
        output.push(' ');
    }
}

fn quote_json(value: &str, output: &mut String, cancel: &dyn Cancel) -> Result<(), String> {
    output.push('"');
    for (index, character) in value.chars().enumerate() {
        if index % 1024 == 0 {
            check_cancel(cancel)?;
        }
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                let _ = write!(output, "\\u{:04x}", u32::from(character));
            }
            character => output.push(character),
        }
    }
    output.push('"');
    Ok(())
}

struct JsonParser<'a> {
    text: &'a str,
    cancel: &'a dyn Cancel,
    cursor: usize,
    depth: usize,
    max_depth: usize,
    layout_items: usize,
    depth_limit: usize,
}

pub(super) fn parse_json(text: &str, cancel: &dyn Cancel) -> Result<JsonValue, String> {
    let mut parser = JsonParser::new(text, cancel);
    parser.depth_limit = 128;
    parser.document()
}

impl<'a> JsonParser<'a> {
    const fn new(text: &'a str, cancel: &'a dyn Cancel) -> Self {
        Self {
            text,
            cancel,
            cursor: 0,
            depth: 0,
            max_depth: 0,
            layout_items: 0,
            depth_limit: 256,
        }
    }

    fn document(&mut self) -> Result<JsonValue, String> {
        let result: Result<JsonValue, String> = (|| {
            let value = self.value()?;
            self.whitespace()?;
            if self.cursor != self.text.len() {
                return Err("unexpected text after the JSON value".to_owned());
            }
            Ok(value)
        })();
        result.map_err(|error| source_error(self.text, self.cursor, &error))
    }

    fn value(&mut self) -> Result<JsonValue, String> {
        check_cancel(self.cancel)?;
        self.layout_items = self.layout_items.saturating_add(1);
        check_parse_memory_bound(
            self.text.len(),
            self.layout_items,
            ESTIMATED_BYTES_PER_LAYOUT_ITEM,
        )?;
        self.whitespace()?;
        match self.peek() {
            Some(b'n') => {
                self.literal("null")?;
                Ok(JsonValue::Null)
            }
            Some(b't') => {
                self.literal("true")?;
                Ok(JsonValue::Boolean(true))
            }
            Some(b'f') => {
                self.literal("false")?;
                Ok(JsonValue::Boolean(false))
            }
            Some(b'"') => self.string().map(JsonValue::String),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) | None => Err("expected a JSON value".to_owned()),
        }
    }

    fn array(&mut self) -> Result<JsonValue, String> {
        self.enter()?;
        let result = self.array_contents();
        self.depth -= 1;
        result
    }

    fn array_contents(&mut self) -> Result<JsonValue, String> {
        self.cursor += 1;
        self.whitespace()?;
        let mut values = Vec::new();
        if self.take(b']') {
            return Ok(JsonValue::Array(values));
        }
        loop {
            values.push(self.value()?);
            self.whitespace()?;
            if self.take(b']') {
                break;
            }
            self.expect(b',')?;
        }
        Ok(JsonValue::Array(values))
    }

    fn object(&mut self) -> Result<JsonValue, String> {
        self.enter()?;
        let result = self.object_contents();
        self.depth -= 1;
        result
    }

    fn object_contents(&mut self) -> Result<JsonValue, String> {
        self.cursor += 1;
        self.whitespace()?;
        let mut entries = Vec::new();
        if self.take(b'}') {
            return Ok(JsonValue::Object(entries));
        }
        loop {
            self.whitespace()?;
            let key_offset = self.cursor;
            let key = self.string()?;
            self.layout_items = self.layout_items.saturating_add(1);
            check_parse_memory_bound(
                self.text.len(),
                self.layout_items,
                ESTIMATED_BYTES_PER_LAYOUT_ITEM,
            )?;
            self.whitespace()?;
            self.expect(b':')?;
            entries.push((key, key_offset, self.value()?));
            self.whitespace()?;
            if self.take(b'}') {
                break;
            }
            self.expect(b',')?;
        }
        Ok(JsonValue::Object(entries))
    }

    fn string(&mut self) -> Result<String, String> {
        let start = self.cursor;
        self.expect(b'"')?;
        let bytes = self.text.as_bytes();
        while let Some(byte) = bytes.get(self.cursor).copied() {
            if self.cursor.is_multiple_of(1024) {
                check_cancel(self.cancel)?;
            }
            self.cursor += 1;
            match byte {
                b'"' => {
                    return serde_json::from_str(&self.text[start..self.cursor]).map_err(|error| {
                        let message = error.to_string();
                        let location =
                            format!(" at line {} column {}", error.line(), error.column());
                        format!(
                            "invalid JSON string: {}",
                            message.strip_suffix(&location).unwrap_or(&message)
                        )
                    });
                }
                b'\\' => {
                    if self.cursor >= bytes.len() {
                        break;
                    }
                    self.cursor += 1;
                }
                _ => {}
            }
        }
        Err("unterminated JSON string".to_owned())
    }

    fn number(&mut self) -> Result<JsonValue, String> {
        let start = self.cursor;
        let mut scanned = 0usize;
        while self
            .peek()
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b',' | b']' | b'}'))
        {
            self.cursor += 1;
            scanned += 1;
            if scanned == 1024 {
                check_cancel(self.cancel)?;
                scanned = 0;
            }
        }
        let number = &self.text[start..self.cursor];
        if serde_json::from_str::<serde_json::Number>(number).is_err() {
            return Err("invalid JSON number".to_owned());
        }
        Ok(JsonValue::Number(number.to_owned()))
    }

    fn literal(&mut self, value: &str) -> Result<(), String> {
        if self.text[self.cursor..].starts_with(value) {
            self.cursor += value.len();
            Ok(())
        } else {
            Err("invalid JSON literal".to_owned())
        }
    }

    fn whitespace(&mut self) -> Result<(), String> {
        let mut scanned = 0usize;
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.cursor += 1;
            scanned += 1;
            if scanned == 1024 {
                check_cancel(self.cancel)?;
                scanned = 0;
            }
        }
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.cursor).copied()
    }

    fn take(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.take(byte) {
            Ok(())
        } else {
            Err(format!("expected '{}' in JSON", char::from(byte)))
        }
    }

    fn enter(&mut self) -> Result<(), String> {
        if self.depth >= self.depth_limit {
            return Err(format!(
                "JSON nesting exceeds the parser limit of {} levels",
                self.depth_limit
            ));
        }
        self.depth += 1;
        self.max_depth = self.max_depth.max(self.depth);
        Ok(())
    }
}

pub(super) fn xml_reason(error: impl std::fmt::Display) -> String {
    let message = error.to_string();
    let reason = message
        .split_once(": ")
        .filter(|(prefix, _)| {
            prefix.strip_prefix("at ").is_some_and(|range| {
                range.split_once("..").is_some_and(|(start, end)| {
                    start.parse::<usize>().is_ok() && end.parse::<usize>().is_ok()
                })
            })
        })
        .map_or(message.as_str(), |(_, reason)| reason);
    reason.to_owned()
}

fn format_xml(text: &str, cancel: &dyn Cancel) -> Result<String, String> {
    let mut error_offset = 0;
    format_xml_at(text, cancel, &mut error_offset)
        .map_err(|error| source_error(text, error_offset, &error))
}

#[allow(clippy::too_many_lines)]
fn format_xml_at(
    text: &str,
    cancel: &dyn Cancel,
    error_offset: &mut usize,
) -> Result<String, String> {
    if let Some((offset, _)) = text
        .char_indices()
        .find(|(_, character)| !crate::structure::xml_char(*character))
    {
        *error_offset = offset;
        return Err("invalid XML 1.0 character".into());
    }
    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = true;
    let mut document = Vec::new();
    let mut stack: Vec<XmlFrame> = Vec::new();
    let mut roots = 0_usize;
    let mut layout_items = 0usize;
    let mut max_depth = 0usize;
    loop {
        *error_offset = usize::try_from(reader.buffer_position()).unwrap_or(text.len());
        check_cancel(cancel)?;
        let event = reader
            .read_event()
            .map_err(|error| format!("invalid XML: {}", diagnostic_reason(error)))?;
        layout_items = layout_items.saturating_add(1);
        check_parse_memory_bound(text.len(), layout_items, ESTIMATED_BYTES_PER_LAYOUT_ITEM)?;
        match event {
            Event::Eof => break,
            Event::Start(start) => {
                if stack.is_empty() {
                    roots += 1;
                }
                if stack.len() >= 512 {
                    return Err("XML nesting exceeds the formatter limit of 512 levels".to_owned());
                }
                max_depth = max_depth.max(stack.len() + 1);
                let inherited = stack.last().is_some_and(|frame| frame.preserve_space);
                let preserve_space = xml_space(&start, inherited, cancel)?;
                stack.push(XmlFrame {
                    start: Event::Start(start.into_owned()),
                    children: Vec::new(),
                    preserve_space,
                });
            }
            Event::End(end) => {
                let Some(frame) = stack.pop() else {
                    return Err("invalid XML: closing tag without an open element".to_owned());
                };
                push_xml_node(
                    &mut document,
                    &mut stack,
                    XmlNode::Element {
                        start: frame.start,
                        children: frame.children,
                        end: Event::End(end.into_owned()),
                        preserve_space: frame.preserve_space,
                    },
                );
            }
            Event::Empty(empty) => {
                let inherited = stack.last().is_some_and(|frame| frame.preserve_space);
                xml_space(&empty, inherited, cancel)?;
                let event = Event::Empty(empty.into_owned());
                if stack.is_empty() {
                    roots += 1;
                }
                push_xml_node(&mut document, &mut stack, XmlNode::Event(event));
            }
            Event::Text(text_event) => {
                if stack.is_empty() && !text_event.iter().all(u8::is_ascii_whitespace) {
                    return Err(
                        "invalid XML: character data outside the document element".to_owned()
                    );
                }
                push_xml_node(
                    &mut document,
                    &mut stack,
                    XmlNode::Text(Event::Text(text_event.into_owned())),
                );
            }
            Event::CData(cdata) => {
                if stack.is_empty() {
                    return Err("invalid XML: CDATA outside the document element".to_owned());
                }
                push_xml_node(
                    &mut document,
                    &mut stack,
                    XmlNode::Text(Event::CData(cdata.into_owned())),
                );
            }
            Event::GeneralRef(reference) => {
                if reference
                    .resolve_char_ref()
                    .map_err(xml_reason)?
                    .is_some_and(|ch| !crate::structure::xml_char(ch))
                {
                    return Err("invalid XML character reference".to_owned());
                }
                if stack.is_empty() {
                    return Err(
                        "invalid XML: entity reference outside the document element".to_owned()
                    );
                }
                push_xml_node(
                    &mut document,
                    &mut stack,
                    XmlNode::Text(Event::GeneralRef(reference.into_owned())),
                );
            }
            Event::DocType(_) if roots > 0 => {
                return Err("invalid XML: document type follows the document element".to_owned());
            }
            Event::Decl(_) if roots > 0 => {
                return Err("invalid XML: declaration follows the document element".to_owned());
            }
            other => push_xml_node(
                &mut document,
                &mut stack,
                XmlNode::Event(other.into_owned()),
            ),
        }
    }
    validate_xml_document(&stack, roots)?;
    check_output_bound(text.len(), layout_items, max_depth)?;
    write_xml_document(text, document, cancel)
}

fn check_output_bound(input_bytes: usize, layout_items: usize, depth: usize) -> Result<(), String> {
    let indentation_and_layout =
        layout_items.saturating_mul(depth.saturating_mul(2).saturating_add(8));
    let estimate = input_bytes.saturating_add(indentation_and_layout);
    if estimate > MAX_FORMATTED_BYTES {
        return Err(format!(
            "formatted output may exceed the {} MiB limit",
            MAX_FORMATTED_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

fn check_formatted_size(bytes: usize) -> Result<(), String> {
    if bytes > MAX_FORMATTED_BYTES {
        return Err(format!(
            "formatted output exceeds the {} MiB limit",
            MAX_FORMATTED_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

fn check_parse_memory_bound(
    input_bytes: usize,
    layout_items: usize,
    bytes_per_item: usize,
) -> Result<(), String> {
    check_storage_estimate(
        input_bytes
            .saturating_mul(2)
            .saturating_add(layout_items.saturating_mul(bytes_per_item)),
    )
}

fn check_storage_estimate(estimated: usize) -> Result<(), String> {
    if estimated > MAX_FORMAT_TEMP_BYTES {
        return Err(format!(
            "structured input exceeds the {} MiB temporary formatting memory limit",
            MAX_FORMAT_TEMP_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

fn validate_xml_document(stack: &[XmlFrame], roots: usize) -> Result<(), String> {
    if !stack.is_empty() {
        return Err("invalid XML: unclosed element".to_owned());
    }
    if roots != 1 {
        return Err("invalid XML: expected one document element".to_owned());
    }
    Ok(())
}

fn write_xml_document(
    text: &str,
    document: Vec<XmlNode>,
    cancel: &dyn Cancel,
) -> Result<String, String> {
    let mut output = String::with_capacity(text.len().saturating_add(32));
    let mut wrote_node = false;
    for node in document {
        check_cancel(cancel)?;
        if matches!(&node, XmlNode::Text(event) if xml_whitespace(event)) {
            continue;
        }
        if wrote_node {
            output.push('\n');
        }
        write_xml_node(&node, 0, &mut output, true, cancel)?;
        wrote_node = true;
    }
    Ok(output)
}

struct XmlFrame {
    start: Event<'static>,
    children: Vec<XmlNode>,
    preserve_space: bool,
}

enum XmlNode {
    Element {
        start: Event<'static>,
        children: Vec<Self>,
        end: Event<'static>,
        preserve_space: bool,
    },
    Event(Event<'static>),
    Text(Event<'static>),
}

fn push_xml_node(document: &mut Vec<XmlNode>, stack: &mut [XmlFrame], node: XmlNode) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else {
        document.push(node);
    }
}

/// Validates every attribute of `start` and returns the effective
/// `xml:space="preserve"` state. Every attribute is read, since a malformed
/// attribute after `xml:space` would otherwise pass through unchecked.
fn xml_space(
    start: &quick_xml::events::BytesStart<'_>,
    inherited: bool,
    cancel: &dyn Cancel,
) -> Result<bool, String> {
    let mut preserve = inherited;
    for attribute in start.attributes() {
        check_cancel(cancel)?;
        let attribute = attribute.map_err(|error| match error {
            quick_xml::events::attributes::AttrError::Duplicated(_, _) => {
                "duplicate XML attribute".to_owned()
            }
            other => {
                let message = other.to_string();
                let reason = message
                    .split_once(": ")
                    .filter(|(position, _)| position.starts_with("position "))
                    .map_or(message.as_str(), |(_, reason)| reason);
                format!("invalid XML attribute: {}", diagnostic_reason(reason))
            }
        })?;
        let value = attribute.unescape_value().map_err(xml_reason)?;
        if value.chars().any(|ch| !crate::structure::xml_char(ch)) {
            return Err("invalid XML character reference".to_owned());
        }
        if attribute.key.as_ref() == b"xml:space" {
            preserve = match value.as_ref() {
                "preserve" => true,
                "default" => false,
                _ => return Err("xml:space must be 'default' or 'preserve'".to_owned()),
            };
        }
    }
    Ok(preserve)
}

fn xml_whitespace(event: &Event<'_>) -> bool {
    match event {
        Event::Text(text) => text
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n')),
        _ => false,
    }
}

fn xml_indentation(event: &Event<'_>) -> bool {
    matches!(event, Event::Text(text) if xml_whitespace(event) && (text.contains(&b'\n') || text.contains(&b'\r')))
}

fn element_only(children: &[XmlNode]) -> bool {
    let has_markup = children.iter().any(|child| {
        matches!(
            child,
            XmlNode::Element { .. }
                | XmlNode::Event(Event::Empty(_) | Event::Comment(_) | Event::PI(_))
        )
    });
    has_markup
        && children.iter().all(|child| match child {
            XmlNode::Text(event) => xml_indentation(event),
            XmlNode::Element { .. }
            | XmlNode::Event(Event::Empty(_) | Event::Comment(_) | Event::PI(_)) => true,
            XmlNode::Event(_) => false,
        })
}

fn write_xml_node(
    node: &XmlNode,
    depth: usize,
    output: &mut String,
    layout: bool,
    cancel: &dyn Cancel,
) -> Result<(), String> {
    check_cancel(cancel)?;
    match node {
        XmlNode::Element {
            start,
            children,
            end,
            preserve_space,
        } if layout && !preserve_space && element_only(children) => {
            write_xml_event(start, output, cancel)?;
            let mut wrote_child = false;
            for child in children {
                check_cancel(cancel)?;
                if matches!(child, XmlNode::Text(event) if xml_whitespace(event)) {
                    continue;
                }
                output.push('\n');
                indent(output, depth + 1);
                write_xml_node(child, depth + 1, output, true, cancel)?;
                wrote_child = true;
            }
            if wrote_child {
                output.push('\n');
                indent(output, depth);
            }
            write_xml_event(end, output, cancel)
        }
        XmlNode::Element {
            start,
            children,
            end,
            ..
        } => {
            write_xml_event(start, output, cancel)?;
            for child in children {
                check_cancel(cancel)?;
                write_xml_node(child, depth, output, false, cancel)?;
            }
            write_xml_event(end, output, cancel)
        }
        XmlNode::Event(event) | XmlNode::Text(event) => write_xml_event(event, output, cancel),
    }
}

fn write_xml_event(
    event: &Event<'_>,
    output: &mut String,
    cancel: &dyn Cancel,
) -> Result<(), String> {
    check_cancel(cancel)?;
    let mut bytes = Vec::new();
    quick_xml::Writer::new(&mut bytes)
        .write_event(event.borrow())
        .map_err(|error| format!("could not format XML: {error}"))?;
    output.push_str(
        std::str::from_utf8(&bytes).map_err(|error| format!("XML is not UTF-8: {error}"))?,
    );
    Ok(())
}

fn check_cancel(cancel: &dyn Cancel) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("formatting cancelled".to_owned())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{format, format_cancellable, pair_format, StructuredFormat};

    #[test]
    fn xml_diagnostics_escape_direction_controls() {
        let error = format("<r></x\u{202e}>", StructuredFormat::Xml)
            .err()
            .unwrap_or_default();
        assert!(error.contains("\\u{202e}"), "{error}");
        assert!(!error.contains('\u{202e}'));
    }

    #[test]
    fn xml_diagnostics_bound_long_tag_names() {
        let name = "x".repeat(100_000);
        let error = format(&format!("<{name}></other>"), StructuredFormat::Xml)
            .err()
            .unwrap_or_default();
        assert!(error.contains("truncated"));
        assert!(error.len() < 1200, "{}", error.len());
    }

    #[test]
    fn xml_rejects_invalid_character_references_and_uses_one_error_position() {
        for input in ["<r>&#1;</r>", "<r>&#xFFFF;</r>", "<r a='&#1;'/>"] {
            assert!(format(input, StructuredFormat::Xml).is_err(), "{input}");
        }
        let error = format("<r xml:space='&bogus;'/>", StructuredFormat::Xml)
            .err()
            .unwrap_or_default();
        assert!(!error.contains(".."), "{error}");
    }

    #[test]
    fn xml_entity_errors_do_not_expose_token_relative_positions() {
        for input in [
            "<r xml:space='&bogus;'/>",
            "<r a='&bogus;'/>",
            "<r>&bogus;</r>",
        ] {
            let error = crate::structure::compare(
                input,
                "<r/>",
                crate::structure::Format::Xml,
                &ca_ui::worker::Cancel::new(),
            )
            .err()
            .unwrap_or_default();
            assert!(!error.is_empty());
            assert!(!error.contains(".."), "{error}");
        }
        let error = format("<r xml:space='&bogus;'/>", StructuredFormat::Xml)
            .err()
            .unwrap_or_default();
        assert!(!error.contains(".."), "{error}");
    }

    #[test]
    fn diagnostics_escape_invisible_format_characters() {
        for ch in ['\u{200b}', '\u{feff}', '\u{ad}', '\u{fff9}', '\u{e0001}'] {
            let reason = super::diagnostic_reason(format!("bad {ch} name"));
            assert!(!reason.contains(ch), "{reason:?}");
        }
    }

    #[test]
    fn xml_rejects_invalid_characters() {
        let error = format("<r>\u{1}</r>", StructuredFormat::Xml)
            .err()
            .unwrap_or_default();
        assert!(
            error.contains("byte 3: invalid XML 1.0 character"),
            "{error}"
        );
    }

    #[test]
    fn xml_attribute_syntax_errors_use_one_position_system() {
        for text in ["<r a=1/>", "<r a/>", "<r a=/>", "<r a b='1'/>"] {
            let error = format(text, StructuredFormat::Xml)
                .err()
                .unwrap_or_default();
            assert!(error.starts_with("line 1, column 1, byte 0: "), "{error}");
            assert!(error.contains("invalid XML attribute: "), "{error}");
            assert!(!error.contains("position"), "{error}");
        }
    }

    #[test]
    fn json_string_errors_use_one_position_system() {
        for text in ["{\"a\u{1}\":1}", "[\"\\q\"]", "\n[\"\\ud800x\"]"] {
            let error = format(text, StructuredFormat::Json)
                .err()
                .unwrap_or_default();
            assert!(error.contains("invalid JSON string: "), "{error}");
            assert_eq!(error.matches("line").count(), 1, "{error}");
            assert_eq!(error.matches("column").count(), 1, "{error}");
        }
    }

    #[test]
    fn xml_duplicate_attributes_use_one_position_system() {
        let error = format("<r a='1' a='2'/>", StructuredFormat::Xml)
            .err()
            .unwrap_or_default();
        assert!(error.contains("duplicate XML attribute"), "{error}");
        assert!(!error.contains("position"), "{error}");
    }

    #[test]
    fn diagnostic_positions_include_the_leading_encoding_signature() {
        for (text, kind, location) in [
            ("{", StructuredFormat::Json, "line 1, column 3, byte 4:"),
            (
                "<r></s>",
                StructuredFormat::Xml,
                "line 1, column 5, byte 6:",
            ),
        ] {
            let marked = format(&format!("\u{feff}{text}"), kind)
                .err()
                .unwrap_or_default();
            assert!(marked.starts_with(location), "{marked}");
        }
    }
    use ca_diff::Cancel;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CancelAfter {
        polls: AtomicUsize,
        limit: usize,
    }

    impl Cancel for CancelAfter {
        fn is_cancelled(&self) -> bool {
            self.polls.fetch_add(1, Ordering::Relaxed) >= self.limit
        }
    }

    fn formatted(text: &str, kind: StructuredFormat) -> String {
        format(text, kind).unwrap_or_default()
    }

    #[test]
    fn cancellation_stops_json_parsing_inside_one_file() {
        let cancel = CancelAfter {
            polls: AtomicUsize::new(0),
            limit: 8,
        };
        let text = format!("[{}]", "0,".repeat(10_000));

        assert!(format_cancellable(&text, StructuredFormat::Json, &cancel)
            .is_err_and(|error| error.contains("cancelled")));
        assert!(cancel.polls.load(Ordering::Relaxed) >= cancel.limit);
    }

    #[test]
    fn cancellation_stops_xml_parsing_inside_one_file() {
        let cancel = CancelAfter {
            polls: AtomicUsize::new(0),
            limit: 8,
        };
        let text = format!("<root>{}</root>", "<item/>".repeat(10_000));

        assert!(format_cancellable(&text, StructuredFormat::Xml, &cancel)
            .is_err_and(|error| error.contains("cancelled")));
        assert!(cancel.polls.load(Ordering::Relaxed) >= cancel.limit);
    }

    #[test]
    fn json_indentation_preserves_order_duplicate_keys_and_large_numbers() {
        let compact = r#"{"z":1.2300,"a":{"x":1,"x":2},"large":123456789012345678901234567890}"#;
        let pretty = formatted(compact, StructuredFormat::Json);
        assert_eq!(
            pretty,
            "{\n  \"z\": 1.2300,\n  \"a\": {\n    \"x\": 1,\n    \"x\": 2\n  },\n  \"large\": 123456789012345678901234567890\n}"
        );
    }

    #[test]
    fn json_formatter_refuses_invalid_or_trailing_content() {
        for input in [
            "{\"x\":}",
            "{} {}",
            "[1,]",
            "\u{000b}{}",
            "{\"x\":\u{000c}1}",
        ] {
            assert!(format(input, StructuredFormat::Json).is_err(), "{input:?}");
        }
    }

    #[test]
    fn json_indentation_refuses_expansion_past_the_output_bound() {
        let nested = "[".repeat(255);
        let values = "0,".repeat(64_000);
        let text = format!("{nested}{values}0{}", "]".repeat(255));

        assert!(format(&text, StructuredFormat::Json)
            .is_err_and(|error| error.contains("24 MiB limit")));
    }

    #[test]
    fn json_parser_refuses_a_tree_that_exceeds_its_memory_budget() {
        let text = format!("[{}0]", "0,".repeat(700_000));

        assert!(format(&text, StructuredFormat::Json)
            .is_err_and(|error| error.contains("temporary formatting memory limit")));
    }

    #[test]
    fn xml_indentation_refuses_expansion_past_the_output_bound() {
        let nested_open = "<a>".repeat(511);
        let children = "<b/>".repeat(50_000);
        let text = format!("{nested_open}{children}{}</a>", "</a>".repeat(510));

        assert!(
            format(&text, StructuredFormat::Xml).is_err_and(|error| error.contains("24 MiB limit"))
        );
    }

    #[test]
    fn xml_parser_refuses_a_tree_that_exceeds_its_memory_budget() {
        let text = format!("<root>{}</root>", "<i/>".repeat(700_000));

        assert!(format(&text, StructuredFormat::Xml)
            .is_err_and(|error| error.contains("temporary formatting memory limit")));
    }

    #[test]
    fn xml_indents_element_only_content_and_retains_attributes() {
        let pretty = formatted(
            "<?xml version=\"1.0\"?><root a='1'><item><name>x</name></item><empty/></root>",
            StructuredFormat::Xml,
        );
        assert_eq!(
            pretty,
            "<?xml version=\"1.0\"?>\n<root a='1'>\n  <item>\n    <name>x</name>\n  </item>\n  <empty/>\n</root>"
        );
    }

    #[test]
    fn xml_mixed_content_and_xml_space_preserve_are_kept_intact() {
        let mixed = "<p>Hello <b>bold</b> world</p>";
        assert_eq!(formatted(mixed, StructuredFormat::Xml), mixed);
        let preserved = "<root xml:space=\"preserve\"><x/><y/></root>";
        assert_eq!(formatted(preserved, StructuredFormat::Xml), preserved);
        let whitespace_text = "<root>   </root>";
        assert_eq!(
            formatted(whitespace_text, StructuredFormat::Xml),
            whitespace_text
        );

        let inline_spacing = "<p> <b/> </p>";
        assert_eq!(
            formatted(inline_spacing, StructuredFormat::Xml),
            inline_spacing
        );
    }

    #[test]
    fn xml_formatter_rejects_mismatched_and_unclosed_elements() {
        for input in ["<a><b></a>", "<a>", "<a/><b/>"] {
            assert!(format(input, StructuredFormat::Xml).is_err(), "{input:?}");
        }
    }

    #[test]
    fn xml_rejects_character_data_outside_the_document_element() {
        for malformed in [
            "text<root/>",
            "<root/>text",
            "<![CDATA[text]]><root/>",
            "<root/><!DOCTYPE other>",
        ] {
            assert!(
                format(malformed, StructuredFormat::Xml).is_err(),
                "accepted malformed XML: {malformed}"
            );
        }
    }

    #[test]
    fn a_leading_byte_order_mark_is_not_part_of_the_structured_text() {
        assert_eq!(
            format("\u{feff}{\"a\":[1,2]}", StructuredFormat::Json),
            Ok("{\n  \"a\": [\n    1,\n    2\n  ]\n}".to_owned())
        );
        assert_eq!(
            format("\u{feff}<root><a/></root>", StructuredFormat::Xml),
            Ok("<root>\n  <a/>\n</root>".to_owned())
        );
        assert!(format("\u{feff}\u{feff}{}", StructuredFormat::Json).is_err());
    }

    #[test]
    fn crlf_indentation_formats_like_lf_indentation() {
        let json_lf = "{\n  \"a\": [\n    1\n  ]\n}";
        let json_crlf = json_lf.replace('\n', "\r\n");
        assert_eq!(
            format(&json_crlf, StructuredFormat::Json),
            format(json_lf, StructuredFormat::Json)
        );
        let xml_lf = "<root>\n  <a>x</a>\n  <b/>\n</root>";
        let xml_crlf = xml_lf.replace('\n', "\r\n");
        assert_eq!(
            format(&xml_crlf, StructuredFormat::Xml),
            Ok(xml_lf.to_owned())
        );
        let mixed = "<p>one\r\ntwo <b>x</b></p>";
        assert_eq!(format(mixed, StructuredFormat::Xml), Ok(mixed.to_owned()));
    }

    #[test]
    fn empty_and_whitespace_only_json_and_xml_are_refused() {
        for text in ["", "  \r\n\t"] {
            assert!(format(text, StructuredFormat::Json).is_err(), "{text:?}");
            assert!(format(text, StructuredFormat::Xml).is_err(), "{text:?}");
        }
    }

    #[test]
    fn nesting_limits_accept_the_limit_and_refuse_one_level_more() {
        let json = |depth: usize| format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert!(format(&json(256), StructuredFormat::Json).is_ok());
        assert!(format(&json(257), StructuredFormat::Json)
            .is_err_and(|error| error.contains("256 levels")));
        let xml = |depth: usize| format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth));
        assert!(format(&xml(512), StructuredFormat::Xml).is_ok());
        assert!(format(&xml(513), StructuredFormat::Xml)
            .is_err_and(|error| error.contains("512 levels")));
    }

    #[test]
    fn input_larger_than_the_input_budget_is_refused_before_parsing() {
        let text = format!("\"{}\"", "a".repeat(16 * 1024 * 1024));
        for kind in [StructuredFormat::Json, StructuredFormat::Xml] {
            assert!(format(&text, kind).is_err_and(|error| error.contains("16 MiB")));
        }
    }

    #[test]
    fn malformed_minified_json_is_refused() {
        for input in [
            "[1,2",
            "{\"a\" 1}",
            "{a:1}",
            "{\"a\":1,}",
            "[01]",
            "[1.]",
            "[.5]",
            "[+1]",
            "[-]",
            "[NaN]",
            "[\"\\x\"]",
            "[\"\\ud800\"]",
            "[\"a\u{1}b\"]",
            "[\"unterminated]",
            "[true false]",
            "[1]]",
            "\u{a0}[]",
        ] {
            assert!(format(input, StructuredFormat::Json).is_err(), "{input:?}");
        }
    }

    #[test]
    fn malformed_attributes_are_refused_on_every_element_form() {
        for input in [
            "<root><a b=1/></root>",
            "<root><a x=\"1\" x=\"2\"/></root>",
            "<root xml:space=\"preserve\" b=1></root>",
            "<root xml:space=\"preserve\" xml:space=\"default\"></root>",
        ] {
            assert!(format(input, StructuredFormat::Xml).is_err(), "{input:?}");
        }
    }

    #[test]
    fn malformed_minified_xml_is_refused() {
        for input in [
            "<a>x & y</a>",
            "<a>1 < 2</a>",
            "<a><b></a></b>",
            "<a",
            "</a>",
        ] {
            assert!(format(input, StructuredFormat::Xml).is_err(), "{input:?}");
        }
    }

    #[test]
    fn structured_pair_selection_requires_matching_supported_formats() {
        assert_eq!(
            pair_format(Path::new("left.json"), Path::new("right.JSON")),
            Some(StructuredFormat::Json)
        );
        assert_eq!(
            pair_format(Path::new("left.xml"), Path::new("right.svg")),
            Some(StructuredFormat::Xml)
        );
        assert_eq!(
            pair_format(Path::new("left.json"), Path::new("right.xml")),
            None
        );
        assert_eq!(
            pair_format(Path::new("left.jsonc"), Path::new("right.json")),
            None
        );
    }
}
