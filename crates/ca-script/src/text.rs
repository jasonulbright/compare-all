//! Writes a parsed script back out as script text.
//!
//! The text a command encodes to parses back to the same command, so a tool may
//! read a script, change one command and write the file again.

use std::fmt::Write as _;

use crate::ast::{
    AttribChange, Command, CompareType, Comparison, ConfirmMode, ContentCriterion, Criteria,
    CutoffSpec, CutoffValue, Direction, FilterClause, LoadSpec, LogLevel, LogSpec, OptionSpec,
    OutputOption, OutputTo, PathOption, PathsArg, RenameSpec, ReportKind, ReportSpec, Script,
    SelectKind, SelectMask, SelectResult, Side, SideArg, SizeSpec, SnapshotSource, SnapshotSpec,
    SyncDirection, SyncMode, SyncSpec, TimezoneCriterion, TouchSpec, TouchValue,
};
use crate::error::EncodeError;

/// Write a whole script, one command per line.
///
/// # Errors
/// Returns [`EncodeError`] for an argument the syntax cannot hold.
pub fn encode_script(script: &Script) -> Result<String, EncodeError> {
    let mut out = String::new();
    for statement in &script.statements {
        out.push_str(&encode(&statement.command)?);
        out.push('\n');
    }
    Ok(out)
}

/// Write one command.
///
/// # Errors
/// Returns [`EncodeError`] for an argument the syntax cannot hold.
#[allow(clippy::too_many_lines)]
pub fn encode(command: &Command) -> Result<String, EncodeError> {
    let mut parts: Vec<String> = vec![command.word().to_string()];
    match command {
        Command::Beep => {}
        Command::Attrib(groups) => {
            for group in groups {
                parts.push(attrib_group(*group));
            }
        }
        Command::Collapse(paths) | Command::Expand(paths) => match paths {
            PathsArg::All => parts.push("all".to_string()),
            PathsArg::Paths(list) => {
                for path in list {
                    parts.push(arg(path, reserved_word(path, &["all"]))?);
                }
            }
        },
        Command::Compare(kind) => {
            if let Some(kind) = kind {
                parts.push(
                    match kind {
                        CompareType::Crc => "crc",
                        CompareType::Binary => "binary",
                        CompareType::RulesBased => "rules-based",
                    }
                    .to_string(),
                );
            }
        }
        Command::Copy(direction) | Command::Move(direction) => {
            parts.push(direction_word(*direction).to_string());
        }
        Command::CopyTo {
            side,
            path_option,
            path,
        }
        | Command::MoveTo {
            side,
            path_option,
            path,
        } => {
            parts.push(side_word(*side).to_string());
            parts.push(format!("path:{}", path_option_word(*path_option)));
            let force = reserved_word(path, &["left", "lt", "right", "rt", "all"])
                || path.to_ascii_lowercase().starts_with("path:");
            parts.push(arg(path, force)?);
        }
        Command::Criteria(criteria) => parts.extend(criteria_parts(criteria)),
        Command::Delete { recycle_bin, side } => {
            if let Some(value) = recycle_bin {
                parts.push(format!("recyclebin={}", if *value { "yes" } else { "no" }));
            }
            parts.push(side_word(*side).to_string());
        }
        Command::Filter(clauses) => {
            if clauses
                .iter()
                .filter(|clause| matches!(clause, FilterClause::Masks(_)))
                .count()
                > 1
            {
                return Err(EncodeError::Unrepresentable {
                    detail: "filter accepts one mask list; join masks with semicolons",
                });
            }
            for clause in clauses {
                parts.push(filter_part(clause)?);
            }
        }
        Command::Load(spec) => match spec {
            LoadSpec::Default => parts.push("<default>".to_string()),
            LoadSpec::Paths {
                create,
                left,
                right,
            } => {
                if let Some(create) = create {
                    parts.push(format!("create:{}", side_word(*create)));
                }
                parts.push(arg(left, reserved_word(left, &["<default>"]))?);
                if let Some(right) = right {
                    parts.push(arg(right, false)?);
                }
            }
        },
        Command::Log(spec) => parts.extend(log_parts(spec)?),
        Command::Option(spec) => parts.push(match spec {
            OptionSpec::StopOnError => "stop-on-error".to_string(),
            OptionSpec::Confirm(mode) => format!(
                "confirm:{}",
                match mode {
                    ConfirmMode::Prompt => "prompt",
                    ConfirmMode::YesToAll => "yes-to-all",
                    ConfirmMode::NoToAll => "no-to-all",
                }
            ),
        }),
        Command::Rename(spec) => match spec {
            RenameSpec::Mask(mask) => {
                parts.push(arg(mask, reserved_word(mask, &["regexpr"]))?);
            }
            RenameSpec::Regex { find, replace } => {
                parts.push("regexpr".to_string());
                parts.push(arg(find, false)?);
                parts.push(arg(replace, false)?);
            }
        },
        Command::Report { kind, spec } => parts.extend(report_parts(*kind, spec)?),
        Command::Select(masks) => {
            for mask in masks {
                parts.push(select_word(*mask));
            }
        }
        Command::Snapshot(spec) => parts.extend(snapshot_parts(spec)?),
        Command::Sync(spec) => parts.extend(sync_parts(*spec)),
        Command::Touch(spec) => parts.push(touch_part(spec)?),
    }
    Ok(parts.join(" "))
}

