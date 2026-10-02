//! A command written back out as text parses to the same command.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_script::ast::{
    AttrSet, AttribChange, Command, CompareType, Comparison, ConfirmMode, ContentCriterion,
    Criteria, CutoffSpec, CutoffValue, Direction, FilterAttrSet, FilterAttribChange, FilterClause,
    LoadSpec, LogLevel, LogSpec, LogTarget, OptionSpec, OutputOption, OutputTo, PathOption,
    PathsArg, RenameSpec, ReportKind, ReportOption, ReportSpec, SelectKind, SelectMask,
    SelectResult, Side, SideArg, SizeSpec, SizeUnit, SnapshotSource, SnapshotSpec, SyncDirection,
    SyncMode, SyncSpec, TimestampCriterion, TimezoneCriterion, TouchSpec, TouchValue,
    UnixTypeChange, UnixTypeSet,
};
use ca_script::parse::{layouts_of, option_allowed};
use proptest::prelude::*;

/// Characters an argument may hold in these cases. The quotation mark, the
/// line break and the NUL byte are left out because the syntax cannot write
/// them.
const VALUE: &str = "[a-zA-Z0-9_ .*?;#&+-]{1,10}";

fn value() -> impl Strategy<Value = String> {
    VALUE.prop_map(|text| text)
}

fn stamp() -> impl Strategy<Value = String> {
    "[0-9]{4}-[0-9]{2}-[0-9]{2}".prop_map(|text| text)
}

fn side_arg() -> impl Strategy<Value = SideArg> {
    prop_oneof![
        Just(SideArg::Left),
        Just(SideArg::Right),
        Just(SideArg::All)
    ]
}

fn direction() -> impl Strategy<Value = Direction> {
    prop_oneof![Just(Direction::LeftToRight), Just(Direction::RightToLeft)]
}

fn attr_set() -> impl Strategy<Value = AttrSet> {
    (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>())
        .prop_filter("at least one letter", |(a, s, h, r)| *a || *s || *h || *r)
        .prop_map(|(archive, system, hidden, read_only)| AttrSet {
            archive,
            system,
            hidden,
            read_only,
        })
}

fn filter_attr_set() -> impl Strategy<Value = FilterAttrSet> {
    proptest::collection::vec(any::<bool>(), 13)
        .prop_filter("at least one letter", |flags| flags.iter().any(|f| *f))
        .prop_map(|flags| FilterAttrSet {
            archive: flags[0],
            compressed: flags[1],
            encrypted: flags[2],
            hidden: flags[3],
            not_indexed: flags[4],
            link: flags[5],
            offline: flags[6],
            pinned: flags[7],
            read_only: flags[8],
            system: flags[9],
            temporary: flags[10],
            unpinned: flags[11],
            sparse: flags[12],
        })
}

fn unix_set() -> impl Strategy<Value = UnixTypeSet> {
    proptest::collection::vec(any::<bool>(), 6)
        .prop_filter("at least one letter", |flags| flags.iter().any(|f| *f))
        .prop_map(|flags| UnixTypeSet {
            block: flags[0],
            character: flags[1],
            link: flags[2],
            fifo: flags[3],
            regular: flags[4],
            socket: flags[5],
        })
}

fn criteria() -> impl Strategy<Value = Criteria> {
    (
        proptest::option::of(attr_set()),
        any::<bool>(),
        proptest::option::of((proptest::option::of(0u32..3600), any::<bool>())),
        proptest::option::of(prop_oneof![
            Just(ContentCriterion::Size),
            Just(ContentCriterion::Crc),
            Just(ContentCriterion::Binary),
            Just(ContentCriterion::RulesBased),
        ]),
        proptest::option::of(prop_oneof![
            Just(TimezoneCriterion::Ignore),
            (
                prop_oneof![Just(Side::Left), Just(Side::Right)],
                -12i8..=12i8
            )
                .prop_map(|(side, hours)| TimezoneCriterion::Offset { side, hours }),
        ]),
        proptest::collection::vec(any::<bool>(), 5),
    )
        .prop_map(
            |(attrib, version, timestamp, content, timezone, flags)| Criteria {
                attrib,
                version,
                timestamp: timestamp.map(|(tolerance_seconds, ignore_dst)| TimestampCriterion {
                    tolerance_seconds,
                    ignore_dst,
                }),
                content,
                timezone,
                follow_symlinks: flags[0],
                ignore_unimportant: flags[1],
                owner: flags[2],
                group: flags[3],
                permissions: flags[4],
            },
        )
}

