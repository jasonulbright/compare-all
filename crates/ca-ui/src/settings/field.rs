//! One editable value of a session's settings, reached without naming the
//! settings type at the call site.
//!
//! A settings dialog draws a list of these rather than one control per field
//! written out by hand, so a field reaches the dialog, the inherited-value
//! test, the reset action and the tests from one declaration.

use ca_session::settings::{SessionSettings, SessionSettingsOverride};

/// What one field holds.
///
/// Numbers are carried widened: the field's own type is restored by the setter,
/// so a control needs to know only whether the value counts, signs or measures.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    /// An on or off value.
    Flag(bool),
    /// A count with no sign.
    Count(u64),
    /// A value that may be negative.
    Signed(i64),
    /// A measured value.
    Decimal(f64),
    /// Free text.
    Text(String),
    /// One of a fixed set, held by its stored identifier.
    Choice(String),
    /// A list of lines.
    Lines(Vec<String>),
    /// Substitutions declared equivalent across the two sides.
    Replacements(Vec<ca_session::settings::ReplacementItem>),
    /// Comparison treatments naming one table column each.
    Columns(Vec<ca_session::settings::table::ColumnHandling>),
}

impl FieldValue {
    /// The text a control shows for the value.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            FieldValue::Flag(value) => (if *value { "on" } else { "off" }).to_owned(),
            FieldValue::Count(value) => value.to_string(),
            FieldValue::Signed(value) => value.to_string(),
            FieldValue::Decimal(value) => value.to_string(),
            FieldValue::Text(value) | FieldValue::Choice(value) => value.clone(),
            FieldValue::Lines(lines) => lines.join(", "),
            FieldValue::Replacements(items) => format!("{} rules", items.len()),
            FieldValue::Columns(items) => format!("{} columns", items.len()),
        }
    }
}

/// How a field is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldShape {
    /// A check box.
    Flag,
    /// A number with no sign.
    Count,
    /// A number that may be negative.
    Signed,
    /// A measured number.
    Decimal,
    /// A single line of text.
    Text,
    /// A path or other side specification.
    Path,
    /// A drop down over the choices the field declares.
    Choice,
    /// One value per line.
    Lines,
    /// A table of substitutions.
    Replacements,
    /// A table of per-column comparison treatments.
    Columns,
}

/// One named choice of a drop down field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// Stored identifier.
    pub id: &'static str,
    /// Label shown in the list.
    pub label: &'static str,
}

/// One editable value of a session's settings.
#[derive(Clone)]
pub struct Field {
    /// Label shown beside the control.
    pub label: &'static str,
    /// Stable identifier, the group name and the field name joined by a dot.
    pub key: &'static str,
    /// How the field is drawn.
    pub shape: FieldShape,
    /// Choices a drop down offers; empty for every other shape.
    pub choices: &'static [Choice],
    /// Greatest value a count accepts, so a control can bound its slider.
    pub limit: u64,
    /// Why no engine reads the field yet, when none does. A field carrying a
    /// reason is drawn but takes no edit, so nothing appears editable while
    /// changing nothing.
    pub unavailable: Option<&'static str>,
    get: fn(&SessionSettings) -> Option<FieldValue>,
    set: fn(&mut SessionSettings, &FieldValue),
    clear: fn(&mut SessionSettingsOverride),
}

impl std::fmt::Debug for Field {
    /// The accessors are function pointers with nothing to print, so the
    /// rendering names the field and how it is drawn.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Field")
            .field("label", &self.label)
            .field("key", &self.key)
            .field("shape", &self.shape)
            .field("choices", &self.choices)
            .field("limit", &self.limit)
            .field("unavailable", &self.unavailable)
            .finish_non_exhaustive()
    }
}

impl Field {
    /// Builds a field from its accessors. Used by the declaration macros.
    #[must_use]
    pub const fn new(
        label: &'static str,
        key: &'static str,
        shape: FieldShape,
        get: fn(&SessionSettings) -> Option<FieldValue>,
        set: fn(&mut SessionSettings, &FieldValue),
        clear: fn(&mut SessionSettingsOverride),
    ) -> Self {
        Self {
            label,
            key,
            shape,
            choices: &[],
            limit: u64::MAX,
            unavailable: None,
            get,
            set,
            clear,
        }
    }

