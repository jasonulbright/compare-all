//! Syntax rules and the argument forms of every command.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use ca_script::ast::{
    AttrSet, AttribChange, Command, CompareType, Comparison, ConfirmMode, ContentCriterion,
    CutoffValue, Direction, FilterClause, LoadSpec, LogLevel, LogSpec, LogTarget, OptionSpec,
    OutputOption, OutputTo, PathOption, PathsArg, RenameSpec, ReportKind, ReportLayout,
    ReportOption, SelectKind, SelectMask, SelectResult, Side, SideArg, SizeUnit, SnapshotSource,
    SyncDirection, SyncMode, TimezoneCriterion, TouchSpec, TouchValue,
};
use ca_script::{parse, Substitution};

fn one(source: &str) -> Command {
    let script = parse(source).unwrap_or_else(|error| panic!("{source}: {error}"));
    assert_eq!(script.statements.len(), 1, "{source}");
    script.statements[0].command.clone()
}

fn fails(source: &str) -> ca_script::ParseError {
    match parse(source) {
        Ok(_) => panic!("{source} should not parse"),
        Err(error) => error,
    }
}

// -- syntax ------------------------------------------------------------------

#[test]
fn blank_lines_and_comments_are_ignored() {
    let script = parse("\n# a note\n  \nbeep # trailing\n").expect("parses");
    assert_eq!(script.commands(), vec![Command::Beep]);
}

#[test]
fn a_hash_inside_quotes_is_not_a_comment() {
    assert_eq!(
        one("expand \"a#b\""),
        Command::Expand(PathsArg::Paths(vec!["a#b".into()]))
    );
}

