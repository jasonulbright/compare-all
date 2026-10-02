//! Contrast of every foreground color against each background it is painted
//! on, by the relative luminance ratio of WCAG 2.

use super::{chrome, folder_merge, hex, merge, palette, picture, records, table, Variant};
use egui::Color32;

/// Text, including text that carries a state.
const TEXT: f32 = 4.5;

/// Marks and icons, disabled text and revealed whitespace.
const MARK: f32 = 3.0;

fn linear(channel: u8) -> f32 {
    let value = f32::from(channel) / 255.0;
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(color: Color32) -> f32 {
    0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
}

/// The contrast ratio of two opaque colors, from 1 to 21.
fn ratio(first: Color32, second: Color32) -> f32 {
    let (a, b) = (luminance(first), luminance(second));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[derive(Default)]
struct Check {
    failures: Vec<String>,
}

impl Check {
    fn pair(&mut self, place: &str, front: (&str, Color32), back: (&str, Color32), minimum: f32) {
        let found = ratio(front.1, back.1);
        if found < minimum {
            self.failures.push(format!(
                "{place}: {} on {} is {found:.2}, below {minimum}",
                front.0, back.0
            ));
        }
    }

    fn done(self) {
        assert!(self.failures.is_empty(), "{:#?}", self.failures);
    }
}

/// Checks each named foreground field against each named background field of
/// one table.
macro_rules! expect {
    ($check:expr, $place:expr, $table:expr, $minimum:expr; $($front:ident on $($back:ident),+;)+) => {
        $($(
            $check.pair(
                $place,
                (stringify!($front), $table.$front),
                (stringify!($back), $table.$back),
                $minimum,
            );
        )+)+
    };
}

const VARIANTS: [Variant; 2] = [Variant::Light, Variant::Dark];

#[test]
fn text_and_folder_colors_meet_the_contrast_floor() {
    let mut check = Check::default();
    for variant in VARIANTS {
        let place = format!("main {variant:?}");
        let t = palette(variant);
        expect!(check, &place, t, TEXT;
            same_text on same_line, stripe, selection, important_line, unimportant_line, orphan_line, caret_line;
            same_text_other on same_line_other, important_line, unimportant_line_other, orphan_line, caret_line;
            important_text on important_line, caret_line;
            unimportant_text on unimportant_line, caret_line;
            unimportant_text_other on unimportant_line_other, caret_line;
            orphan_text on orphan_line, caret_line;
            gutter_text on gutter_background;
            folder_same on folder_background, stripe, folder_selection;
            folder_different on folder_background, stripe, folder_selection;
            folder_orphan on folder_background, stripe, folder_selection;
            folder_error on folder_background, stripe, folder_selection;
            folder_unknown on folder_background, stripe, folder_selection;
            folder_header_text on folder_header_background;
        );
        expect!(check, &place, t, MARK;
            gutter_arrow on gutter_background;
            important_text on gutter_background;
            thumbnail_marker on thumbnail_background;
            thumbnail_caret on thumbnail_background, gutter_background;
            folder_different on folder_header_background, toolbar, chrome;
            folder_orphan on folder_header_background, toolbar, chrome;
            folder_error on folder_header_background, toolbar, chrome;
            folder_unknown on folder_header_background, toolbar, chrome;
        );
        let s = t.syntax;
        for (name, color, minimum) in [
            ("plain", s.plain, TEXT),
            ("comment", s.comment, TEXT),
            ("literal", s.literal, TEXT),
            ("number", s.number, TEXT),
            ("keyword", s.keyword, TEXT),
            ("identifier", s.identifier, TEXT),
            ("directive", s.directive, TEXT),
            ("operator", s.operator, TEXT),
            ("tag", s.tag, TEXT),
            ("attribute", s.attribute, TEXT),
            ("section", s.section, TEXT),
            ("block", s.block, TEXT),
            ("other", s.other, TEXT),
            ("whitespace", s.whitespace, MARK),
        ] {
            for back in [
                ("same_line", t.same_line),
                ("same_line_other", t.same_line_other),
            ] {
                check.pair(&place, (name, color), back, minimum);
            }
        }
        let visuals = chrome::visuals(variant);
        check.pair(
            &place,
            ("selected toggle label", visuals.selection.stroke.color),
            ("selected toggle", visuals.selection.bg_fill),
            TEXT,
        );
        for back in [
            ("panel_fill", visuals.panel_fill),
            ("window_fill", visuals.window_fill),
        ] {
            for front in [
                ("settings_overridden", t.settings_overridden),
                ("settings_inherited", t.settings_inherited),
                ("notice_info", t.notice_info),
                ("notice_warning", t.notice_warning),
                ("notice_error", t.notice_error),
            ] {
                check.pair(&place, front, back, TEXT);
            }
        }
    }
    check.done();
}

#[test]
fn hex_colors_meet_the_contrast_floor() {
    let mut check = Check::default();
    for variant in VARIANTS {
        let place = format!("hex {variant:?}");
        let t = hex::palette(variant);
        expect!(check, &place, t, TEXT;
            same_text on pane_focused, pane_other, selection, gap_background;
            different_text on different_background;
            orphan_text on orphan_background;
            address_text on address_background;
        );
        expect!(check, &place, t, MARK;
            unavailable_text on pane_focused, pane_other;
            caret on pane_focused, pane_other;
            thumbnail_marker on thumbnail_background;
            thumbnail_caret on thumbnail_background;
        );
    }
    check.done();
}

#[test]
fn table_colors_meet_the_contrast_floor() {
    let mut check = Check::default();
    for variant in VARIANTS {
        let place = format!("table {variant:?}");
        let t = table::palette(variant);
        expect!(check, &place, t, TEXT;
            header_text on header_background, header_hover;
            gutter_text on gutter_background;
            same_text on same_background, stripe, selection, gap_background;
            different_text on different_background, selection;
            unimportant_text on unimportant_background, selection;
            orphan_text on orphan_background, selection;
            notice_text on notice_background;
            details_text on details_background;
        );
        expect!(check, &place, t, MARK;
            key_marker on header_background;
            unimportant_marker on header_background;
            spot_important on gutter_background;
            spot_unimportant on gutter_background;
            spot_orphan on gutter_background;
            current_cell_border on same_background, stripe;
            thumbnail_marker on thumbnail_background;
        );
    }
    check.done();
}

#[test]
fn picture_colors_meet_the_contrast_floor() {
    let mut check = Check::default();
    for variant in VARIANTS {
        let place = format!("picture {variant:?}");
        let t = picture::palette(variant);
        expect!(check, &place, t, TEXT;
            label_text on panel_background, background;
            value_text on panel_background, background;
            error_text on panel_background, background;
            notice_text on panel_background, background;
            progress_text on panel_background, background;
        );
        expect!(check, &place, t, MARK;
            crosshair on pane_background;
            pane_border_focused on background;
        );
    }
    check.done();
}

#[test]
fn record_colors_meet_the_contrast_floor() {
    let mut check = Check::default();
    for variant in VARIANTS {
        let place = format!("records {variant:?}");
        let t = records::palette(variant);
        expect!(check, &place, t, TEXT;
            header_text on header_background;
            same_text on background, selection, gap_background;
            different_text on different_background, inline_difference, selection;
            unimportant_text on unimportant_background, inline_difference, selection;
            orphan_text on orphan_background, selection;
            type_text on background, different_background, unimportant_background, orphan_background, selection;
            details_text on details_background;
        );
        expect!(check, &place, t, MARK;
            thumbnail_marker on thumbnail_background;
            different_text on thumbnail_background;
            unimportant_text on thumbnail_background;
            orphan_text on thumbnail_background;
        );
    }
    check.done();
}

#[test]
fn merge_colors_meet_the_contrast_floor() {
    let mut check = Check::default();
    for variant in VARIANTS {
        let place = format!("merge {variant:?}");
        let t = merge::palette(variant);
        for class in merge::MergeClass::ALL {
            let (back, front) = t.row(class);
            check.pair(&place, (class.label(), front), ("its row", back), TEXT);
        }
        let panel = chrome::visuals(variant).panel_fill;
        expect!(check, &place, t, MARK;
            left_text on unchanged_line;
            right_text on unchanged_line;
        );
        for front in [("left_text", t.left_text), ("right_text", t.right_text)] {
            check.pair(&place, front, ("panel_fill", panel), MARK);
        }
    }
    check.done();
}

#[test]
fn folder_merge_colors_meet_the_contrast_floor() {
    let mut check = Check::default();
    for variant in VARIANTS {
        let place = format!("folder merge {variant:?}");
        let t = folder_merge::palette(variant);
        let backs = [
            ("panel_fill", chrome::visuals(variant).panel_fill),
            ("folder_selection", palette(variant).folder_selection),
            ("output_column", t.output_column),
        ];
        for class in folder_merge::FolderMergeClass::ALL {
            for back in backs {
                check.pair(&place, (class.label(), t.text(class)), back, TEXT);
            }
        }
    }
    check.done();
}

#[test]
fn the_dark_chrome_meets_the_contrast_floor() {
    let mut check = Check::default();
    let place = "chrome Dark";
    let c = chrome::DARK;
    expect!(check, place, c, TEXT;
        label on panel, window;
        text on widget, panel, window, extreme;
        text_bright on hovered, pressed, accent_fill;
        warning on panel, window;
        error on panel, window;
        accent on panel, window;
    );
    let visuals = chrome::visuals(Variant::Dark);
    check.pair(
        place,
        ("disabled label", visuals.gray_out(c.label)),
        ("panel", c.panel),
        MARK,
    );
    check.pair(
        place,
        ("disabled button text", visuals.gray_out(c.text)),
        ("disabled button", visuals.gray_out(c.widget)),
        MARK,
    );
    check.done();
}

#[test]
fn the_dark_layers_step_up_from_pane_to_button() {
    let main = palette(Variant::Dark);
    let c = chrome::DARK;
    let layers = [
        ("text field", c.extreme),
        ("pane", main.same_line),
        ("other pane", main.same_line_other),
        ("panel", c.panel),
        ("toolbar", main.toolbar),
        ("button", c.widget),
        ("hovered button", c.hovered),
        ("pressed button", c.pressed),
    ];
    for pair in layers.windows(2) {
        let (lower, upper) = (pair[0], pair[1]);
        assert!(
            luminance(upper.1) > luminance(lower.1),
            "{} is not lighter than {}",
            upper.0,
            lower.0
        );
    }
}

/// Least distance in the Oklab space between two state colors of one view, for
/// normal vision and for simulated protanopia and deuteranopia.
const APART: f32 = 0.06;

/// Linear RGB transforms of the physiological simulation model for full
/// protanopia and deuteranopia.
const PROTANOPIA: [[f32; 3]; 3] = [
    [0.152_286, 1.052_583, -0.204_868],
    [0.114_503, 0.786_281, 0.099_216],
    [-0.003_882, -0.048_116, 1.051_998],
];
const DEUTERANOPIA: [[f32; 3]; 3] = [
    [0.367_322, 0.860_646, -0.227_968],
    [0.280_085, 0.672_501, 0.047_413],
    [-0.011_820, 0.042_940, 0.968_881],
];
const NORMAL: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn oklab(color: Color32, vision: &[[f32; 3]; 3]) -> [f32; 3] {
    let rgb = [linear(color.r()), linear(color.g()), linear(color.b())];
    let [red, green, blue] =
        vision.map(|row| (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]).clamp(0.0, 1.0));
    let long = (0.412_221_46 * red + 0.536_332_55 * green + 0.051_445_995 * blue).cbrt();
    let medium = (0.211_903_5 * red + 0.680_699_5 * green + 0.107_396_96 * blue).cbrt();
    let short = (0.088_302_46 * red + 0.281_718_85 * green + 0.629_978_7 * blue).cbrt();
    [
        0.210_454_26 * long + 0.793_617_8 * medium - 0.004_072_047 * short,
        1.977_998_5 * long - 2.428_592_2 * medium + 0.450_593_7 * short,
        0.025_904_037 * long + 0.782_771_77 * medium - 0.808_675_77 * short,
    ]
}

fn apart(place: &str, colors: &[(&str, Color32)], failures: &mut Vec<String>) {
    for (index, first) in colors.iter().enumerate() {
        for second in colors.iter().skip(index + 1) {
            for (vision, matrix) in [
                ("normal", &NORMAL),
                ("protanopia", &PROTANOPIA),
                ("deuteranopia", &DEUTERANOPIA),
            ] {
                let (a, b) = (oklab(first.1, matrix), oklab(second.1, matrix));
                let distance = a
                    .iter()
                    .zip(b.iter())
                    .map(|(x, y)| (x - y).powi(2))
                    .sum::<f32>()
                    .sqrt();
                if distance < APART {
                    failures.push(format!(
                        "{place}: {} and {} are {distance:.3} apart under {vision}",
                        first.0, second.0
                    ));
                }
            }
        }
    }
}

#[test]
fn dark_state_colors_stay_apart_for_red_green_color_blindness() {
    let mut failures = Vec::new();
    let t = palette(Variant::Dark);
    apart(
        "text",
        &[
            ("same", t.same_text),
            ("important", t.important_text),
            ("unimportant", t.unimportant_text),
            ("orphan", t.orphan_text),
        ],
        &mut failures,
    );
    apart(
        "folder",
        &[
            ("same", t.folder_same),
            ("different", t.folder_different),
            ("orphan", t.folder_orphan),
            ("error", t.folder_error),
            ("unknown", t.folder_unknown),
        ],
        &mut failures,
    );
    let m = merge::DARK;
    apart(
        "merge",
        &[
            ("left", m.left_text),
            ("right", m.right_text),
            ("same change", m.same_change_text),
            ("conflict", m.conflict_text),
            ("edited", m.edited_text),
        ],
        &mut failures,
    );
    let f = folder_merge::DARK;
    let classes: Vec<(&str, Color32)> = folder_merge::FolderMergeClass::ALL
        .iter()
        .map(|class| (class.label(), f.text(*class)))
        .collect();
    apart("folder merge", &classes, &mut failures);
    let n = [
        ("info", t.notice_info),
        ("warning", t.notice_warning),
        ("error", t.notice_error),
    ];
    apart("notice", &n, &mut failures);
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn the_ratio_matches_known_values() {
    let black = Color32::from_rgb(0, 0, 0);
    let white = Color32::from_rgb(0xFF, 0xFF, 0xFF);
    assert!((ratio(black, white) - 21.0).abs() < 0.01);
    assert!((ratio(white, white) - 1.0).abs() < 0.001);
    let gray = Color32::from_rgb(0x76, 0x76, 0x76);
    assert!((ratio(gray, white) - 4.54).abs() < 0.01);
}