    /// The same field, drawn but not editable, with the reason no engine reads
    /// it yet.
    #[must_use]
    pub const fn unavailable(mut self, reason: &'static str) -> Self {
        self.unavailable = Some(reason);
        self
    }

    /// True when an engine reads the field.
    #[must_use]
    pub const fn is_bound(&self) -> bool {
        self.unavailable.is_none()
    }

    /// The same field with a choice list.
    #[must_use]
    pub const fn with_choices(mut self, choices: &'static [Choice]) -> Self {
        self.choices = choices;
        self
    }

    /// The same field with an upper bound on what a count accepts.
    #[must_use]
    pub const fn with_limit(mut self, limit: u64) -> Self {
        self.limit = limit;
        self
    }

    /// Reads the field, or nothing when `settings` is of another kind.
    #[must_use]
    pub fn read(&self, settings: &SessionSettings) -> Option<FieldValue> {
        (self.get)(settings)
    }

    /// Writes the field. A value of the wrong shape is ignored.
    pub fn write(&self, settings: &mut SessionSettings, value: &FieldValue) {
        (self.set)(settings, value);
    }

    /// Drops whatever an override sets for this field, so the layer below
    /// supplies it again.
    pub fn clear_override(&self, overrides: &mut SessionSettingsOverride) {
        (self.clear)(overrides);
    }

    /// True when `settings` and `defaults` disagree about this field.
    #[must_use]
    pub fn is_overridden(&self, settings: &SessionSettings, defaults: &SessionSettings) -> bool {
        match (self.read(settings), self.read(defaults)) {
            (Some(left), Some(right)) => left != right,
            _ => false,
        }
    }
}

/// Reads an enum field as its stored identifier.
///
/// The identifier is what serde writes, so the drop down and the document
/// agree without a second table naming the variants.
#[must_use]
pub fn choice_of<T: serde::Serialize>(value: &T) -> FieldValue {
    FieldValue::Choice(
        serde_json::to_value(value)
            .ok()
            .and_then(|json| json.as_str().map(str::to_owned))
            .unwrap_or_default(),
    )
}

/// Writes an enum field from a stored identifier, leaving it alone when the
/// identifier names no variant this build has.
pub fn set_choice<T: serde::de::DeserializeOwned>(target: &mut T, value: &FieldValue) {
    let FieldValue::Choice(id) = value else {
        return;
    };
    if let Ok(parsed) = serde_json::from_value::<T>(serde_json::Value::String(id.clone())) {
        *target = parsed;
    }
}

/// Declares one field of one session kind.
///
/// The three accessors are generated together from one path, so a field can
/// never be read from one place and cleared in another.
macro_rules! declare_field {
    ($variant:ident, $group:ident . $name:ident, $shape:ident, $wrap:ident, $cast:ty, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!(stringify!($group), ".", stringify!($name)),
            $crate::settings::field::FieldShape::$shape,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => Some(
                    $crate::settings::field::FieldValue::$wrap(value.$group.$name.clone().into()),
                ),
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::$wrap(given),
                ) = (settings, value)
                {
                    target.$group.$name = <$cast>::try_from(given.clone()).unwrap_or_default();
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.$group.$name = None;
                }
            },
        )
    };
}

/// Declares a check box field.
macro_rules! flag_field {
    ($variant:ident, $group:ident . $name:ident, $label:literal) => {
        $crate::settings::field::declare_field!($variant, $group.$name, Flag, Flag, bool, $label)
    };
}

/// Declares a text field.
macro_rules! text_field {
    ($variant:ident, $group:ident . $name:ident, $label:literal) => {
        $crate::settings::field::declare_field!($variant, $group.$name, Text, Text, String, $label)
    };
}

/// Declares a list of lines.
macro_rules! lines_field {
    ($variant:ident, $group:ident . $name:ident, $label:literal) => {
        $crate::settings::field::declare_field!(
            $variant,
            $group.$name,
            Lines,
            Lines,
            Vec<String>,
            $label
        )
    };
}

