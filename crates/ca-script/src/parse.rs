//! The script parser.
//!
//! Command words and keywords are read without regard to letter case. `lt`
//! stands for `left` and `rt` for `right` everywhere a side is named. A token
//! that stood between quotation marks is never read as a keyword, so a folder
//! named `all` can still be passed to `expand`.

use crate::ast::{
    AttrSet, AttribChange, Command, CompareType, Comparison, ConfirmMode, ContentCriterion,
    Criteria, CutoffSpec, CutoffValue, Direction, FilterAttrSet, FilterAttribChange, FilterClause,
    LoadSpec, LogLevel, LogSpec, LogTarget, OptionSpec, OutputOption, OutputTo, PathOption,
    PathsArg, RenameSpec, ReportKind, ReportLayout, ReportOption, ReportSpec, Script, SelectKind,
    SelectMask, SelectResult, Side, SideArg, SizeSpec, SizeUnit, SnapshotSource, SnapshotSpec,
    Span, Statement, SyncDirection, SyncMode, SyncSpec, TimestampCriterion, TimezoneCriterion,
    TouchSpec, TouchValue, UnixTypeChange, UnixTypeSet,
};
use crate::error::ParseError;
use crate::lex::{logical_lines, LogicalLine, Token};
use crate::subst::Substitution;

/// Parse script text with no substitution.
///
/// # Errors
/// Returns the first [`ParseError`] the text raises.
pub fn parse(source: &str) -> Result<Script, ParseError> {
    parse_with(source, &Substitution::none())
}

/// Parse script text, replacing argument references as each line is read.
///
/// # Errors
/// Returns the first [`ParseError`] the text raises.
pub fn parse_with(source: &str, subst: &Substitution) -> Result<Script, ParseError> {
    let lines = logical_lines(source)?;
    let mut statements = Vec::with_capacity(lines.len());
    for line in &lines {
        statements.push(parse_line(line, subst)?);
    }
    Ok(Script { statements })
}

fn parse_line(line: &LogicalLine, subst: &Substitution) -> Result<Statement, ParseError> {
    let tokens: Vec<Token> = line
        .tokens
        .iter()
        .map(|token| Token {
            text: subst.apply(&token.text),
            ..token.clone()
        })
        .collect();
    let mut cursor = Cursor::new(&tokens, line.line, line.column);
    let word = cursor.take("a command word")?;
    let lowered = word.text.to_ascii_lowercase();
    let command = match lowered.as_str() {
        "attrib" => Command::Attrib(parse_attrib(&mut cursor)?),
        "beep" => Command::Beep,
        "collapse" => Command::Collapse(parse_paths(&mut cursor, "collapse")?),
        "compare" => Command::Compare(parse_compare(&mut cursor)?),
        "copy" => Command::Copy(parse_direction_argument(&mut cursor)?),
        "copyto" => parse_to_folder(&mut cursor, false)?,
        "criteria" => Command::Criteria(parse_criteria(&mut cursor)?),
        "delete" => parse_delete(&mut cursor)?,
        "expand" => Command::Expand(parse_paths(&mut cursor, "expand")?),
        "filter" => Command::Filter(parse_filter(&mut cursor)?),
        "load" => Command::Load(parse_load(&mut cursor)?),
        "log" => Command::Log(parse_log(&mut cursor)?),
        "move" => Command::Move(parse_direction_argument(&mut cursor)?),
        "moveto" => parse_to_folder(&mut cursor, true)?,
        "option" => Command::Option(parse_option(&mut cursor)?),
        "rename" => Command::Rename(parse_rename(&mut cursor)?),
        "select" => Command::Select(parse_select(&mut cursor)?),
        "snapshot" => Command::Snapshot(Box::new(parse_snapshot(&mut cursor)?)),
        "sync" => Command::Sync(parse_sync(&mut cursor)?),
        "touch" => Command::Touch(parse_touch(&mut cursor)?),
        _ => match report_kind(&lowered) {
            Some(kind) => Command::Report {
                kind,
                spec: Box::new(parse_report(&mut cursor, kind)?),
            },
            None => {
                return Err(ParseError::new(
                    word.line,
                    word.column,
                    format!("{} is not a script command", word.text),
                ))
            }
        },
    };
    cursor.expect_end()?;
    Ok(Statement {
        command,
        span: Span::new(line.line, line.column),
    })
}

fn report_kind(word: &str) -> Option<ReportKind> {
    Some(match word {
        "file-report" => ReportKind::File,
        "folder-report" => ReportKind::Folder,
        "hex-report" => ReportKind::Hex,
        "text-report" => ReportKind::Text,
        "data-report" | "table-report" => ReportKind::Data,
        "media-report" => ReportKind::Media,
        "picture-report" => ReportKind::Picture,
        "registry-report" => ReportKind::Registry,
        "version-report" => ReportKind::Version,
        _ => return None,
    })
}

// -- cursor ------------------------------------------------------------------

struct Cursor<'a> {
    tokens: &'a [Token],
    index: usize,
    line: u32,
    column: u32,
}