fn reserved_word(value: &str, words: &[&str]) -> bool {
    words.iter().any(|word| value.eq_ignore_ascii_case(word))
}

/// Write one argument, adding quotation marks where the reader needs them.
fn arg(value: &str, force: bool) -> Result<String, EncodeError> {
    if value
        .chars()
        .any(|c| c == '"' || c == '\n' || c == '\r' || c == '\0')
    {
        return Err(EncodeError::Unwritable {
            value: value.to_string(),
        });
    }
    let needs = force
        || value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '#' || c == '&');
    Ok(if needs {
        format!("\"{value}\"")
    } else {
        value.to_string()
    })
}

/// Write `key:value`, quoting the value alone so the key stays readable.
fn keyed_arg(key: &str, value: &str, force: bool) -> Result<String, EncodeError> {
    Ok(format!("{key}:{}", arg(value, force)?))
}

fn side_word(side: SideArg) -> &'static str {
    match side {
        SideArg::Left => "left",
        SideArg::Right => "right",
        SideArg::All => "all",
    }
}

fn direction_word(direction: Direction) -> &'static str {
    match direction {
        Direction::LeftToRight => "left->right",
        Direction::RightToLeft => "right->left",
    }
}

fn path_option_word(option: PathOption) -> &'static str {
    match option {
        PathOption::Relative => "relative",
        PathOption::Base => "base",
        PathOption::None => "none",
    }
}

fn attrib_group(group: AttribChange) -> String {
    format!(
        "{}{}",
        if group.set { '+' } else { '-' },
        group.attrs.letters()
    )
}

fn criteria_parts(criteria: &Criteria) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(attrib) = criteria.attrib {
        parts.push(format!("attrib:{}", attrib.letters()));
    }
    if criteria.version {
        parts.push("version".to_string());
    }
    if let Some(stamp) = criteria.timestamp {
        let mut text = "timestamp".to_string();
        if stamp.tolerance_seconds.is_some() || stamp.ignore_dst {
            text.push(':');
            if let Some(seconds) = stamp.tolerance_seconds {
                let _ = write!(text, "{seconds}sec");
            }
            if stamp.ignore_dst {
                text.push_str(";IgnoreDST");
            }
        }
        parts.push(text);
    }
    if let Some(content) = criteria.content {
        parts.push(
            match content {
                ContentCriterion::Size => "size",
                ContentCriterion::Crc => "crc",
                ContentCriterion::Binary => "binary",
                ContentCriterion::RulesBased => "rules-based",
            }
            .to_string(),
        );
    }
    if let Some(zone) = criteria.timezone {
        parts.push(match zone {
            TimezoneCriterion::Ignore => "timezone:ignore".to_string(),
            TimezoneCriterion::Offset { side, hours } => format!(
                "timezone:{}{}{}",
                match side {
                    Side::Left => "left",
                    Side::Right => "right",
                },
                if hours < 0 { '-' } else { '+' },
                hours.unsigned_abs()
            ),
        });
    }
    for (flag, word) in [
        (criteria.follow_symlinks, "follow-symlinks"),
        (criteria.ignore_unimportant, "ignore-unimportant"),
        (criteria.owner, "owner"),
        (criteria.group, "group"),
        (criteria.permissions, "permissions"),
    ] {
        if flag {
            parts.push(word.to_string());
        }
    }
    parts
}

