//! What the command line asks the window to open.

use ca_session::SessionKind;
use ca_ui::view::{OpenRequest, Titles};
pub use ca_view_merge::automerge::Switches;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// The switch that names a comparison type instead of letting the paths decide.
const KIND_SWITCH: &str = "--kind";
/// The switch that opens one file in the text editor.
const EDIT_SWITCH: &str = "/edit";
/// The switch that remembers one path as the left side of a later comparison.
pub const LEFT_SIDE_SWITCH: &str = "/leftside";
/// The switch that compares one path against the remembered left side.
pub const COMPARE_LEFT_SWITCH: &str = "/compareleft";
/// The file in the settings folder that holds the remembered left side.
const LEFT_SIDE_FILE: &str = "left-side.txt";

/// What a path turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// An existing file.
    File,
    /// An existing folder.
    Folder,
    /// An existing file whose name marks a container the folder view opens.
    Archive,
    /// Nothing at that path.
    Missing,
}

/// What the window shows at launch.
///
/// The open request is the large variant. One is built per launch, so boxing
/// it would cost an allocation to save nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Startup {
    /// The launcher.
    Home,
    /// A comparison of two paths.
    Open(OpenRequest),
    /// The arguments could not be used.
    Rejected(String),
    /// A merge that runs with no window, and the switches that shape it.
    AutoMerge(OpenRequest, Switches),
    /// Remember a path as the left side of a later comparison. No window opens.
    RememberLeft(PathBuf),
    /// Compare a path against the remembered left side.
    CompareToLeft(PathBuf),
}

impl Startup {
    /// A request for `kind` over the two paths.
    #[must_use]
    pub fn open(kind: SessionKind, left: &Path, right: &Path) -> Self {
        Startup::Open(OpenRequest::new(
            kind,
            left.to_path_buf(),
            right.to_path_buf(),
        ))
    }
}

/// The switches that ask for the version on the console.
const VERSION_SWITCHES: [&str; 2] = ["--version", "-V"];

/// True when the arguments after the program name are one version switch
/// alone.
///
/// A version switch next to other arguments is not one: `-V` can be the name
/// of a file to compare, and such a command line keeps its earlier meaning.
#[must_use]
pub fn asks_for_version(arguments: &[OsString]) -> bool {
    matches!(arguments, [only] if VERSION_SWITCHES.iter().any(|switch| only == switch))
}

/// Decide what to open from the arguments after the program name.
///
/// `probe` reports what each path is, so the decision is testable without a
/// file system. Two sides that are each a folder or an archive open a folder
/// comparison; anything else that exists opens a text comparison, because a
/// plain file paired with a folder has no folder comparison to run. A `--kind`
/// switch names the comparison instead, and then the paths are only checked for
/// existence.
pub fn parse<I>(arguments: I, probe: &dyn Fn(&Path) -> PathKind) -> Startup
where
    I: IntoIterator<Item = OsString>,
{
    let mut kind: Option<SessionKind> = None;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut center: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut titles = Titles::default();
    let mut automatic = Automatic::default();
    let mut edit = false;
    let mut left_side: Option<String> = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let text = argument.to_string_lossy().into_owned();
        match text.to_ascii_lowercase().as_str() {
            EDIT_SWITCH => {
                edit = true;
                continue;
            }
            lowered if lowered.starts_with("/edit=") => {
                return Startup::Rejected(format!("{EDIT_SWITCH} takes no value"));
            }
            lowered @ (LEFT_SIDE_SWITCH | COMPARE_LEFT_SWITCH) => {
                left_side = Some(lowered.to_owned());
                continue;
            }
            _ => {}
        }
        if let Some(outcome) = switch(&text, &mut center, &mut output, &mut titles, &mut automatic)
        {
            match outcome {
                Ok(()) => continue,
                Err(reason) => return Startup::Rejected(reason),
            }
        }
        if let Some(named) = text.strip_prefix(KIND_SWITCH) {
            let name = match named.strip_prefix('=') {
                Some(name) => name.to_string(),
                None if named.is_empty() => match arguments.next() {
                    Some(value) => value.to_string_lossy().into_owned(),
                    None => return Startup::Rejected(format!("{KIND_SWITCH} names no comparison")),
                },
                None => return Startup::Rejected(format!("{text} is not a known switch")),
            };
            match SessionKind::from_str(&name) {
                Ok(named) => kind = Some(named),
                Err(_) => return Startup::Rejected(format!("{name} is not a comparison type")),
            }
            continue;
        }
        if text.starts_with("--") {
            return Startup::Rejected(format!("{text} is not a known switch"));
        }
        paths.push(PathBuf::from(argument));
    }
    if let Some(name) = left_side {
        return left_side_startup(&name, &paths, probe);
    }
    // The third and fourth positional paths are the ancestor and the output,
    // which is the order every version control recipe passes them in.
    if paths.len() > 2 {
        center = center.or_else(|| paths.get(2).cloned());
    }
    if paths.len() > 3 {
        output = output.or_else(|| paths.get(3).cloned());
    }
    let merging = center.is_some() || output.is_some();
    let merge_named = kind
        .as_ref()
        .is_none_or(|named| *named == SessionKind::TextMerge || *named == SessionKind::FolderMerge);
    let merge_case = matches!(paths.len(), 2..=4) && merging && merge_named;
    if !merge_case && (automatic.requested || automatic.named.is_some()) {
        let name = automatic.named.unwrap_or_else(|| "/automerge".to_owned());
        return Startup::Rejected(format!("{name} needs a merge of two files"));
    }
    if edit {
        if kind
            .as_ref()
            .is_some_and(|named| *named != SessionKind::TextEdit)
        {
            return Startup::Rejected(format!("{EDIT_SWITCH} opens the text editor only"));
        }
        return match paths.as_slice() {
            [path] => single(path, SessionKind::TextEdit, probe),
            _ => Startup::Rejected(format!("{EDIT_SWITCH} needs one file")),
        };
    }
    match paths.len() {
        0 if kind.is_none() => Startup::Home,
        1 => match (kind, paths.first()) {
            (Some(named), Some(path)) if named.side_count() == 1 => single(path, named, probe),
            (None, Some(path)) if is_patch_name(path) => {
                single(path, SessionKind::TextPatch, probe)
            }
            _ => Startup::Rejected("expected two paths or none, received 1".to_string()),
        },
        0 => Startup::Rejected("a comparison type needs two paths".to_string()),
        2..=4 if merging && merge_named => {
            let startup = merge(&paths[0], &paths[1], center, output, titles, kind, probe);
            automatic.apply(startup)
        }
        2 => with_titles(classify(&paths[0], &paths[1], kind, probe), titles),
        count => Startup::Rejected(format!("expected two paths or none, received {count}")),
    }
}

