//! Standard-table TOML projection for structured text comparison.
//!
//! Keys and values keep their source spellings and every table keeps its key
//! order. Inline tables, dotted keys and standard tables that hold the same
//! ordered tree produce the same projection. The projection is parsed again
//! and must produce the same tree and the same comments as the source,
//! otherwise the input is refused.

use super::{check_cancel, check_formatted_size, check_parse_memory_bound};
use ca_diff::Cancel;
use toml_edit::{Array, Decor, DocumentMut, InlineTable, Item, Key, RawString, Table, Value};

/// Deepest table and array nesting the projection writes.
const MAX_DEPTH: usize = 256;
/// Conservative storage per TOML token for the parsed documents and the
/// projection trees of the source and the formatted text together.
const ESTIMATED_BYTES_PER_ITEM: usize = 1024;
/// Budget items for each `.` outside strings and comments. Every segment of a
/// dotted key or table header adds a table to both parsed documents.
const ITEMS_PER_DOT: usize = 3;
/// Storage counted for each source byte: the text copies both parsed
/// documents and both projection trees keep.
const BYTES_PER_INPUT_BYTE: usize = 12;
/// Stack of the thread that parses a document and drops it. The parser
/// accepts 80 key segments and 80 levels of value nesting, so a document it
/// reads, valid or not, can nest about 6,500 tables, and dropping them
/// recurses once for each level. That drop needs more than 2 MiB of stack in
/// an unoptimized build.
const PARSE_STACK_BYTES: usize = 32 * 1024 * 1024;
const REFUSED: &str = "formatting would change the TOML content, so the original text is kept";

pub(super) fn format(text: &str, cancel: &dyn Cancel) -> Result<String, String> {
    let source = scan(text, cancel)?;
    check_parse_memory_bound(
        text.len().saturating_mul(BYTES_PER_INPUT_BYTE / 2),
        source.items,
        ESTIMATED_BYTES_PER_ITEM,
    )?;
    let tree = parse(text, cancel)?;
    let mut writer = Writer::new(cancel);
    writer.document(&tree)?;
    let output = writer.finish()?;
    verify(&tree, source.comments, &output, cancel)?;
    Ok(output)
}

fn verify(
    tree: &TableNode,
    mut comments: Vec<String>,
    output: &str,
    cancel: &dyn Cancel,
) -> Result<(), String> {
    let refused = || {
        if cancel.is_cancelled() {
            "formatting cancelled".to_owned()
        } else {
            REFUSED.to_owned()
        }
    };
    let formatted = parse(output, cancel).map_err(|_| refused())?;
    if !tree.same(&formatted) {
        return Err(refused());
    }
    let mut written = scan(output, cancel).map_err(|_| refused())?.comments;
    comments.sort_unstable();
    written.sort_unstable();
    if comments == written {
        Ok(())
    } else {
        Err(refused())
    }
}

/// Comments and an upper bound on the token count, read without the parser.
struct Scan {
    comments: Vec<String>,
    items: usize,
}

/// Lexes `text` for comments outside strings and counts structural tokens,
/// so the parse budget is checked before the parser retains a tree.
fn scan(text: &str, cancel: &dyn Cancel) -> Result<Scan, String> {
    let bytes = text.as_bytes();
    let mut comments = Vec::new();
    let mut items = 1usize;
    let mut index = 0;
    let mut steps = 0usize;
    while index < bytes.len() {
        steps += 1;
        if steps.is_multiple_of(4096) {
            check_cancel(cancel)?;
        }
        match bytes[index] {
            b'#' => {
                let stop = bytes[index..]
                    .iter()
                    .position(|&byte| byte == b'\n')
                    .map_or(bytes.len(), |offset| index + offset);
                comments.push(text[index..stop].trim_end_matches('\r').to_owned());
                items = items.saturating_add(1);
                index = stop;
            }
            quote @ (b'"' | b'\'') => {
                let escapes = quote == b'"';
                index = if bytes[index..].starts_with(&[quote; 3]) {
                    skip_multiline_string(bytes, index + 3, quote, escapes)
                } else {
                    skip_string(bytes, index + 1, quote, escapes)
                };
            }
            b'=' | b',' | b'[' | b'{' | b'\n' => {
                items = items.saturating_add(1);
                index += 1;
            }
            b'.' => {
                items = items.saturating_add(ITEMS_PER_DOT);
                index += 1;
            }
            _ => index += 1,
        }
    }
    Ok(Scan { comments, items })
}

fn skip_string(bytes: &[u8], mut index: usize, quote: u8, escapes: bool) -> usize {
    while let Some(&byte) = bytes.get(index) {
        if escapes && byte == b'\\' {
            index += 2;
        } else if byte == quote {
            return index + 1;
        } else if byte == b'\n' {
            return index;
        } else {
            index += 1;
        }
    }
    bytes.len()
}