impl<'a> Cursor<'a> {
    fn new(tokens: &'a [Token], line: u32, column: u32) -> Self {
        Self {
            tokens,
            index: 0,
            line,
            column,
        }
    }

    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.index)
    }

    fn advance(&mut self) -> Option<&'a Token> {
        let token = self.tokens.get(self.index)?;
        self.index += 1;
        self.line = token.line;
        self.column = token.column;
        Some(token)
    }

    fn take(&mut self, want: &str) -> Result<&'a Token, ParseError> {
        match self.advance() {
            Some(token) => Ok(token),
            None => Err(self.error(format!("{want} is missing"))),
        }
    }

    fn remaining(&self) -> usize {
        self.tokens.len().saturating_sub(self.index)
    }

    fn error(&self, message: impl Into<String>) -> ParseError {
        ParseError::new(self.line, self.column, message)
    }

    fn expect_end(&self) -> Result<(), ParseError> {
        match self.peek() {
            None => Ok(()),
            Some(token) => Err(ParseError::new(
                token.line,
                token.column,
                format!("{} is not part of this command", token.text),
            )),
        }
    }
}

fn token_error(token: &Token, message: impl Into<String>) -> ParseError {
    ParseError::new(token.line, token.column, message)
}

/// The value after `key:`, when the token carries that key.
///
/// A token that began inside quotation marks carries no key, so a path or a
/// mask that happens to look like one is still read as a value.
fn keyed<'t>(token: &'t Token, key: &str) -> Option<&'t str> {
    if token.quoted {
        return None;
    }
    let (head, rest) = token.text.split_once(':')?;
    if head.eq_ignore_ascii_case(key) {
        Some(rest)
    } else {
        None
    }
}

/// True when the token is an unquoted keyword.
fn keyword(token: &Token, word: &str) -> bool {
    !token.quoted && token.text.eq_ignore_ascii_case(word)
}

fn side_word(text: &str) -> Option<SideArg> {
    Some(match text.to_ascii_lowercase().as_str() {
        "left" | "lt" => SideArg::Left,
        "right" | "rt" => SideArg::Right,
        "all" => SideArg::All,
        _ => return None,
    })
}

fn one_side_word(text: &str) -> Option<Side> {
    match side_word(text)? {
        SideArg::Left => Some(Side::Left),
        SideArg::Right => Some(Side::Right),
        SideArg::All => None,
    }
}

fn direction(text: &str) -> Option<Direction> {
    let (from, to) = text.split_once("->")?;
    match (one_side_word(from)?, one_side_word(to)?) {
        (Side::Left, Side::Right) => Some(Direction::LeftToRight),
        (Side::Right, Side::Left) => Some(Direction::RightToLeft),
        _ => None,
    }
}

// -- commands ----------------------------------------------------------------

fn parse_attrib(cursor: &mut Cursor<'_>) -> Result<Vec<AttribChange>, ParseError> {
    let mut out = Vec::new();
    while let Some(token) = cursor.advance() {
        out.push(attrib_change(token)?);
    }
    if out.is_empty() {
        return Err(cursor.error("attrib needs at least one +letters or -letters group"));
    }
    Ok(out)
}

fn attrib_change(token: &Token) -> Result<AttribChange, ParseError> {
    let (sign, letters) = split_sign(token)?;
    let mut attrs = AttrSet::default();
    for letter in letters.chars() {
        match letter.to_ascii_lowercase() {
            'a' => attrs.archive = true,
            's' => attrs.system = true,
            'h' => attrs.hidden = true,
            'r' => attrs.read_only = true,
            other => {
                return Err(token_error(
                    token,
                    format!("{other} is not an attribute letter; use a, s, h or r"),
                ))
            }
        }
    }
    if attrs.is_empty() {
        return Err(token_error(token, "the attribute group names no attribute"));
    }
    Ok(AttribChange { set: sign, attrs })
}

fn split_sign(token: &Token) -> Result<(bool, &str), ParseError> {
    match token.text.strip_prefix('+') {
        Some(rest) => Ok((true, rest)),
        None => match token.text.strip_prefix('-') {
            Some(rest) => Ok((false, rest)),
            None => Err(token_error(token, "an attribute group starts with + or -")),
        },
    }
}

fn parse_paths(cursor: &mut Cursor<'_>, word: &str) -> Result<PathsArg, ParseError> {
    let mut paths = Vec::new();
    while let Some(token) = cursor.advance() {
        if paths.is_empty() && keyword(token, "all") {
            if cursor.remaining() > 0 {
                return Err(cursor.error(format!("{word} all takes no other argument")));
            }
            return Ok(PathsArg::All);
        }
        paths.push(token.text.clone());
    }
    if paths.is_empty() {
        return Err(cursor.error(format!("{word} needs all or at least one path")));
    }
    Ok(PathsArg::Paths(paths))
}

fn parse_compare(cursor: &mut Cursor<'_>) -> Result<Option<CompareType>, ParseError> {
    let Some(token) = cursor.advance() else {
        return Ok(None);
    };
    Ok(Some(match token.text.to_ascii_lowercase().as_str() {
        "crc" => CompareType::Crc,
        "binary" => CompareType::Binary,
        "rules-based" => CompareType::RulesBased,
        _ => {
            return Err(token_error(
                token,
                "compare takes crc, binary or rules-based",
            ))
        }
    }))
}