/// One of the two steps of a comparison started from the file manager.
fn left_side_startup(name: &str, paths: &[PathBuf], probe: &dyn Fn(&Path) -> PathKind) -> Startup {
    match paths {
        [path] if probe(path) == PathKind::Missing => {
            Startup::Rejected(format!("{} does not exist", path.display()))
        }
        [path] if name == LEFT_SIDE_SWITCH => Startup::RememberLeft(path.clone()),
        [path] => Startup::CompareToLeft(path.clone()),
        _ => Startup::Rejected(format!("{name} needs one path")),
    }
}

/// A merge over two versions, an optional ancestor and an optional output.
///
/// The output need not exist: a merge that creates its result is the ordinary
/// case. Every other path is checked, so a typo is named rather than opened.
/// Two folders make a folder merge, and then the ancestor and an existing
/// output must be folders too.
fn merge(
    left: &Path,
    right: &Path,
    center: Option<PathBuf>,
    output: Option<PathBuf>,
    titles: Titles,
    named: Option<SessionKind>,
    probe: &dyn Fn(&Path) -> PathKind,
) -> Startup {
    for path in [Some(left), Some(right), center.as_deref()]
        .into_iter()
        .flatten()
    {
        if probe(path) == PathKind::Missing {
            return Startup::Rejected(format!("{} does not exist", path.display()));
        }
    }
    let folder_like = |path: &Path| matches!(probe(path), PathKind::Folder | PathKind::Archive);
    let folders = folder_like(left) && folder_like(right);
    let kind = named.unwrap_or(if folders {
        SessionKind::FolderMerge
    } else {
        SessionKind::TextMerge
    });
    if kind == SessionKind::FolderMerge {
        let inputs = [Some(left), Some(right), center.as_deref()];
        let existing_output = output
            .as_deref()
            .filter(|path| probe(path) != PathKind::Missing);
        for path in inputs.into_iter().flatten() {
            if !folder_like(path) {
                return Startup::Rejected(format!(
                    "{} is not a folder or an archive",
                    path.display()
                ));
            }
        }
        if let Some(path) = existing_output {
            if probe(path) != PathKind::Folder {
                return Startup::Rejected(format!("{} is not a folder", path.display()));
            }
        }
    }
    Startup::Open(
        OpenRequest::new(kind, left.to_path_buf(), right.to_path_buf())
            .with_center(center)
            .with_output(output)
            .with_titles(titles),
    )
}

/// The switches that run a merge with no window, as the command line gave them.
#[derive(Debug, Default)]
struct Automatic {
    /// `/automerge` was given.
    requested: bool,
    /// The switches that shape the merge.
    switches: Switches,
    /// The first automatic merge switch given, for a refusal to name.
    named: Option<String>,
}