fn filter_clause() -> impl Strategy<Value = FilterClause> {
    prop_oneof![
        value().prop_map(FilterClause::Masks),
        Just(FilterClause::Cutoff(None)),
        (any::<bool>(), 0u32..999).prop_map(|(newer, days)| FilterClause::Cutoff(Some(
            CutoffSpec {
                newer,
                value: CutoffValue::Days(days)
            }
        ))),
        (any::<bool>(), stamp()).prop_map(|(newer, text)| FilterClause::Cutoff(Some(CutoffSpec {
            newer,
            value: CutoffValue::Timestamp(text)
        }))),
        Just(FilterClause::Size(None)),
        (
            any::<bool>(),
            0u64..1_000_000,
            prop_oneof![
                Just(SizeUnit::Bytes),
                Just(SizeUnit::Kilobytes),
                Just(SizeUnit::Megabytes),
                Just(SizeUnit::Gigabytes),
                Just(SizeUnit::Terabytes),
            ]
        )
            .prop_map(|(larger, value, unit)| FilterClause::Size(Some(SizeSpec {
                larger,
                value,
                unit
            }))),
        Just(FilterClause::Attrib(None)),
        (any::<bool>(), filter_attr_set()).prop_map(|(include, attrs)| FilterClause::Attrib(Some(
            FilterAttribChange { include, attrs }
        ))),
        Just(FilterClause::UnixType(None)),
        (any::<bool>(), unix_set()).prop_map(|(include, kinds)| FilterClause::UnixType(Some(
            UnixTypeChange { include, kinds }
        ))),
        Just(FilterClause::ExcludeProtected),
        Just(FilterClause::IncludeProtected),
    ]
}

fn select_mask() -> impl Strategy<Value = SelectMask> {
    prop_oneof![
        Just(SelectMask::EmptyFolders),
        (
            side_arg(),
            prop_oneof![
                Just(SelectResult::Exact),
                Just(SelectResult::Diff),
                Just(SelectResult::Newer),
                Just(SelectResult::Older),
                Just(SelectResult::Orphan),
                Just(SelectResult::All),
            ],
            prop_oneof![
                Just(SelectKind::Files),
                Just(SelectKind::Folders),
                Just(SelectKind::All),
            ]
        )
            .prop_map(|(side, result, kind)| SelectMask::Mask { side, result, kind }),
    ]
}

fn report_kind() -> impl Strategy<Value = ReportKind> {
    prop_oneof![
        Just(ReportKind::File),
        Just(ReportKind::Folder),
        Just(ReportKind::Hex),
        Just(ReportKind::Text),
        Just(ReportKind::Data),
        Just(ReportKind::Media),
        Just(ReportKind::Picture),
        Just(ReportKind::Registry),
        Just(ReportKind::Version),
    ]
}

const OPTIONS: &[ReportOption] = &[
    ReportOption::IgnoreUnimportant,
    ReportOption::DisplayAll,
    ReportOption::DisplayMismatches,
    ReportOption::DisplayMatches,
    ReportOption::DisplayContext,
    ReportOption::DisplayOrphans,
    ReportOption::LineNumbers,
    ReportOption::PatchUnified,
    ReportOption::ColumnCrc,
    ReportOption::ColumnSize,
];