/// Declares an unsigned number field of any width.
macro_rules! count_field {
    ($variant:ident, $group:ident . $name:ident, $cast:ty, $limit:expr, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!(stringify!($group), ".", stringify!($name)),
            $crate::settings::field::FieldShape::Count,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => Some(
                    $crate::settings::field::FieldValue::Count(u64::from(value.$group.$name)),
                ),
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Count(given),
                ) = (settings, value)
                {
                    target.$group.$name = <$cast>::try_from(*given).unwrap_or(<$cast>::MAX);
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.$group.$name = None;
                }
            },
        )
        .with_limit($limit)
    };
}

/// Declares a signed number field.
macro_rules! signed_field {
    ($variant:ident, $group:ident . $name:ident, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!(stringify!($group), ".", stringify!($name)),
            $crate::settings::field::FieldShape::Signed,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => Some(
                    $crate::settings::field::FieldValue::Signed(i64::from(value.$group.$name)),
                ),
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Signed(given),
                ) = (settings, value)
                {
                    target.$group.$name = i32::try_from(*given).unwrap_or_default();
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.$group.$name = None;
                }
            },
        )
    };
}

/// Declares a drop down over a stored enum.
macro_rules! choice_field {
    ($variant:ident, $group:ident . $name:ident, $choices:expr, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!(stringify!($group), ".", stringify!($name)),
            $crate::settings::field::FieldShape::Choice,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => {
                    Some($crate::settings::field::choice_of(&value.$group.$name))
                }
                _ => None,
            },
            |settings, value| {
                if let ca_session::settings::SessionSettings::$variant(target) = settings {
                    $crate::settings::field::set_choice(&mut target.$group.$name, value);
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.$group.$name = None;
                }
            },
        )
        .with_choices($choices)
    };
}

/// Declares the non-name filter list, edited as one criterion per line.
///
/// A line the parser cannot read is dropped rather than kept as an item that
/// excludes nothing, and a criterion this build does not understand is carried
/// through unchanged at the end of the list.
macro_rules! other_filters_field {
    ($variant:ident) => {
        $crate::settings::field::Field::new(
            "Exclusion criteria",
            "other_filters.items",
            $crate::settings::field::FieldShape::Lines,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => {
                    Some($crate::settings::field::FieldValue::Lines(
                        value
                            .other_filters
                            .items
                            .iter()
                            .map($crate::settings::field::other_filter_line)
                            .collect(),
                    ))
                }
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Lines(given),
                ) = (settings, value)
                {
                    target.other_filters.items = $crate::settings::field::other_filter_items(
                        given,
                        &target.other_filters.items,
                    );
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.other_filters.items = None;
                }
            },
        )
    };
}

/// Declares the alignment rules, edited as one rule per line.
macro_rules! alignment_overrides_field {
    ($variant:ident) => {
        $crate::settings::field::Field::new(
            "Alignment rules",
            "misc.alignment_overrides",
            $crate::settings::field::FieldShape::Lines,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => {
                    Some($crate::settings::field::FieldValue::Lines(
                        value
                            .misc
                            .alignment_overrides
                            .iter()
                            .map($crate::settings::field::alignment_line)
                            .collect(),
                    ))
                }
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Lines(given),
                ) = (settings, value)
                {
                    target.misc.alignment_overrides = given
                        .iter()
                        .filter_map(|line| $crate::settings::field::alignment_override(line))
                        .collect();
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.misc.alignment_overrides = None;
                }
            },
        )
    };
}

/// Declares one side of the comparison, edited as its specification text.
macro_rules! side_field {
    ($variant:ident, $name:ident, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!("specs.", stringify!($name)),
            $crate::settings::field::FieldShape::Path,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => {
                    Some($crate::settings::field::FieldValue::Text(
                        value
                            .specs
                            .$name
                            .as_ref()
                            .map(ca_session::SideLocation::to_spec)
                            .unwrap_or_default(),
                    ))
                }
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Text(given),
                ) = (settings, value)
                {
                    if let Ok(side) = $crate::settings::field::parse_side(given) {
                        target.specs.$name = side;
                    }
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.specs.$name = None;
                }
            },
        )
    };
}