#[test]
fn a_trailing_ampersand_joins_the_next_line() {
    let command = one("folder-report layout:summary &\n  output-to:\"out.txt\"\n");
    match command {
        Command::Report { kind, spec } => {
            assert_eq!(kind, ReportKind::Folder);
            assert_eq!(spec.layout, ReportLayout::Summary);
            assert_eq!(spec.output_to, OutputTo::File("out.txt".into()));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn quotes_keep_spaces_in_one_argument() {
    assert_eq!(
        one("expand \"My Folder\""),
        Command::Expand(PathsArg::Paths(vec!["My Folder".into()]))
    );
}

#[test]
fn a_quoted_argument_is_never_a_keyword() {
    assert_eq!(
        one("expand \"all\""),
        Command::Expand(PathsArg::Paths(vec!["all".into()]))
    );
}

#[test]
fn command_words_and_keywords_ignore_letter_case() {
    assert_eq!(
        one("COPY Left->Right"),
        Command::Copy(Direction::LeftToRight)
    );
}

#[test]
fn lt_and_rt_stand_for_left_and_right() {
    assert_eq!(one("copy lt->rt"), Command::Copy(Direction::LeftToRight));
    assert_eq!(
        one("delete rt"),
        Command::Delete {
            recycle_bin: None,
            side: SideArg::Right
        }
    );
}

#[test]
fn an_unclosed_quote_names_its_line_and_column() {
    let error = fails("beep\nexpand \"open\n");
    assert_eq!(error.line, 2);
    assert!(error.column > 0);
    assert!(error.message.contains("not closed"), "{}", error.message);
}

#[test]
fn an_unknown_command_names_the_word() {
    let error = fails("wobble");
    assert_eq!((error.line, error.column), (1, 1));
    assert!(error.message.contains("wobble"));
}

#[test]
fn a_stray_argument_is_reported_at_its_column() {
    let error = fails("beep loudly");
    assert_eq!((error.line, error.column), (1, 6));
}

// -- substitution ------------------------------------------------------------

#[test]
fn numbered_arguments_are_replaced() {
    let subst = Substitution::fixed(
        vec!["My Session".into(), "second".into()],
        BTreeMap::new(),
        0,
    );
    let script = ca_script::parse_with("expand \"%1\"", &subst).expect("parses");
    assert_eq!(
        script.commands(),
        vec![Command::Expand(PathsArg::Paths(vec!["My Session".into()]))]
    );
}

#[test]
fn environment_names_and_clock_values_are_replaced() {
    let mut env = BTreeMap::new();
    env.insert("TMP".to_string(), "/tmp/work".to_string());
    let subst = Substitution::fixed(Vec::new(), env, 0);
    let script =
        ca_script::parse_with("expand %TMP% %date% %fn_time% %MISSING%", &subst).expect("parses");
    assert_eq!(
        script.commands(),
        vec![Command::Expand(PathsArg::Paths(vec![
            "/tmp/work".into(),
            "1970-01-01".into(),
            "00-00-00".into(),
            "%MISSING%".into(),
        ]))]
    );
}

// -- commands ----------------------------------------------------------------

#[test]
fn attrib_takes_signed_letter_groups() {
    assert_eq!(
        one("attrib +sh -a"),
        Command::Attrib(vec![
            AttribChange {
                set: true,
                attrs: AttrSet {
                    system: true,
                    hidden: true,
                    ..AttrSet::default()
                }
            },
            AttribChange {
                set: false,
                attrs: AttrSet {
                    archive: true,
                    ..AttrSet::default()
                }
            },
        ])
    );
    assert!(parse("attrib +q").is_err());
    assert!(parse("attrib").is_err());
}

#[test]
fn beep_takes_no_argument() {
    assert_eq!(one("beep"), Command::Beep);
}

#[test]
fn collapse_and_expand_take_all_or_paths() {
    assert_eq!(one("collapse all"), Command::Collapse(PathsArg::All));
    assert_eq!(
        one("collapse \"My Folder 1\" \"My Folder 2\""),
        Command::Collapse(PathsArg::Paths(vec![
            "My Folder 1".into(),
            "My Folder 2".into()
        ]))
    );
    assert_eq!(one("expand all"), Command::Expand(PathsArg::All));
    assert!(parse("expand").is_err());
    assert!(parse("expand all extra").is_err());
}

#[test]
fn compare_takes_one_optional_type() {
    assert_eq!(one("compare"), Command::Compare(None));
    assert_eq!(one("compare CRC"), Command::Compare(Some(CompareType::Crc)));
    assert_eq!(
        one("compare binary"),
        Command::Compare(Some(CompareType::Binary))
    );
    assert_eq!(
        one("compare rules-based"),
        Command::Compare(Some(CompareType::RulesBased))
    );
    assert!(parse("compare sideways").is_err());
}

#[test]
fn copy_and_move_take_a_direction() {
    assert_eq!(
        one("copy right->left"),
        Command::Copy(Direction::RightToLeft)
    );
    assert_eq!(
        one("move left->right"),
        Command::Move(Direction::LeftToRight)
    );
    assert!(parse("copy left->left").is_err());
    assert!(parse("copy").is_err());
}

#[test]
fn copyto_defaults_to_all_and_path_none() {
    assert_eq!(
        one("copyto \"C:\\Target\""),
        Command::CopyTo {
            side: SideArg::All,
            path_option: PathOption::None,
            path: "C:\\Target".into()
        }
    );
    assert_eq!(
        one("moveto left path:base /target"),
        Command::MoveTo {
            side: SideArg::Left,
            path_option: PathOption::Base,
            path: "/target".into()
        }
    );
    assert_eq!(
        one("copyto right path:relative /t"),
        Command::CopyTo {
            side: SideArg::Right,
            path_option: PathOption::Relative,
            path: "/t".into()
        }
    );
    assert!(parse("copyto path:sideways /t").is_err());
    assert!(parse("copyto left").is_err());
}

#[test]
fn criteria_reads_every_keyword() {
    let command = one("criteria attrib:sh timestamp:2sec;IgnoreDST rules-based timezone:left+6");
    match command {
        Command::Criteria(criteria) => {
            assert_eq!(
                criteria.attrib,
                Some(AttrSet {
                    system: true,
                    hidden: true,
                    ..AttrSet::default()
                })
            );
            let stamp = criteria.timestamp.expect("timestamp");
            assert_eq!(stamp.tolerance_seconds, Some(2));
            assert!(stamp.ignore_dst);
            assert_eq!(criteria.content, Some(ContentCriterion::RulesBased));
            assert_eq!(
                criteria.timezone,
                Some(TimezoneCriterion::Offset {
                    side: Side::Left,
                    hours: 6
                })
            );
        }
        other => panic!("{other:?}"),
    }
    let command = one("criteria version timestamp size follow-symlinks ignore-unimportant owner group permissions timezone:ignore");
    match command {
        Command::Criteria(criteria) => {
            assert!(criteria.version);
            assert!(criteria.follow_symlinks);
            assert!(criteria.ignore_unimportant);
            assert!(criteria.owner);
            assert!(criteria.group);
            assert!(criteria.permissions);
            assert_eq!(criteria.content, Some(ContentCriterion::Size));
            assert_eq!(criteria.timezone, Some(TimezoneCriterion::Ignore));
        }
        other => panic!("{other:?}"),
    }
    assert!(parse("criteria timezone:left+13").is_err());
    assert!(parse("criteria wobble").is_err());
}

#[test]
fn delete_takes_a_recycle_bin_answer_and_a_side() {
    assert_eq!(
        one("delete recyclebin=no all"),
        Command::Delete {
            recycle_bin: Some(false),
            side: SideArg::All
        }
    );
    assert_eq!(
        one("delete recyclebin=yes left"),
        Command::Delete {
            recycle_bin: Some(true),
            side: SideArg::Left
        }
    );
    assert!(parse("delete").is_err());
    assert!(parse("delete recyclebin=maybe all").is_err());
}

#[test]
fn filter_reads_masks_and_every_clause() {
    assert_eq!(
        one("filter \"*.pas;*.dpr\""),
        Command::Filter(vec![FilterClause::Masks("*.pas;*.dpr".into())])
    );
    match one("filter cutoff:<7days") {
        Command::Filter(clauses) => match &clauses[0] {
            FilterClause::Cutoff(Some(spec)) => {
                assert!(!spec.newer);
                assert_eq!(spec.value, CutoffValue::Days(7));
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    match one("filter cutoff:>\"2012-12-31 12:00\"") {
        Command::Filter(clauses) => match &clauses[0] {
            FilterClause::Cutoff(Some(spec)) => {
                assert!(spec.newer);
                assert_eq!(
                    spec.value,
                    CutoffValue::Timestamp("2012-12-31 12:00".into())
                );
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    assert_eq!(
        one("filter cutoff:none"),
        Command::Filter(vec![FilterClause::Cutoff(None)])
    );
    match one("filter size:>10MB") {
        Command::Filter(clauses) => match &clauses[0] {
            FilterClause::Size(Some(spec)) => {
                assert!(spec.larger);
                assert_eq!(spec.value, 10);
                assert_eq!(spec.unit, SizeUnit::Megabytes);
                assert_eq!(spec.bytes(), 10 * 1024 * 1024);
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    assert!(parse("filter size:10MB").is_err());
    match one("filter attrib:-sh") {
        Command::Filter(clauses) => match &clauses[0] {
            FilterClause::Attrib(Some(change)) => {
                assert!(!change.include);
                assert!(change.attrs.system && change.attrs.hidden);
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    assert_eq!(
        one("filter exclude-protected"),
        Command::Filter(vec![FilterClause::ExcludeProtected])
    );
    assert_eq!(
        one("filter include-protected"),
        Command::Filter(vec![FilterClause::IncludeProtected])
    );
    match one("filter unixtype:+rl") {
        Command::Filter(clauses) => match &clauses[0] {
            FilterClause::UnixType(Some(change)) => {
                assert!(change.include);
                assert!(change.kinds.regular && change.kinds.link);
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    match one("filter \"*.txt;-My Folder\\\" attrib:+r") {
        Command::Filter(clauses) => {
            assert_eq!(clauses.len(), 2);
            assert_eq!(clauses[0], FilterClause::Masks("*.txt;-My Folder\\".into()));
        }
        other => panic!("{other:?}"),
    }
    assert!(parse("filter -*.keep -*.bak").is_err());
}

#[test]
fn load_takes_a_name_a_pair_or_the_default() {
    assert_eq!(one("load <default>"), Command::Load(LoadSpec::Default));
    assert_eq!(
        one("load \"My Session\""),
        Command::Load(LoadSpec::Paths {
            create: None,
            left: "My Session".into(),
            right: None
        })
    );
    assert_eq!(
        one("load create:all \"C:\\L\" \"X:\\R\""),
        Command::Load(LoadSpec::Paths {
            create: Some(SideArg::All),
            left: "C:\\L".into(),
            right: Some("X:\\R".into())
        })
    );
    assert!(parse("load a b c").is_err());
    assert!(parse("load").is_err());
}

#[test]
fn log_takes_a_level_and_a_file() {
    assert_eq!(
        one("log verbose"),
        Command::Log(LogSpec {
            level: Some(LogLevel::Verbose),
            target: None
        })
    );
    assert_eq!(
        one("log normal append:\"My Log.txt\""),
        Command::Log(LogSpec {
            level: Some(LogLevel::Normal),
            target: Some(LogTarget {
                append: true,
                file: "My Log.txt".into()
            })
        })
    );
    assert_eq!(
        one("log none"),
        Command::Log(LogSpec {
            level: Some(LogLevel::None),
            target: None
        })
    );
    assert!(parse("log").is_err());
}

#[test]
fn option_takes_the_two_documented_settings() {
    assert_eq!(
        one("option stop-on-error"),
        Command::Option(OptionSpec::StopOnError)
    );
    for (text, mode) in [
        ("prompt", ConfirmMode::Prompt),
        ("yes-to-all", ConfirmMode::YesToAll),
        ("no-to-all", ConfirmMode::NoToAll),
    ] {
        assert_eq!(
            one(&format!("option confirm:{text}")),
            Command::Option(OptionSpec::Confirm(mode))
        );
    }
    assert!(parse("option confirm:maybe").is_err());
}

#[test]
fn rename_takes_a_mask_or_an_expression() {
    assert_eq!(
        one("rename *.bak"),
        Command::Rename(RenameSpec::Mask("*.bak".into()))
    );
    assert_eq!(
        one("rename regexpr (...)(...)\\.txt $2$1.txt"),
        Command::Rename(RenameSpec::Regex {
            find: "(...)(...)\\.txt".into(),
            replace: "$2$1.txt".into()
        })
    );
    assert!(parse("rename regexpr only").is_err());
}

#[test]
fn select_reads_every_mask_component() {
    let mask = |side, result, kind| SelectMask::Mask { side, result, kind };
    assert_eq!(
        one("select all"),
        Command::Select(vec![mask(SideArg::All, SelectResult::All, SelectKind::All)])
    );
    assert_eq!(
        one("select left"),
        Command::Select(vec![mask(
            SideArg::Left,
            SelectResult::All,
            SelectKind::All
        )])
    );
    assert_eq!(
        one("select exact"),
        Command::Select(vec![mask(
            SideArg::All,
            SelectResult::Exact,
            SelectKind::All
        )])
    );
    assert_eq!(
        one("select files"),
        Command::Select(vec![mask(
            SideArg::All,
            SelectResult::All,
            SelectKind::Files
        )])
    );
    assert_eq!(
        one("select right.diff"),
        Command::Select(vec![mask(
            SideArg::Right,
            SelectResult::Diff,
            SelectKind::All
        )])
    );
    assert_eq!(
        one("select left.folders"),
        Command::Select(vec![mask(
            SideArg::Left,
            SelectResult::All,
            SelectKind::Folders
        )])
    );
    assert_eq!(
        one("select newer.files right.older.files"),
        Command::Select(vec![
            mask(SideArg::All, SelectResult::Newer, SelectKind::Files),
            mask(SideArg::Right, SelectResult::Older, SelectKind::Files),
        ])
    );
    assert_eq!(
        one("select orphan"),
        Command::Select(vec![mask(
            SideArg::All,
            SelectResult::Orphan,
            SelectKind::All
        )])
    );
    assert_eq!(
        one("select empty.folders"),
        Command::Select(vec![SelectMask::EmptyFolders])
    );
    assert!(parse("select files.left").is_err());
    assert!(parse("select").is_err());
}

#[test]
fn snapshot_reads_every_flag() {
    match one(
        "snapshot save-crc save-version expand-archives follow-symlinks include-empty no-filters path:C:\\ output:D:\\",
    ) {
        Command::Snapshot(spec) => {
            assert!(spec.save_crc);
            assert!(spec.save_version);
            assert!(spec.expand_archives);
            assert!(spec.follow_symlinks);
            assert!(spec.include_empty);
            assert!(spec.no_filters);
            assert_eq!(spec.source, SnapshotSource::Path("C:\\".into()));
            assert_eq!(spec.output.as_deref(), Some("D:\\"));
        }
        other => panic!("{other:?}"),
    }
    match one("snapshot left output:\"My Snapshot.cass\"") {
        Command::Snapshot(spec) => {
            assert_eq!(spec.source, SnapshotSource::Left);
            assert_eq!(spec.output.as_deref(), Some("My Snapshot.cass"));
        }
        other => panic!("{other:?}"),
    }
    assert!(parse("snapshot save-crc").is_err());
}

#[test]
fn sync_reads_its_modes_and_directions() {
    match one("sync create-empty mirror:left->right") {
        Command::Sync(spec) => {
            assert!(spec.create_empty);
            assert!(!spec.visible);
            assert_eq!(spec.mode, SyncMode::Mirror);
            assert_eq!(spec.direction, SyncDirection::LeftToRight);
        }
        other => panic!("{other:?}"),
    }
    match one("sync visible update:all") {
        Command::Sync(spec) => {
            assert!(spec.visible);
            assert_eq!(spec.mode, SyncMode::Update);
            assert_eq!(spec.direction, SyncDirection::All);
        }
        other => panic!("{other:?}"),
    }
    assert!(parse("sync mirror:all").is_err());
    assert!(parse("sync").is_err());
}

#[test]
fn touch_takes_a_direction_or_a_value() {
    assert_eq!(
        one("touch left->right"),
        Command::Touch(TouchSpec::Copy(Direction::LeftToRight))
    );
    assert_eq!(
        one("touch all:now"),
        Command::Touch(TouchSpec::Set {
            side: SideArg::All,
            value: TouchValue::Now
        })
    );
    assert_eq!(
        one("touch \"left:2020-01-02 03:04:05\""),
        Command::Touch(TouchSpec::Set {
            side: SideArg::Left,
            value: TouchValue::Timestamp("2020-01-02 03:04:05".into())
        })
    );
    assert!(parse("touch sideways").is_err());
}

#[test]
fn every_report_command_is_known() {
    for word in [
        "file-report",
        "folder-report",
        "hex-report",
        "text-report",
        "data-report",
        "table-report",
        "media-report",
        "picture-report",
        "registry-report",
        "version-report",
    ] {
        let source = format!("{word} layout:summary output-to:out.txt");
        let command = one(&source);
        assert!(matches!(command, Command::Report { .. }), "{word}");
    }
}

#[test]
fn a_report_layout_is_checked_against_its_command() {
    assert!(parse("text-report layout:patch output-to:o.txt").is_ok());
    assert!(parse("folder-report layout:patch output-to:o.txt").is_err());
    assert!(parse("hex-report layout:interleaved output-to:o.txt").is_ok());
    assert!(parse("picture-report layout:interleaved output-to:o.txt").is_err());
}

#[test]
fn a_report_option_is_checked_against_its_command() {
    assert!(parse("folder-report layout:xml options:column-crc output-to:o.xml").is_ok());
    assert!(parse("text-report layout:summary options:column-crc output-to:o.txt").is_err());
    assert!(parse("text-report layout:patch options:patch-unified output-to:o.txt").is_ok());
}

#[test]
fn report_arguments_are_read_in_full() {
    let command = one(
        "text-report layout:interleaved options:ignore-unimportant,display-context & \n title:\"My Title\" output-to:\"My Report.txt\" output-options:html-color,wrap-word left.txt right.txt",
    );
    match command {
        Command::Report { kind, spec } => {
            assert_eq!(kind, ReportKind::Text);
            assert_eq!(spec.layout, ReportLayout::Interleaved);
            assert_eq!(
                spec.options,
                vec![
                    ReportOption::IgnoreUnimportant,
                    ReportOption::DisplayContext
                ]
            );
            assert_eq!(spec.title.as_deref(), Some("My Title"));
            assert_eq!(spec.output_to, OutputTo::File("My Report.txt".into()));
            assert_eq!(
                spec.output_options,
                vec![OutputOption::HtmlColor, OutputOption::WrapWord]
            );
            assert_eq!(
                spec.comparison,
                Some(Comparison::Files("left.txt".into(), "right.txt".into()))
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_report_needs_a_layout_and_a_target() {
    assert!(parse("text-report output-to:o.txt").is_err());
    assert!(parse("text-report layout:summary").is_err());
    assert!(parse("folder-report layout:summary output-to:printer session").is_err());
}

#[test]
fn the_print_and_clipboard_targets_parse() {
    match one(
        "file-report layout:summary output-to:printer output-options:print-color,print-landscape",
    ) {
        Command::Report { spec, .. } => {
            assert_eq!(spec.output_to, OutputTo::Printer);
            assert_eq!(
                spec.output_options,
                vec![OutputOption::PrintColor, OutputOption::PrintLandscape]
            );
        }
        other => panic!("{other:?}"),
    }
    match one("file-report layout:summary output-to:clipboard output-options:html-custom,sheet.css")
    {
        Command::Report { spec, .. } => {
            assert_eq!(spec.output_to, OutputTo::Clipboard);
            assert_eq!(
                spec.output_options,
                vec![OutputOption::HtmlCustom("sheet.css".into())]
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn every_command_carries_its_position() {
    let script = parse("beep\n\nbeep\n").expect("parses");
    assert_eq!(script.statements[0].span.line, 1);
    assert_eq!(script.statements[1].span.line, 3);
    assert_eq!(script.statements[1].span.column, 1);
}

#[test]
fn a_filter_with_two_mask_arguments_is_refused() {
    let error = parse("filter \"*.keep\" \"*.tmp\"").expect_err("two mask arguments");
    assert!(error.to_string().contains("one mask argument"), "{error}");
    assert!(parse("filter \"*.keep;*.tmp\"").is_ok());
}