fn parse_direction_argument(cursor: &mut Cursor<'_>) -> Result<Direction, ParseError> {
    let token = cursor.take("a direction")?;
    direction(&token.text)
        .ok_or_else(|| token_error(token, "the direction is left->right or right->left"))
}

fn parse_to_folder(cursor: &mut Cursor<'_>, moving: bool) -> Result<Command, ParseError> {
    let mut side = SideArg::All;
    let mut path_option = PathOption::None;
    let mut destination: Option<String> = None;
    while let Some(token) = cursor.advance() {
        if destination.is_none() && !token.quoted {
            if let Some(value) = side_word(&token.text) {
                side = value;
                continue;
            }
            if let Some(value) = keyed(token, "path") {
                path_option = match value.to_ascii_lowercase().as_str() {
                    "relative" => PathOption::Relative,
                    "base" => PathOption::Base,
                    "none" => PathOption::None,
                    _ => return Err(token_error(token, "path: takes relative, base or none")),
                };
                continue;
            }
        }
        if destination.is_some() {
            return Err(token_error(token, "only one destination folder is allowed"));
        }
        destination = Some(token.text.clone());
    }
    let Some(path) = destination else {
        return Err(cursor.error("the destination folder is missing"));
    };
    Ok(if moving {
        Command::MoveTo {
            side,
            path_option,
            path,
        }
    } else {
        Command::CopyTo {
            side,
            path_option,
            path,
        }
    })
}

fn parse_criteria(cursor: &mut Cursor<'_>) -> Result<Criteria, ParseError> {
    let mut out = Criteria::default();
    while let Some(token) = cursor.advance() {
        let lowered = token.text.to_ascii_lowercase();
        if let Some(value) = keyed(token, "attrib") {
            out.attrib = Some(attrib_letters(token, value)?);
            continue;
        }
        if let Some(value) = keyed(token, "timestamp") {
            out.timestamp = Some(timestamp_criterion(token, value)?);
            continue;
        }
        if let Some(value) = keyed(token, "timezone") {
            out.timezone = Some(timezone_criterion(token, value)?);
            continue;
        }
        match lowered.as_str() {
            "version" => out.version = true,
            "timestamp" => out.timestamp = Some(TimestampCriterion::default()),
            "size" => out.content = Some(ContentCriterion::Size),
            "crc" => out.content = Some(ContentCriterion::Crc),
            "binary" => out.content = Some(ContentCriterion::Binary),
            "rules-based" => out.content = Some(ContentCriterion::RulesBased),
            "follow-symlinks" => out.follow_symlinks = true,
            "ignore-unimportant" => out.ignore_unimportant = true,
            "owner" => out.owner = true,
            "group" => out.group = true,
            "permissions" => out.permissions = true,
            _ => {
                return Err(token_error(
                    token,
                    format!("{} is not a criteria keyword", token.text),
                ))
            }
        }
    }
    Ok(out)
}

fn attrib_letters(token: &Token, letters: &str) -> Result<AttrSet, ParseError> {
    let mut attrs = AttrSet::default();
    for letter in letters.chars() {
        match letter.to_ascii_lowercase() {
            'a' => attrs.archive = true,
            's' => attrs.system = true,
            'h' => attrs.hidden = true,
            'r' => attrs.read_only = true,
            other => {
                return Err(token_error(
                    token,
                    format!("{other} is not an attribute letter; use a, s, h or r"),
                ))
            }
        }
    }
    Ok(attrs)
}

fn timestamp_criterion(token: &Token, value: &str) -> Result<TimestampCriterion, ParseError> {
    let mut out = TimestampCriterion::default();
    for part in value.split(';').filter(|part| !part.is_empty()) {
        if part.eq_ignore_ascii_case("ignoredst") {
            out.ignore_dst = true;
            continue;
        }
        let digits = part
            .strip_suffix("sec")
            .or_else(|| part.strip_suffix("SEC"))
            .or_else(|| part.strip_suffix("Sec"));
        match digits.and_then(|digits| digits.parse::<u32>().ok()) {
            Some(seconds) => out.tolerance_seconds = Some(seconds),
            None => {
                return Err(token_error(
                    token,
                    "the timestamp clause takes <number>sec and IgnoreDST",
                ))
            }
        }
    }
    Ok(out)
}

fn timezone_criterion(token: &Token, value: &str) -> Result<TimezoneCriterion, ParseError> {
    if value.eq_ignore_ascii_case("ignore") {
        return Ok(TimezoneCriterion::Ignore);
    }
    let split = value.find(['+', '-']).ok_or_else(|| {
        token_error(
            token,
            "the timezone clause takes ignore or a side with a signed hour offset",
        )
    })?;
    let (name, signed) = value.split_at(split);
    let side = one_side_word(name)
        .ok_or_else(|| token_error(token, "the timezone offset names left or right"))?;
    let hours: i8 = signed
        .parse()
        .map_err(|_| token_error(token, "the timezone offset is a whole number of hours"))?;
    if !(-12..=12).contains(&hours) {
        return Err(token_error(
            token,
            "the timezone offset runs from -12 to +12 hours",
        ));
    }
    Ok(TimezoneCriterion::Offset { side, hours })
}

