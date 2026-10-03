//! Which view crate answers for which session kind.
//!
//! This is the one place a view type is named. A view crate never constructs
//! another view crate's view; it returns an [`OpenRequest`] and the shell asks
//! here. Adding a view type is one new crate, one workspace member line, one
//! dependency line and one entry in [`VIEWS`].

use ca_session::SessionKind;
use ca_ui::view::{self, OpenRequest, SessionView, ViewContext};
use std::path::PathBuf;

/// How the registry builds a view once it knows which one to build.
type Constructor = fn(&OpenRequest, &ViewContext, u64) -> Box<dyn SessionView>;

/// Every kind this build has a view for.
pub const VIEWS: &[(SessionKind, Constructor)] = &[
    (
        SessionKind::TextCompare,
        view::build::<ca_view_text::TextView>,
    ),
    (
        SessionKind::FolderCompare,
        view::build::<ca_view_folder::FolderView>,
    ),
    (
        SessionKind::FolderSync,
        view::build::<ca_view_folder::FolderSyncView>,
    ),
    (
        SessionKind::FolderMerge,
        view::build::<ca_view_folder::FolderMergeView>,
    ),
    (SessionKind::HexCompare, view::build::<ca_view_hex::HexView>),
    (
        SessionKind::TableCompare,
        view::build::<ca_view_table::TableView>,
    ),
    (
        SessionKind::PictureCompare,
        view::build::<ca_view_picture::PictureView>,
    ),
    (
        SessionKind::TextMerge,
        view::build::<ca_view_merge::MergeView>,
    ),
    (
        SessionKind::TextEdit,
        view::build::<ca_view_text::TextEditView>,
    ),
    (
        SessionKind::TextPatch,
        view::build::<ca_view_text::TextPatchView>,
    ),
    (
        SessionKind::RegistryCompare,
        view::build::<ca_view_records::RegistryView>,
    ),
    (
        SessionKind::MediaCompare,
        view::build::<ca_view_records::MediaView>,
    ),
    (
        SessionKind::VersionCompare,
        view::build::<ca_view_records::VersionView>,
    ),
];

/// Build the view that answers for `kind`, or report that none does.
#[must_use]
pub fn construct(
    kind: &SessionKind,
    left: PathBuf,
    right: PathBuf,
    context: &ViewContext,
    instance: u64,
) -> Option<Box<dyn SessionView>> {
    open(
        &OpenRequest::new(kind.clone(), left, right),
        context,
        instance,
    )
}

/// Build the view a request names.
#[must_use]
pub fn open(
    request: &OpenRequest,
    context: &ViewContext,
    instance: u64,
) -> Option<Box<dyn SessionView>> {
    let (_, build) = VIEWS.iter().find(|(known, _)| *known == request.kind)?;
    Some(build(request, context, instance))
}

/// True when this build has a view for `kind`.
#[must_use]
pub fn is_available(kind: &SessionKind) -> bool {
    VIEWS.iter().any(|(known, _)| known == kind)
}

/// Every kind this build has a view for, in the order the launcher lists them.
#[must_use]
pub fn available() -> Vec<SessionKind> {
    SessionKind::ALL
        .iter()
        .filter(|kind| is_available(kind))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{available, construct, is_available};
    use ca_session::SessionKind;
    use ca_ui::testing::context;
    use std::path::PathBuf;

    #[test]
    fn every_registered_kind_constructs_a_view() {
        for kind in available() {
            let view = construct(
                &kind,
                PathBuf::from("left"),
                PathBuf::from("right"),
                &context(),
                1,
            );
            assert!(view.is_some(), "{kind} has no view");
        }
    }

    /// The launcher, the command line and a view's open request all name a
    /// kind; a kind with no view would be a dead entry in each of them.
    #[test]
    fn every_session_kind_has_a_view() {
        for kind in SessionKind::ALL {
            assert!(is_available(kind), "{kind} has no view");
        }
        assert_eq!(available().len(), SessionKind::ALL.len());
    }

    #[test]
    fn the_launcher_order_follows_the_stored_order() {
        let listed = available();
        assert_eq!(listed.first(), Some(&SessionKind::FolderCompare));
        assert!(listed.contains(&SessionKind::TextCompare));
        assert_eq!(listed.len(), super::VIEWS.len());
    }

    #[test]
    fn the_record_kinds_each_have_a_view() {
        for kind in [
            SessionKind::RegistryCompare,
            SessionKind::VersionCompare,
            SessionKind::MediaCompare,
        ] {
            assert!(is_available(&kind), "{kind} has no view");
        }
    }
}