/// Declares the file format reading one side.
///
/// Empty text means the format is resolved from the file name, which is the
/// state the settings call detected.
macro_rules! declare_format {
    ($variant:ident, $name:ident, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!("format.", stringify!($name)),
            $crate::settings::field::FieldShape::Text,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => {
                    Some($crate::settings::field::FieldValue::Text(
                        $crate::settings::field::format_name(&value.format.$name),
                    ))
                }
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Text(given),
                ) = (settings, value)
                {
                    target.format.$name = $crate::settings::field::named_format(given);
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.format.$name = None;
                }
            },
        )
    };
}

/// Declares the encoding override of one side. Empty text leaves the choice to
/// the file format.
macro_rules! declare_encoding {
    ($variant:ident, $name:ident, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!("format.", stringify!($name)),
            $crate::settings::field::FieldShape::Text,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => {
                    Some($crate::settings::field::FieldValue::Text(
                        $crate::settings::field::encoding_name(&value.format.$name),
                    ))
                }
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Text(given),
                ) = (settings, value)
                {
                    target.format.$name = $crate::settings::field::named_encoding(given);
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.format.$name = None;
                }
            },
        )
    };
}

/// Declares the importance of one grammar element class.
///
/// The value lives in the stored element list, so the checklist and any other
/// route to the same class edit one place.
macro_rules! element_flag {
    ($variant:ident, $element:expr, $key:literal, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!("importance.element.", $key),
            $crate::settings::field::FieldShape::Flag,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => {
                    Some($crate::settings::field::FieldValue::Flag(
                        value.importance.element_important($element),
                    ))
                }
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Flag(given),
                ) = (settings, value)
                {
                    target.importance.set_element_important($element, *given);
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.importance.grammar_elements = None;
                }
            },
        )
    };
}

/// Declares the substitutions of a kind that carries a replacements group.
macro_rules! replacements_field {
    ($variant:ident) => {
        $crate::settings::field::Field::new(
            "Replacements",
            "replacements.items",
            $crate::settings::field::FieldShape::Replacements,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => {
                    Some($crate::settings::field::FieldValue::Replacements(
                        value.replacements.items.clone(),
                    ))
                }
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Replacements(given),
                ) = (settings, value)
                {
                    target.replacements.items = given.clone();
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.replacements.items = None;
                }
            },
        )
    };
}

/// Declares the pairing rule of a sheet or column list.
///
/// Choosing the explicit rule keeps whatever pairs the session already holds,
/// so picking it in the drop down never empties the list.
macro_rules! pairing_field {
    ($variant:ident, $group:ident, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!(stringify!($group), ".pairing"),
            $crate::settings::field::FieldShape::Choice,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => {
                    Some($crate::settings::field::pairing_mode(&value.$group.pairing))
                }
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Choice(id),
                ) = (settings, value)
                {
                    if id == $crate::settings::field::CUSTOM_PAIRING {
                        if !matches!(
                            target.$group.pairing,
                            ca_session::settings::table::TablePairing::Custom { .. }
                        ) {
                            target.$group.pairing =
                                ca_session::settings::table::TablePairing::Custom {
                                    pairs: Vec::new(),
                                    unknown: std::collections::BTreeMap::new(),
                                };
                        }
                    } else if let Some(pairing) = $crate::settings::field::pairing_from(id) {
                        target.$group.pairing = pairing;
                    }
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.$group.pairing = None;
                }
            },
        )
        .with_choices($crate::settings::field::PAIRING_CHOICES)
    };
}

/// Declares one flag of the column treatment every column inherits.
macro_rules! column_flag {
    ($variant:ident, $name:ident, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!("columns.default_handling.", stringify!($name)),
            $crate::settings::field::FieldShape::Flag,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => Some(
                    $crate::settings::field::FieldValue::Flag(value.columns.default_handling.$name),
                ),
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Flag(given),
                ) = (settings, value)
                {
                    target.columns.default_handling.$name = *given;
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.columns.default_handling = None;
                }
            },
        )
    };
}

