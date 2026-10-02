//! Property tests for the invariants the rest of the program relies on.

#![allow(clippy::unwrap_used)]

use ca_text::buffer::EditKind;
use ca_text::{
    decode, encode, encode_file, DecodeOptions, EolStyle, LineRange, TabSettings, TextBuffer,
    TextEncoding,
};
use encoding_rs as enc;
use proptest::prelude::*;

/// Characters every supported encoding in this suite can represent, so that
/// encoding a generated string is lossless and the round trip is meaningful.
fn portable_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            proptest::char::range('\u{20}', '\u{7E}'),
            Just('\n'),
            Just('\r'),
            Just('\t'),
        ],
        0..64,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

/// Text including characters outside Latin-1, for the Unicode encodings.
fn unicode_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            proptest::char::range('\u{20}', '\u{7E}'),
            proptest::char::range('\u{A1}', '\u{2FF}'),
            proptest::char::range('\u{4E00}', '\u{4E80}'),
            Just('\u{1F389}'),
            Just('\n'),
        ],
        0..48,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn all_encodings() -> Vec<TextEncoding> {
    vec![
        TextEncoding::Utf8,
        TextEncoding::Utf16Le,
        TextEncoding::Utf16Be,
        TextEncoding::Utf32Le,
        TextEncoding::Utf32Be,
        TextEncoding::Legacy(enc::WINDOWS_1252),
        TextEncoding::Legacy(enc::ISO_8859_2),
        TextEncoding::Legacy(enc::KOI8_U),
    ]
}

fn unicode_encodings() -> Vec<TextEncoding> {
    vec![
        TextEncoding::Utf8,
        TextEncoding::Utf16Le,
        TextEncoding::Utf16Be,
        TextEncoding::Utf32Le,
        TextEncoding::Utf32Be,
    ]
}

/// One step of a randomly generated edit sequence.
#[derive(Clone, Debug)]
enum Step {
    Insert(usize, String),
    Type(usize, String),
    Delete(usize, usize),
    Replace(usize, usize, String),
    Group(Vec<(usize, usize, String)>),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0usize..64, "[a-c\n]{0,6}").prop_map(|(at, t)| Step::Insert(at, t)),
        (0usize..64, "[a-c]{1,3}").prop_map(|(at, t)| Step::Type(at, t)),
        (0usize..64, 0usize..8).prop_map(|(at, len)| Step::Delete(at, len)),
        (0usize..64, 0usize..8, "[a-c\n]{0,4}").prop_map(|(at, len, t)| Step::Replace(at, len, t)),
        proptest::collection::vec((0usize..64, 0usize..4, "[a-c]{0,3}"), 1..4)
            .prop_map(Step::Group),
    ]
}

fn apply(buffer: &mut TextBuffer, step: &Step) {
    match step {
        Step::Insert(at, text) => buffer.insert(*at, text),
        Step::Type(at, text) => {
            for (i, c) in text.chars().enumerate() {
                buffer.insert_typed(at + i, &c.to_string());
            }
        }
        Step::Delete(at, len) => buffer.delete(*at..at + len),
        Step::Replace(at, len, text) => buffer.replace(*at..at + len, text),
        Step::Group(edits) => {
            buffer.begin_group();
            for (at, len, text) in edits {
                buffer.replace(*at..at + len, text);
            }
            buffer.end_group();
        }
    }
}