/// Skips past the closing delimiter, which may carry up to two extra quote
/// characters that belong to the content.
fn skip_multiline_string(bytes: &[u8], mut index: usize, quote: u8, escapes: bool) -> usize {
    while let Some(&byte) = bytes.get(index) {
        if escapes && byte == b'\\' {
            index += 2;
        } else if byte == quote && bytes[index..].starts_with(&[quote; 3]) {
            let run = bytes[index..]
                .iter()
                .take_while(|&&next| next == quote)
                .count();
            return index + run.min(5);
        } else {
            index += 1;
        }
    }
    bytes.len()
}

#[derive(Debug)]
enum Node {
    Scalar(String),
    Array(ArrayNode),
    Table(TableNode),
}

impl Node {
    /// Compares content, ignoring comments.
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Scalar(left), Self::Scalar(right)) => left == right,
            (Self::Array(left), Self::Array(right)) => {
                left.elements.len() == right.elements.len()
                    && left
                        .elements
                        .iter()
                        .zip(&right.elements)
                        .all(|(left, right)| left.value.same(&right.value))
            }
            (Self::Table(left), Self::Table(right)) => left.same(right),
            _ => false,
        }
    }

    fn has_comments(&self) -> bool {
        match self {
            Self::Scalar(_) => false,
            Self::Array(array) => {
                !array.opening.is_empty()
                    || !array.closing.is_empty()
                    || array
                        .elements
                        .iter()
                        .any(|element| !element.notes.is_empty() || element.value.has_comments())
            }
            Self::Table(table) => table.has_comments(),
        }
    }
}

/// Comments before an item and after it on its last line; comments after
/// the first trailing one follow on their own lines.
#[derive(Debug, Default)]
struct Notes {
    before: Vec<String>,
    trailing: Vec<String>,
}

impl Notes {
    fn is_empty(&self) -> bool {
        self.before.is_empty() && self.trailing.is_empty()
    }
}

#[derive(Debug, Default)]
struct TableNode {
    entries: Vec<Entry>,
    /// Comments after an inline table's `{` on the same line or before its
    /// first key.
    opening: Vec<String>,
    /// Comments before an inline table's `}`, or at the end of the document.
    closing: Vec<String>,
}

impl TableNode {
    fn same(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self
                .entries
                .iter()
                .zip(&other.entries)
                .all(|(left, right)| left.key == right.key && left.value.same(&right.value))
    }

    fn has_comments(&self) -> bool {
        !self.opening.is_empty()
            || !self.closing.is_empty()
            || self
                .entries
                .iter()
                .any(|entry| !entry.notes.is_empty() || entry.value.has_comments())
    }

    fn attach_trailing(&mut self, comment: Option<String>) {
        if let Some(comment) = comment {
            match self.entries.last_mut() {
                Some(entry) => entry.notes.trailing.push(comment),
                None => self.opening.push(comment),
            }
        }
    }
}

#[derive(Debug)]
struct Entry {
    key: String,
    notes: Notes,
    value: Node,
}

#[derive(Debug, Default)]
struct ArrayNode {
    elements: Vec<Element>,
    opening: Vec<String>,
    closing: Vec<String>,
}

impl ArrayNode {
    fn attach_trailing(&mut self, comment: Option<String>) {
        if let Some(comment) = comment {
            match self.elements.last_mut() {
                Some(element) => element.notes.trailing.push(comment),
                None => self.opening.push(comment),
            }
        }
    }
}

#[derive(Debug)]
struct Element {
    notes: Notes,
    value: Node,
}

/// Parses on a thread with a stack sized for the deepest document the parser
/// builds; a partial document of a rejected text is dropped there too.
fn parse(text: &str, cancel: &dyn Cancel) -> Result<TableNode, String> {
    check_cancel(cancel)?;
    let tree = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(PARSE_STACK_BYTES)
            .spawn_scoped(scope, || parse_tree(text))
            .map_err(|error| format!("could not start the TOML parser: {error}"))?
            .join()
            .map_err(|_| "the TOML parser stopped unexpectedly".to_owned())?
    })?;
    check_cancel(cancel)?;
    Ok(tree)
}

fn parse_tree(text: &str) -> Result<TableNode, String> {
    let document: DocumentMut = text.parse().map_err(|error| describe(text, &error))?;
    let mut root = table_node(document.as_table(), 0)?;
    let (trailing, lines) = comments(Some(document.trailing()), false)?;
    root.closing.extend(trailing);
    root.closing.extend(lines);
    Ok(root)
}

fn describe(text: &str, error: &toml_edit::TomlError) -> String {
    let message = error.message().lines().next().unwrap_or("syntax error");
    match error.span() {
        Some(span) => {
            let before = text.get(..span.start).unwrap_or(text);
            let line = before.matches('\n').count() + 1;
            let column = before
                .rsplit('\n')
                .next()
                .map_or(0, |line| line.chars().count())
                + 1;
            format!("invalid TOML at line {line}, column {column}: {message}")
        }
        None => format!("invalid TOML: {message}"),
    }
}

fn check_depth(depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        Err(format!(
            "TOML nesting exceeds the formatter limit of {MAX_DEPTH} levels"
        ))
    } else {
        Ok(())
    }
}