/// Declares one seconds tolerance of the inherited column treatment.
macro_rules! column_seconds {
    ($variant:ident, $name:ident, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!("columns.default_handling.", stringify!($name)),
            $crate::settings::field::FieldShape::Count,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => {
                    Some($crate::settings::field::FieldValue::Count(u64::from(
                        value.columns.default_handling.$name,
                    )))
                }
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Count(given),
                ) = (settings, value)
                {
                    target.columns.default_handling.$name =
                        u32::try_from(*given).unwrap_or(u32::MAX);
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.columns.default_handling = None;
                }
            },
        )
    };
}

/// Declares the value type of the column treatment every column inherits.
macro_rules! column_choice {
    ($variant:ident, $name:ident, $choices:expr, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!("columns.default_handling.", stringify!($name)),
            $crate::settings::field::FieldShape::Choice,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => Some(
                    $crate::settings::field::choice_of(&value.columns.default_handling.$name),
                ),
                _ => None,
            },
            |settings, value| {
                if let ca_session::settings::SessionSettings::$variant(target) = settings {
                    $crate::settings::field::set_choice(
                        &mut target.columns.default_handling.$name,
                        value,
                    );
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.columns.default_handling = None;
                }
            },
        )
        .with_choices($choices)
    };
}

/// Declares the numeric tolerance of the inherited column treatment.
macro_rules! column_decimal {
    ($variant:ident, $name:ident, $label:literal) => {
        $crate::settings::field::Field::new(
            $label,
            concat!("columns.default_handling.", stringify!($name)),
            $crate::settings::field::FieldShape::Decimal,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => {
                    Some($crate::settings::field::FieldValue::Decimal(
                        value.columns.default_handling.$name,
                    ))
                }
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Decimal(given),
                ) = (settings, value)
                {
                    // A tolerance is a distance, so a negative entry states
                    // none rather than widening the match.
                    target.columns.default_handling.$name = given.max(0.0);
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.columns.default_handling = None;
                }
            },
        )
    };
}

/// Declares the per-column treatments of a table comparison.
macro_rules! columns_field {
    ($variant:ident) => {
        $crate::settings::field::Field::new(
            "Per-column treatment",
            "columns.per_column",
            $crate::settings::field::FieldShape::Columns,
            |settings| match settings {
                ca_session::settings::SessionSettings::$variant(value) => Some(
                    $crate::settings::field::FieldValue::Columns(value.columns.per_column.clone()),
                ),
                _ => None,
            },
            |settings, value| {
                if let (
                    ca_session::settings::SessionSettings::$variant(target),
                    $crate::settings::field::FieldValue::Columns(given),
                ) = (settings, value)
                {
                    target.columns.per_column = given.clone();
                }
            },
            |overrides| {
                if let ca_session::settings::SessionSettingsOverride::$variant(target) = overrides {
                    target.columns.per_column = None;
                }
            },
        )
    };
}

/// The named pairing rules the dialog offers.
pub const PAIRING_CHOICES: &[Choice] = &[
    Choice {
        id: "unaligned",
        label: "Unaligned",
    },
    Choice {
        id: "by-left-name",
        label: "Align by left name",
    },
    Choice {
        id: "by-right-name",
        label: "Align by right name",
    },
    Choice {
        id: CUSTOM_PAIRING,
        label: "Explicit pairs",
    },
];

/// The identifier of the rule that pairs by an explicit list.
pub const CUSTOM_PAIRING: &str = "custom";

/// The name a format choice carries, empty when the format is detected.
#[must_use]
pub fn format_name(choice: &ca_session::settings::FileFormatChoice) -> String {
    match choice {
        ca_session::settings::FileFormatChoice::Named { name, .. } => name.clone(),
        _ => String::new(),
    }
}

/// Builds a format choice from a name, treating empty text as detection.
#[must_use]
pub fn named_format(name: &str) -> ca_session::settings::FileFormatChoice {
    let name = name.trim();
    if name.is_empty() {
        return ca_session::settings::FileFormatChoice::detected();
    }
    ca_session::settings::FileFormatChoice::Named {
        name: name.to_owned(),
        unknown: std::collections::BTreeMap::new(),
    }
}

