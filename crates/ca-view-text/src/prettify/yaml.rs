//! Block-style YAML projection for structured text comparison.
//!
//! Scalar, anchor, alias, tag, directive and comment spellings are copied from
//! the source text. Only indentation, the line breaks between nodes, and flow
//! collections change. The projection is parsed again and must produce the
//! same event stream and the same comment sequence as the source, otherwise
//! the input is refused.

use super::{check_cancel, check_storage_estimate};
use ca_diff::Cancel;
use saphyr_parser::input::SkipTabs;
use saphyr_parser::{Event, Input, Parser, ScalarStyle, Span, StrInput, Tag};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

/// Deepest collection nesting the projection writes.
const MAX_DEPTH: usize = 256;
/// Indentation step of the block projection.
const INDENT: usize = 2;
const REFUSED: &str = "formatting would change the YAML content, so the original text is kept";
/// Storage counted for each source byte: the text copies, scalar values and
/// spellings that the parser, the projection and both sides of the content
/// check hold at once.
const BYTES_PER_INPUT_BYTE: usize = 12;
/// Storage counted for each item read from the source.
const BYTES_PER_ITEM: usize = 96;
/// Storage counted for each item waiting in the queue; one gap can queue any
/// number of comments at once.
const BYTES_PER_QUEUED_ITEM: usize = 256;
/// Storage counted for each comment, which the content check keeps to the
/// end on both sides.
const BYTES_PER_COMMENT: usize = 256;
/// Storage counted for each flow indicator the scanner reads past the last
/// parser event. The scanner queues every token of a flow collection that may
/// be a mapping key until the collection closes, and flow tokens are
/// separated by these indicators. The queue grows by doubling, so the count
/// covers the moment the old and the new queue both exist.
const BYTES_PER_READ_AHEAD_MARK: usize = 2560;
/// Storage counted for each token fetch the scanner starts past the last
/// parser event. A fetch queues at most three tokens.
const BYTES_PER_READ_AHEAD_FETCH: usize = 1280;

pub(super) fn format(text: &str, cancel: &dyn Cancel) -> Result<String, String> {
    check_characters(text)?;
    let text = &*unify_line_breaks(text);
    let mut items = Items::new(text, cancel);
    let mut writer = Writer::new(cancel);
    while let Some(item) = items.next_item()? {
        writer.item(item)?;
    }
    let output = writer.finish()?;
    verify(text, &output, cancel)?;
    Ok(output)
}

/// Refuses characters outside the YAML 1.2 character set, which the parser
/// accepts: control characters, and a byte order mark after the leading one
/// the caller removes. The parser reads such a mark as scalar text, and the
/// projection can then start with it, where it is an encoding signature.
fn check_characters(text: &str) -> Result<(), String> {
    let refused = text.chars().find(|&character| {
        matches!(
            character,
            '\0'..='\u{8}'
                | '\u{b}'
                | '\u{c}'
                | '\u{e}'..='\u{1f}'
                | '\u{7f}'..='\u{84}'
                | '\u{86}'..='\u{9f}'
                | '\u{feff}'
                | '\u{fffe}'
                | '\u{ffff}'
        )
    });
    match refused {
        Some(character) => Err(format!(
            "invalid YAML: U+{:04X} is not allowed in YAML text",
            u32::from(character)
        )),
        None => Ok(()),
    }
}

/// YAML reads CR, LF and CRLF as the same line break, in scalar content too,
/// so every line break becomes LF before the source is read.
fn unify_line_breaks(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    let mut unified = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\r') {
        unified.push_str(&rest[..at]);
        unified.push('\n');
        rest = &rest[at + 1..];
        rest = rest.strip_prefix('\n').unwrap_or(rest);
    }
    unified.push_str(rest);
    Cow::Owned(unified)
}

/// Compares the event streams and comment sequences of both texts.
fn verify(original: &str, formatted: &str, cancel: &dyn Cancel) -> Result<(), String> {
    let refused = || {
        if cancel.is_cancelled() {
            "formatting cancelled".to_owned()
        } else {
            REFUSED.to_owned()
        }
    };
    let mut left = Items::new(original, cancel);
    let mut right = Items::new(formatted, cancel);
    let mut left_comments = Vec::new();
    let mut right_comments = Vec::new();
    loop {
        let expected = next_compared(&mut left, &mut left_comments)?;
        let actual = next_compared(&mut right, &mut right_comments).map_err(|error| {
            if right.read_ahead.failure.borrow().is_some() {
                error
            } else {
                refused()
            }
        })?;
        match (expected, actual) {
            (None, None) => break,
            (Some(expected), Some(actual)) if expected == actual => {}
            _ => return Err(refused()),
        }
    }
    if left_comments == right_comments {
        Ok(())
    } else {
        Err(refused())
    }
}

fn next_compared(
    items: &mut Items<'_>,
    comments: &mut Vec<String>,
) -> Result<Option<Item>, String> {
    loop {
        match items.next_item()? {
            Some(Item::Comment { text, .. }) => comments.push(text),
            Some(Item::Entry) => {}
            other => return Ok(other),
        }
    }
}

#[derive(Debug, PartialEq)]
enum Item {
    Directive(String),
    Comment {
        text: String,
        trailing: bool,
    },
    /// A block sequence entry indicator; flow sequences have none.
    Entry,
    Property(String),
    BlockHeader(String),
    DocumentStart {
        explicit: bool,
    },
    DocumentEnd {
        explicit: bool,
    },
    Collection {
        mapping: bool,
        anchor: usize,
        tag: Option<Tag>,
        empty: bool,
    },
    CollectionEnd {
        mapping: bool,
    },
    Scalar(Scalar),
    Alias(String),
}

#[derive(Debug)]
struct Scalar {
    /// Source text: the whole token for flow scalars, the content lines after
    /// the header for block scalars.
    raw: String,
    value: String,
    style: ScalarStyle,
    anchor: usize,
    tag: Option<Tag>,
    /// An omitted node, such as the value of `key:` with nothing after it.
    empty: bool,
}

impl Scalar {
    fn is_block(&self) -> bool {
        matches!(self.style, ScalarStyle::Literal | ScalarStyle::Folded)
    }
}

impl PartialEq for Scalar {
    /// Flow scalar spellings are compared without the indentation of their
    /// continuation lines, which line folding discards.
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
            && self.style == other.style
            && self.anchor == other.anchor
            && self.tag == other.tag
            && self.empty == other.empty
            && (self.is_block() || folded_lines(&self.raw).eq(folded_lines(&other.raw)))
    }
}