fn parse_delete(cursor: &mut Cursor<'_>) -> Result<Command, ParseError> {
    let mut recycle_bin = None;
    let mut side = None;
    while let Some(token) = cursor.advance() {
        if let Some((head, value)) = token.text.split_once('=') {
            if head.eq_ignore_ascii_case("recyclebin") {
                recycle_bin = Some(match value.to_ascii_lowercase().as_str() {
                    "yes" => true,
                    "no" => false,
                    _ => return Err(token_error(token, "recyclebin= takes yes or no")),
                });
                continue;
            }
        }
        match side_word(&token.text) {
            Some(value) if side.is_none() => side = Some(value),
            _ => {
                return Err(token_error(
                    token,
                    format!("{} is not a delete argument", token.text),
                ))
            }
        }
    }
    let Some(side) = side else {
        return Err(cursor.error("delete names left, right or all"));
    };
    Ok(Command::Delete { recycle_bin, side })
}

fn parse_filter(cursor: &mut Cursor<'_>) -> Result<Vec<FilterClause>, ParseError> {
    let mut out = Vec::new();
    let mut has_masks = false;
    while let Some(token) = cursor.advance() {
        let clause = parse_filter_clause(token)?;
        if matches!(clause, FilterClause::Masks(_)) {
            if has_masks {
                return Err(token_error(
                    token,
                    "filter takes one mask argument; separate masks with semicolons",
                ));
            }
            has_masks = true;
        }
        out.push(clause);
    }
    if out.is_empty() {
        return Err(cursor.error("filter needs masks or a clause"));
    }
    Ok(out)
}

fn cutoff_spec(token: &Token, value: &str) -> Result<Option<CutoffSpec>, ParseError> {
    if value.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let (newer, rest) = match value.strip_prefix('>') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('<').unwrap_or(value)),
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return Err(token_error(token, "the date filter names no value"));
    }
    let lowered = rest.to_ascii_lowercase();
    if let Some(digits) = lowered.strip_suffix("days") {
        let days: u32 = digits
            .trim()
            .parse()
            .map_err(|_| token_error(token, "the day count is a whole number"))?;
        return Ok(Some(CutoffSpec {
            newer,
            value: CutoffValue::Days(days),
        }));
    }
    Ok(Some(CutoffSpec {
        newer,
        value: CutoffValue::Timestamp(rest.to_string()),
    }))
}

fn size_spec(token: &Token, value: &str) -> Result<Option<SizeSpec>, ParseError> {
    if value.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let (larger, rest) = match value.strip_prefix('>') {
        Some(rest) => (true, rest),
        None => match value.strip_prefix('<') {
            Some(rest) => (false, rest),
            None => {
                return Err(token_error(
                    token,
                    "the size filter starts with < or > to say which side is excluded",
                ))
            }
        },
    };
    let rest = rest.trim();
    let lowered = rest.to_ascii_lowercase();
    let (digits, unit) = if let Some(head) = lowered.strip_suffix("kb") {
        (head, SizeUnit::Kilobytes)
    } else if let Some(head) = lowered.strip_suffix("mb") {
        (head, SizeUnit::Megabytes)
    } else if let Some(head) = lowered.strip_suffix("gb") {
        (head, SizeUnit::Gigabytes)
    } else if let Some(head) = lowered.strip_suffix("tb") {
        (head, SizeUnit::Terabytes)
    } else {
        (lowered.as_str(), SizeUnit::Bytes)
    };
    let value: u64 = digits
        .trim()
        .parse()
        .map_err(|_| token_error(token, "the size is a whole number of units"))?;
    Ok(Some(SizeSpec {
        larger,
        value,
        unit,
    }))
}

fn filter_attrib(token: &Token, value: &str) -> Result<Option<FilterAttribChange>, ParseError> {
    if value.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let wide = Token {
        text: value.to_string(),
        ..token.clone()
    };
    let (sign, letters) = split_sign(&wide)?;
    let mut set = FilterAttrSet::default();
    for letter in letters.chars() {
        match letter.to_ascii_lowercase() {
            'a' => set.archive = true,
            'c' => set.compressed = true,
            'e' => set.encrypted = true,
            'h' => set.hidden = true,
            'i' => set.not_indexed = true,
            'l' => set.link = true,
            'o' => set.offline = true,
            'p' => set.pinned = true,
            'r' => set.read_only = true,
            's' => set.system = true,
            't' => set.temporary = true,
            'u' => set.unpinned = true,
            'z' => set.sparse = true,
            other => {
                return Err(token_error(
                    token,
                    format!("{other} is not a filter attribute letter"),
                ))
            }
        }
    }
    if set.is_empty() {
        return Err(token_error(token, "the attribute group names no attribute"));
    }
    Ok(Some(FilterAttribChange {
        include: sign,
        attrs: set,
    }))
}