fn table_node(table: &Table, depth: usize) -> Result<TableNode, String> {
    check_depth(depth)?;
    let mut node = TableNode::default();
    for (name, item) in table {
        let key = table
            .key(name)
            .ok_or_else(|| "TOML key without a source spelling".to_owned())?;
        let mut notes = key_notes(key, false)?;
        let value = match item {
            Item::None => continue,
            Item::Value(value) => {
                value_decor(value.decor(), &mut notes)?;
                value_node(value, depth + 1)?
            }
            Item::Table(sub) => {
                decor_notes(sub.decor(), &mut notes)?;
                Node::Table(table_node(sub, depth + 1)?)
            }
            Item::ArrayOfTables(tables) => {
                let mut array = ArrayNode::default();
                for sub in tables {
                    let mut element_notes = Notes::default();
                    decor_notes(sub.decor(), &mut element_notes)?;
                    array.elements.push(Element {
                        notes: element_notes,
                        value: Node::Table(table_node(sub, depth + 1)?),
                    });
                }
                Node::Array(array)
            }
        };
        node.entries.push(Entry {
            key: spelling(key)?,
            notes,
            value,
        });
    }
    Ok(node)
}

fn inline_node(table: &InlineTable, depth: usize) -> Result<TableNode, String> {
    check_depth(depth)?;
    let mut node = TableNode::default();
    for (name, value) in table {
        let key = table
            .key(name)
            .ok_or_else(|| "TOML key without a source spelling".to_owned())?;
        let (trailing, before) = comments(key.leaf_decor().prefix(), true)?;
        node.attach_trailing(trailing);
        let mut notes = key_notes(key, true)?;
        notes.before = before;
        value_decor(value.decor(), &mut notes)?;
        node.entries.push(Entry {
            key: spelling(key)?,
            notes,
            value: value_node(value, depth + 1)?,
        });
    }
    let (trailing, lines) = comments(Some(table.trailing()), true)?;
    node.attach_trailing(trailing);
    node.closing = lines;
    Ok(node)
}

fn array_node(array: &Array, depth: usize) -> Result<ArrayNode, String> {
    check_depth(depth)?;
    let mut node = ArrayNode::default();
    for value in array {
        let (trailing, before) = comments(value.decor().prefix(), true)?;
        node.attach_trailing(trailing);
        let (first, rest) = comments(value.decor().suffix(), true)?;
        let mut notes = Notes {
            before,
            trailing: Vec::new(),
        };
        notes.trailing.extend(first);
        notes.trailing.extend(rest);
        node.elements.push(Element {
            notes,
            value: value_node(value, depth + 1)?,
        });
    }
    let (trailing, lines) = comments(Some(array.trailing()), true)?;
    node.attach_trailing(trailing);
    node.closing = lines;
    Ok(node)
}

fn value_node(value: &Value, depth: usize) -> Result<Node, String> {
    let raw = match value {
        Value::String(value) => value.as_repr(),
        Value::Integer(value) => value.as_repr(),
        Value::Float(value) => value.as_repr(),
        Value::Boolean(value) => value.as_repr(),
        Value::Datetime(value) => value.as_repr(),
        Value::Array(array) => return Ok(Node::Array(array_node(array, depth)?)),
        Value::InlineTable(table) => return Ok(Node::Table(inline_node(table, depth)?)),
    };
    raw.and_then(|raw| raw.as_raw().as_str())
        .map(|raw| Node::Scalar(raw.to_owned()))
        .ok_or_else(|| "TOML value without a source spelling".to_owned())
}

fn spelling(key: &Key) -> Result<String, String> {
    key.as_repr()
        .and_then(|raw| raw.as_raw().as_str())
        .map(str::to_owned)
        .ok_or_else(|| "TOML key without a source spelling".to_owned())
}

/// Comments before a key; `mid_line` keys follow `{` or `,` on a line, so
/// their first comment belongs to that line and is read by the caller.
fn key_notes(key: &Key, mid_line: bool) -> Result<Notes, String> {
    let mut notes = Notes::default();
    if !mid_line {
        let (trailing, before) = comments(key.leaf_decor().prefix(), false)?;
        notes.before.extend(trailing);
        notes.before.extend(before);
    }
    no_comments(key.leaf_decor().suffix())?;
    no_comments(key.dotted_decor().prefix())?;
    no_comments(key.dotted_decor().suffix())?;
    Ok(notes)
}

fn value_decor(decor: &Decor, notes: &mut Notes) -> Result<(), String> {
    no_comments(decor.prefix())?;
    let (first, rest) = comments(decor.suffix(), true)?;
    notes.trailing.extend(first);
    notes.trailing.extend(rest);
    Ok(())
}

/// Comments of a table header: lines before it and a comment after it.
fn decor_notes(decor: &Decor, notes: &mut Notes) -> Result<(), String> {
    let (first, before) = comments(decor.prefix(), false)?;
    notes.before.extend(first);
    notes.before.extend(before);
    let (first, rest) = comments(decor.suffix(), true)?;
    notes.trailing.extend(first);
    notes.trailing.extend(rest);
    Ok(())
}