fn report_command() -> impl Strategy<Value = Command> {
    (
        report_kind(),
        0usize..OPTIONS.len(),
        proptest::option::of(value()),
        prop_oneof![
            Just(OutputTo::Printer),
            Just(OutputTo::Clipboard),
            value().prop_map(OutputTo::File),
        ],
        proptest::collection::vec(
            prop_oneof![
                Just(OutputOption::PrintColor),
                Just(OutputOption::PrintLandscape),
                Just(OutputOption::WrapWord),
                Just(OutputOption::HtmlColor),
            ],
            0..3,
        ),
        proptest::option::of((value(), value())),
        0usize..6,
    )
        .prop_map(
            |(kind, option_count, title, output_to, output_options, files, layout_pick)| {
                let layouts = layouts_of(kind);
                let layout = layouts[layout_pick % layouts.len()];
                let options: Vec<ReportOption> = OPTIONS
                    .iter()
                    .take(option_count)
                    .copied()
                    .filter(|option| option_allowed(kind, *option))
                    .collect();
                let comparison = if kind == ReportKind::Folder {
                    None
                } else {
                    files.map(|(left, right)| Comparison::Files(left, right))
                };
                Command::Report {
                    kind,
                    spec: Box::new(ReportSpec {
                        layout,
                        options,
                        title,
                        output_to,
                        output_options,
                        comparison,
                    }),
                }
            },
        )
}