fn folded_lines(raw: &str) -> impl Iterator<Item = &str> {
    raw.split('\n')
        .map(|line| line.trim_end_matches('\r').trim_start_matches([' ', '\t']))
}

/// Converts the parser's character positions into byte offsets.
struct Offsets<'a> {
    text: &'a str,
    ascii: bool,
    chars: usize,
    bytes: usize,
}

impl<'a> Offsets<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            ascii: text.is_ascii(),
            chars: 0,
            bytes: 0,
        }
    }

    fn byte(&mut self, index: usize) -> usize {
        if self.ascii {
            return index.min(self.text.len());
        }
        while self.chars < index {
            let Some(character) = self.text[self.bytes..].chars().next() else {
                break;
            };
            self.bytes += character.len_utf8();
            self.chars += 1;
        }
        while self.chars > index {
            let Some(character) = self.text[..self.bytes].chars().next_back() else {
                break;
            };
            self.bytes -= character.len_utf8();
            self.chars -= 1;
        }
        self.bytes
    }
}

/// Storage estimate of one source, shared by `Items` and the parser input.
struct ReadAhead {
    text_len: usize,
    /// Items produced from the source so far.
    items: Cell<usize>,
    /// Items waiting in the queue of `Items`.
    queued: Cell<usize>,
    /// Comments produced from the source so far.
    comments: Cell<usize>,
    /// Scanner character index where the last parser event ends.
    event_end: Cell<usize>,
    /// Flow indicators read after `event_end`.
    marks: RefCell<Positions>,
    /// Token fetches started after `event_end`.
    fetches: RefCell<Positions>,
    /// The budget error, once the estimate exceeds it.
    failure: RefCell<Option<String>>,
}

impl ReadAhead {
    fn new(text_len: usize) -> Self {
        Self {
            text_len,
            items: Cell::new(0),
            queued: Cell::new(0),
            comments: Cell::new(0),
            event_end: Cell::new(0),
            marks: RefCell::new(Positions::default()),
            fetches: RefCell::new(Positions::default()),
            failure: RefCell::new(None),
        }
    }

    fn check(&self) -> Result<(), String> {
        if let Some(error) = self.failure.borrow().as_ref() {
            return Err(error.clone());
        }
        let parsed = self.event_end.get();
        // Both counts bound the queued tokens: indicators overcount scalar
        // content that holds punctuation, fetches overcount white space in
        // quoted scalars. The smaller bound applies.
        let marks = self.marks.borrow_mut().unparsed(parsed);
        let fetches = self.fetches.borrow_mut().unparsed(parsed);
        let read_ahead = marks.saturating_mul(BYTES_PER_READ_AHEAD_MARK).min(
            fetches
                .saturating_add(1)
                .saturating_mul(BYTES_PER_READ_AHEAD_FETCH),
        );
        let result = check_storage_estimate(
            self.text_len
                .saturating_mul(BYTES_PER_INPUT_BYTE)
                .saturating_add(self.items.get().saturating_mul(BYTES_PER_ITEM))
                .saturating_add(self.queued.get().saturating_mul(BYTES_PER_QUEUED_ITEM))
                .saturating_add(self.comments.get().saturating_mul(BYTES_PER_COMMENT))
                .saturating_add(read_ahead),
        );
        if let Err(error) = &result {
            *self.failure.borrow_mut() = Some(error.clone());
        }
        result
    }

    fn queue_item(&self, comment: bool) -> Result<(), String> {
        self.items.set(self.items.get().saturating_add(1));
        self.queued.set(self.queued.get().saturating_add(1));
        if comment {
            self.comments.set(self.comments.get().saturating_add(1));
        }
        self.check()
    }

    fn dequeue_item(&self) {
        self.queued.set(self.queued.get().saturating_sub(1));
    }

    fn event_ended(&self, index: usize) {
        self.event_end.set(self.event_end.get().max(index));
    }

    /// Records a flow indicator at scanner index `index`; returns false once
    /// the estimate exceeds the budget.
    fn mark(&self, index: usize) -> bool {
        self.marks.borrow_mut().push(index);
        self.check().is_ok()
    }

    /// Records a token fetch that starts at scanner index `index`; returns
    /// false once the estimate exceeds the budget.
    fn fetch(&self, index: usize) -> bool {
        self.fetches.borrow_mut().push(index);
        self.check().is_ok()
    }
}

/// Increasing scanner positions, kept as chunks so the record stays small
/// next to the tokens it counts.
#[derive(Default)]
struct Positions {
    /// Last position and number of positions of each chunk.
    chunks: VecDeque<(usize, usize)>,
    count: usize,
}

impl Positions {
    const CHUNK: usize = 64;

    fn push(&mut self, position: usize) {
        match self.chunks.back_mut() {
            Some((last, count)) if *count < Self::CHUNK => {
                *last = position;
                *count += 1;
            }
            _ => self.chunks.push_back((position, 1)),
        }
        self.count += 1;
    }

    /// Positions not wholly before `parsed`; a chunk that straddles it still
    /// counts in full.
    fn unparsed(&mut self, parsed: usize) -> usize {
        while let Some(&(last, count)) = self.chunks.front() {
            if last >= parsed {
                break;
            }
            self.count -= count;
            self.chunks.pop_front();
        }
        self.count
    }
}

/// Parser input that reports what the scanner reads to `ReadAhead`. The
/// scanner queues tokens without a bound while a flow collection may still be
/// a mapping key, so the input ends early once the queued tokens can exceed
/// the storage budget, and `Items` then refuses the source.
///
/// The fetch count relies on the scanner of `saphyr-parser` 0.1.0 requesting
/// a look-ahead of one character at the start of every token fetch.
struct ReadAheadInput<'a> {
    inner: StrInput<'a>,
    text: &'a str,
    /// Byte offset of the next unread character.
    byte: usize,
    /// Scanner character index of the next unread character; it advances by
    /// the counts the scanner adds to its own index.
    index: usize,
    read_ahead: Rc<ReadAhead>,
}

impl<'a> ReadAheadInput<'a> {
    fn new(text: &'a str, read_ahead: Rc<ReadAhead>) -> Self {
        Self {
            inner: StrInput::new(text),
            text,
            byte: 0,
            index: 0,
            read_ahead,
        }
    }

    /// Records `chars` consumed characters. Comments and white space cannot
    /// hold tokens, so they pass `structural` false.
    fn advance(&mut self, chars: usize, structural: bool) {
        let mut stop = false;
        for _ in 0..chars {
            let Some(character) = self
                .text
                .get(self.byte..)
                .and_then(|rest| rest.chars().next())
            else {
                break;
            };
            if structural
                && matches!(
                    character,
                    ',' | '[' | ']' | '{' | '}' | ':' | '?' | '&' | '!' | '*' | '\'' | '"'
                )
                && !self.read_ahead.mark(self.index)
            {
                stop = true;
            }
            self.byte += character.len_utf8();
            self.index += 1;
        }
        if stop {
            self.stop();
        }
    }