/// The name an encoding choice carries, empty when the format decides.
#[must_use]
pub fn encoding_name(choice: &ca_session::settings::EncodingChoice) -> String {
    match choice {
        ca_session::settings::EncodingChoice::Named { name, .. } => name.clone(),
        _ => String::new(),
    }
}

/// Builds an encoding choice from a name, treating empty text as deferring to
/// the file format.
#[must_use]
pub fn named_encoding(name: &str) -> ca_session::settings::EncodingChoice {
    let name = name.trim();
    if name.is_empty() {
        return ca_session::settings::EncodingChoice::from_format();
    }
    ca_session::settings::EncodingChoice::Named {
        name: name.to_owned(),
        unknown: std::collections::BTreeMap::new(),
    }
}

/// The identifier of a pairing rule, for the drop down.
#[must_use]
pub fn pairing_mode(pairing: &ca_session::settings::table::TablePairing) -> FieldValue {
    use ca_session::settings::table::TablePairing;
    let id = match pairing {
        TablePairing::Unaligned { .. } => "unaligned",
        TablePairing::ByLeftName { .. } => "by-left-name",
        TablePairing::ByRightName { .. } => "by-right-name",
        TablePairing::Custom { .. } => "custom",
        _ => "",
    };
    FieldValue::Choice(id.to_owned())
}

/// The pairing rule an identifier names, for the three the dialog offers.
#[must_use]
pub fn pairing_from(id: &str) -> Option<ca_session::settings::table::TablePairing> {
    use ca_session::settings::table::TablePairing;
    match id {
        "unaligned" => Some(TablePairing::unaligned()),
        "by-left-name" => Some(TablePairing::by_left_name()),
        "by-right-name" => Some(TablePairing::by_right_name()),
        _ => None,
    }
}

/// One non-name criterion as a line of text.
///
/// The form is `kind: argument`, with a leading `not ` where the criterion is
/// the negated one. A criterion this build does not understand renders as its
/// stored document, which the parser hands back unchanged.
#[must_use]
pub fn other_filter_line(item: &ca_session::settings::folder::OtherFilterItem) -> String {
    use ca_session::settings::folder::OtherFilterItem as Item;
    let negated = |negated: bool| if negated { "not " } else { "" };
    match item {
        Item::Modified {
            older_than,
            days_ago,
            absolute_seconds,
            ..
        } => {
            let bound = days_ago.map_or_else(
                || absolute_seconds.map_or_else(String::new, |at| format!("at {at}")),
                |days| format!("{days} days"),
            );
            format!(
                "modified {}: {bound}",
                if *older_than { "before" } else { "after" }
            )
        }
        Item::Size {
            smaller_than,
            bytes,
            ..
        } => format!(
            "size {}: {bytes}",
            if *smaller_than { "under" } else { "over" }
        ),
        Item::Content {
            not_containing,
            text,
            ..
        } => format!("content {}: {text}", negated(*not_containing)).replace("  ", " "),
        Item::Attribute {
            is_not_set,
            attribute,
            ..
        } => format!("attribute {}: {attribute}", negated(*is_not_set)).replace("  ", " "),
        Item::UnixFileType {
            is_not, file_type, ..
        } => format!("type {}: {file_type}", negated(*is_not)).replace("  ", " "),
        _ => serde_json::to_string(item).unwrap_or_default(),
    }
}

/// The criteria a block of lines states.
///
/// Every line this build cannot read is dropped; the criteria `stored` holds
/// that this build does not understand are kept, so an edit in an older build
/// does not discard a newer one's filters.
#[must_use]
pub fn other_filter_items(
    lines: &[String],
    stored: &[ca_session::settings::folder::OtherFilterItem],
) -> Vec<ca_session::settings::folder::OtherFilterItem> {
    use ca_session::settings::folder::OtherFilterItem as Item;
    let mut out: Vec<Item> = lines.iter().filter_map(|line| other_filter(line)).collect();
    out.extend(
        stored
            .iter()
            .filter(|item| matches!(item, Item::Unknown(_)))
            .cloned(),
    );
    out
}