impl Automatic {
    /// Turn a merge request into an automatic merge when `/automerge` asks for
    /// one, and refuse a switch that has no automatic merge to change.
    fn apply(self, startup: Startup) -> Startup {
        let Startup::Open(request) = startup else {
            return startup;
        };
        if request.kind == SessionKind::FolderMerge && (self.requested || self.named.is_some()) {
            let name = self.named.unwrap_or_else(|| "/automerge".to_owned());
            return Startup::Rejected(format!("{name} needs a merge of two files"));
        }
        if self.switches.favor_left && self.switches.favor_right {
            return Startup::Rejected("/favorleft and /favorright exclude each other".to_owned());
        }
        if self.requested {
            if request.output.is_none() {
                return Startup::Rejected("/automerge needs an output file".to_owned());
            }
            return Startup::AutoMerge(request, self.switches);
        }
        match self.named {
            Some(name) => Startup::Rejected(format!("{name} needs /automerge")),
            None => Startup::Open(request),
        }
    }
}

/// Read one documented switch, or report that the argument is not one.
///
/// `None` says the argument names no switch, so it is a path.
fn switch(
    text: &str,
    center: &mut Option<PathBuf>,
    output: &mut Option<PathBuf>,
    titles: &mut Titles,
    automatic: &mut Automatic,
) -> Option<Result<(), String>> {
    let (name, value) = match text.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (text, None),
    };
    let lowered = name.to_ascii_lowercase();
    let needs_value = |slot: &mut Option<PathBuf>| match value {
        Some(value) if !value.is_empty() => {
            *slot = Some(PathBuf::from(value));
            Ok(())
        }
        _ => Err(format!("{name} names no file")),
    };
    let needs_title = |slot: &mut Option<String>| match value {
        Some(value) if !value.is_empty() => {
            *slot = Some(value.to_owned());
            Ok(())
        }
        _ => Err(format!("{name} names no title")),
    };
    Some(match lowered.as_str() {
        "/center" | "/centerfile" => needs_value(center),
        "/mergeoutput" => needs_value(output),
        "/title1" | "/lefttitle" => needs_title(&mut titles.left),
        "/title2" | "/righttitle" => needs_title(&mut titles.right),
        "/title3" | "/centertitle" => needs_title(&mut titles.center),
        "/title4" | "/outputtitle" => needs_title(&mut titles.output),
        "/automerge" | "/favorleft" | "/favorright" | "/force" | "/reviewconflicts" | "/iu"
        | "/ignoreunimportant" => {
            if value.is_some() {
                return Some(Err(format!("{name} takes no value")));
            }
            let switches = &mut automatic.switches;
            match lowered.as_str() {
                "/automerge" => automatic.requested = true,
                "/favorleft" => switches.favor_left = true,
                "/favorright" => switches.favor_right = true,
                "/force" => switches.force = true,
                "/reviewconflicts" => switches.review_conflicts = true,
                _ => switches.ignore_unimportant = true,
            }
            if lowered != "/automerge" && automatic.named.is_none() {
                automatic.named = Some(name.to_owned());
            }
            Ok(())
        }
        _ => return None,
    })
}

/// A view of one file, which has to exist.
fn single(path: &Path, kind: SessionKind, probe: &dyn Fn(&Path) -> PathKind) -> Startup {
    if !matches!(probe(path), PathKind::File | PathKind::Archive) {
        return Startup::Rejected(format!("{} is not a file", path.display()));
    }
    Startup::open(kind, path, Path::new(""))
}

/// True when the name marks a patch file.
fn is_patch_name(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("diff") || extension.eq_ignore_ascii_case("patch")
        })
}

fn classify(
    left: &Path,
    right: &Path,
    kind: Option<SessionKind>,
    probe: &dyn Fn(&Path) -> PathKind,
) -> Startup {
    let left_kind = probe(left);
    let right_kind = probe(right);
    for (path, kind) in [(left, left_kind), (right, right_kind)] {
        if kind == PathKind::Missing {
            return Startup::Rejected(format!("{} does not exist", path.display()));
        }
    }
    if let Some(named) = kind {
        return Startup::open(named, left, right);
    }
    let folder_like = |kind: PathKind| matches!(kind, PathKind::Folder | PathKind::Archive);
    if folder_like(left_kind) && folder_like(right_kind) {
        return Startup::open(SessionKind::FolderCompare, left, right);
    }
    Startup::open(SessionKind::TextCompare, left, right)
}

fn with_titles(startup: Startup, titles: Titles) -> Startup {
    match startup {
        Startup::Open(request) => Startup::Open(request.with_titles(titles)),
        other => other,
    }
}