    /// Tracks the consumed bytes separately from parser character positions.
    fn advance_bytes(&mut self, bytes: usize, characters: usize) {
        self.byte = self.byte.saturating_add(bytes).min(self.text.len());
        self.index = self.index.saturating_add(characters);
    }

    /// Ends the input at the current position, as if the text ended there.
    fn stop(&mut self) {
        let buffered = self.inner.buflen();
        self.inner = StrInput::new("");
        self.inner.lookahead(buffered);
        self.byte = self.text.len();
    }
}

impl Input for ReadAheadInput<'_> {
    fn lookahead(&mut self, count: usize) {
        if count == 1 && self.byte < self.text.len() && !self.read_ahead.fetch(self.index) {
            self.stop();
        }
        self.inner.lookahead(count);
    }

    fn buflen(&self) -> usize {
        self.inner.buflen()
    }

    fn bufmaxlen(&self) -> usize {
        self.inner.bufmaxlen()
    }

    fn buf_is_empty(&self) -> bool {
        self.inner.buf_is_empty()
    }

    fn raw_read_ch(&mut self) -> char {
        let character = self.inner.raw_read_ch();
        self.advance(1, true);
        character
    }

    fn raw_read_non_breakz_ch(&mut self) -> Option<char> {
        let character = self.inner.raw_read_non_breakz_ch();
        if character.is_some() {
            self.advance(1, true);
        }
        character
    }

    fn skip(&mut self) {
        self.inner.skip();
        self.advance(1, true);
    }

    fn skip_n(&mut self, count: usize) {
        self.inner.skip_n(count);
        self.advance(count, true);
    }

    fn peek(&self) -> char {
        self.inner.peek()
    }

    fn peek_nth(&self, n: usize) -> char {
        self.inner.peek_nth(n)
    }

    fn look_ch(&mut self) -> char {
        self.inner.look_ch()
    }

    fn next_char_is(&self, c: char) -> bool {
        self.inner.next_char_is(c)
    }

    fn nth_char_is(&self, n: usize, c: char) -> bool {
        self.inner.nth_char_is(n, c)
    }

    fn next_2_are(&self, c1: char, c2: char) -> bool {
        self.inner.next_2_are(c1, c2)
    }

    fn next_3_are(&self, c1: char, c2: char, c3: char) -> bool {
        self.inner.next_3_are(c1, c2, c3)
    }

    fn next_is_document_indicator(&self) -> bool {
        self.inner.next_is_document_indicator()
    }

    fn next_is_document_start(&self) -> bool {
        self.inner.next_is_document_start()
    }

    fn next_is_document_end(&self) -> bool {
        self.inner.next_is_document_end()
    }

    fn skip_ws_to_eol(&mut self, skip_tabs: SkipTabs) -> (usize, Result<SkipTabs, &'static str>) {
        let (count, result) = self.inner.skip_ws_to_eol(skip_tabs);
        self.advance(count, false);
        (count, result)
    }

    fn next_can_be_plain_scalar(&self, in_flow: bool) -> bool {
        self.inner.next_can_be_plain_scalar(in_flow)
    }

    fn next_is_blank_or_break(&self) -> bool {
        self.inner.next_is_blank_or_break()
    }

    fn next_is_blank_or_breakz(&self) -> bool {
        self.inner.next_is_blank_or_breakz()
    }

    fn next_is_blank(&self) -> bool {
        self.inner.next_is_blank()
    }

    fn next_is_break(&self) -> bool {
        self.inner.next_is_break()
    }

    fn next_is_breakz(&self) -> bool {
        self.inner.next_is_breakz()
    }

    fn next_is_z(&self) -> bool {
        self.inner.next_is_z()
    }

    fn next_is_flow(&self) -> bool {
        self.inner.next_is_flow()
    }

    fn next_is_digit(&self) -> bool {
        self.inner.next_is_digit()
    }

    fn next_is_alpha(&self) -> bool {
        self.inner.next_is_alpha()
    }

    fn skip_while_non_breakz(&mut self) -> usize {
        let count = self.inner.skip_while_non_breakz();
        self.advance(count, false);
        count
    }

    fn skip_while_blank(&mut self) -> usize {
        let count = self.inner.skip_while_blank();
        self.advance(count, false);
        count
    }

    fn fetch_while_is_alpha(&mut self, out: &mut String) -> usize {
        let before = out.len();
        self.inner.fetch_while_is_alpha(out);
        let count = out[before..].chars().count();
        self.advance_bytes(out.len().saturating_sub(before), count);
        count
    }

    fn fetch_while_is_yaml_non_space(&mut self, out: &mut String) -> usize {
        let before = out.len();
        self.inner.fetch_while_is_yaml_non_space(out);
        let count = out[before..].chars().count();
        self.advance_bytes(out.len().saturating_sub(before), count);
        count
    }
}

/// Parser events merged with the source text between them: comments,
/// directives, node properties and block scalar headers, in source order.
struct Items<'a> {
    text: &'a str,
    parser: Parser<'a, ReadAheadInput<'a>>,
    read_ahead: Rc<ReadAhead>,
    cancel: &'a dyn Cancel,
    offsets: Offsets<'a>,
    /// End of the source text already turned into items.
    cursor: usize,
    /// Whether the source line at `cursor` has text before it.
    line_content: bool,
    header_end: Option<usize>,
    pending: VecDeque<Item>,
    finished: bool,
}

impl<'a> Items<'a> {
    fn new(text: &'a str, cancel: &'a dyn Cancel) -> Self {
        let read_ahead = Rc::new(ReadAhead::new(text.len()));
        Self {
            text,
            parser: Parser::new(ReadAheadInput::new(text, Rc::clone(&read_ahead))),
            read_ahead,
            cancel,
            offsets: Offsets::new(text),
            cursor: 0,
            line_content: false,
            header_end: None,
            pending: VecDeque::new(),
            finished: false,
        }
    }

    fn next_item(&mut self) -> Result<Option<Item>, String> {
        loop {
            // An exceeded budget ends the parser input early, so no event
            // after that point describes the source.
            self.read_ahead.check()?;
            if let Some(item) = self.pending.pop_front() {
                self.read_ahead.dequeue_item();
                return Ok(Some(item));
            }
            if self.finished {
                return Ok(None);
            }
            check_cancel(self.cancel)?;
            let event = self.parser.next_event();
            self.read_ahead.check()?;
            match event {
                None => self.finished = true,
                Some(Err(error)) => return Err(format!("invalid YAML: {error}")),
                Some(Ok((event, span))) => {
                    self.read_ahead.event_ended(span.end.index());
                    self.read(event, span)?;
                }
            }
        }
    }

