//! The file name search that the folder views share.

/// Accessible label of the search field.
pub const FIELD_LABEL: &str = "Find filename";
/// Accessible label of the button that finds the next name.
pub const NEXT_LABEL: &str = "Find Next Filename";
/// Accessible label of the button that finds the previous name.
pub const PREVIOUS_LABEL: &str = "Find Previous Filename";
/// Accessible label of the button that closes the search bar.
pub const CLOSE_LABEL: &str = "Close Find";
/// Why a repeat search has no text to look for.
pub const NO_TEXT: &str = "Enter a name with Find Filename first";
/// Why a search has no item to look in.
pub const NO_ITEMS: &str = "There are no items to search";
/// Why a search found no name.
pub const NO_MATCH: &str = "No file name matches the search text";

/// The text a search looks for, and whether its bar is open.
#[derive(Debug, Clone, Default)]
pub struct NameFind {
    open: bool,
    focus: bool,
    text: String,
    lowered: String,
}

/// What the search bar asks for after a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// The text changed: look from the current row, including it.
    Edited,
    /// Move to the next match.
    Next,
    /// Move to the previous match.
    Previous,
    /// Hide the bar.
    Close,
}

impl NameFind {
    /// Show the bar and move the keyboard into its field.
    pub fn open(&mut self) {
        self.open = true;
        self.focus = true;
    }

    /// Hide the bar. The text stays, so a repeat search keeps working.
    pub fn close(&mut self) {
        self.open = false;
        self.focus = false;
    }

    /// True while the bar is on screen.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// The text searched for.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replace the text searched for.
    pub fn set_text(&mut self, text: &str) {
        text.clone_into(&mut self.text);
        self.refresh();
    }

    /// True when a search has text to look for.
    #[must_use]
    pub fn has_text(&self) -> bool {
        !self.lowered.is_empty()
    }

    /// True when `name` holds the text, ignoring case.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        self.has_text() && name.to_lowercase().contains(&self.lowered)
    }

    fn refresh(&mut self) {
        self.lowered = self.text.to_lowercase();
    }
}

/// Draw the search bar and report what the user asked for.
pub fn show(
    ui: &mut egui::Ui,
    id: egui::Id,
    find: &mut NameFind,
    note: Option<&str>,
) -> Option<Request> {
    let mut request = None;
    ui.horizontal(|ui| {
        ui.label("Find filename");
        let field = ui.add(
            egui::TextEdit::singleline(&mut find.text)
                .id(id.with("find-text"))
                .hint_text(FIELD_LABEL)
                .desired_width(220.0),
        );
        field.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, ui.is_enabled(), FIELD_LABEL)
        });
        if std::mem::take(&mut find.focus) {
            field.request_focus();
        }
        if field.changed() {
            find.refresh();
            request = Some(Request::Edited);
        }
        if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            let backwards = ui.input(|input| input.modifiers.shift);
            request = Some(if backwards {
                Request::Previous
            } else {
                Request::Next
            });
        }
        if field.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            request = Some(Request::Close);
        }
        if labelled_button(ui, NEXT_LABEL, "Next").clicked() {
            request = Some(Request::Next);
        }
        if labelled_button(ui, PREVIOUS_LABEL, "Previous").clicked() {
            request = Some(Request::Previous);
        }
        if labelled_button(ui, CLOSE_LABEL, "Close").clicked() {
            request = Some(Request::Close);
        }
        if let Some(note) = note {
            ui.label(note);
        }
    });
    request
}

fn labelled_button(ui: &mut egui::Ui, label: &str, text: &str) -> egui::Response {
    let response = ui.button(text);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    response
}