fn unix_type(token: &Token, value: &str) -> Result<Option<UnixTypeChange>, ParseError> {
    if value.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let wide = Token {
        text: value.to_string(),
        ..token.clone()
    };
    let (include, letters) = split_sign(&wide)?;
    let mut kinds = UnixTypeSet::default();
    for letter in letters.chars() {
        match letter.to_ascii_lowercase() {
            'b' => kinds.block = true,
            'c' => kinds.character = true,
            'l' => kinds.link = true,
            'p' => kinds.fifo = true,
            'r' => kinds.regular = true,
            's' => kinds.socket = true,
            other => {
                return Err(token_error(
                    token,
                    format!("{other} is not a file kind letter"),
                ))
            }
        }
    }
    if kinds.is_empty() {
        return Err(token_error(token, "the file kind group names no kind"));
    }
    Ok(Some(UnixTypeChange { include, kinds }))
}

fn parse_filter_clause(token: &Token) -> Result<FilterClause, ParseError> {
    if let Some(value) = keyed(token, "cutoff") {
        return Ok(FilterClause::Cutoff(cutoff_spec(token, value)?));
    }
    if let Some(value) = keyed(token, "size") {
        return Ok(FilterClause::Size(size_spec(token, value)?));
    }
    if let Some(value) = keyed(token, "attrib") {
        return Ok(FilterClause::Attrib(filter_attrib(token, value)?));
    }
    if let Some(value) = keyed(token, "unixtype") {
        return Ok(FilterClause::UnixType(unix_type(token, value)?));
    }
    if keyword(token, "exclude-protected") {
        return Ok(FilterClause::ExcludeProtected);
    }
    if keyword(token, "include-protected") {
        return Ok(FilterClause::IncludeProtected);
    }
    Ok(FilterClause::Masks(token.text.clone()))
}

fn parse_load(cursor: &mut Cursor<'_>) -> Result<LoadSpec, ParseError> {
    let mut create = None;
    let mut paths: Vec<String> = Vec::new();
    while let Some(token) = cursor.advance() {
        if !token.quoted {
            if keyword(token, "<default>") {
                if cursor.remaining() > 0 || !paths.is_empty() {
                    return Err(token_error(token, "load <default> takes no other argument"));
                }
                return Ok(LoadSpec::Default);
            }
            if let Some(value) = keyed(token, "create") {
                if !paths.is_empty() {
                    return Err(token_error(token, "create: comes before the folders"));
                }
                create = Some(
                    side_word(value)
                        .ok_or_else(|| token_error(token, "create: takes all, left or right"))?,
                );
                continue;
            }
        }
        if paths.len() == 2 {
            return Err(token_error(token, "load takes at most two folders"));
        }
        paths.push(token.text.clone());
    }
    let mut paths = paths.into_iter();
    let Some(left) = paths.next() else {
        return Err(cursor.error("load needs a session name or a folder"));
    };
    Ok(LoadSpec::Paths {
        create,
        left,
        right: paths.next(),
    })
}

fn parse_log(cursor: &mut Cursor<'_>) -> Result<LogSpec, ParseError> {
    let mut out = LogSpec::default();
    while let Some(token) = cursor.advance() {
        if out.level.is_none() && out.target.is_none() && !token.quoted {
            match token.text.to_ascii_lowercase().as_str() {
                "none" => {
                    out.level = Some(LogLevel::None);
                    continue;
                }
                "normal" => {
                    out.level = Some(LogLevel::Normal);
                    continue;
                }
                "verbose" => {
                    out.level = Some(LogLevel::Verbose);
                    continue;
                }
                _ => {}
            }
        }
        if out.target.is_some() {
            return Err(token_error(token, "log takes one file name"));
        }
        let (append, file) = match keyed(token, "append") {
            Some(rest) => (true, rest.to_string()),
            None => (false, token.text.clone()),
        };
        if file.is_empty() {
            return Err(token_error(token, "the log file name is empty"));
        }
        out.target = Some(LogTarget { append, file });
    }
    if out.level.is_none() && out.target.is_none() {
        return Err(cursor.error("log needs a level, a file name, or both"));
    }
    Ok(out)
}

fn parse_option(cursor: &mut Cursor<'_>) -> Result<OptionSpec, ParseError> {
    let token = cursor.take("an option name")?;
    if keyword(token, "stop-on-error") {
        return Ok(OptionSpec::StopOnError);
    }
    if let Some(value) = keyed(token, "confirm") {
        return Ok(OptionSpec::Confirm(
            match value.to_ascii_lowercase().as_str() {
                "prompt" => ConfirmMode::Prompt,
                "yes-to-all" => ConfirmMode::YesToAll,
                "no-to-all" => ConfirmMode::NoToAll,
                _ => {
                    return Err(token_error(
                        token,
                        "confirm: takes prompt, yes-to-all or no-to-all",
                    ))
                }
            },
        ));
    }
    Err(token_error(
        token,
        "option takes stop-on-error or confirm:<answer>",
    ))
}

fn parse_rename(cursor: &mut Cursor<'_>) -> Result<RenameSpec, ParseError> {
    let first = cursor.take("a rename mask")?;
    if keyword(first, "regexpr") {
        let find = cursor.take("the expression to match")?.text.clone();
        let replace = cursor.take("the replacement template")?.text.clone();
        return Ok(RenameSpec::Regex { find, replace });
    }
    Ok(RenameSpec::Mask(first.text.clone()))
}