    fn push(&mut self, item: Item) -> Result<(), String> {
        self.read_ahead
            .queue_item(matches!(item, Item::Comment { .. }))?;
        self.pending.push_back(item);
        Ok(())
    }

    fn read(&mut self, event: Event<'a>, span: Span) -> Result<(), String> {
        let start = self.offsets.byte(span.start.index());
        let end = self.offsets.byte(span.end.index()).max(start);
        match event {
            Event::Nothing | Event::StreamStart => {}
            Event::StreamEnd => self.gap(self.text.len(), false)?,
            Event::DocumentStart(explicit) => {
                self.gap(start, false)?;
                if explicit {
                    self.consume(end);
                }
                self.push(Item::DocumentStart { explicit })?;
            }
            Event::DocumentEnd => {
                let explicit = self.text.get(start..end) == Some("...");
                self.gap(start, false)?;
                if explicit {
                    self.consume(end);
                }
                self.push(Item::DocumentEnd { explicit })?;
            }
            Event::Alias(_) => {
                self.gap(start, false)?;
                let raw = self.slice(start, end)?;
                self.consume(end);
                self.push(Item::Alias(raw))?;
            }
            Event::Scalar(value, style, anchor, tag) => {
                self.scalar(
                    value.into_owned(),
                    style,
                    anchor,
                    tag.map(std::borrow::Cow::into_owned),
                    start,
                    end,
                )?;
            }
            Event::SequenceStart(anchor, tag) => {
                self.collection(
                    false,
                    anchor,
                    tag.map(std::borrow::Cow::into_owned),
                    start,
                    end,
                )?;
            }
            Event::MappingStart(anchor, tag) => {
                self.collection(
                    true,
                    anchor,
                    tag.map(std::borrow::Cow::into_owned),
                    start,
                    end,
                )?;
            }
            Event::SequenceEnd => self.collection_end(false, start, end)?,
            Event::MappingEnd => self.collection_end(true, start, end)?,
        }
        Ok(())
    }

    fn scalar(
        &mut self,
        value: String,
        style: ScalarStyle,
        anchor: usize,
        tag: Option<Tag>,
        start: usize,
        end: usize,
    ) -> Result<(), String> {
        let block = matches!(style, ScalarStyle::Literal | ScalarStyle::Folded);
        // A plain scalar cannot be empty in the source, so an empty plain
        // value is an omitted node whose span belongs to the next token.
        let empty = style == ScalarStyle::Plain && value.is_empty();
        self.gap(start, block)?;
        let raw = if empty {
            String::new()
        } else if block {
            let header_end = self
                .header_end
                .take()
                .ok_or_else(|| "invalid YAML: block scalar without a header".to_owned())?;
            let content = self
                .text
                .get(header_end..end)
                .and_then(|text| text.find('\n'))
                .map_or(end, |offset| header_end + offset + 1);
            // The span can end after the indentation of the next line, which
            // is not content.
            let mut raw = self.slice(content, end)?;
            if let Some(line_break) = raw.rfind('\n') {
                if raw[line_break + 1..]
                    .trim_matches([' ', '\t', '\r'])
                    .is_empty()
                {
                    raw.truncate(line_break + 1);
                }
            }
            self.consume(end);
            raw
        } else {
            let end = match style {
                ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted => {
                    quoted_end(self.text.as_bytes(), start, end)?
                }
                _ => end,
            };
            let raw = self.slice(start, end)?;
            self.consume(end);
            raw
        };
        self.push(Item::Scalar(Scalar {
            raw,
            value,
            style,
            anchor,
            tag,
            empty,
        }))?;
        Ok(())
    }

    fn collection(
        &mut self,
        mapping: bool,
        anchor: usize,
        tag: Option<Tag>,
        start: usize,
        end: usize,
    ) -> Result<(), String> {
        self.gap(start, false)?;
        if self.text.get(start..end) == Some(if mapping { "{" } else { "[" }) {
            self.consume(end);
        }
        let empty = match self.parser.peek() {
            Some(Ok((Event::MappingEnd, _))) => mapping,
            Some(Ok((Event::SequenceEnd, _))) => !mapping,
            _ => false,
        };
        self.push(Item::Collection {
            mapping,
            anchor,
            tag,
            empty,
        })?;
        Ok(())
    }

    fn collection_end(&mut self, mapping: bool, start: usize, end: usize) -> Result<(), String> {
        self.gap(start, false)?;
        if self.text.get(start..end) == Some(if mapping { "}" } else { "]" }) {
            self.consume(end);
        }
        self.push(Item::CollectionEnd { mapping })?;
        Ok(())
    }

    fn slice(&self, start: usize, end: usize) -> Result<String, String> {
        self.text
            .get(start..end)
            .map(str::to_owned)
            .ok_or_else(|| "invalid YAML source position".to_owned())
    }

    fn consume(&mut self, end: usize) {
        if end <= self.cursor {
            return;
        }
        let consumed = &self.text[self.cursor..end];
        self.line_content = match consumed.rfind('\n') {
            Some(line_break) => !consumed[line_break + 1..].trim().is_empty(),
            None => self.line_content || !consumed.trim().is_empty(),
        };
        self.cursor = end;
    }

