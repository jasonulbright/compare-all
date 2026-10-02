//! Picture comparison reports.

use crate::cancel::Cancel;
use crate::doc;
use crate::error::{ReportError, Result};
use crate::escape::{text_single_line, write_html};
use crate::input::PictureFacts;
use crate::options::{OutputOptions, PairLayout, PictureReportOptions, ReportMeta};
use crate::plain::pad;
use std::io::Write;

/// Title used when the caller supplies none.
const FALLBACK_TITLE: &str = "Picture Compare Report";

/// Alphabet of the transfer encoding an embedded picture uses.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode bytes for a data address.
fn encode_base64(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = chunk.get(1).copied().map_or(0, u32::from);
        let third = chunk.get(2).copied().map_or(0, u32::from);
        let packed = (first << 16) | (second << 8) | third;
        let indexes = [
            (packed >> 18) & 0x3f,
            (packed >> 12) & 0x3f,
            (packed >> 6) & 0x3f,
            packed & 0x3f,
        ];
        for (position, index) in indexes.into_iter().enumerate() {
            if position > chunk.len() {
                text.push('=');
            } else {
                text.push(char::from(BASE64[index as usize]));
            }
        }
    }
    text
}

fn side_rows(facts: &PictureFacts) -> Vec<(&'static str, String, String)> {
    let mut rows = vec![
        (
            "size",
            format!("{} x {}", facts.left.width, facts.left.height),
            format!("{} x {}", facts.right.width, facts.right.height),
        ),
        (
            "format",
            facts.left.format.clone(),
            facts.right.format.clone(),
        ),
        (
            "bits per channel",
            facts.left.bits_per_channel.to_string(),
            facts.right.bits_per_channel.to_string(),
        ),
        (
            "precision reduced",
            facts.left.precision_reduced.to_string(),
            facts.right.precision_reduced.to_string(),
        ),
        (
            "cyan magenta yellow black",
            facts.left.cmyk.to_string(),
            facts.right.cmyk.to_string(),
        ),
        (
            "color profile",
            facts.left.icc_profile.to_string(),
            facts.right.icc_profile.to_string(),
        ),
    ];
    let mut names: Vec<&str> = Vec::new();
    for (name, _) in facts.left.metadata.iter().chain(&facts.right.metadata) {
        if !names.contains(&name.as_str()) {
            names.push(name.as_str());
        }
    }
    for name in names {
        let left = facts
            .left
            .metadata
            .iter()
            .find(|(key, _)| key == name)
            .map_or(String::new(), |(_, value)| value.clone());
        let right = facts
            .right
            .metadata
            .iter()
            .find(|(key, _)| key == name)
            .map_or(String::new(), |(_, value)| value.clone());
        rows.push((
            "metadata",
            format!("{name}: {left}"),
            format!("{name}: {right}"),
        ));
    }
    rows
}