fn parse_select(cursor: &mut Cursor<'_>) -> Result<Vec<SelectMask>, ParseError> {
    let mut out = Vec::new();
    while let Some(token) = cursor.advance() {
        out.push(select_mask(token)?);
    }
    if out.is_empty() {
        return Err(cursor.error("select needs at least one mask"));
    }
    Ok(out)
}

fn select_mask(token: &Token) -> Result<SelectMask, ParseError> {
    let lowered = token.text.to_ascii_lowercase();
    if lowered == "empty.folders" {
        return Ok(SelectMask::EmptyFolders);
    }
    let mut side = SideArg::All;
    let mut result = SelectResult::All;
    let mut kind = SelectKind::All;
    let mut slot = 0u8;
    for part in lowered.split('.') {
        if part.is_empty() {
            return Err(token_error(token, "a selection mask has an empty part"));
        }
        let mut placed = false;
        while slot < 3 && !placed {
            match slot {
                0 => {
                    if let Some(value) = side_word(part) {
                        side = value;
                        placed = true;
                    }
                }
                1 => {
                    if let Some(value) = select_result(part) {
                        result = value;
                        placed = true;
                    }
                }
                _ => {
                    if let Some(value) = select_kind(part) {
                        kind = value;
                        placed = true;
                    }
                }
            }
            slot += 1;
        }
        if !placed {
            return Err(token_error(
                token,
                format!("{part} is not a selection mask part in this position"),
            ));
        }
    }
    Ok(SelectMask::Mask { side, result, kind })
}

fn select_result(part: &str) -> Option<SelectResult> {
    Some(match part {
        "exact" => SelectResult::Exact,
        "diff" => SelectResult::Diff,
        "newer" => SelectResult::Newer,
        "older" => SelectResult::Older,
        "orphan" => SelectResult::Orphan,
        "all" => SelectResult::All,
        _ => return None,
    })
}

fn select_kind(part: &str) -> Option<SelectKind> {
    Some(match part {
        "files" => SelectKind::Files,
        "folders" => SelectKind::Folders,
        "all" => SelectKind::All,
        _ => return None,
    })
}

fn parse_snapshot(cursor: &mut Cursor<'_>) -> Result<SnapshotSpec, ParseError> {
    let mut spec = SnapshotSpec {
        save_crc: false,
        save_version: false,
        expand_archives: false,
        follow_symlinks: false,
        include_empty: false,
        no_filters: false,
        source: SnapshotSource::Left,
        output: None,
    };
    let mut source = None;
    while let Some(token) = cursor.advance() {
        if !token.quoted {
            let lowered = token.text.to_ascii_lowercase();
            let mut matched = true;
            match lowered.as_str() {
                "save-crc" => spec.save_crc = true,
                "save-version" => spec.save_version = true,
                "expand-archives" => spec.expand_archives = true,
                "follow-symlinks" => spec.follow_symlinks = true,
                "include-empty" => spec.include_empty = true,
                "no-filters" => spec.no_filters = true,
                "left" | "lt" => source = Some(SnapshotSource::Left),
                "right" | "rt" => source = Some(SnapshotSource::Right),
                _ => matched = false,
            }
            if matched {
                continue;
            }
        }
        if let Some(value) = keyed(token, "path") {
            source = Some(SnapshotSource::Path(value.to_string()));
            continue;
        }
        if let Some(value) = keyed(token, "output") {
            spec.output = Some(value.to_string());
            continue;
        }
        return Err(token_error(
            token,
            format!("{} is not a snapshot argument", token.text),
        ));
    }
    spec.source =
        source.ok_or_else(|| cursor.error("snapshot names left, right or path:<path>"))?;
    Ok(spec)
}

fn parse_sync(cursor: &mut Cursor<'_>) -> Result<SyncSpec, ParseError> {
    let mut visible = false;
    let mut create_empty = false;
    let mut rule = None;
    while let Some(token) = cursor.advance() {
        if keyword(token, "visible") {
            visible = true;
            continue;
        }
        if keyword(token, "create-empty") {
            create_empty = true;
            continue;
        }
        let mode = if let Some(value) = keyed(token, "update") {
            Some((SyncMode::Update, value))
        } else {
            keyed(token, "mirror").map(|value| (SyncMode::Mirror, value))
        };
        let Some((mode, value)) = mode else {
            return Err(token_error(
                token,
                format!("{} is not a sync argument", token.text),
            ));
        };
        if rule.is_some() {
            return Err(token_error(token, "sync takes one update: or mirror: rule"));
        }
        let direction = if value.eq_ignore_ascii_case("all") {
            SyncDirection::All
        } else {
            match direction(value) {
                Some(Direction::LeftToRight) => SyncDirection::LeftToRight,
                Some(Direction::RightToLeft) => SyncDirection::RightToLeft,
                None => {
                    return Err(token_error(
                        token,
                        "the sync direction is left->right, right->left or all",
                    ))
                }
            }
        };
        if mode == SyncMode::Mirror && direction == SyncDirection::All {
            return Err(token_error(token, "mirror needs one direction, not all"));
        }
        rule = Some((mode, direction));
    }
    let Some((mode, direction)) = rule else {
        return Err(cursor.error("sync needs update:<direction> or mirror:<direction>"));
    };
    Ok(SyncSpec {
        visible,
        create_empty,
        mode,
        direction,
    })
}