    /// Reads the source text between the previous token and `until`, which
    /// holds no scalar content: only separators, indicators, comments,
    /// directives, node properties and block scalar headers.
    fn gap(&mut self, until: usize, block_scalar_next: bool) -> Result<(), String> {
        let bytes = self.text.as_bytes();
        let mut index = self.cursor;
        let mut scanned = 0usize;
        while index < until {
            scanned += 1;
            if scanned.is_multiple_of(4096) {
                check_cancel(self.cancel)?;
            }
            match bytes[index] {
                b' ' | b'\t' | b'\r' => index += 1,
                b'\n' => {
                    self.line_content = false;
                    index += 1;
                }
                b'#' => {
                    let stop = line_end(bytes, index, until);
                    self.push(Item::Comment {
                        text: self.text[index..stop].trim_end_matches('\r').to_owned(),
                        trailing: self.line_content,
                    })?;
                    index = stop;
                }
                b'%' if index == 0 || bytes[index - 1] == b'\n' => {
                    let stop = line_end(bytes, index, until);
                    self.push(Item::Directive(
                        self.text[index..stop].trim_end_matches('\r').to_owned(),
                    ))?;
                    self.line_content = true;
                    index = stop;
                }
                b'&' | b'!' => {
                    let stop = property_end(bytes, index, until);
                    self.push(Item::Property(self.text[index..stop].to_owned()))?;
                    self.line_content = true;
                    index = stop;
                }
                b'-' if bytes
                    .get(index + 1)
                    .is_none_or(|next| matches!(next, b' ' | b'\t' | b'\r' | b'\n')) =>
                {
                    self.push(Item::Entry)?;
                    self.line_content = true;
                    index += 1;
                }
                b'?' | b':' | b',' | b'[' | b']' | b'{' | b'}' => {
                    self.line_content = true;
                    index += 1;
                }
                b'|' | b'>' if block_scalar_next && self.header_end.is_none() => {
                    let mut stop = index + 1;
                    while stop < until && matches!(bytes[stop], b'+' | b'-' | b'0'..=b'9') {
                        stop += 1;
                    }
                    if bytes[index + 1..stop].iter().any(u8::is_ascii_digit) {
                        return Err(
                            "YAML block scalars with an explicit indentation indicator are not formatted"
                                .to_owned(),
                        );
                    }
                    self.push(Item::BlockHeader(self.text[index..stop].to_owned()))?;
                    self.header_end = Some(stop);
                    self.line_content = true;
                    index = stop;
                }
                _ => return Err(format!("unexpected YAML text at byte {index}")),
            }
        }
        self.cursor = self.cursor.max(until);
        Ok(())
    }
}

/// End of a quoted scalar token. The parser's span of a quoted scalar in
/// block context can also cover the white space and comment after it.
fn quoted_end(bytes: &[u8], start: usize, span_end: usize) -> Result<usize, String> {
    let quote = bytes.get(start).copied();
    let mut index = start + 1;
    while index < span_end {
        match (quote, bytes[index]) {
            (Some(b'"'), b'\\') => index += 2,
            (Some(b'\''), b'\'') if bytes.get(index + 1) == Some(&b'\'') => index += 2,
            (Some(opening), byte) if byte == opening => return Ok(index + 1),
            _ => index += 1,
        }
    }
    Err("invalid YAML: unterminated quoted scalar".to_owned())
}

fn line_end(bytes: &[u8], from: usize, until: usize) -> usize {
    bytes[from..until]
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(until, |offset| from + offset)
}

/// End of an anchor or tag token. Verbatim tags run to their closing `>`;
/// other properties stop at white space or a flow indicator.
fn property_end(bytes: &[u8], from: usize, until: usize) -> usize {
    if bytes.get(from..from + 2) == Some(b"!<") {
        return bytes[from..until]
            .iter()
            .position(|&byte| byte == b'>')
            .map_or(until, |offset| from + offset + 1);
    }
    let mut stop = from + 1;
    while stop < until
        && !matches!(
            bytes[stop],
            b' ' | b'\t' | b'\r' | b'\n' | b',' | b'[' | b']' | b'{' | b'}'
        )
    {
        stop += 1;
    }
    stop
}

#[derive(Debug, Clone, Copy)]
enum Frame {
    Document,
    Mapping {
        indent: usize,
        /// The next node is a key.
        key: bool,
        /// The first key may continue the current `- ` line.
        inline_first: bool,
    },
    Sequence {
        indent: usize,
        /// The first entry may continue the current `- ` line.
        inline_first: bool,
    },
    /// A flow `{}` or `[]` that is already written.
    Empty {
        mapping: bool,
    },
}

#[allow(clippy::struct_excessive_bools)]
struct Writer<'a> {
    out: String,
    /// Inline content may follow on the current line.
    open: bool,
    /// The current line holds only indentation.
    fresh: bool,
    /// The current line holds node text a trailing comment may follow.
    content: bool,
    /// The output ends inside block scalar content without a final line
    /// break, so no line break may follow it.
    unterminated: bool,
    /// Whole-line comments that take the indentation of the next line, so
    /// the projection of a projection places them in the same columns.
    comments: Vec<String>,
    stack: Vec<Frame>,
    /// The current node has written its indicator or first token.
    begun: bool,
    properties: bool,
    cancel: &'a dyn Cancel,
}

impl<'a> Writer<'a> {
    fn new(cancel: &'a dyn Cancel) -> Self {
        Self {
            out: String::new(),
            open: false,
            fresh: true,
            content: false,
            unterminated: false,
            comments: Vec::new(),
            stack: Vec::new(),
            begun: false,
            properties: false,
            cancel,
        }
    }

    fn finish(mut self) -> Result<String, String> {
        if !self.stack.is_empty() {
            return Err("invalid YAML: unterminated document".to_owned());
        }
        self.flush_comments(0)?;
        if !self.out.is_empty() && !self.unterminated {
            self.out.push('\n');
        }
        super::check_formatted_size(self.out.len())?;
        Ok(self.out)
    }

    fn item(&mut self, item: Item) -> Result<(), String> {
        check_cancel(self.cancel)?;
        match item {
            Item::Directive(text) => {
                self.new_line(0)?;
                self.push(&text)?;
                self.open = false;
            }
            Item::Comment { text, trailing } => self.comment(&text, trailing)?,
            Item::Entry => {
                if matches!(self.stack.last(), Some(Frame::Sequence { .. })) {
                    self.begin_node()?;
                }
            }
            Item::Property(text) => {
                self.begin_node()?;
                self.inline(&text)?;
                self.properties = true;
            }
            Item::BlockHeader(text) => {
                if self.expecting_key() {
                    return Err(key_refusal());
                }
                self.begin_node()?;
                self.inline(&text)?;
            }
            Item::DocumentStart { explicit } => {
                if explicit {
                    self.new_line(0)?;
                    self.push("---")?;
                } else {
                    self.open = false;
                }
                self.push_frame(Frame::Document)?;
            }
            Item::DocumentEnd { explicit } => {
                if !matches!(self.stack.pop(), Some(Frame::Document)) {
                    return Err("invalid YAML: unbalanced document".to_owned());
                }
                if explicit {
                    self.new_line(0)?;
                    self.push("...")?;
                    self.open = false;
                }
            }
            Item::Collection { mapping, empty, .. } => self.collection(mapping, empty)?,
            Item::CollectionEnd { mapping } => {
                match self.stack.pop() {
                    Some(Frame::Mapping { key: true, .. }) if mapping => {}
                    Some(Frame::Sequence { .. }) if !mapping => {}
                    Some(Frame::Empty { mapping: empty }) if empty == mapping => {}
                    _ => return Err("invalid YAML: unbalanced collection".to_owned()),
                }
                self.complete_node(false);
            }
            Item::Alias(raw) => {
                let key = self.expecting_key();
                self.begin_node()?;
                self.inline(&raw)?;
                self.complete_node(key);
            }
            Item::Scalar(scalar) => self.scalar(&scalar)?,
        }
        Ok(())
    }