/// Run an automatic merge before any window exists.
///
/// Returns the exit code when the merge is over, or the startup the window
/// opens with: every other startup unchanged, and an automatic merge whose
/// conflicts are to be reviewed as an ordinary merge.
///
/// # Errors
///
/// Returns the exit code of a finished automatic merge, which ends the process
/// with no window.
pub fn run_automatic(startup: Startup) -> Result<Startup, i32> {
    let Startup::AutoMerge(request, switches) = startup else {
        return Ok(startup);
    };
    let paths = ca_view_merge::jobs::MergePaths {
        left: request.left.clone(),
        center: request.center.clone(),
        right: request.right.clone(),
        output: request.output.clone(),
    };
    let stored = ca_session::settings::TextMergeSettings::default();
    match ca_view_merge::automerge::run(&paths, &stored, switches) {
        ca_view_merge::automerge::Finish::Done { code, .. } => Err(code),
        ca_view_merge::automerge::Finish::Review => Ok(Startup::Open(request)),
    }
}

/// Carries out the two steps of a comparison started from the file manager.
///
/// `RememberLeft` writes the path into the settings folder and ends the
/// process with exit code 0. `CompareToLeft` reads that path back and becomes
/// the comparison of the two paths, decided by `probe` as for two arguments.
/// The remembered path stays, so several items can be compared against it.
/// Every other startup is returned unchanged.
///
/// # Errors
///
/// Returns the exit code when the step ends the process with no window.
pub fn run_left_side(
    startup: Startup,
    settings_directory: &Path,
    probe: &dyn Fn(&Path) -> PathKind,
) -> Result<Startup, i32> {
    let file = settings_directory.join(LEFT_SIDE_FILE);
    match startup {
        Startup::RememberLeft(path) => {
            let path = std::path::absolute(&path).unwrap_or(path);
            let written = std::fs::create_dir_all(settings_directory)
                .and_then(|()| ca_io::write_atomic(&file, path.to_string_lossy().as_bytes()));
            match written {
                Ok(()) => Err(0),
                Err(error) => Ok(Startup::Rejected(format!(
                    "The left side could not be remembered: {error}"
                ))),
            }
        }
        Startup::CompareToLeft(right) => {
            let Ok(text) = std::fs::read_to_string(&file) else {
                return Ok(Startup::Rejected(format!(
                    "No left side is remembered. Use {LEFT_SIDE_SWITCH} first."
                )));
            };
            let left = PathBuf::from(text.trim_end_matches(['\r', '\n']));
            Ok(parse(
                [left.into_os_string(), right.into_os_string()],
                probe,
            ))
        }
        other => Ok(other),
    }
}

/// Ask the file system what a path is, with the Archive Types masks the
/// stored options state.
#[must_use]
pub fn probe_file_system(path: &Path) -> PathKind {
    let stored = stored_options();
    probe_with(path, stored.as_ref().map(|options| &options.archives))
}

/// The stored options document, read without changing anything on disk.
#[must_use]
pub fn stored_options() -> Option<ca_session::ProgramOptions> {
    let directory = ca_session::SettingsDirectory::PerUser(ca_ui::paths::settings_directory());
    ca_session::ProgramOptions::peek(&ca_session::SettingsPaths::at(directory).options_file())
}

/// Ask the file system what a path is, with the stated Archive Types masks.
#[must_use]
pub fn probe_with(path: &Path, masks: Option<&ca_session::options::ArchiveOptions>) -> PathKind {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => PathKind::Folder,
        Ok(_) if is_archive_name(path, masks) => PathKind::Archive,
        Ok(_) => PathKind::File,
        Err(_) => PathKind::Missing,
    }
}