#[allow(clippy::too_many_lines)]
fn command() -> impl Strategy<Value = Command> {
    prop_oneof![
        Just(Command::Beep),
        proptest::collection::vec(
            (any::<bool>(), attr_set()).prop_map(|(set, attrs)| AttribChange { set, attrs }),
            1..3
        )
        .prop_map(Command::Attrib),
        prop_oneof![
            Just(PathsArg::All),
            proptest::collection::vec(value(), 1..3).prop_map(PathsArg::Paths),
        ]
        .prop_map(Command::Collapse),
        prop_oneof![
            Just(PathsArg::All),
            proptest::collection::vec(value(), 1..3).prop_map(PathsArg::Paths),
        ]
        .prop_map(Command::Expand),
        proptest::option::of(prop_oneof![
            Just(CompareType::Crc),
            Just(CompareType::Binary),
            Just(CompareType::RulesBased),
        ])
        .prop_map(Command::Compare),
        direction().prop_map(Command::Copy),
        direction().prop_map(Command::Move),
        (
            side_arg(),
            prop_oneof![
                Just(PathOption::Relative),
                Just(PathOption::Base),
                Just(PathOption::None)
            ],
            value()
        )
            .prop_map(|(side, path_option, path)| Command::CopyTo {
                side,
                path_option,
                path
            }),
        (
            side_arg(),
            prop_oneof![
                Just(PathOption::Relative),
                Just(PathOption::Base),
                Just(PathOption::None)
            ],
            value()
        )
            .prop_map(|(side, path_option, path)| Command::MoveTo {
                side,
                path_option,
                path
            }),
        criteria().prop_map(Command::Criteria),
        (proptest::option::of(any::<bool>()), side_arg())
            .prop_map(|(recycle_bin, side)| Command::Delete { recycle_bin, side }),
        proptest::collection::vec(filter_clause(), 1..3)
            .prop_filter("filter has at most one mask list", |clauses| {
                clauses
                    .iter()
                    .filter(|clause| matches!(clause, FilterClause::Masks(_)))
                    .count()
                    <= 1
            })
            .prop_map(Command::Filter),
        prop_oneof![
            Just(Command::Load(LoadSpec::Default)),
            (
                proptest::option::of(side_arg()),
                value(),
                proptest::option::of(value())
            )
                .prop_map(|(create, left, right)| Command::Load(LoadSpec::Paths {
                    create,
                    left,
                    right
                })),
        ],
        (
            proptest::option::of(prop_oneof![
                Just(LogLevel::None),
                Just(LogLevel::Normal),
                Just(LogLevel::Verbose),
            ]),
            proptest::option::of((any::<bool>(), value())),
        )
            .prop_filter("log needs one argument", |(level, target)| level.is_some()
                || target.is_some())
            .prop_map(|(level, target)| Command::Log(LogSpec {
                level,
                target: target.map(|(append, file)| LogTarget { append, file }),
            })),
        prop_oneof![
            Just(Command::Option(OptionSpec::StopOnError)),
            Just(Command::Option(OptionSpec::Confirm(ConfirmMode::Prompt))),
            Just(Command::Option(OptionSpec::Confirm(ConfirmMode::YesToAll))),
            Just(Command::Option(OptionSpec::Confirm(ConfirmMode::NoToAll))),
        ],
        prop_oneof![
            value().prop_map(|mask| Command::Rename(RenameSpec::Mask(mask))),
            (value(), value())
                .prop_map(|(find, replace)| Command::Rename(RenameSpec::Regex { find, replace })),
        ],
        proptest::collection::vec(select_mask(), 1..4).prop_map(Command::Select),
        (
            proptest::collection::vec(any::<bool>(), 5),
            prop_oneof![
                Just(SnapshotSource::Left),
                Just(SnapshotSource::Right),
                value().prop_map(SnapshotSource::Path),
            ],
            proptest::option::of(value()),
        )
            .prop_map(|(flags, source, output)| Command::Snapshot(Box::new(
                SnapshotSpec {
                    save_crc: flags[0],
                    save_version: flags[1],
                    expand_archives: flags[2],
                    follow_symlinks: flags[3],
                    include_empty: flags[4],
                    no_filters: false,
                    source,
                    output,
                }
            ))),
        (
            any::<bool>(),
            any::<bool>(),
            prop_oneof![Just(SyncMode::Update), Just(SyncMode::Mirror)],
            prop_oneof![
                Just(SyncDirection::LeftToRight),
                Just(SyncDirection::RightToLeft),
                Just(SyncDirection::All),
            ],
        )
            .prop_filter("mirror takes one direction", |(_, _, mode, direction)| {
                *mode != SyncMode::Mirror || *direction != SyncDirection::All
            })
            .prop_map(
                |(visible, create_empty, mode, direction)| Command::Sync(SyncSpec {
                    visible,
                    create_empty,
                    mode,
                    direction
                })
            ),
        prop_oneof![
            direction().prop_map(|d| Command::Touch(TouchSpec::Copy(d))),
            side_arg().prop_map(|side| Command::Touch(TouchSpec::Set {
                side,
                value: TouchValue::Now
            })),
            (side_arg(), stamp()).prop_map(|(side, text)| Command::Touch(TouchSpec::Set {
                side,
                value: TouchValue::Timestamp(text)
            })),
        ],
        report_command(),
    ]
}

proptest! {
    #[test]
    fn a_command_survives_a_trip_through_text(command in command()) {
        let written = ca_script::encode(&command).expect("writable");
        let script = ca_script::parse(&written)
            .unwrap_or_else(|error| panic!("{written}: {error}"));
        prop_assert_eq!(script.commands(), vec![command], "{}", written);
    }

    #[test]
    fn a_script_survives_a_trip_through_text(
        commands in proptest::collection::vec(command(), 1..6)
    ) {
        let mut written = String::new();
        for command in &commands {
            written.push_str(&ca_script::encode(command).expect("writable"));
            written.push('\n');
        }
        let script = ca_script::parse(&written)
            .unwrap_or_else(|error| panic!("{written}: {error}"));
        prop_assert_eq!(script.commands(), commands);
    }
}

#[test]
fn the_encoder_refuses_a_filter_with_two_mask_lists() {
    let command = Command::Filter(vec![
        FilterClause::Masks("_".to_owned()),
        FilterClause::Masks(";".to_owned()),
    ]);

    let error = ca_script::encode(&command).unwrap_err();
    assert!(error.to_string().contains("one mask list"), "{error}");
}