    fn collection(&mut self, mapping: bool, empty: bool) -> Result<(), String> {
        if self.expecting_key() {
            return Err(key_refusal());
        }
        self.begin_node()?;
        if empty {
            self.inline(if mapping { "{}" } else { "[]" })?;
            return self.push_frame(Frame::Empty { mapping });
        }
        let (indent, inline_first) = match self.stack.last() {
            Some(Frame::Mapping { indent, .. }) => (indent + INDENT, false),
            Some(Frame::Sequence { indent, .. }) => {
                (indent + INDENT, !self.properties && self.open)
            }
            _ => (0, false),
        };
        if !inline_first {
            self.open = false;
        }
        self.push_frame(if mapping {
            Frame::Mapping {
                indent,
                key: true,
                inline_first,
            }
        } else {
            Frame::Sequence {
                indent,
                inline_first,
            }
        })
    }

    fn scalar(&mut self, scalar: &Scalar) -> Result<(), String> {
        if self.expecting_key() && (scalar.empty || scalar.is_block() || scalar.raw.contains('\n'))
        {
            return Err(key_refusal());
        }
        if scalar.empty {
            if matches!(self.stack.last(), Some(Frame::Sequence { .. })) {
                self.begin_node()?;
            }
            self.complete_node(false);
            return Ok(());
        }
        self.begin_node()?;
        if scalar.is_block() {
            self.block_lines(&scalar.raw)?;
        } else {
            self.flow_lines(&scalar.raw)?;
        }
        self.complete_node(false);
        Ok(())
    }

    /// Writes a flow scalar; continuation lines are re-indented, since line
    /// folding discards their leading white space.
    fn flow_lines(&mut self, raw: &str) -> Result<(), String> {
        let indent = self.block_indent();
        let mut lines = raw.split('\n');
        if let Some(first) = lines.next() {
            self.inline(first.trim_end_matches('\r'))?;
        }
        for line in lines {
            let line = line.trim_end_matches('\r').trim_start_matches([' ', '\t']);
            if line.is_empty() {
                self.blank_line()?;
            } else {
                self.new_line(indent)?;
                self.push(line)?;
            }
        }
        Ok(())
    }

    /// Writes block scalar content at the projection's indentation. Every
    /// character after the source content indentation is kept, including
    /// white space that is content in a literal scalar.
    fn block_lines(&mut self, raw: &str) -> Result<(), String> {
        let indent = self.block_indent();
        if !raw.is_empty() {
            let content = raw.strip_suffix('\n').unwrap_or(raw);
            let lines = || content.split('\n').map(|line| line.trim_end_matches('\r'));
            // Without a line of text, the longest line sets the content
            // indentation, so every line is empty.
            let width = lines()
                .find(|line| !line.trim_start_matches(' ').is_empty())
                .map_or_else(
                    || lines().map(str::len).max().unwrap_or(0),
                    |line| line.len() - line.trim_start_matches(' ').len(),
                );
            for line in content.split('\n') {
                let line = line.trim_end_matches('\r');
                if line.len() <= width && line.bytes().all(|byte| byte == b' ') {
                    self.blank_line()?;
                    continue;
                }
                match (line.get(..width), line.get(width..)) {
                    (Some(prefix), Some(rest)) if prefix.bytes().all(|byte| byte == b' ') => {
                        self.new_line(indent)?;
                        self.push(rest)?;
                    }
                    _ => {
                        return Err("YAML block scalar indentation is not uniform".to_owned());
                    }
                }
            }
        }
        self.open = false;
        self.content = false;
        self.unterminated = !raw.is_empty() && !raw.ends_with('\n');
        Ok(())
    }

    fn comment(&mut self, text: &str, trailing: bool) -> Result<(), String> {
        if trailing && self.content {
            self.push(" ")?;
            self.push(text)?;
        } else {
            self.comments.push(text.to_owned());
            self.content = false;
        }
        self.open = false;
        Ok(())
    }