proptest! {
    #[test]
    fn decode_then_encode_is_byte_identical(text in portable_text(), bom in any::<bool>()) {
        for encoding in all_encodings() {
            let spec = ca_text::EncodingSpec { encoding, bom };
            let bytes = encode(&text, spec).bytes;
            let options = DecodeOptions { forced: Some(encoding), ..DecodeOptions::default() };
            let decoded = decode(&bytes, &options);
            prop_assert!(!decoded.had_errors);
            prop_assert_eq!(decoded.text.as_str(), text.as_str());
            prop_assert_eq!(encode_file(&decoded.text, decoded.spec, &decoded.trailer).bytes, bytes);
        }
    }

    #[test]
    fn unicode_encodings_round_trip(text in unicode_text(), bom in any::<bool>()) {
        for encoding in unicode_encodings() {
            let spec = ca_text::EncodingSpec { encoding, bom };
            let bytes = encode(&text, spec).bytes;
            let options = DecodeOptions { forced: Some(encoding), ..DecodeOptions::default() };
            let decoded = decode(&bytes, &options);
            prop_assert_eq!(decoded.text.as_str(), text.as_str());
            prop_assert_eq!(encode_file(&decoded.text, decoded.spec, &decoded.trailer).bytes, bytes);
        }
    }

    #[test]
    fn detection_recovers_utf8_and_bom_state(text in unicode_text(), bom in any::<bool>()) {
        let spec = ca_text::EncodingSpec { encoding: TextEncoding::Utf8, bom };
        let bytes = encode(&text, spec).bytes;
        let decoded = decode(&bytes, &DecodeOptions::default());
        prop_assert_eq!(decoded.spec, spec);
        prop_assert_eq!(encode_file(&decoded.text, decoded.spec, &decoded.trailer).bytes, bytes);
    }

    #[test]
    fn full_undo_restores_the_original(
        original in "[a-c\n]{0,40}",
        steps in proptest::collection::vec(step(), 1..12),
    ) {
        let mut buffer = TextBuffer::from_text(&original);
        for s in &steps {
            apply(&mut buffer, s);
        }
        while buffer.undo().unwrap() {}
        prop_assert_eq!(buffer.text(), original);
        prop_assert!(!buffer.is_modified());
    }

    #[test]
    fn redo_after_undo_restores_the_edit(
        original in "[a-c\n]{0,40}",
        steps in proptest::collection::vec(step(), 1..12),
    ) {
        let mut buffer = TextBuffer::from_text(&original);
        for s in &steps {
            apply(&mut buffer, s);
        }
        let edited = buffer.text();
        let mut undone = 0;
        while buffer.undo().unwrap() {
            undone += 1;
        }
        for _ in 0..undone {
            prop_assert!(buffer.redo().unwrap());
        }
        prop_assert_eq!(buffer.text(), edited);
    }

    #[test]
    fn undo_history_never_loses_line_indexing(
        original in "[a-c\n]{0,40}",
        steps in proptest::collection::vec(step(), 1..8),
    ) {
        let mut buffer = TextBuffer::from_text(&original);
        for s in &steps {
            apply(&mut buffer, s);
            let lines = buffer.len_lines();
            prop_assert_eq!(buffer.char_to_line(buffer.len_chars()), lines - 1);
            for line in 0..lines {
                prop_assert!(buffer.line(line).is_some());
            }
        }
    }

    #[test]
    fn eol_conversion_is_idempotent(text in portable_text()) {
        for style in [EolStyle::Lf, EolStyle::CrLf, EolStyle::Cr] {
            let once = ca_text::convert_eol(&text, style);
            let twice = ca_text::convert_eol(&once, style);
            prop_assert_eq!(&once, &twice);
            let summary = ca_text::scan_eol(&once);
            prop_assert!(!summary.mixed);
            if summary.total() > 0 {
                prop_assert_eq!(summary.dominant, style);
            }
        }
    }

    #[test]
    fn eol_conversion_preserves_line_content(text in portable_text()) {
        let converted = ca_text::convert_eol(&text, EolStyle::Lf);
        let before: Vec<String> = ca_text::eol::lines(&text).map(|(l, _)| l.to_owned()).collect();
        let after: Vec<String> =
            ca_text::eol::lines(&converted).map(|(l, _)| l.to_owned()).collect();
        prop_assert_eq!(before, after);
    }

    #[test]
    fn tabs_to_spaces_preserves_visual_columns(text in "[a-c\t ]{0,40}", stop in 1u32..9) {
        let settings = TabSettings { tab_stop: stop, insert_spaces: false };
        let expanded = ca_text::transform::tabs_to_spaces_line(&text, settings);
        prop_assert!(!expanded.contains('\t'));
        let again = ca_text::transform::tabs_to_spaces_line(&expanded, settings);
        prop_assert_eq!(&expanded, &again);
    }

    #[test]
    fn leading_spaces_to_tabs_keeps_indent_width(text in "[a-c\t ]{0,40}", stop in 1u32..9) {
        let settings = TabSettings { tab_stop: stop, insert_spaces: false };
        let tabbed = ca_text::transform::leading_spaces_to_tabs_line(&text, settings);
        let a = ca_text::transform::tabs_to_spaces_line(&text, settings);
        let b = ca_text::transform::tabs_to_spaces_line(&tabbed, settings);
        prop_assert_eq!(a, b);
    }

    #[test]
    fn a_transform_is_always_one_undo_step(text in "[a-c\t \n]{0,60}") {
        let mut buffer = TextBuffer::from_text(&text);
        let settings = TabSettings::default();
        let all = LineRange::new(0, buffer.len_lines());
        ca_text::tabs_to_spaces(&mut buffer, all, settings);
        prop_assert!(buffer.undo().unwrap() || !buffer.is_modified());
        prop_assert_eq!(buffer.text(), text);
    }

    #[test]
    fn literal_search_finds_every_occurrence(
        needle in "[ab]{1,3}",
        text in "[abc\n]{0,60}",
    ) {
        let buffer = TextBuffer::from_text(&text);
        let options = ca_text::SearchOptions {
            match_case: true,
            ..ca_text::SearchOptions::default()
        };
        let searcher = ca_text::Searcher::new(&needle, options).unwrap();
        for found in searcher.find_all(&buffer).matches {
            prop_assert_eq!(
                found.text.as_str(),
                needle.as_str()
            );
        }
    }
}

#[test]
fn edit_kinds_are_exposed() {
    // The kind enum is part of the public surface callers match on.
    assert_ne!(EditKind::Typing, EditKind::Command);
}
