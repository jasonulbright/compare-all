//! The report command, shared by every view.
//!
//! A view hands over a [`Payload`]: the rows of its comparison as it is
//! displayed, with the large buffers shared by reference count rather than
//! copied. The dialog collects the layout and the output settings, and the job
//! streams the document to a temporary file and renames it over the target, so
//! a run that stops leaves nothing under the name the user chose.
//!
//! Generating a report never runs on the frame thread. The dialog raises the
//! native picker through the job system and starts the write on a worker; a
//! newer request supersedes an older one by replacing the handle, which raises
//! the older run's flag.

mod dialog;
mod hold;
mod job;
mod payload;
mod plan;

pub use dialog::{ReportAction, ReportDialog};
pub use hold::{record, take_records, ViewReport};
pub use job::{spawn, ReportMessage};
pub use plan::ReportPlan;

pub use ca_report::input::format_timestamp;
pub use ca_report::input::{
    CellStatus, EntryStatus, FolderRow, Importance, PictureFacts, PictureSide, PixelTotals,
    RecordRow, RowKind, SideFacts, TableCell, TableHeader, TableRow,
};
/// The heading and the two side labels a report carries.
///
/// Re-exported so a view states its report's heading without taking a
/// dependency on the report engine.
pub use ca_report::options::ReportMeta;
/// Which record comparison a record report is written for.
pub use ca_report::RecordKind;
pub use payload::{HexPayload, Payload, TextPayload, TextRowRef};
pub use plan::{
    DisplayChoice, LayoutChoice, ReportKind, ReportSettings, Target, DIALOG_WIDTH,
    PRINTER_UNAVAILABLE,
};

impl ca_report::Cancel for crate::worker::Cancel {
    fn is_cancelled(&self) -> bool {
        crate::worker::Cancel::is_cancelled(self)
    }
}