fn filter_part(clause: &FilterClause) -> Result<String, EncodeError> {
    Ok(match clause {
        FilterClause::Masks(masks) => {
            let force = reserved_word(masks, &["exclude-protected", "include-protected"])
                || masks.split_once(':').is_some_and(|(head, _)| {
                    reserved_word(head, &["cutoff", "size", "attrib", "unixtype"])
                });
            arg(masks, force)?
        }
        FilterClause::Cutoff(None) => "cutoff:none".to_string(),
        FilterClause::Cutoff(Some(spec)) => cutoff_part(spec)?,
        FilterClause::Size(None) => "size:none".to_string(),
        FilterClause::Size(Some(spec)) => size_part(*spec),
        FilterClause::Attrib(None) => "attrib:none".to_string(),
        FilterClause::Attrib(Some(change)) => format!(
            "attrib:{}{}",
            if change.include { '+' } else { '-' },
            change.attrs.letters()
        ),
        FilterClause::UnixType(None) => "unixtype:none".to_string(),
        FilterClause::UnixType(Some(change)) => format!(
            "unixtype:{}{}",
            if change.include { '+' } else { '-' },
            change.kinds.letters()
        ),
        FilterClause::ExcludeProtected => "exclude-protected".to_string(),
        FilterClause::IncludeProtected => "include-protected".to_string(),
    })
}

fn cutoff_part(spec: &CutoffSpec) -> Result<String, EncodeError> {
    let comparator = if spec.newer { '>' } else { '<' };
    match &spec.value {
        CutoffValue::Days(days) => Ok(format!("cutoff:{comparator}{days}days")),
        CutoffValue::Timestamp(text) => {
            let lowered = text.to_ascii_lowercase();
            if lowered == "none" || lowered.ends_with("days") || text.starts_with(['<', '>']) {
                return Err(EncodeError::Unwritable {
                    value: text.clone(),
                });
            }
            keyed_arg("cutoff", &format!("{comparator}{text}"), false)
        }
    }
}

fn size_part(spec: SizeSpec) -> String {
    format!(
        "size:{}{}{}",
        if spec.larger { '>' } else { '<' },
        spec.value,
        spec.unit.suffix()
    )
}

fn log_parts(spec: &LogSpec) -> Result<Vec<String>, EncodeError> {
    let mut parts = Vec::new();
    if let Some(level) = spec.level {
        parts.push(
            match level {
                LogLevel::None => "none",
                LogLevel::Normal => "normal",
                LogLevel::Verbose => "verbose",
            }
            .to_string(),
        );
    }
    if let Some(target) = &spec.target {
        let force = reserved_word(&target.file, &["none", "normal", "verbose"])
            || target
                .file
                .split_once(':')
                .is_some_and(|(head, _)| head.eq_ignore_ascii_case("append"));
        if target.append {
            parts.push(keyed_arg("append", &target.file, force)?);
        } else {
            parts.push(arg(&target.file, force)?);
        }
    }
    Ok(parts)
}

fn select_word(mask: SelectMask) -> String {
    match mask {
        SelectMask::EmptyFolders => "empty.folders".to_string(),
        SelectMask::Mask { side, result, kind } => format!(
            "{}.{}.{}",
            side_word(side),
            match result {
                SelectResult::Exact => "exact",
                SelectResult::Diff => "diff",
                SelectResult::Newer => "newer",
                SelectResult::Older => "older",
                SelectResult::Orphan => "orphan",
                SelectResult::All => "all",
            },
            match kind {
                SelectKind::Files => "files",
                SelectKind::Folders => "folders",
                SelectKind::All => "all",
            }
        ),
    }
}