/// True when the name matches the mask of a container format this build
/// reads. The name decides, so a probe reads no bytes of the file.
fn is_archive_name(path: &Path, masks: Option<&ca_session::options::ArchiveOptions>) -> bool {
    let types = ca_view_folder::settings::archive_types(
        &ca_session::settings::folder::ArchiveHandling::AsFolders,
        masks,
    );
    path.file_name()
        .and_then(|name| types.format_for_name(&name.to_string_lossy()))
        .is_some_and(|format| format.is_supported() && types.is_enabled(format))
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::{parse, PathKind, Startup};
    use ca_session::SessionKind;
    use std::ffi::OsString;
    use std::path::Path;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn probe<'a>(folders: &'a [&'a str]) -> impl Fn(&Path) -> PathKind + 'a {
        move |path: &Path| {
            let text = path.to_string_lossy().into_owned();
            if text.contains("missing") {
                PathKind::Missing
            } else if folders.iter().any(|folder| text.contains(folder)) {
                PathKind::Folder
            } else {
                PathKind::File
            }
        }
    }

    #[test]
    fn a_version_switch_alone_asks_for_the_version() {
        assert!(super::asks_for_version(&args(&["--version"])));
        assert!(super::asks_for_version(&args(&["-V"])));
        for other in [
            &[][..],
            &["-v"][..],
            &["--Version"][..],
            &["--version", "b.txt"][..],
            &["-V", "b.txt"][..],
            &["a.txt", "--version"][..],
        ] {
            assert!(!super::asks_for_version(&args(other)), "{other:?}");
        }
        assert_eq!(
            parse(args(&["-V", "b.txt"]), &probe(&[])),
            Startup::open(
                SessionKind::TextCompare,
                Path::new("-V"),
                Path::new("b.txt")
            )
        );
    }

    #[test]
    fn the_left_side_switches_take_one_existing_path() {
        assert_eq!(
            parse(args(&["/leftside", "a.txt"]), &probe(&[])),
            Startup::RememberLeft("a.txt".into())
        );
        assert_eq!(
            parse(args(&["/COMPARELEFT", "dir-b"]), &probe(&["dir-"])),
            Startup::CompareToLeft("dir-b".into())
        );
        for bad in [
            &["/leftside"][..],
            &["/leftside", "a.txt", "b.txt"][..],
            &["/compareleft", "missing.txt"][..],
        ] {
            assert!(
                matches!(parse(args(bad), &probe(&[])), Startup::Rejected(_)),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn a_remembered_left_side_is_compared_to_the_next_path() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings");
        let left = dir.path().join("dir-left");
        let right = dir.path().join("dir-right");
        let folders = probe(&["dir-"]);
        let missing =
            super::run_left_side(Startup::CompareToLeft(right.clone()), &settings, &folders);
        assert!(matches!(missing, Ok(Startup::Rejected(_))));
        assert_eq!(
            super::run_left_side(Startup::RememberLeft(left.clone()), &settings, &folders),
            Err(0)
        );
        let expected = Startup::open(SessionKind::FolderCompare, &left, &right);
        for _ in 0..2 {
            assert_eq!(
                super::run_left_side(Startup::CompareToLeft(right.clone()), &settings, &folders),
                Ok(expected.clone())
            );
        }
        assert_eq!(
            super::run_left_side(Startup::Home, &settings, &folders),
            Ok(Startup::Home)
        );
    }

    #[test]
    fn no_arguments_open_the_launcher() {
        assert_eq!(parse(args(&[]), &probe(&[])), Startup::Home);
    }

    #[test]
    fn two_folders_open_a_folder_comparison() {
        let startup = parse(args(&["dir-a", "dir-b"]), &probe(&["dir-"]));
        assert_eq!(
            startup,
            Startup::open(
                SessionKind::FolderCompare,
                Path::new("dir-a"),
                Path::new("dir-b"),
            )
        );
    }

    fn probe_with_archives(path: &Path) -> PathKind {
        let text = path.to_string_lossy();
        if text.ends_with(".zip") {
            PathKind::Archive
        } else if text.contains("dir-") {
            PathKind::Folder
        } else {
            PathKind::File
        }
    }

    #[test]
    fn an_archive_beside_a_folder_or_an_archive_opens_a_folder_comparison() {
        for (left, right) in [("a.zip", "b.zip"), ("dir-a", "b.zip"), ("a.zip", "dir-b")] {
            assert_eq!(
                parse(args(&[left, right]), &probe_with_archives),
                Startup::open(
                    SessionKind::FolderCompare,
                    Path::new(left),
                    Path::new(right)
                ),
                "{left} {right}"
            );
        }
        assert_eq!(
            parse(args(&["a.zip", "b.txt"]), &probe_with_archives),
            Startup::open(
                SessionKind::TextCompare,
                Path::new("a.zip"),
                Path::new("b.txt")
            )
        );
    }

    #[test]
    fn the_file_system_probe_names_an_archive_by_its_extension() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let zip = dir.path().join("pack.zip");
        let text = dir.path().join("notes.txt");
        for path in [&zip, &text] {
            std::fs::write(path, b"x").unwrap_or_else(|error| panic!("{error}"));
        }
        assert_eq!(super::probe_with(&zip, None), PathKind::Archive);
        assert_eq!(super::probe_with(&text, None), PathKind::File);
        assert_eq!(super::probe_with(dir.path(), None), PathKind::Folder);
    }

    #[test]
    fn the_file_system_probe_follows_the_stored_archive_masks() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let zip = dir.path().join("pack.zip");
        let pak = dir.path().join("pack.pak");
        for path in [&zip, &pak] {
            std::fs::write(path, b"x").unwrap_or_else(|error| panic!("{error}"));
        }
        let mut masks = ca_session::options::ArchiveOptions::default();
        masks.masks.insert("zip".to_owned(), "*.pak".to_owned());
        assert_eq!(super::probe_with(&pak, Some(&masks)), PathKind::Archive);
        assert_eq!(super::probe_with(&zip, Some(&masks)), PathKind::File);
    }

    #[test]
    fn two_files_open_a_text_comparison() {
        let startup = parse(args(&["one.txt", "two.txt"]), &probe(&[]));
        assert_eq!(
            startup,
            Startup::open(
                SessionKind::TextCompare,
                Path::new("one.txt"),
                Path::new("two.txt"),
            )
        );
    }

    #[test]
    fn a_visual_studio_diff_invocation_keeps_paths_and_friendly_titles_with_spaces() {
        let left = r"C:\work tree\left file.json";
        let right = r"C:\work tree\right file.json";
        let startup = parse(
            args(&[
                left,
                "/title1=Checked out source",
                right,
                "/title2=Workspace version",
            ]),
            &probe(&[]),
        );
        let Startup::Open(request) = startup else {
            panic!("expected a text comparison, got {startup:?}");
        };
        assert_eq!(request.kind, SessionKind::TextCompare);
        assert_eq!(request.left, Path::new(left));
        assert_eq!(request.right, Path::new(right));
        assert_eq!(request.titles.left.as_deref(), Some("Checked out source"));
        assert_eq!(request.titles.right.as_deref(), Some("Workspace version"));
    }

    #[test]
    fn a_file_paired_with_a_folder_opens_a_text_comparison() {
        let startup = parse(args(&["one.txt", "dir-b"]), &probe(&["dir-"]));
        match startup {
            Startup::Open(request) => assert_eq!(request.kind, SessionKind::TextCompare),
            other => panic!("expected a comparison, received {other:?}"),
        }
    }

    #[test]
    fn a_missing_path_is_refused_by_name() {
        let startup = parse(args(&["missing.txt", "two.txt"]), &probe(&[]));
        match startup {
            Startup::Rejected(reason) => assert!(reason.contains("missing.txt")),
            other => panic!("expected a rejection, received {other:?}"),
        }
    }

    #[test]
    fn a_single_path_is_refused() {
        assert!(matches!(
            parse(args(&["one.txt"]), &probe(&[])),
            Startup::Rejected(_)
        ));
        assert!(matches!(
            parse(args(&["a", "b", "c", "d", "e"]), &probe(&[])),
            Startup::Rejected(_)
        ));
    }

    #[test]
    fn three_paths_open_a_merge_over_the_ancestor() {
        let startup = parse(args(&["mine.txt", "theirs.txt", "base.txt"]), &probe(&[]));
        match startup {
            Startup::Open(request) => {
                assert_eq!(request.kind, SessionKind::TextMerge);
                assert_eq!(request.left, Path::new("mine.txt"));
                assert_eq!(request.right, Path::new("theirs.txt"));
                assert_eq!(request.center.as_deref(), Some(Path::new("base.txt")));
                assert!(request.output.is_none());
            }
            other => panic!("expected a merge, received {other:?}"),
        }
    }

    #[test]
    fn four_paths_name_the_output_as_well() {
        let startup = parse(
            args(&["mine.txt", "theirs.txt", "base.txt", "merged.txt"]),
            &probe(&[]),
        );
        match startup {
            Startup::Open(request) => {
                assert_eq!(request.output.as_deref(), Some(Path::new("merged.txt")));
            }
            other => panic!("expected a merge, received {other:?}"),
        }
    }

    #[test]
    fn the_merge_switches_name_the_ancestor_the_output_and_the_titles() {
        let startup = parse(
            args(&[
                "mine.txt",
                "theirs.txt",
                "/center=base.txt",
                "/mergeoutput=merged.txt",
                "/title1=Mine",
                "/righttitle=Theirs",
                "/title3=Base",
                "/outputtitle=Merged",
            ]),
            &probe(&[]),
        );
        match startup {
            Startup::Open(request) => {
                assert_eq!(request.kind, SessionKind::TextMerge);
                assert_eq!(request.center.as_deref(), Some(Path::new("base.txt")));
                assert_eq!(request.output.as_deref(), Some(Path::new("merged.txt")));
                assert_eq!(request.titles.left.as_deref(), Some("Mine"));
                assert_eq!(request.titles.right.as_deref(), Some("Theirs"));
                assert_eq!(request.titles.center.as_deref(), Some("Base"));
                assert_eq!(request.titles.output.as_deref(), Some("Merged"));
            }
            other => panic!("expected a merge, received {other:?}"),
        }
    }

    #[test]
    fn three_or_four_folders_open_a_folder_merge() {
        let startup = parse(
            args(&["dir-mine", "dir-theirs", "dir-base", "dir-merged"]),
            &probe(&["dir-"]),
        );
        match startup {
            Startup::Open(request) => {
                assert_eq!(request.kind, SessionKind::FolderMerge);
                assert_eq!(request.center.as_deref(), Some(Path::new("dir-base")));
                assert_eq!(request.output.as_deref(), Some(Path::new("dir-merged")));
            }
            other => panic!("expected a folder merge, received {other:?}"),
        }
        let switched = parse(
            args(&["dir-mine", "dir-theirs", "/mergeoutput=missing-out"]),
            &probe(&["dir-"]),
        );
        match switched {
            Startup::Open(request) => {
                assert_eq!(request.kind, SessionKind::FolderMerge);
                assert!(request.center.is_none());
                assert_eq!(request.output.as_deref(), Some(Path::new("missing-out")));
            }
            other => panic!("expected a folder merge, received {other:?}"),
        }
    }

    #[test]
    fn a_folder_merge_refuses_a_file_among_its_folders_and_an_automatic_merge() {
        for arguments in [
            &["dir-mine", "dir-theirs", "base.txt"][..],
            &["dir-mine", "dir-theirs", "dir-base", "merged.txt"][..],
            &[
                "dir-mine",
                "dir-theirs",
                "dir-base",
                "dir-out",
                "/automerge",
            ][..],
            &["--kind", "folder-merge", "a.txt", "b.txt", "dir-base"][..],
        ] {
            match parse(args(arguments), &probe(&["dir-"])) {
                Startup::Rejected(reason) => assert!(!reason.is_empty()),
                other => panic!("{arguments:?} was accepted as {other:?}"),
            }
        }
    }

    #[test]
    fn a_merge_refuses_an_ancestor_that_does_not_exist() {
        let startup = parse(
            args(&["mine.txt", "theirs.txt", "/center=missing.txt"]),
            &probe(&[]),
        );
        match startup {
            Startup::Rejected(reason) => assert!(reason.contains("missing.txt")),
            other => panic!("expected a rejection, received {other:?}"),
        }
    }

    /// An output that does not exist yet is the ordinary case, so the merge
    /// opens and creates it.
    #[test]
    fn a_merge_accepts_an_output_that_does_not_exist_yet() {
        let startup = parse(
            args(&["mine.txt", "theirs.txt", "base.txt", "missing.txt"]),
            &probe(&[]),
        );
        assert!(matches!(startup, Startup::Open(_)));
    }

    #[test]
    fn a_merge_switch_without_automerge_is_refused_by_name() {
        let startup = parse(
            args(&["mine.txt", "theirs.txt", "base.txt", "/favorleft"]),
            &probe(&[]),
        );
        match startup {
            Startup::Rejected(reason) => assert!(reason.contains("/favorleft")),
            other => panic!("expected a rejection, received {other:?}"),
        }
    }

    #[test]
    fn automerge_with_an_output_starts_a_merge_with_no_window() {
        let startup = parse(
            args(&[
                "mine.txt",
                "theirs.txt",
                "base.txt",
                "out.txt",
                "/automerge",
                "/force",
                "/iu",
                "/favorright",
                "/reviewconflicts",
            ]),
            &probe(&[]),
        );
        match startup {
            Startup::AutoMerge(request, switches) => {
                assert_eq!(request.kind, SessionKind::TextMerge);
                assert_eq!(request.output.as_deref(), Some(Path::new("out.txt")));
                assert!(switches.force);
                assert!(switches.ignore_unimportant);
                assert!(switches.favor_right);
                assert!(!switches.favor_left);
                assert!(switches.review_conflicts);
            }
            other => panic!("expected an automatic merge, received {other:?}"),
        }
    }

    #[test]
    fn automerge_is_refused_without_an_output_or_without_a_merge() {
        for arguments in [
            &["mine.txt", "theirs.txt", "base.txt", "/automerge"][..],
            &["mine.txt", "theirs.txt", "/automerge"][..],
            &["/automerge"][..],
            &[
                "mine.txt",
                "theirs.txt",
                "base.txt",
                "out.txt",
                "/automerge",
                "/favorleft",
                "/favorright",
            ][..],
            &[
                "mine.txt",
                "theirs.txt",
                "base.txt",
                "out.txt",
                "/automerge=yes",
            ][..],
        ] {
            match parse(args(arguments), &probe(&[])) {
                Startup::Rejected(reason) => assert!(!reason.is_empty()),
                other => panic!("{arguments:?} was accepted as {other:?}"),
            }
        }
    }

    #[test]
    fn a_finished_automatic_merge_ends_with_its_exit_code() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let write = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).unwrap_or_else(|error| panic!("{error}"));
            path
        };
        let left = write("left.txt", "a\nL\nc\n");
        let center = write("center.txt", "a\nb\nc\n");
        let right = write("right.txt", "a\nR\nc\n");
        let output = dir.path().join("out.txt");
        let request = |switches| {
            super::Startup::AutoMerge(
                ca_ui::view::OpenRequest::new(SessionKind::TextMerge, left.clone(), right.clone())
                    .with_center(Some(center.clone()))
                    .with_output(Some(output.clone())),
                switches,
            )
        };
        assert_eq!(
            super::run_automatic(request(super::Switches::default())),
            Err(101)
        );
        let review = super::Switches {
            review_conflicts: true,
            ..super::Switches::default()
        };
        assert!(matches!(
            super::run_automatic(request(review)),
            Ok(Startup::Open(_))
        ));
        let favor = super::Switches {
            favor_left: true,
            ..super::Switches::default()
        };
        assert_eq!(super::run_automatic(request(favor)), Err(0));
        assert_eq!(
            std::fs::read_to_string(&output).unwrap_or_default(),
            "a\nL\nc\n"
        );
        assert_eq!(super::run_automatic(Startup::Home), Ok(Startup::Home));
    }

    #[test]
    fn a_named_kind_overrides_what_the_paths_suggest() {
        for arguments in [
            args(&["--kind", "hex-compare", "one.bin", "two.bin"]),
            args(&["--kind=hex-compare", "one.bin", "two.bin"]),
            args(&["one.bin", "--kind=hex-compare", "two.bin"]),
        ] {
            assert_eq!(
                parse(arguments, &probe(&[])),
                Startup::open(
                    SessionKind::HexCompare,
                    Path::new("one.bin"),
                    Path::new("two.bin"),
                )
            );
        }
    }

    #[test]
    fn a_named_kind_still_refuses_a_missing_path() {
        let startup = parse(
            args(&["--kind=hex-compare", "missing.bin", "two.bin"]),
            &probe(&[]),
        );
        assert!(matches!(startup, Startup::Rejected(_)));
    }

    #[test]
    fn an_unknown_kind_and_an_unknown_switch_are_refused() {
        assert!(matches!(
            parse(args(&["--kind=nonsense", "a", "b"]), &probe(&[])),
            Startup::Rejected(_)
        ));
        assert!(matches!(
            parse(args(&["--other", "a", "b"]), &probe(&[])),
            Startup::Rejected(_)
        ));
        assert!(matches!(
            parse(args(&["--kind"]), &probe(&[])),
            Startup::Rejected(_)
        ));
        assert!(matches!(
            parse(args(&["--kind=hex-compare"]), &probe(&[])),
            Startup::Rejected(_)
        ));
    }

    /// A kind the registry answers for has to be reachable from the command
    /// line, or a view exists that no switch opens.
    #[test]
    fn every_registered_kind_is_accepted_by_the_kind_switch() {
        for kind in crate::registry::available() {
            let switch = format!("--kind={}", kind.id());
            let startup = parse(args(&[&switch, "one", "two"]), &probe(&[]));
            match startup {
                Startup::Open(request) => assert_eq!(request.kind, kind),
                other => panic!("{} was refused: {other:?}", kind.id()),
            }
        }
    }

    #[test]
    fn the_edit_switch_opens_one_file_in_the_editor() {
        assert_eq!(
            parse(args(&["/edit", "notes.txt"]), &probe(&[])),
            Startup::open(SessionKind::TextEdit, Path::new("notes.txt"), Path::new(""))
        );
        assert_eq!(
            parse(args(&["notes.txt", "/EDIT"]), &probe(&[])),
            Startup::open(SessionKind::TextEdit, Path::new("notes.txt"), Path::new(""))
        );
        for refused in [
            &["/edit"][..],
            &["/edit", "a.txt", "b.txt"][..],
            &["/edit=yes", "a.txt"][..],
            &["/edit", "--kind=hex-compare", "a.txt"][..],
            &["/edit", "missing.txt"][..],
        ] {
            assert!(
                matches!(parse(args(refused), &probe(&[])), Startup::Rejected(_)),
                "{refused:?} was accepted"
            );
        }
    }

    #[test]
    fn a_patch_file_alone_opens_the_patch_view() {
        for name in ["fix.diff", "fix.PATCH"] {
            assert_eq!(
                parse(args(&[name]), &probe(&[])),
                Startup::open(SessionKind::TextPatch, Path::new(name), Path::new(""))
            );
        }
        assert!(matches!(
            parse(args(&["missing.diff"]), &probe(&[])),
            Startup::Rejected(_)
        ));
    }

    #[test]
    fn a_one_sided_kind_takes_one_path() {
        assert_eq!(
            parse(args(&["--kind=text-edit", "a.txt"]), &probe(&[])),
            Startup::open(SessionKind::TextEdit, Path::new("a.txt"), Path::new(""))
        );
        assert!(matches!(
            parse(args(&["--kind=hex-compare", "a.bin"]), &probe(&[])),
            Startup::Rejected(_)
        ));
    }

    #[test]
    fn a_path_that_starts_with_a_dash_is_still_a_path() {
        let startup = parse(args(&["-one.txt", "two.txt"]), &probe(&[]));
        assert_eq!(
            startup,
            Startup::open(
                SessionKind::TextCompare,
                Path::new("-one.txt"),
                Path::new("two.txt"),
            )
        );
    }
}