fn parse_touch(cursor: &mut Cursor<'_>) -> Result<TouchSpec, ParseError> {
    let token = cursor.take("a touch argument")?;
    if let Some(value) = direction(&token.text) {
        return Ok(TouchSpec::Copy(value));
    }
    let Some((name, value)) = token.text.split_once(':') else {
        return Err(token_error(
            token,
            "touch takes a direction or <side>:(now|<timestamp>)",
        ));
    };
    let side =
        side_word(name).ok_or_else(|| token_error(token, "touch names left, right or all"))?;
    let value = if value.eq_ignore_ascii_case("now") {
        TouchValue::Now
    } else if value.is_empty() {
        return Err(token_error(token, "the timestamp is missing"));
    } else {
        TouchValue::Timestamp(value.to_string())
    };
    Ok(TouchSpec::Set { side, value })
}

// -- reports -----------------------------------------------------------------

fn parse_report(cursor: &mut Cursor<'_>, kind: ReportKind) -> Result<ReportSpec, ParseError> {
    let mut layout = None;
    let mut options: Vec<ReportOption> = Vec::new();
    let mut title = None;
    let mut output_to = None;
    let mut output_options: Vec<OutputOption> = Vec::new();
    let mut trailing: Vec<String> = Vec::new();

    while let Some(token) = cursor.advance() {
        if let Some(value) = keyed(token, "layout") {
            layout = Some(report_layout(token, value, kind)?);
            continue;
        }
        if let Some(value) = keyed(token, "options") {
            for part in value.split(',').filter(|part| !part.is_empty()) {
                options.push(report_option(token, part, kind)?);
            }
            continue;
        }
        if let Some(value) = keyed(token, "title") {
            title = Some(value.to_string());
            continue;
        }
        if let Some(value) = keyed(token, "output-to") {
            output_to = Some(match value.to_ascii_lowercase().as_str() {
                "printer" => OutputTo::Printer,
                "clipboard" => OutputTo::Clipboard,
                _ => OutputTo::File(value.to_string()),
            });
            continue;
        }
        if let Some(value) = keyed(token, "output-options") {
            let mut parts = value.split(',').filter(|part| !part.is_empty()).peekable();
            while let Some(part) = parts.next() {
                if part.eq_ignore_ascii_case("html-custom") {
                    let sheet = parts.next().ok_or_else(|| {
                        token_error(token, "html-custom needs a stylesheet after it")
                    })?;
                    output_options.push(OutputOption::HtmlCustom(sheet.to_string()));
                    continue;
                }
                output_options.push(output_option(token, part)?);
            }
            continue;
        }
        if trailing.len() == 2 {
            return Err(token_error(
                token,
                "a report names a session or two files, not more",
            ));
        }
        trailing.push(token.text.clone());
    }

    let Some(layout) = layout else {
        return Err(cursor.error("the report needs layout:<layout>"));
    };
    let Some(output_to) = output_to else {
        return Err(cursor.error("the report needs output-to:<target>"));
    };
    let comparison = match trailing.len() {
        0 => None,
        1 => trailing
            .first()
            .map(|name| Comparison::Session(name.clone())),
        _ => match (trailing.first(), trailing.get(1)) {
            (Some(left), Some(right)) => Some(Comparison::Files(left.clone(), right.clone())),
            _ => None,
        },
    };
    if comparison.is_some() && kind == ReportKind::Folder {
        return Err(cursor.error("a folder report reads the loaded base folders"));
    }
    Ok(ReportSpec {
        layout,
        options,
        title,
        output_to,
        output_options,
        comparison,
    })
}

fn report_layout(token: &Token, value: &str, kind: ReportKind) -> Result<ReportLayout, ParseError> {
    let layout = match value.to_ascii_lowercase().as_str() {
        "side-by-side" => ReportLayout::SideBySide,
        "summary" => ReportLayout::Summary,
        "interleaved" => ReportLayout::Interleaved,
        "patch" => ReportLayout::Patch,
        "statistics" => ReportLayout::Statistics,
        "xml" => ReportLayout::Xml,
        _ => {
            return Err(token_error(
                token,
                format!("{value} is not a report layout"),
            ))
        }
    };
    if !layouts_of(kind).contains(&layout) {
        return Err(token_error(
            token,
            format!("{} does not take the {value} layout", kind.word()),
        ));
    }
    Ok(layout)
}

/// The layouts each report command accepts.
#[must_use]
pub fn layouts_of(kind: ReportKind) -> &'static [ReportLayout] {
    use ReportLayout as L;
    match kind {
        ReportKind::Text => &[
            L::SideBySide,
            L::Summary,
            L::Interleaved,
            L::Patch,
            L::Statistics,
            L::Xml,
        ],
        ReportKind::Folder => &[L::SideBySide, L::Summary, L::Xml],
        ReportKind::Hex | ReportKind::Data => &[L::SideBySide, L::Summary, L::Interleaved],
        ReportKind::File
        | ReportKind::Media
        | ReportKind::Picture
        | ReportKind::Registry
        | ReportKind::Version => &[L::SideBySide, L::Summary],
    }
}