fn count_rows(facts: &PictureFacts, ignore_unimportant: bool) -> [(&'static str, u64); 7] {
    [
        ("pixels", facts.totals.total()),
        ("same", facts.totals.same),
        ("similar", facts.totals.similar),
        ("different", facts.totals.different),
        ("left only", facts.totals.left_only),
        ("right only", facts.totals.right_only),
        (
            "differences",
            facts.totals.difference_count(ignore_unimportant),
        ),
    ]
}

/// Write a picture comparison report.
///
/// The difference picture is embedded only when the caller supplies its encoded
/// bytes and the document is HTML. No layout encodes a picture on its own, so a
/// report over a large picture costs no memory for one.
///
/// # Errors
///
/// Returns [`ReportError::Io`] when the writer refuses the bytes,
/// [`ReportError::Cancelled`] when the caller raises the flag, and
/// [`ReportError::Unsupported`] for an option the chosen document cannot carry.
pub fn write_picture_report<W: Write + ?Sized>(
    out: &mut W,
    meta: &ReportMeta,
    options: &PictureReportOptions,
    output: &OutputOptions,
    facts: &PictureFacts,
    cancel: &dyn Cancel,
) -> Result<()> {
    doc::validate_output(output)?;
    doc::poll_cancel_now(cancel)?;
    let side_by_side = match options.layout {
        PairLayout::SideBySide => true,
        PairLayout::Summary => false,
        PairLayout::Unknown(ref value) => {
            return Err(ReportError::Unsupported(format!(
                "unknown picture report layout: {value}"
            )))
        }
    };
    if output.is_html() {
        doc::open_html(out, meta, output, FALLBACK_TITLE)?;
        if side_by_side {
            out.write_all(b"<table>\n<tr><th>Property</th><th>")?;
            write_html(out, &meta.left_label)?;
            out.write_all(b"</th><th>")?;
            write_html(out, &meta.right_label)?;
            out.write_all(b"</th></tr>\n")?;
            for (name, left, right) in side_rows(facts) {
                let class = if left == right { "same" } else { "diff" };
                write!(out, "<tr class=\"{class}\"><td>{name}</td><td>")?;
                write_html(out, &left)?;
                out.write_all(b"</td><td>")?;
                write_html(out, &right)?;
                out.write_all(b"</td></tr>\n")?;
            }
            out.write_all(b"</table>\n")?;
        }
        writeln!(out, "<p>tolerance {}</p>", facts.tolerance)?;
        out.write_all(b"<dl class=\"counts\">\n")?;
        for (name, value) in count_rows(facts, options.ignore_unimportant) {
            writeln!(out, "<dt>{name}</dt><dd>{value}</dd>")?;
        }
        out.write_all(b"</dl>\n")?;
        if side_by_side {
            if let Some(bytes) = facts.difference_png.as_deref() {
                doc::poll_cancel_now(cancel)?;
                out.write_all(
                    b"<img class=\"difference\" alt=\"difference\" src=\"data:image/png;base64,",
                )?;
                out.write_all(encode_base64(bytes).as_bytes())?;
                out.write_all(b"\">\n")?;
            }
        }
        doc::close_html(out)?;
        return Ok(());
    }

    doc::open_text(out, meta, FALLBACK_TITLE)?;
    if side_by_side {
        writeln!(
            out,
            "{}{}{}",
            pad("Property", 26),
            pad(&text_single_line(&meta.left_label), 26),
            text_single_line(&meta.right_label)
        )?;
        for (name, left, right) in side_rows(facts) {
            writeln!(
                out,
                "{}{}{}",
                pad(name, 26),
                pad(&text_single_line(&left), 26),
                text_single_line(&right)
            )?;
        }
        writeln!(out)?;
    }
    writeln!(out, "{}{}", pad("tolerance", 26), facts.tolerance)?;
    for (name, value) in count_rows(facts, options.ignore_unimportant) {
        writeln!(out, "{}{value}", pad(name, 26))?;
    }
    if facts.difference_png.is_some() {
        writeln!(
            out,
            "{}{} byte(s), not embedded in a plain text report",
            pad("difference picture", 26),
            facts.difference_png.as_ref().map_or(0, Vec::len)
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{encode_base64, write_picture_report};
    use crate::cancel::NeverCancel;
    use crate::input::{PictureFacts, PictureSide, PixelTotals};
    use crate::options::{OutputOptions, PairLayout, PictureReportOptions, ReportMeta};

    fn facts() -> PictureFacts {
        PictureFacts {
            left: PictureSide {
                width: 4,
                height: 4,
                format: "png".into(),
                bits_per_channel: 8,
                metadata: vec![("camera".into(), "left".into())],
                ..PictureSide::default()
            },
            right: PictureSide {
                width: 4,
                height: 5,
                format: "png".into(),
                bits_per_channel: 16,
                precision_reduced: true,
                metadata: vec![("camera".into(), "right".into())],
                ..PictureSide::default()
            },
            totals: PixelTotals {
                same: 12,
                similar: 2,
                different: 2,
                left_only: 0,
                right_only: 4,
            },
            tolerance: 3,
            difference_png: None,
        }
    }

    fn render(
        options: &PictureReportOptions,
        output: &OutputOptions,
        facts: &PictureFacts,
    ) -> String {
        let mut out = Vec::new();
        write_picture_report(
            &mut out,
            &ReportMeta::new("left.png", "right.png"),
            options,
            output,
            facts,
            &NeverCancel,
        )
        .expect("render");
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn the_transfer_encoding_matches_its_specification() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_side_by_side_layout_lists_each_property() {
        let html = render(
            &PictureReportOptions::default(),
            &OutputOptions::html_color(),
            &facts(),
        );
        assert!(html.contains("bits per channel"));
        assert!(html.contains("camera: left"));
        assert!(html.contains("tolerance 3"));
    }

    #[test]
    fn the_summary_layout_writes_counts_only() {
        let options = PictureReportOptions {
            layout: PairLayout::Summary,
            ..PictureReportOptions::default()
        };
        let text = render(&options, &OutputOptions::plain_text(), &facts());
        assert!(!text.contains("bits per channel"));
        assert!(text.contains("differences"));
    }

    #[test]
    fn ignoring_unimportant_pixels_lowers_the_difference_count() {
        let all = render(
            &PictureReportOptions::default(),
            &OutputOptions::plain_text(),
            &facts(),
        );
        let options = PictureReportOptions {
            ignore_unimportant: true,
            ..PictureReportOptions::default()
        };
        let some = render(&options, &OutputOptions::plain_text(), &facts());
        assert!(all.contains("differences               8"), "{all}");
        assert!(some.contains("differences               6"), "{some}");
    }

    #[test]
    fn a_supplied_picture_is_embedded_in_html_only() {
        let mut facts = facts();
        facts.difference_png = Some(b"foobar".to_vec());
        let html = render(
            &PictureReportOptions::default(),
            &OutputOptions::html_color(),
            &facts,
        );
        assert!(
            html.contains("src=\"data:image/png;base64,Zm9vYmFy\""),
            "{html}"
        );
        let text = render(
            &PictureReportOptions::default(),
            &OutputOptions::plain_text(),
            &facts,
        );
        assert!(text.contains("6 byte(s), not embedded"), "{text}");
    }

    #[test]
    fn a_report_with_no_picture_embeds_nothing() {
        let html = render(
            &PictureReportOptions::default(),
            &OutputOptions::html_color(),
            &facts(),
        );
        assert!(!html.contains("data:image"));
    }
}