fn no_comments(raw: Option<&RawString>) -> Result<(), String> {
    let (first, rest) = comments(raw, true)?;
    if first.is_none() && rest.is_empty() {
        Ok(())
    } else {
        Err("TOML comment in an unexpected position".to_owned())
    }
}

/// Splits layout text into comments. When `mid_line` is set the text starts
/// after other text on a line, so a comment before its first line break
/// trails that line.
fn comments(
    raw: Option<&RawString>,
    mid_line: bool,
) -> Result<(Option<String>, Vec<String>), String> {
    let Some(raw) = raw else {
        return Ok((None, Vec::new()));
    };
    let text = raw
        .as_str()
        .ok_or_else(|| "TOML layout without source text".to_owned())?;
    let mut trailing = None;
    let mut lines = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        let line = line.trim_end_matches('\r').trim_start_matches([' ', '\t']);
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('#') {
            return Err("unexpected TOML layout text".to_owned());
        }
        if mid_line && index == 0 {
            trailing = Some(line.to_owned());
        } else {
            lines.push(line.to_owned());
        }
    }
    Ok((trailing, lines))
}

/// Whether an entry can become a `[table]` or `[[array]]` section. Only
/// entries after the last non-section entry of a table can, since text after
/// a table header belongs to that table.
fn sectionable(entry: &Entry) -> bool {
    match &entry.value {
        Node::Scalar(_) => false,
        Node::Table(_) => true,
        Node::Array(array) => {
            entry.notes.is_empty()
                && array.opening.is_empty()
                && array.closing.is_empty()
                && !array.elements.is_empty()
                && array
                    .elements
                    .iter()
                    .all(|element| matches!(element.value, Node::Table(_)))
        }
    }
}

fn section_split(table: &TableNode) -> usize {
    let mut split = table.entries.len();
    while split > 0 && sectionable(&table.entries[split - 1]) {
        split -= 1;
    }
    split
}

struct Writer<'a> {
    out: String,
    /// The current line is a comment, so no trailing comment may join it.
    comment_line: bool,
    /// Start of the suffix of whole-line comments. Section separators go
    /// before this suffix, since reparsing attaches it to the next header.
    whole_comments_start: Option<usize>,
    #[cfg(test)]
    path_storage: std::rc::Rc<PathStorage>,
    cancel: &'a dyn Cancel,
}

impl<'a> Writer<'a> {
    fn new(cancel: &'a dyn Cancel) -> Self {
        Self {
            out: String::new(),
            comment_line: false,
            whole_comments_start: None,
            #[cfg(test)]
            path_storage: std::rc::Rc::new(PathStorage::default()),
            cancel,
        }
    }

    fn finish(mut self) -> Result<String, String> {
        if !self.out.is_empty() {
            check_formatted_size(self.out.len().saturating_add(1))?;
            self.out.push('\n');
        }
        check_formatted_size(self.out.len())?;
        Ok(self.out)
    }

    fn document(&mut self, root: &TableNode) -> Result<(), String> {
        let split = section_split(root);
        let mut path = Vec::new();
        for entry in &root.entries[..split] {
            self.line_entry(entry, &mut path)?;
        }
        for entry in &root.entries[split..] {
            self.section(entry, &mut path)?;
        }
        self.comments(0, &root.closing)
    }