    /// Writes the indicator or line break that starts the current node.
    fn begin_node(&mut self) -> Result<(), String> {
        if self.begun {
            return Ok(());
        }
        self.begun = true;
        match self.stack.last_mut() {
            Some(Frame::Mapping {
                indent,
                key: true,
                inline_first,
            }) => {
                let indent = *indent;
                let inline = std::mem::take(inline_first) && self.open;
                if !inline {
                    self.new_line(indent)?;
                }
            }
            Some(Frame::Sequence {
                indent,
                inline_first,
            }) => {
                let indent = *indent;
                let inline = std::mem::take(inline_first) && self.open;
                if !inline {
                    self.new_line(indent)?;
                }
                self.inline("-")?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Ends the current node. A finished key gets its `:` indicator; an alias
    /// key needs a space first, since `:` may be part of an alias name.
    fn complete_node(&mut self, alias_key: bool) {
        self.begun = false;
        self.properties = false;
        if let Some(Frame::Mapping { key, .. }) = self.stack.last_mut() {
            if *key {
                *key = false;
                self.out.push_str(if alias_key { " :" } else { ":" });
                self.fresh = false;
                self.content = true;
            } else {
                *key = true;
            }
        }
    }

    fn push_frame(&mut self, frame: Frame) -> Result<(), String> {
        if self.stack.len() > MAX_DEPTH {
            return Err(format!(
                "YAML nesting exceeds the formatter limit of {MAX_DEPTH} levels"
            ));
        }
        self.stack.push(frame);
        self.begun = false;
        self.properties = false;
        Ok(())
    }

    fn expecting_key(&self) -> bool {
        matches!(self.stack.last(), Some(Frame::Mapping { key: true, .. }))
    }

    /// Indentation of a node's text that cannot continue the current line.
    fn continuation_indent(&self) -> usize {
        match self.stack.last() {
            Some(Frame::Mapping {
                indent, key: true, ..
            }) => *indent,
            Some(Frame::Mapping { indent, .. } | Frame::Sequence { indent, .. }) => indent + INDENT,
            _ => 0,
        }
    }

    /// Indentation of block scalar content and flow scalar continuation lines.
    fn block_indent(&self) -> usize {
        self.continuation_indent().max(INDENT)
    }

    fn flush_comments(&mut self, indent: usize) -> Result<(), String> {
        for comment in std::mem::take(&mut self.comments) {
            if !self.out.is_empty() {
                self.out.push('\n');
            }
            for _ in 0..indent {
                self.out.push(' ');
            }
            self.out.push_str(&comment);
            self.unterminated = false;
            super::check_formatted_size(self.out.len())?;
        }
        Ok(())
    }

    fn new_line(&mut self, indent: usize) -> Result<(), String> {
        self.flush_comments(indent)?;
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        for _ in 0..indent {
            self.out.push(' ');
        }
        self.open = true;
        self.fresh = true;
        self.content = false;
        self.unterminated = false;
        super::check_formatted_size(self.out.len())
    }

    fn blank_line(&mut self) -> Result<(), String> {
        self.out.push('\n');
        self.open = false;
        self.fresh = true;
        self.content = false;
        super::check_formatted_size(self.out.len())
    }

    fn inline(&mut self, text: &str) -> Result<(), String> {
        if !self.open {
            let indent = self.continuation_indent();
            self.new_line(indent)?;
        }
        if !self.fresh {
            self.out.push(' ');
        }
        self.push(text)
    }

    fn push(&mut self, text: &str) -> Result<(), String> {
        self.out.push_str(text);
        self.fresh = false;
        self.content = true;
        self.unterminated = false;
        super::check_formatted_size(self.out.len())
    }
}

fn key_refusal() -> String {
    "YAML mapping keys that are collections, block scalars, empty, or span lines are not formatted"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use crate::prettify::{format, StructuredFormat};
    use saphyr_parser::{Event, Parser};
    use std::fmt::Write as _;

    fn yaml(text: &str) -> Result<String, String> {
        format(text, StructuredFormat::Yaml)
    }

    /// Events of the whole stream without positions.
    fn events(text: &str) -> Vec<String> {
        Parser::new_from_str(text)
            .map(|event| match event {
                Ok((Event::Scalar(value, style, anchor, tag), _)) => {
                    format!("scalar {value:?} {style:?} {anchor} {tag:?}")
                }
                Ok((event, _)) => format!("{event:?}"),
                Err(error) => format!("error {error}"),
            })
            .collect()
    }

    fn comments(text: &str) -> Vec<&str> {
        text.lines()
            .filter_map(|line| {
                line.find(" #").map(|at| &line[at + 1..]).or_else(|| {
                    line.trim_start()
                        .starts_with('#')
                        .then(|| line.trim_start())
                })
            })
            .collect()
    }

    fn assert_round_trip(text: &str) -> String {
        let formatted = yaml(text).unwrap_or_else(|error| format!("refused: {error}"));
        assert_eq!(events(text), events(&formatted), "{formatted}");
        formatted
    }

    #[test]
    fn flow_and_minified_yaml_format_like_block_yaml() {
        let expected = "a: 1\nb:\n  - x\n  - 'y'\nc:\n  d: \"q\"\n  e: []\n  f: {}\n";
        assert_eq!(
            yaml("{a: 1, b: [x, 'y'], c: {d: \"q\", e: [], f: {}}}").as_deref(),
            Ok(expected)
        );
        assert_eq!(
            yaml("a:   1\nb:\n- x\n- 'y'\nc:\n    d: \"q\"\n    e: []\n    f: {}\n").as_deref(),
            Ok(expected)
        );
        assert_eq!(yaml(expected).as_deref(), Ok(expected));
        assert_round_trip("{a: 1, b: [x, 'y'], c: {d: \"q\", e: [], f: {}}}");
    }

    #[test]
    fn sequences_of_mappings_and_nested_sequences_use_compact_entries() {
        assert_eq!(
            yaml("[{name: a, ports: [80, 443]}, [1, [2]], {}]").as_deref(),
            Ok("- name: a\n  ports:\n    - 80\n    - 443\n- - 1\n  - - 2\n- {}\n")
        );
    }

    #[test]
    fn key_order_and_scalar_spellings_are_kept() {
        let text = "z: 0x1F\ny: yes\nx: ~\nw: 1.50\nv: 'it''s'\nu: \"tab\\t\"\nt: 2001-12-14\n";
        assert_eq!(yaml(text).as_deref(), Ok(text));
        assert_eq!(
            yaml("{z: 0x1F, y: yes, x: ~, w: 1.50, v: 'it''s', u: \"tab\\t\", t: 2001-12-14}")
                .as_deref(),
            Ok(text)
        );
    }

    #[test]
    fn anchors_aliases_and_tags_are_kept() {
        let formatted = assert_round_trip(
            "base: &b {x: 1}\nuse: *b\nlist: !custom [1, !!str 2]\n&k key: v\n*b : aliased key\n",
        );
        assert_eq!(
            formatted,
            "base: &b\n  x: 1\nuse: *b\nlist: !custom\n  - 1\n  - !!str 2\n&k key: v\n*b : aliased key\n"
        );
    }

    #[test]
    fn multi_document_streams_keep_every_document_and_marker() {
        let text = "%YAML 1.2\n--- {a: 1}\n--- [x]\n...\n---\nplain\n";
        let formatted = assert_round_trip(text);
        assert_eq!(
            formatted,
            "%YAML 1.2\n---\na: 1\n---\n- x\n...\n--- plain\n"
        );
    }

    #[test]
    fn non_ascii_directives_preserve_positions_and_spelling() {
        for directive in ["%é value", "%名前 参数", "%reserved 参数", "%😀"] {
            let text = format!("{directive}\n---\nclé: [été, 東京]\n");
            assert!(Parser::new_from_str(&text).all(|event| event.is_ok()));
            let expected = format!("{directive}\n---\nclé:\n  - été\n  - 東京\n");
            assert_eq!(yaml(&text).as_deref(), Ok(expected.as_str()));
            assert_eq!(yaml(&expected).as_deref(), Ok(expected.as_str()));
            assert_round_trip(&text);
        }
    }

    #[test]
    fn comments_are_kept_next_to_the_node_they_follow_or_precede() {
        let text = "# head\n{a: 1, # one\n # before b\n b: [x, # ex\n y]} # end\n";
        let formatted = assert_round_trip(text);
        assert_eq!(
            formatted,
            "# head\na: 1 # one\n# before b\nb:\n  - x # ex\n  - y # end\n"
        );
        assert_eq!(comments(text), comments(&formatted));
        let block = "# head\na: 1 # one\n# before b\nb: # bee\n  - x\n  # before y\n  - y\n";
        assert_eq!(yaml(block).as_deref(), Ok(block));
    }

    #[test]
    fn block_scalars_are_re_indented_without_changing_their_content() {
        let text = "root:\n    text: |\n        line one\n          indented\n\n        end\n    folded: >-\n        a\n        b\n    keep: |+\n        t\n\n";
        let formatted = assert_round_trip(text);
        assert_eq!(
            formatted,
            "root:\n  text: |\n    line one\n      indented\n\n    end\n  folded: >-\n    a\n    b\n  keep: |+\n    t\n\n"
        );
        assert_eq!(
            yaml("k: |\n  no final break").as_deref(),
            Ok("k: |\n  no final break")
        );
    }

    #[test]
    fn multi_line_flow_scalars_keep_their_folded_value() {
        let formatted = assert_round_trip("{a: \"one\n    two\n\n    three\", b: plain\n   words}");
        assert_eq!(
            formatted,
            "a: \"one\n  two\n\n  three\"\nb: plain\n  words\n"
        );
    }

    #[test]
    fn crlf_and_byte_order_mark_format_like_lf() {
        let lf = "a:\n  - x # c\nb: |\n  t\n";
        let crlf = format!("\u{feff}{}", lf.replace('\n', "\r\n"));
        assert_eq!(yaml(&crlf).as_deref(), Ok(lf));
    }

    #[test]
    fn empty_and_comment_only_streams_are_kept() {
        assert_eq!(yaml("").as_deref(), Ok(""));
        assert_eq!(yaml("# only\n").as_deref(), Ok("# only\n"));
        assert_eq!(yaml("---\n").as_deref(), Ok("---\n"));
    }

    #[test]
    fn malformed_yaml_is_refused() {
        for text in [
            "a: [1, 2",
            "{a: 1",
            "a: b: c",
            "- a\nb: c",
            "key: \"unterminated",
            "a:\n\tb: 1",
            "a: *missing",
            "a: 1\na: [",
            "]",
        ] {
            assert!(yaml(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn inputs_that_cannot_be_kept_exactly_are_refused() {
        for text in [
            "? [a, b]\n: c\n",
            "? {a: 1}\n: c\n",
            "a: |2\n   x\n",
            "? |\n  block key\n: v\n",
            "{\"multi\n line\": 1}",
        ] {
            assert!(yaml(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn nesting_beyond_the_limit_is_refused() {
        let nested = |depth: usize| format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert!(yaml(&nested(255)).is_ok());
        assert!(yaml(&nested(300)).is_err());
        let block = |depth: usize| format!("{}x\n", "- ".repeat(depth));
        assert!(yaml(&block(255)).is_ok());
        assert!(yaml(&block(300)).is_err_and(|error| error.contains("256 levels")));
    }

    #[test]
    fn yaml_budgets_are_enforced_while_parsing_and_writing() {
        let many = format!("[{}0]", "0,".repeat(700_000));
        assert!(yaml(&many).is_err_and(|error| error.contains("temporary formatting memory limit")));
        // Whole-line comments take the indentation of the node after them.
        let nesting = (0..250).fold(String::new(), |mut text, depth| {
            let _ = writeln!(text, "{}k:", " ".repeat(depth));
            text
        });
        let wide = format!("{nesting}{}{}v: 1\n", "#\n".repeat(60_000), " ".repeat(250));
        assert!(yaml(&wide).is_err_and(|error| error.contains("24 MiB limit")));
    }

    #[test]
    fn characters_outside_the_yaml_character_set_are_refused() {
        for text in [
            "a: \u{1}\n",
            "a: \"\u{7f}\"\n",
            "a: \u{9f}\n",
            "a: 1\n\u{feff}b: 2\n",
            "\u{feff}\u{feff}a: 1\n",
            "\r\u{feff}{\u{feff}\"",
            "a: \u{fffe}\n",
        ] {
            assert!(
                yaml(text).is_err_and(|error| error.contains("not allowed in YAML text")),
                "{text:?}"
            );
        }
        let allowed = "a: \"\t\u{85}\u{a0}\u{2028}\u{1f600}\"\n";
        assert_eq!(yaml(allowed).as_deref(), Ok(allowed));
    }

    #[test]
    fn block_scalars_of_white_space_lines_are_written_as_empty_lines() {
        let expected = "a: |+\n\n\nb: >-\n\nc: 1\n";
        assert_eq!(
            yaml("a: |+\n  \n\nb: >-\n     \nc: 1\n").as_deref(),
            Ok(expected)
        );
        assert_eq!(yaml(expected).as_deref(), Ok(expected));
    }

    #[test]
    fn lone_carriage_returns_are_line_breaks() {
        assert_eq!(
            yaml("a: 1\r# own line\rb: |\r  x\r   y\rc: [1,\r  2]\r").as_deref(),
            Ok("a: 1\n# own line\nb: |\n  x\n   y\nc:\n  - 1\n  - 2\n")
        );
        assert_eq!(
            yaml("a: 1\r\n# c\r\nb: 2\r\n").as_deref(),
            Ok("a: 1\n# c\nb: 2\n")
        );
    }

    #[test]
    fn flow_collections_the_scanner_queues_count_toward_the_storage_budget() {
        // The scanner queues every token of a top-level flow collection
        // before the parser reports its first event.
        let queued = format!("[{}0]", "0,".repeat(30_000));
        assert!(
            yaml(&queued).is_err_and(|error| error.contains("temporary formatting memory limit"))
        );
        let small = format!("[{}0]", "0,".repeat(2_000));
        assert!(yaml(&small).is_ok());
        // Punctuation inside one large scalar is not a queue of tokens.
        let scalar = format!("k: |\n{}", "  {\"a\": [1, 2], \"b\": 'x'}\n".repeat(40_000));
        assert!(yaml(&scalar).is_ok());
    }

    #[test]
    fn comments_count_toward_the_storage_budget_when_they_are_queued() {
        let comments = "# c\n".repeat(200_000);
        assert!(
            yaml(&comments).is_err_and(|error| error.contains("temporary formatting memory limit"))
        );
        assert!(yaml(&"# c\n".repeat(2_000)).is_ok());
    }

    #[test]
    fn mutated_inputs_are_formatted_or_refused_without_a_panic() {
        let seeds = [
            "a: &x {b: [1, 'two', \"three\"], c: |\n  text\n}\nd: *x # c\n---\n- !t [k: v]\n",
            "key: >-\n  folded\n  text\nlist:\n- a\n- ? b\n  : c\n",
        ];
        let replacements = [
            "", "\n", " ", "-", ":", "[", "]", "{", "}", "#", "&", "*", "!", "|", ">", "'", "\"",
            ",", "?", "\t",
        ];
        let mut state = 0x2545_f491_u32;
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
                if let Ok(formatted) = yaml(&text) {
                    assert_eq!(events(&text), events(&formatted), "{text:?}");
                }
            }
        }
    }
}