/// One criterion from one line, or nothing when the line names none.
#[must_use]
pub fn other_filter(line: &str) -> Option<ca_session::settings::folder::OtherFilterItem> {
    use ca_session::settings::folder::OtherFilterItem as Item;
    let (head, argument) = line.split_once(':')?;
    let argument = argument.trim();
    let head = head.trim().to_ascii_lowercase();
    let mut words = head.split_whitespace();
    let kind = words.next()?;
    let qualifier: Vec<&str> = words.collect();
    let negated = qualifier.contains(&"not");
    let unknown = std::collections::BTreeMap::new();
    match kind {
        "modified" => {
            let older_than = qualifier.contains(&"before");
            let (days_ago, absolute_seconds) = match argument.strip_prefix("at ") {
                Some(at) => (None, at.trim().parse::<i64>().ok()),
                None => (
                    argument
                        .trim_end_matches(" days")
                        .trim()
                        .parse::<u32>()
                        .ok(),
                    None,
                ),
            };
            (days_ago.is_some() || absolute_seconds.is_some()).then_some(Item::Modified {
                older_than,
                days_ago,
                absolute_seconds,
                unknown,
            })
        }
        "size" => Some(Item::Size {
            smaller_than: qualifier.contains(&"under"),
            bytes: argument.parse::<u64>().ok()?,
            unknown,
        }),
        "content" => (!argument.is_empty()).then_some(Item::Content {
            not_containing: negated,
            text: argument.to_owned(),
            unknown,
        }),
        "attribute" => (!argument.is_empty()).then_some(Item::Attribute {
            is_not_set: negated,
            attribute: argument.to_owned(),
            unknown,
        }),
        "type" => (!argument.is_empty()).then_some(Item::UnixFileType {
            is_not: negated,
            file_type: argument.to_owned(),
            unknown,
        }),
        _ => None,
    }
}

/// One alignment rule as a line of text, `left => right` with an optional
/// `in folder` suffix.
#[must_use]
pub fn alignment_line(item: &ca_session::settings::folder::AlignmentOverrideItem) -> String {
    let mut line = format!("{} => {}", item.left, item.right);
    if !item.limit_to_folder.is_empty() {
        line.push_str(" in ");
        line.push_str(&item.limit_to_folder);
    }
    line
}

/// One alignment rule from one line, or nothing when the line names none.
#[must_use]
pub fn alignment_override(
    line: &str,
) -> Option<ca_session::settings::folder::AlignmentOverrideItem> {
    let (left, rest) = line.split_once("=>")?;
    let (right, folder) = match rest.split_once(" in ") {
        Some((right, folder)) => (right, folder.trim().to_owned()),
        None => (rest, String::new()),
    };
    let left = left.trim().to_owned();
    let right = right.trim().to_owned();
    if left.is_empty() || right.is_empty() {
        return None;
    }
    Some(ca_session::settings::folder::AlignmentOverrideItem {
        left,
        right,
        regular_expression: false,
        limit_to_folder: folder,
        unknown: std::collections::BTreeMap::new(),
    })
}

/// Reads a side from its specification text, treating empty text as no side.
///
/// Text that names no known scheme is kept as a local path rather than
/// discarded, so a half-typed entry is not silently lost. A malformed remote
/// user-info separator is an error: turning it into a local path could persist
/// a password as ordinary path text.
///
/// # Errors
/// Returns [`ca_session::LocationParseError::UnescapedUserInfoSeparator`] if a
/// slash occurs before a possible credential delimiter.
pub fn parse_side(
    text: &str,
) -> Result<Option<ca_session::SideLocation>, ca_session::LocationParseError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    match text.parse::<ca_session::SideLocation>() {
        Ok(side) => Ok(Some(side)),
        Err(error @ ca_session::LocationParseError::UnescapedUserInfoSeparator) => Err(error),
        Err(_) => Ok(Some(ca_session::SideLocation::local(text))),
    }
}