    fn section<'b>(&mut self, entry: &'b Entry, path: &mut Vec<&'b str>) -> Result<(), String> {
        check_cancel(self.cancel)?;
        path.push(&entry.key);
        match &entry.value {
            Node::Table(table) => {
                let split = section_split(table);
                let header = split > 0
                    || table.entries.is_empty()
                    || !entry.notes.is_empty()
                    || !table.opening.is_empty()
                    || !table.closing.is_empty();
                if header {
                    self.section_header(path, false, &entry.notes)?;
                }
                self.section_body(table, split, path)?;
            }
            Node::Array(array) => {
                for element in &array.elements {
                    let Node::Table(table) = &element.value else {
                        return Err("TOML array of tables holds a value".to_owned());
                    };
                    self.section_header(path, true, &element.notes)?;
                    self.section_body(table, section_split(table), path)?;
                }
            }
            Node::Scalar(_) => return Err("TOML section holds a value".to_owned()),
        }
        path.pop();
        Ok(())
    }

    /// Materializes a path only for its header, before entering child tables.
    fn section_header(&mut self, path: &[&str], array: bool, notes: &Notes) -> Result<(), String> {
        check_key_segments(path.len())?;
        let name = path.join(".");
        #[cfg(test)]
        let _name_storage = self.path_storage.hold(name.capacity());
        let header = if array {
            format!("[[{name}]]")
        } else {
            format!("[{name}]")
        };
        self.header(&header, notes)
    }

    fn header(&mut self, header: &str, notes: &Notes) -> Result<(), String> {
        if !self.out.is_empty() {
            check_formatted_size(self.out.len().saturating_add(1))?;
            match self.whole_comments_start {
                Some(0) => {}
                Some(at) => self.out.insert(at, '\n'),
                None => self.out.push('\n'),
            }
        }
        self.comments(0, &notes.before)?;
        self.line(0, header)?;
        self.trailing(0, &notes.trailing)
    }

    fn section_body<'b>(
        &mut self,
        table: &'b TableNode,
        split: usize,
        path: &mut Vec<&'b str>,
    ) -> Result<(), String> {
        self.comments(0, &table.opening)?;
        let mut keys = Vec::new();
        for entry in &table.entries[..split] {
            self.line_entry(entry, &mut keys)?;
        }
        self.comments(0, &table.closing)?;
        for entry in &table.entries[split..] {
            self.section(entry, path)?;
        }
        Ok(())
    }

    /// Writes an entry as `key = value` lines; a table becomes one line per
    /// entry under a dotted key.
    fn line_entry<'b>(&mut self, entry: &'b Entry, path: &mut Vec<&'b str>) -> Result<(), String> {
        check_cancel(self.cancel)?;
        path.push(&entry.key);
        self.comments(0, &entry.notes.before)?;
        match &entry.value {
            Node::Scalar(raw) => {
                self.value_line(path, raw)?;
            }
            Node::Table(table) if table.entries.is_empty() => {
                self.comments(0, &table.opening)?;
                self.comments(0, &table.closing)?;
                self.value_line(path, "{}")?;
            }
            Node::Table(table) => {
                self.comments(0, &table.opening)?;
                for child in &table.entries {
                    self.line_entry(child, path)?;
                }
                self.comments(0, &table.closing)?;
            }
            Node::Array(array) => {
                self.value_line(path, "")?;
                self.array(array, 0)?;
            }
        }
        self.trailing(0, &entry.notes.trailing)?;
        path.pop();
        Ok(())
    }

    /// A dotted path borrows the tree until the final line needs its spelling.
    fn value_line(&mut self, path: &[&str], raw: &str) -> Result<(), String> {
        check_key_segments(path.len())?;
        let key = path.join(".");
        #[cfg(test)]
        let _key_storage = self.path_storage.hold(key.capacity());
        self.line(0, &format!("{key} = {raw}"))
    }

    /// Writes an array from the end of the current line, one element per line.
    fn array(&mut self, array: &ArrayNode, indent: usize) -> Result<(), String> {
        if array.elements.is_empty() && array.opening.is_empty() && array.closing.is_empty() {
            return self.append("[]");
        }
        self.append("[")?;
        self.trailing(indent + 2, &array.opening)?;
        for element in &array.elements {
            check_cancel(self.cancel)?;
            self.comments(indent + 2, &element.notes.before)?;
            self.line(indent + 2, "")?;
            match &element.value {
                Node::Scalar(raw) => self.append(raw)?,
                Node::Array(inner) => self.array(inner, indent + 2)?,
                Node::Table(table) => {
                    if table.has_comments() {
                        return Err(
                            "TOML comments inside an inline table in an array are not formatted"
                                .to_owned(),
                        );
                    }
                    self.inline_table(table)?;
                }
            }
            self.append(",")?;
            self.trailing(indent + 2, &element.notes.trailing)?;
        }
        self.comments(indent + 2, &array.closing)?;
        self.line(indent, "]")
    }

    /// Writes inline tables directly through the bounded output buffer.
    fn inline_table(&mut self, table: &TableNode) -> Result<(), String> {
        if table.entries.is_empty() {
            return self.append("{}");
        }
        self.append("{ ")?;
        self.inline_entries(table, &mut Vec::new(), &mut false)?;
        self.append(" }")
    }

    fn inline_entries<'b>(
        &mut self,
        table: &'b TableNode,
        path: &mut Vec<&'b str>,
        wrote: &mut bool,
    ) -> Result<(), String> {
        for entry in &table.entries {
            check_cancel(self.cancel)?;
            path.push(&entry.key);
            match &entry.value {
                Node::Table(child) if !child.entries.is_empty() => {
                    self.inline_entries(child, path, wrote)?;
                }
                value => {
                    if *wrote {
                        self.append(", ")?;
                    }
                    self.inline_key(path)?;
                    self.inline_value(value)?;
                    *wrote = true;
                }
            }
            path.pop();
        }
        Ok(())
    }

    fn inline_key(&mut self, path: &[&str]) -> Result<(), String> {
        check_key_segments(path.len())?;
        let key = path.join(".");
        #[cfg(test)]
        let _key_storage = self.path_storage.hold(key.capacity());
        self.append(&key)?;
        self.append(" = ")
    }

    fn inline_value(&mut self, value: &Node) -> Result<(), String> {
        check_cancel(self.cancel)?;
        match value {
            Node::Scalar(raw) => self.append(raw),
            Node::Table(table) => self.inline_table(table),
            Node::Array(array) => {
                self.append("[")?;
                for (index, element) in array.elements.iter().enumerate() {
                    if index != 0 {
                        self.append(", ")?;
                    }
                    self.inline_value(&element.value)?;
                }
                self.append("]")
            }
        }
    }

    fn comments(&mut self, indent: usize, comments: &[String]) -> Result<(), String> {
        for comment in comments {
            self.whole_comment(indent, comment)?;
        }
        Ok(())
    }

    fn whole_comment(&mut self, indent: usize, comment: &str) -> Result<(), String> {
        let start = self.whole_comments_start.unwrap_or(self.out.len());
        self.line(indent, comment)?;
        self.whole_comments_start = Some(start);
        self.comment_line = true;
        Ok(())
    }

    fn trailing(&mut self, indent: usize, comments: &[String]) -> Result<(), String> {
        let Some((first, rest)) = comments.split_first() else {
            return Ok(());
        };
        if self.comment_line || self.out.is_empty() {
            self.whole_comment(indent, first)?;
        } else {
            self.append(" ")?;
            self.append(first)?;
        }
        self.comment_line = true;
        self.comments(indent, rest)
    }

    fn line(&mut self, indent: usize, text: &str) -> Result<(), String> {
        check_formatted_size(
            self.out
                .len()
                .saturating_add(usize::from(!self.out.is_empty()))
                .saturating_add(indent)
                .saturating_add(text.len()),
        )?;
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        for _ in 0..indent {
            self.out.push(' ');
        }
        self.comment_line = false;
        self.whole_comments_start = None;
        self.append(text)
    }

    fn append(&mut self, text: &str) -> Result<(), String> {
        check_formatted_size(self.out.len().saturating_add(text.len()))?;
        self.out.push_str(text);
        Ok(())
    }
}