fn report_option(token: &Token, value: &str, kind: ReportKind) -> Result<ReportOption, ParseError> {
    let lowered = value.to_ascii_lowercase();
    let option = ALL_REPORT_OPTIONS
        .iter()
        .copied()
        .find(|candidate| candidate.word() == lowered)
        .ok_or_else(|| token_error(token, format!("{value} is not a report option")))?;
    if !option_allowed(kind, option) {
        return Err(token_error(
            token,
            format!("{} does not take the {value} option", kind.word()),
        ));
    }
    Ok(option)
}

const ALL_REPORT_OPTIONS: &[ReportOption] = &[
    ReportOption::IgnoreUnimportant,
    ReportOption::DisplayAll,
    ReportOption::DisplayMismatches,
    ReportOption::DisplayMatches,
    ReportOption::DisplayContext,
    ReportOption::DisplayNoOrphans,
    ReportOption::DisplayMismatchesNoOrphans,
    ReportOption::DisplayOrphans,
    ReportOption::DisplayLeftNewer,
    ReportOption::DisplayRightNewer,
    ReportOption::DisplayLeftNewerOrphans,
    ReportOption::DisplayRightNewerOrphans,
    ReportOption::DisplayLeftOrphans,
    ReportOption::DisplayRightOrphans,
    ReportOption::LineNumbers,
    ReportOption::StrikeoutLeftDiffs,
    ReportOption::StrikeoutRightDiffs,
    ReportOption::PatchNormal,
    ReportOption::PatchContext,
    ReportOption::PatchUnified,
    ReportOption::ColumnVcs,
    ReportOption::ColumnRevision,
    ReportOption::ColumnVersion,
    ReportOption::ColumnSize,
    ReportOption::ColumnCrc,
    ReportOption::ColumnTimestamp,
    ReportOption::ColumnAttributes,
    ReportOption::ColumnOwner,
    ReportOption::ColumnGroup,
    ReportOption::ColumnNone,
    ReportOption::IncludeFileLinks,
    ReportOption::StatsDescriptive,
    ReportOption::StatsTabular,
];

/// True when the report command accepts the option.
#[must_use]
pub fn option_allowed(kind: ReportKind, option: ReportOption) -> bool {
    use ReportOption as O;
    let common = matches!(
        option,
        O::DisplayAll | O::DisplayMismatches | O::DisplayMatches
    );
    match kind {
        ReportKind::Text => {
            common
                || matches!(
                    option,
                    O::IgnoreUnimportant
                        | O::DisplayContext
                        | O::LineNumbers
                        | O::StrikeoutLeftDiffs
                        | O::StrikeoutRightDiffs
                        | O::PatchNormal
                        | O::PatchContext
                        | O::PatchUnified
                        | O::StatsDescriptive
                        | O::StatsTabular
                )
        }
        ReportKind::Folder => matches!(
            option,
            O::DisplayAll
                | O::DisplayMismatches
                | O::DisplayMatches
                | O::DisplayNoOrphans
                | O::DisplayMismatchesNoOrphans
                | O::DisplayOrphans
                | O::DisplayLeftNewer
                | O::DisplayRightNewer
                | O::DisplayLeftNewerOrphans
                | O::DisplayRightNewerOrphans
                | O::DisplayLeftOrphans
                | O::DisplayRightOrphans
                | O::ColumnVcs
                | O::ColumnRevision
                | O::ColumnVersion
                | O::ColumnSize
                | O::ColumnCrc
                | O::ColumnTimestamp
                | O::ColumnAttributes
                | O::ColumnOwner
                | O::ColumnGroup
                | O::ColumnNone
                | O::IncludeFileLinks
        ),
        ReportKind::Hex => common || option == O::LineNumbers,
        ReportKind::Data => common || matches!(option, O::IgnoreUnimportant | O::LineNumbers),
        ReportKind::File => common || matches!(option, O::IgnoreUnimportant | O::LineNumbers),
        ReportKind::Media | ReportKind::Version => common || option == O::IgnoreUnimportant,
        ReportKind::Picture => option == O::IgnoreUnimportant,
        ReportKind::Registry => common,
    }
}

fn output_option(token: &Token, value: &str) -> Result<OutputOption, ParseError> {
    Ok(match value.to_ascii_lowercase().as_str() {
        "print-color" => OutputOption::PrintColor,
        "print-mono" => OutputOption::PrintMono,
        "print-portrait" => OutputOption::PrintPortrait,
        "print-landscape" => OutputOption::PrintLandscape,
        "wrap-none" => OutputOption::WrapNone,
        "wrap-character" => OutputOption::WrapCharacter,
        "wrap-word" => OutputOption::WrapWord,
        "html-color" => OutputOption::HtmlColor,
        "html-mono" => OutputOption::HtmlMono,
        _ => {
            return Err(token_error(
                token,
                format!("{value} is not a report output option"),
            ))
        }
    })
}