fn snapshot_parts(spec: &SnapshotSpec) -> Result<Vec<String>, EncodeError> {
    let mut parts = Vec::new();
    for (flag, word) in [
        (spec.save_crc, "save-crc"),
        (spec.save_version, "save-version"),
        (spec.expand_archives, "expand-archives"),
        (spec.follow_symlinks, "follow-symlinks"),
        (spec.include_empty, "include-empty"),
        (spec.no_filters, "no-filters"),
    ] {
        if flag {
            parts.push(word.to_string());
        }
    }
    match &spec.source {
        SnapshotSource::Left => parts.push("left".to_string()),
        SnapshotSource::Right => parts.push("right".to_string()),
        SnapshotSource::Path(path) => parts.push(keyed_arg("path", path, false)?),
    }
    if let Some(output) = &spec.output {
        parts.push(keyed_arg("output", output, false)?);
    }
    Ok(parts)
}

fn sync_parts(spec: SyncSpec) -> Vec<String> {
    let mut parts = Vec::new();
    if spec.visible {
        parts.push("visible".to_string());
    }
    if spec.create_empty {
        parts.push("create-empty".to_string());
    }
    parts.push(format!(
        "{}:{}",
        match spec.mode {
            SyncMode::Update => "update",
            SyncMode::Mirror => "mirror",
        },
        match spec.direction {
            SyncDirection::LeftToRight => "left->right",
            SyncDirection::RightToLeft => "right->left",
            SyncDirection::All => "all",
        }
    ));
    parts
}

fn touch_part(spec: &TouchSpec) -> Result<String, EncodeError> {
    Ok(match spec {
        TouchSpec::Copy(direction) => direction_word(*direction).to_string(),
        TouchSpec::Set { side, value } => match value {
            TouchValue::Now => format!("{}:now", side_word(*side)),
            TouchValue::Timestamp(text) => {
                if text.eq_ignore_ascii_case("now") || text.is_empty() {
                    return Err(EncodeError::Unwritable {
                        value: text.clone(),
                    });
                }
                keyed_arg(side_word(*side), text, false)?
            }
        },
    })
}

fn report_parts(kind: ReportKind, spec: &ReportSpec) -> Result<Vec<String>, EncodeError> {
    let _ = kind;
    let mut parts = vec![format!("layout:{}", spec.layout.word())];
    if !spec.options.is_empty() {
        let words: Vec<&str> = spec.options.iter().map(|o| o.word()).collect();
        parts.push(format!("options:{}", words.join(",")));
    }
    if let Some(title) = &spec.title {
        parts.push(keyed_arg("title", title, false)?);
    }
    parts.push(match &spec.output_to {
        OutputTo::Printer => "output-to:printer".to_string(),
        OutputTo::Clipboard => "output-to:clipboard".to_string(),
        OutputTo::File(path) => keyed_arg(
            "output-to",
            path,
            reserved_word(path, &["printer", "clipboard"]),
        )?,
    });
    if !spec.output_options.is_empty() {
        let mut words: Vec<String> = Vec::new();
        for option in &spec.output_options {
            match option {
                OutputOption::HtmlCustom(sheet) => {
                    if sheet.contains(',') || sheet.contains(' ') || sheet.is_empty() {
                        return Err(EncodeError::Unwritable {
                            value: sheet.clone(),
                        });
                    }
                    words.push("html-custom".to_string());
                    words.push(sheet.clone());
                }
                other => words.push(output_option_word(other).to_string()),
            }
        }
        parts.push(format!("output-options:{}", words.join(",")));
    }
    match &spec.comparison {
        None => {}
        Some(Comparison::Session(name)) => parts.push(arg(name, true)?),
        Some(Comparison::Files(left, right)) => {
            parts.push(arg(left, true)?);
            parts.push(arg(right, true)?);
        }
    }
    Ok(parts)
}

fn output_option_word(option: &OutputOption) -> &'static str {
    match option {
        OutputOption::PrintColor => "print-color",
        OutputOption::PrintMono => "print-mono",
        OutputOption::PrintPortrait => "print-portrait",
        OutputOption::PrintLandscape => "print-landscape",
        OutputOption::WrapNone => "wrap-none",
        OutputOption::WrapCharacter => "wrap-character",
        OutputOption::WrapWord => "wrap-word",
        OutputOption::HtmlColor => "html-color",
        OutputOption::HtmlMono => "html-mono",
        OutputOption::HtmlCustom(_) => "html-custom",
    }
}