fn check_key_segments(segments: usize) -> Result<(), String> {
    if segments > 80 {
        return Err("TOML projected key nesting exceeds the limit of 80 segments".to_owned());
    }
    Ok(())
}

/// Measures live path-string capacities without a timing or allocator hook.
#[cfg(test)]
#[derive(Default)]
struct PathStorage {
    current: std::cell::Cell<usize>,
    peak: std::cell::Cell<usize>,
}

#[cfg(test)]
impl PathStorage {
    fn hold(self: &std::rc::Rc<Self>, bytes: usize) -> PathAllocation {
        let current = self.current.get() + bytes;
        self.current.set(current);
        self.peak.set(self.peak.get().max(current));
        PathAllocation {
            storage: self.clone(),
            bytes,
        }
    }
}

#[cfg(test)]
struct PathAllocation {
    storage: std::rc::Rc<PathStorage>,
    bytes: usize,
}

#[cfg(test)]
impl Drop for PathAllocation {
    fn drop(&mut self) {
        self.storage
            .current
            .set(self.storage.current.get() - self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, scan, Writer};
    use crate::prettify::{format, StructuredFormat};
    use ca_diff::NeverCancel;
    use std::fmt::Write as _;

    fn toml(text: &str) -> Result<String, String> {
        format(text, StructuredFormat::Toml)
    }

    fn assert_round_trip(text: &str) -> String {
        let formatted = toml(text).unwrap_or_else(|error| format!("refused: {error}"));
        let original = parse(text, &NeverCancel).unwrap_or_default();
        let reparsed = parse(&formatted, &NeverCancel).unwrap_or_default();
        assert!(original.same(&reparsed), "{formatted}");
        let mut before = scan(text, &NeverCancel)
            .map(|scan| scan.comments)
            .unwrap_or_default();
        let mut after = scan(&formatted, &NeverCancel)
            .map(|scan| scan.comments)
            .unwrap_or_default();
        before.sort_unstable();
        after.sort_unstable();
        assert_eq!(before, after, "{formatted}");
        formatted
    }

    #[test]
    fn inline_tables_dotted_keys_and_standard_tables_format_alike() {
        let expected = "name = \"x\"\n\n[server]\nhost = \"h\"\nport = 80\n";
        for text in [
            "name = \"x\"\nserver = { host = \"h\", port = 80 }\n",
            "name = \"x\"\n[server]\nhost = \"h\"\nport = 80\n",
            "name = \"x\"\nserver.host = \"h\"\nserver.port = 80\n",
            "name=\"x\"\n  [ server ]\n  host=\"h\"\n\n\n  port=80",
        ] {
            assert_eq!(toml(text).as_deref(), Ok(expected), "{text:?}");
            assert_round_trip(text);
        }
    }

    #[test]
    fn a_table_before_other_keys_keeps_its_place_as_dotted_keys() {
        assert_eq!(
            toml("a = { x = 1, y = { z = 2 } }\nb = 2\n").as_deref(),
            Ok("a.x = 1\na.y.z = 2\nb = 2\n")
        );
        assert_eq!(
            toml("[t]\ninner = {}\nlast = true\n").as_deref(),
            Ok("[t]\ninner = {}\nlast = true\n")
        );
    }

    #[test]
    fn arrays_of_tables_and_arrays_of_inline_tables_format_alike() {
        let expected =
            "[[bin]]\nname = \"a\"\n\n[[bin]]\nname = \"b\"\n\n[bin.path]\nmain = \"m\"\n";
        for text in [
            "[[bin]]\nname = \"a\"\n[[bin]]\nname = \"b\"\npath = { main = \"m\" }\n",
            "bin = [{ name = \"a\" }, { name = \"b\", path.main = \"m\" }]\n",
        ] {
            assert_eq!(toml(text).as_deref(), Ok(expected), "{text:?}");
            assert_round_trip(text);
        }
        assert_eq!(
            toml("list = [{ a = 1 }, { b = [1, 2] }]\nafter = 0\n").as_deref(),
            Ok("list = [\n  { a = 1 },\n  { b = [1, 2] },\n]\nafter = 0\n")
        );
    }

    #[test]
    fn arrays_are_written_one_element_per_line() {
        let expected = "ports = [\n  80,\n  443,\n]\nnested = [\n  [\n    1,\n  ],\n  [],\n]\n";
        assert_eq!(
            toml("ports = [80,443]\nnested = [[1], []]").as_deref(),
            Ok(expected)
        );
        assert_eq!(toml(expected).as_deref(), Ok(expected));
    }

    #[test]
    fn key_order_and_value_spellings_are_kept() {
        let text = "z = 0xff\ny = 1_000.5e3\nx = 1979-05-27T07:32:00Z\nw = 'C:\\path'\nv = \"\\u00e9\"\n\"quoted key\" = true\nu = \"\"\"\n  multi\n    line\"\"\"\nt = inf\n";
        assert_eq!(toml(text).as_deref(), Ok(text));
        assert_round_trip(text);
    }

    #[test]
    fn comments_stay_with_the_item_they_precede_or_follow() {
        let text = "# head\nname = \"x\" # trail\n\n# before table\n[server] # header\n# before port\nport = 80\nopts = [\n  1, # one\n  # before two\n  2,\n  # closing\n]\n# end\n";
        let formatted = assert_round_trip(text);
        assert_eq!(
            formatted,
            "# head\nname = \"x\" # trail\n\n# before table\n[server] # header\n# before port\nport = 80\nopts = [\n  1, # one\n  # before two\n  2,\n  # closing\n]\n# end\n"
        );
        let inline = "server = { host = \"h\", # host\n  # before port\n  port = 80 } # after\n";
        assert_eq!(
            assert_round_trip(inline),
            "\n[server] # after\nhost = \"h\" # host\n# before port\nport = 80\n".trim_start()
        );
    }

    #[test]
    fn closing_inline_comments_format_idempotently() {
        for text in [
            "a = { x = 1,\n# closing\n}\nb = { y = 2 }\n",
            "a = { x = 1,\n# closing\n}\nb = { y = 2,\n# last\n}\n",
            "# leading\n[a]\nx = 1\n# between\n[b]\ny = 2\n",
        ] {
            let formatted = assert_round_trip(text);
            assert_eq!(
                toml(&formatted).as_deref(),
                Ok(formatted.as_str()),
                "{text}"
            );
        }
    }

    #[test]
    fn long_key_prefixes_are_not_retained_at_every_table_level() {
        let key = format!("{}.{}", "k".repeat(16 * 1024), vec!["k"; 79].join("."));
        for suffix in ["", "after = 0\n"] {
            let text = format!("{key} = 1\n{suffix}");
            let tree = parse(&text, &NeverCancel);
            assert!(tree.is_ok(), "{tree:?}");
            let tree = tree.unwrap_or_default();
            let mut writer = Writer::new(&NeverCancel);
            assert!(writer.document(&tree).is_ok());
            assert_eq!(writer.path_storage.current.get(), 0);
            assert!(
                writer.path_storage.peak.get() <= text.len() * 2,
                "retained {} path bytes for {} source bytes",
                writer.path_storage.peak.get(),
                text.len()
            );
            let formatted = writer.finish().unwrap_or_default();
            assert_eq!(toml(&formatted).as_deref(), Ok(formatted.as_str()));
        }
    }

    #[test]
    fn expanded_inline_table_stops_writing_at_the_output_budget() {
        let mut text = format!("list = [{{ {} = {{", "k".repeat(512 * 1024));
        for n in 0..50 {
            let _ = write!(text, "x{n} = {n}, ");
        }
        text.push_str("} }]\nafter = 0\n");
        let tree = parse(&text, &NeverCancel);
        assert!(tree.is_ok(), "{tree:?}");
        let mut writer = Writer::new(&NeverCancel);
        let result = writer.document(&tree.unwrap_or_default());
        assert!(result.is_err_and(|error| error.contains("24 MiB")));
        assert!(
            writer.out.len() <= crate::prettify::MAX_FORMATTED_BYTES,
            "retained {} output bytes after refusal",
            writer.out.len()
        );
    }

    #[test]
    fn projected_paths_report_the_key_nesting_limit() {
        let key = vec!["k"; 79].join(".");
        for text in [
            format!("{key} = {{ {key} = {{ z = 1 }} }}\n"),
            format!("{key} = {{ {key} = {{ z = 1 }} }}\nafter = 0\n"),
            format!("a = [{{ {key} = {{ {key} = {{ z = 1 }} }} }}]\nafter = 0\n"),
        ] {
            assert!(
                toml(&text).is_err_and(|error| error.contains("80 segments")),
                "{text}"
            );
        }
        let boundary = format!("{} = 1\n", vec!["k"; 80].join("."));
        assert_round_trip(&boundary);
    }

    #[test]
    fn comments_that_have_no_place_in_the_projection_are_refused() {
        let text = "list = [{ a = 1, # inside\n b = 2 }]\nafter = 0\n";
        assert!(toml(text).is_err_and(|error| error.contains("inline table")));
    }

    #[test]
    fn crlf_and_byte_order_mark_format_like_lf() {
        let lf = "a = 1 # c\n\n[t]\nb = [\n  2,\n]\n";
        let crlf = format!("\u{feff}{}", lf.replace('\n', "\r\n"));
        assert_eq!(toml(&crlf).as_deref(), Ok(lf));
    }

    #[test]
    fn empty_and_comment_only_documents_are_kept() {
        assert_eq!(toml("").as_deref(), Ok(""));
        assert_eq!(toml("\n\n").as_deref(), Ok(""));
        assert_eq!(toml("# only\n").as_deref(), Ok("# only\n"));
        assert_eq!(toml("[empty]\n").as_deref(), Ok("[empty]\n"));
    }

    #[test]
    fn malformed_toml_is_refused() {
        for text in [
            "a = ",
            "[a\nb = 1",
            "a = 1\na = 2",
            "a = { b = 1 }\n[a]\nc = 2",
            "a = \"unterminated",
            "a = [1, 2",
            "a = 1 b = 2",
            "[[a]]\n[a]",
            "a = 01",
        ] {
            assert!(
                toml(text).is_err_and(|error| error.starts_with("invalid TOML")),
                "{text:?}"
            );
        }
    }

    #[test]
    fn toml_budgets_and_nesting_limits_are_enforced() {
        let many = format!("a = [{}0]", "0,".repeat(200_000));
        assert!(toml(&many).is_err_and(|error| error.contains("temporary formatting memory limit")));
        let deep = format!("a = {}{}", "[".repeat(200), "]".repeat(200));
        assert!(toml(&deep).is_err());
        let key = vec!["k"; 200].join(".");
        assert!(toml(&format!("{key} = 1")).is_err());
    }

    /// The deepest nesting the parser builds: arrays of tables along a
    /// header path, then inline tables under keys of the most segments.
    fn deepest_document(error_at_end: bool) -> String {
        let mut text = String::new();
        let mut path = String::from("h");
        for _ in 0..79 {
            let _ = writeln!(text, "[[{path}]]");
            path.push_str(".h");
        }
        let key = vec!["k"; 79].join(".");
        for _ in 0..79 {
            let _ = write!(text, "{key} = {{ ");
        }
        text.push_str("z = 1");
        text.push_str(&" }".repeat(79));
        text.push('\n');
        if error_at_end {
            text.push_str("x =\n");
        }
        text
    }

    #[test]
    fn the_deepest_documents_are_refused_on_a_two_mebibyte_worker_stack() {
        for error_at_end in [false, true] {
            let text = deepest_document(error_at_end);
            let worker = std::thread::Builder::new()
                .stack_size(2 * 1024 * 1024)
                .spawn(move || toml(&text));
            let result = worker.map(std::thread::JoinHandle::join);
            assert!(
                matches!(&result, Ok(Ok(Err(error))) if error.contains("256 levels") || error.starts_with("invalid TOML")),
                "{result:?}"
            );
        }
    }

    #[test]
    fn dotted_key_segments_count_toward_the_storage_budget() {
        let segments = vec!["a"; 79].join(".");
        let mut dotted = String::new();
        let mut headers = String::new();
        for i in 0..600 {
            let _ = writeln!(dotted, "k{i}.{segments} = 1");
            let _ = writeln!(headers, "[k{i}.{segments}]");
        }
        assert!(
            toml(&dotted).is_err_and(|error| error.contains("temporary formatting memory limit"))
        );
        assert!(
            toml(&headers).is_err_and(|error| error.contains("temporary formatting memory limit"))
        );
        assert!(toml("a.b.c = 1\n[d.e]\nf = 1.5\n").is_ok());
    }

    #[test]
    fn mutated_inputs_are_formatted_or_refused_without_a_panic() {
        let seeds = [
            "# c\na = { b = [1, 'two', \"\"\"three\"\"\"], c.d = 1979-05-27 } # t\n[[e]]\nf = 0x1\n[g.h]\ni = [ { j = 1 } ]\n",
            "x = [ # o\n  1, # p\n]\n[t] # h\ny = inf\n",
        ];
        let replacements = [
            "", "\n", " ", "=", ".", "[", "]", "{", "}", "#", ",", "'", "\"", "[[", "]]", "\t",
        ];
        let mut state = 0x9e37_79b9_u32;
        for seed in seeds {
            for _ in 0..600 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let at = (state as usize) % (seed.len() + 1);
                let replacement = replacements[(state as usize >> 8) % replacements.len()];
                let mut text = seed.to_owned();
                if seed.is_char_boundary(at) {
                    text.insert_str(at, replacement);
                }
                if toml(&text).is_ok() {
                    assert_round_trip(&text);
                }
            }
        }
    }
}