pub(crate) use {
    alignment_overrides_field, choice_field, column_choice, column_decimal, column_flag,
    column_seconds, columns_field, count_field, declare_encoding, declare_field, declare_format,
    element_flag, flag_field, lines_field, other_filters_field, pairing_field, replacements_field,
    side_field, signed_field, text_field,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{parse_side, FieldValue};
    use ca_session::settings::common::AlignmentAlgorithm;

    #[test]
    fn a_choice_reads_as_its_stored_identifier() {
        let value = super::choice_of(&AlignmentAlgorithm::MyersOnd);
        assert_eq!(value, FieldValue::Choice("myers-ond".to_owned()));
    }

    #[test]
    fn a_choice_writes_back_from_its_identifier() {
        let mut target = AlignmentAlgorithm::Standard;
        super::set_choice(&mut target, &FieldValue::Choice("patience".to_owned()));
        assert_eq!(target, AlignmentAlgorithm::Patience);
    }

    #[test]
    fn an_identifier_this_build_does_not_know_leaves_the_value_alone() {
        let mut target = AlignmentAlgorithm::Standard;
        super::set_choice(&mut target, &FieldValue::Flag(true));
        assert_eq!(target, AlignmentAlgorithm::Standard);
    }

    #[test]
    fn empty_side_text_is_no_side() {
        assert_eq!(parse_side("   ").unwrap(), None);
        assert_eq!(
            parse_side("/srv/left").unwrap().unwrap(),
            ca_session::SideLocation::local("/srv/left")
        );
    }

    #[test]
    fn a_malformed_remote_password_is_not_fallback_path_text() {
        let result = parse_side("ftp://alice:pa/ss@files.example.test/pub");
        assert_eq!(
            result,
            Err(ca_session::LocationParseError::UnescapedUserInfoSeparator)
        );
        assert!(!format!("{result:?}").contains("pa/ss"));
    }

    #[test]
    fn every_filter_criterion_round_trips_through_its_line() {
        use ca_session::settings::folder::OtherFilterItem as Item;
        let empty = std::collections::BTreeMap::new;
        let items = vec![
            Item::Modified {
                older_than: true,
                days_ago: Some(7),
                absolute_seconds: None,
                unknown: empty(),
            },
            Item::Modified {
                older_than: false,
                days_ago: None,
                absolute_seconds: Some(1_000),
                unknown: empty(),
            },
            Item::Size {
                smaller_than: true,
                bytes: 2_048,
                unknown: empty(),
            },
            Item::Content {
                not_containing: true,
                text: "marker".to_owned(),
                unknown: empty(),
            },
            Item::Attribute {
                is_not_set: false,
                attribute: "H".to_owned(),
                unknown: empty(),
            },
            Item::UnixFileType {
                is_not: true,
                file_type: "symlink".to_owned(),
                unknown: empty(),
            },
        ];
        let lines: Vec<String> = items.iter().map(super::other_filter_line).collect();
        assert_eq!(super::other_filter_items(&lines, &[]), items, "{lines:?}");
    }

    #[test]
    fn a_line_naming_no_criterion_is_dropped() {
        let lines = vec!["nonsense".to_owned(), "size over: many".to_owned()];
        assert!(super::other_filter_items(&lines, &[]).is_empty());
    }

    #[test]
    fn a_criterion_this_build_does_not_understand_survives_an_edit() {
        use ca_session::settings::folder::OtherFilterItem as Item;
        let stored = vec![Item::Unknown(serde_json::json!({"kind": "future"}))];
        let kept = super::other_filter_items(&["size over: 10".to_owned()], &stored);
        assert_eq!(kept.len(), 2);
        assert!(matches!(kept[1], Item::Unknown(_)));
    }

    #[test]
    fn an_alignment_rule_round_trips_through_its_line() {
        let item = ca_session::settings::folder::AlignmentOverrideItem {
            left: "a.*".to_owned(),
            right: "b.*".to_owned(),
            limit_to_folder: "src".to_owned(),
            ..Default::default()
        };
        let line = super::alignment_line(&item);
        assert_eq!(line, "a.* => b.* in src");
        assert_eq!(super::alignment_override(&line), Some(item));
        assert!(super::alignment_override("a.* =>").is_none());
        assert!(super::alignment_override("no arrow").is_none());
    }

    #[test]
    fn a_value_renders_for_a_summary_line() {
        assert_eq!(FieldValue::Flag(true).display(), "on");
        assert_eq!(FieldValue::Count(7).display(), "7");
        assert_eq!(
            FieldValue::Lines(vec!["a".into(), "b".into()]).display(),
            "a, b"
        );
    }
}
