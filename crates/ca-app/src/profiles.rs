//! The remote connection profile manager.
//!
//! Profile documents hold settings and secret references, never secret
//! values. A credential entry belongs in the operating system credential
//! store and is selected here by its reference name.

use std::fs;
use std::path::{Path, PathBuf};

use ca_ui::worker::{Job, Terminal};
use ca_vfs::remote::profile::{
    CloudProfile, FtpProfile, FtpProtocol, HttpAuthScheme, S3Auth, S3Profile, ServiceProfile,
    SubversionProfile, WebDavProfile,
};
use ca_vfs::{RemoteProfile, SecretRef};
use std::sync::Arc;

const PROFILE_FOLDER: &str = "profiles";
use ca_vfs::remote::profile::MAX_PROFILE_BYTES;

/// Editable profile collection and the currently selected profile.
#[allow(
    clippy::struct_excessive_bools,
    reason = "open, modal and exit-request states are independent"
)]
pub struct ProfileManager {
    open: bool,
    folder: PathBuf,
    profiles: Vec<RemoteProfile>,
    selected: Option<usize>,
    draft: RemoteProfile,
    saved_name: Option<String>,
    saved_draft: Option<RemoteProfile>,
    notice: Option<String>,
    confirm_delete: bool,
    confirm_discard: bool,
    discard_action: Option<DiscardAction>,
    notify: Arc<dyn Fn() + Send + Sync>,
    job: Option<Job<ProfileMessage>>,
    job_kind: Option<ProfileJobKind>,
    focus_requested: bool,
    exit_after_job: bool,
    exit_allowed: bool,
}

impl ProfileManager {
    /// Load the profiles below the application's settings directory.
    #[must_use]
    pub fn open(settings_directory: &Path, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::open_with_loader(settings_directory, notify, |folder| load_profiles(&folder))
    }

    fn open_with_loader(
        settings_directory: &Path,
        notify: Arc<dyn Fn() + Send + Sync>,
        loader: impl FnOnce(PathBuf) -> (Vec<RemoteProfile>, Option<String>) + Send + 'static,
    ) -> Self {
        let folder = settings_directory.join(PROFILE_FOLDER);
        let load_folder = folder.clone();
        let job_notify = Arc::clone(&notify);
        let job = Job::spawn_notifying(
            move |emitter, cancel| {
                if cancel.is_cancelled() {
                    return;
                }
                emitter.send(ProfileMessage::Loaded(loader(load_folder)));
            },
            job_notify,
        );
        let mut manager = Self {
            open: true,
            folder,
            profiles: Vec::new(),
            selected: None,
            draft: RemoteProfile::default(),
            saved_name: None,
            saved_draft: None,
            notice: Some("Loading saved profiles…".to_owned()),
            confirm_delete: false,
            confirm_discard: false,
            discard_action: None,
            notify,
            job: Some(job),
            job_kind: Some(ProfileJobKind::Loading),
            focus_requested: false,
            exit_after_job: false,
            exit_allowed: false,
        };
        manager.poll_job();
        manager
    }

    /// Draw the manager. Returns true after it closes.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let was_open = self.open;
        self.poll_job();
        if self.focus_requested {
            ctx.move_to_top(egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new("Remote Profiles"),
            ));
            self.focus_requested = false;
        }
        let mut is_open = self.open;
        egui::Window::new("Remote Profiles")
            .open(&mut is_open)
            .collapsible(false)
            .resizable(true)
            .default_width(720.0)
            .default_height(480.0)
            .show(ctx, |ui| self.contents(ui));
        self.open = is_open;

        if was_open
            && !self.open
            && matches!(
                self.job_kind,
                Some(ProfileJobKind::Saving | ProfileJobKind::Deleting)
            )
        {
            self.open = true;
            self.notice = Some("Please wait for the profile operation to finish.".to_owned());
        } else if was_open && !self.open && self.is_dirty() {
            self.open = true;
            self.confirm_discard = true;
            self.discard_action = Some(DiscardAction::Close);
        }
        self.confirmations(ctx);
        !self.open
    }

    /// Bring an already open manager window above other application windows.
    pub fn bring_to_front(&mut self) {
        self.focus_requested = true;
    }

    /// Ask whether the manager permits an application exit.
    ///
    /// A running operation delays the answer. An unwritten draft raises the
    /// same discard question as closing the manager window.
    pub fn request_exit(&mut self) -> bool {
        if self.job.is_some() {
            self.exit_after_job = true;
            self.notice = Some("Please wait for the profile operation to finish.".to_owned());
            return false;
        }
        if self.is_dirty() {
            self.confirm_discard = true;
            self.discard_action = Some(DiscardAction::Exit);
            return false;
        }
        true
    }

    /// Take approval produced by an answer to an exit request or a finished job.
    pub fn take_exit_allowed(&mut self) -> bool {
        std::mem::take(&mut self.exit_allowed)
    }

    /// Drop an exit request when the user continues working in the window.
    pub fn cancel_exit_request(&mut self) {
        self.exit_after_job = false;
        self.exit_allowed = false;
        if matches!(self.discard_action, Some(DiscardAction::Exit)) {
            self.confirm_discard = false;
            self.discard_action = None;
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        ui.label("Profiles contain connection settings and secret references. Secret values are kept in the credential store.");
        ui.label("Saved remote profiles cannot be opened as comparison sides yet.");
        ui.separator();
        let enabled = self.job.is_none();
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| self.profile_list(ui));
                ui.separator();
                ui.vertical(|ui| self.editor(ui));
            });
        });
        if let Some(notice) = &self.notice {
            ui.separator();
            ui.label(notice);
        }
    }

    fn profile_list(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(190.0);
        ui.heading("Saved profiles");
        egui::ScrollArea::vertical()
            .max_height(330.0)
            .show(ui, |ui| {
                let mut selected = None;
                for (index, profile) in self.profiles.iter().enumerate() {
                    let label = format!("{}  ·  {}", profile.name, service_label(&profile.service));
                    if ui
                        .selectable_label(self.selected == Some(index), label)
                        .clicked()
                    {
                        selected = Some(index);
                    }
                }
                if let Some(index) = selected {
                    self.request_select(index);
                }
            });
        ui.horizontal(|ui| {
            if ui.button("New").clicked() {
                self.request_new();
            }
            if ui
                .add_enabled(self.selected.is_some(), egui::Button::new("Delete"))
                .clicked()
            {
                self.confirm_delete = true;
            }
        });
    }

    fn editor(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(440.0);
        ui.heading(if self.saved_name.is_some() {
            "Profile settings"
        } else {
            "New profile"
        });
        egui::Grid::new("remote-profile-fields")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Profile name");
                if self.saved_name.is_some() {
                    ui.label(&self.draft.name);
                } else {
                    ui.text_edit_singleline(&mut self.draft.name);
                }
                ui.end_row();
                ui.label("Description");
                ui.text_edit_singleline(&mut self.draft.description);
                ui.end_row();
            });
        ui.separator();
        self.service_editor(ui);
        ui.separator();
        ui.horizontal(|ui| {
            let enabled = self.is_dirty();
            if ui
                .add_enabled(enabled, egui::Button::new("Save profile"))
                .clicked()
            {
                self.save();
            }
            if ui.button("Close").clicked() {
                self.request_close();
            }
        });
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the profile forms stay together so service changes have one consistent transition"
    )]
    fn service_editor(&mut self, ui: &mut egui::Ui) {
        let mut selected = profile_kind(&self.draft.service);
        let service_is_unknown = matches!(self.draft.service, ServiceProfile::Unknown(_));
        ui.add_enabled_ui(!service_is_unknown, |ui| {
            egui::ComboBox::from_label("Service")
                .selected_text(selected.label())
                .show_ui(ui, |ui| {
                    for kind in ProfileKind::ALL {
                        ui.selectable_value(&mut selected, kind, kind.label());
                    }
                });
        });
        change_service(&mut self.draft.service, selected);

        match &mut self.draft.service {
            ServiceProfile::Ftp(ftp) => {
                egui::Grid::new("profile-ftp-fields")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Protocol");
                        egui::ComboBox::from_id_salt("profile-ftp-protocol")
                            .selected_text(protocol_label(&ftp.login.protocol))
                            .show_ui(ui, |ui| {
                                for (protocol, label) in [
                                    (FtpProtocol::Ftp, "FTP"),
                                    (FtpProtocol::FtpsExplicit, "FTP over TLS (explicit)"),
                                    (FtpProtocol::FtpsImplicit, "FTP over TLS (implicit)"),
                                    (FtpProtocol::Sftp, "SFTP"),
                                ] {
                                    ui.selectable_value(&mut ftp.login.protocol, protocol, label);
                                }
                            });
                        ui.end_row();
                        text_field(ui, "Server", &mut ftp.login.host);
                        ui.label("Port");
                        let mut port = ftp
                            .login
                            .port
                            .unwrap_or_else(|| ftp.login.protocol.default_port())
                            .to_string();
                        if ui.text_edit_singleline(&mut port).changed() {
                            ftp.login.port = port.parse::<u16>().ok();
                        }
                        ui.end_row();
                        text_field(ui, "Username", &mut ftp.login.username);
                        text_field(ui, "Starting folder", &mut ftp.root_path);
                        text_field_ref(ui, "Password reference", &mut ftp.login.password);
                    });
                secret_note(ui);
            }
            ServiceProfile::WebDav(webdav) => {
                egui::Grid::new("profile-webdav-fields")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        text_field(ui, "Share URL", &mut webdav.url);
                        text_field(ui, "Username", &mut webdav.username);
                        text_field_ref(ui, "Password reference", &mut webdav.password);
                        ui.label("Authentication");
                        egui::ComboBox::from_id_salt("profile-webdav-auth")
                            .selected_text(auth_label(&webdav.auth))
                            .show_ui(ui, |ui| {
                                for (auth, label) in [
                                    (HttpAuthScheme::Negotiate, "Negotiate"),
                                    (HttpAuthScheme::Basic, "Basic"),
                                    (HttpAuthScheme::Digest, "Digest"),
                                    (HttpAuthScheme::None, "None"),
                                ] {
                                    ui.selectable_value(&mut webdav.auth, auth, label);
                                }
                            });
                        ui.end_row();
                    });
                secret_note(ui);
            }
            ServiceProfile::S3(s3) => {
                egui::Grid::new("profile-s3-fields")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        text_field(ui, "Bucket", &mut s3.bucket);
                        text_field(ui, "Region", &mut s3.region);
                        text_field(ui, "Service endpoint", &mut s3.endpoint);
                        ui.label("Path style");
                        ui.checkbox(&mut s3.path_style, "Use bucket in the URL path");
                        ui.end_row();
                        ui.label("Credentials");
                        let mut auth = s3_auth_kind(&s3.auth);
                        egui::ComboBox::from_id_salt("profile-s3-auth")
                            .selected_text(auth.label())
                            .show_ui(ui, |ui| {
                                for kind in S3AuthKind::ALL {
                                    ui.selectable_value(&mut auth, kind, kind.label());
                                }
                            });
                        if auth != s3_auth_kind(&s3.auth) {
                            s3.auth = auth.default_auth();
                        }
                        ui.end_row();
                        match &mut s3.auth {
                            S3Auth::Saved {
                                access_key_id,
                                secret_access_key,
                                session_token,
                                ..
                            } => {
                                text_field(ui, "Access key ID", access_key_id);
                                text_field_ref(ui, "Secret key reference", secret_access_key);
                                text_field_ref(ui, "Session token reference", session_token);
                            }
                            S3Auth::CredentialsFile {
                                path, profile_name, ..
                            } => {
                                text_field(ui, "Credentials file", path);
                                text_field(ui, "Section name", profile_name);
                            }
                            S3Auth::Environment { profile_name, .. } => {
                                text_field(ui, "Section name", profile_name);
                            }
                            S3Auth::Anonymous { .. } | S3Auth::Unknown(_) => {}
                        }
                    });
                secret_note(ui);
            }
            ServiceProfile::Dropbox(cloud) | ServiceProfile::OneDrive(cloud) => {
                egui::Grid::new("profile-cloud-fields")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        text_field(ui, "Account", &mut cloud.account);
                        text_field(ui, "Client ID", &mut cloud.client_id);
                        text_field_ref(ui, "Refresh token reference", &mut cloud.refresh_token);
                    });
                secret_note(ui);
                ui.label("Cloud connection support still needs the service registration and authentication flow.");
            }
            ServiceProfile::Subversion(svn) => {
                egui::Grid::new("profile-svn-fields")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        text_field(ui, "Repository URL", &mut svn.url);
                        text_field(ui, "Username", &mut svn.username);
                        ui.label("Revision");
                        let mut revision = svn.revision.map_or_else(String::new, |n| n.to_string());
                        if ui.text_edit_singleline(&mut revision).changed() {
                            svn.revision = if revision.trim().is_empty() {
                                None
                            } else {
                                revision.parse().ok()
                            };
                        }
                        ui.end_row();
                        ui.label("Password reference");
                        ui.label("Use the svn client's credential provider");
                        ui.end_row();
                    });
                ui.label("This profile is read only. Password references are refused; configure credentials in the svn client.");
            }
            ServiceProfile::Unknown(_) => {
                ui.label("This profile uses a service this build does not recognize. Its settings will be preserved when saved.");
            }
        }
    }

    fn select(&mut self, index: usize) {
        if self.job.is_some() {
            return;
        }
        let Some(profile) = self.profiles.get(index).cloned() else {
            return;
        };
        self.selected = Some(index);
        self.saved_name = Some(profile.name.clone());
        self.saved_draft = Some(profile.clone());
        self.draft = profile;
        self.notice = None;
        self.confirm_delete = false;
    }

    fn is_dirty(&self) -> bool {
        if let Some(saved) = &self.saved_draft {
            saved != &self.draft
        } else {
            !self.draft.name.is_empty()
                || !self.draft.description.is_empty()
                || self.draft.service != ServiceProfile::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn set_description_for_test(&mut self, description: &str) {
        self.draft.description = description.to_owned();
    }

    #[cfg(test)]
    pub(crate) fn is_dirty_for_test(&self) -> bool {
        self.is_dirty()
    }

    #[cfg(test)]
    pub(crate) fn is_busy_for_test(&self) -> bool {
        self.job.is_some()
    }

    fn request_select(&mut self, index: usize) {
        if self.job.is_some() || self.selected == Some(index) {
            return;
        }
        let Some(name) = self.profiles.get(index).map(|profile| profile.name.clone()) else {
            return;
        };
        if self.is_dirty() {
            self.confirm_discard = true;
            self.discard_action = Some(DiscardAction::Select(name));
        } else {
            self.select(index);
        }
    }

    fn select_named(&mut self, name: &str) {
        let Some(index) = self
            .profiles
            .iter()
            .position(|profile| profile.name == name)
        else {
            self.notice = Some("That profile is no longer in the saved list.".to_owned());
            return;
        };
        self.select(index);
    }

    fn discard_changes(&mut self) {
        self.confirm_discard = false;
        self.exit_after_job = false;
        match self.discard_action.take() {
            Some(DiscardAction::Close) => self.open = false,
            Some(DiscardAction::New) => self.new_draft(),
            Some(DiscardAction::Select(name)) => self.select_named(&name),
            Some(DiscardAction::Exit) => {
                if let Some(saved) = &self.saved_draft {
                    self.draft = saved.clone();
                } else {
                    self.draft = RemoteProfile::default();
                }
                self.exit_allowed = true;
            }
            None => {}
        }
    }

    fn keep_editing(&mut self) {
        self.confirm_discard = false;
        self.discard_action = None;
        self.exit_after_job = false;
    }

    fn request_new(&mut self) {
        if self.job.is_some() {
            return;
        }
        if self.is_dirty() {
            self.confirm_discard = true;
            self.discard_action = Some(DiscardAction::New);
        } else {
            self.new_draft();
        }
    }

    fn request_close(&mut self) {
        if self.job.is_some() {
            return;
        }
        if self.is_dirty() {
            self.confirm_discard = true;
            self.discard_action = Some(DiscardAction::Close);
        } else {
            self.open = false;
        }
    }

    fn new_draft(&mut self) {
        if self.job.is_some() {
            return;
        }
        self.selected = None;
        self.saved_name = None;
        self.saved_draft = None;
        self.draft = RemoteProfile::default();
        self.notice = None;
        self.confirm_delete = false;
    }

    fn save(&mut self) {
        if self.job.is_some() {
            return;
        }
        let folder = self.folder.clone();
        let draft = self.draft.clone();
        let saved_name = self.saved_name.clone();
        let worker_draft = draft.clone();
        let notify = Arc::clone(&self.notify);
        self.notice = Some("Saving profile…".to_owned());
        self.job_kind = Some(ProfileJobKind::Saving);
        self.job = Some(Job::spawn_notifying(
            move |emitter, cancel| {
                if cancel.is_cancelled() {
                    return;
                }
                let result = write_profile(&folder, &worker_draft, saved_name.as_deref())
                    .map(|()| load_profiles(&folder));
                emitter.send(ProfileMessage::Saved(Box::new(draft), result));
            },
            notify,
        ));
    }

    fn confirmations(&mut self, ctx: &egui::Context) {
        let actions_enabled = self.job.is_none();
        if self.confirm_delete {
            let mut answer = None;
            egui::Window::new("Delete profile?")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ca_ui::widgets::notice_current(
                        ui,
                        ca_ui::icons::Icon::Question,
                        24.0,
                        "Delete profile?",
                    );
                    ui.label(format!("Delete the profile ‘{}’?", self.draft.name));
                    ui.add_enabled_ui(actions_enabled, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("Delete").clicked() {
                                answer = Some(true);
                            }
                            if ui.button("Cancel").clicked() {
                                answer = Some(false);
                            }
                        });
                    });
                    if !actions_enabled {
                        ui.label("Please wait for the profile operation to finish.");
                    }
                });
            if let Some(yes) = answer {
                self.confirm_delete = false;
                if yes {
                    self.delete_selected();
                }
            }
        }
        if self.confirm_discard {
            let mut answer = None;
            egui::Window::new("Discard profile changes?")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ca_ui::widgets::notice_current(
                        ui,
                        ca_ui::icons::Icon::Question,
                        24.0,
                        "This profile has unsaved changes.",
                    );
                    ui.add_enabled_ui(actions_enabled, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("Discard and close").clicked() {
                                answer = Some(true);
                            }
                            if ui.button("Keep editing").clicked() {
                                answer = Some(false);
                            }
                        });
                    });
                    if !actions_enabled {
                        ui.label("Please wait for the profile operation to finish.");
                    }
                });
            if let Some(discard) = answer {
                if discard {
                    self.discard_changes();
                } else {
                    self.keep_editing();
                }
            }
        }
    }

    fn delete_selected(&mut self) {
        if self.job.is_some() {
            return;
        }
        let Some(name) = self.saved_name.clone() else {
            return;
        };
        let path = self.folder.join(format!("{name}.json"));
        let notify = Arc::clone(&self.notify);
        self.notice = Some("Deleting profile…".to_owned());
        self.job_kind = Some(ProfileJobKind::Deleting);
        self.job = Some(Job::spawn_notifying(
            move |emitter, cancel| {
                if cancel.is_cancelled() {
                    return;
                }
                let result = ca_io::remove_file(&path).map_err(|error| error.to_string());
                emitter.send(ProfileMessage::Deleted(name, result));
            },
            notify,
        ));
    }

    fn poll_job(&mut self) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            if matches!(
                &message,
                ProfileMessage::Saved(..) | ProfileMessage::Deleted(..)
            ) {
                self.confirm_discard = false;
                self.discard_action = None;
            }
            match message {
                ProfileMessage::Loaded((profiles, notice)) => {
                    self.profiles = profiles;
                    self.notice = notice;
                }
                ProfileMessage::Saved(profile, Ok((profiles, load_notice))) => {
                    let profile = *profile;
                    self.draft = profile.clone();
                    self.saved_name = Some(profile.name.clone());
                    self.saved_draft = Some(profile.clone());
                    self.profiles = profiles;
                    self.selected = self
                        .profiles
                        .iter()
                        .position(|item| item.name == profile.name);
                    self.notice = load_notice.or_else(|| Some("Profile saved.".to_owned()));
                }
                ProfileMessage::Saved(_, Err(error)) => self.notice = Some(error),
                ProfileMessage::Deleted(name, Ok(())) => {
                    self.profiles.retain(|profile| profile.name != name);
                    self.selected = None;
                    self.saved_name = None;
                    self.saved_draft = None;
                    self.draft = RemoteProfile::default();
                    self.notice = Some("Profile deleted.".to_owned());
                }
                ProfileMessage::Deleted(_, Err(error)) => {
                    self.notice = Some(format!("Could not delete the profile: {error}"));
                }
                ProfileMessage::Cancelled => {
                    self.notice = Some("The profile operation was stopped.".to_owned());
                }
                ProfileMessage::Failed(detail) => {
                    self.notice = Some(format!("The profile operation failed: {detail}"));
                }
            }
        }
        if finished {
            self.job = None;
            self.job_kind = None;
            if self.exit_after_job {
                self.exit_after_job = false;
                self.exit_allowed = self.request_exit();
            }
        }
    }
}

enum ProfileMessage {
    Loaded((Vec<RemoteProfile>, Option<String>)),
    Saved(
        Box<RemoteProfile>,
        Result<(Vec<RemoteProfile>, Option<String>), String>,
    ),
    Deleted(String, Result<(), String>),
    Cancelled,
    Failed(String),
}

#[derive(Clone, Copy)]
enum ProfileJobKind {
    Loading,
    Saving,
    Deleting,
}

impl Terminal for ProfileMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Cancelled
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

enum DiscardAction {
    Close,
    New,
    Select(String),
    Exit,
}

fn text_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.text_edit_singleline(value);
    ui.end_row();
}

fn text_field_ref(ui: &mut egui::Ui, label: &str, value: &mut SecretRef) {
    ui.label(label);
    let mut reference = value.id().to_owned();
    if ui.text_edit_singleline(&mut reference).changed() {
        *value = SecretRef::new(reference);
    }
    ui.end_row();
}

fn secret_note(ui: &mut egui::Ui) {
    ui.small(
        "Enter a credential-store reference name only. Secret values are not shown or saved here.",
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProfileKind {
    Ftp,
    WebDav,
    S3,
    Dropbox,
    OneDrive,
    Subversion,
    Unknown,
}

impl ProfileKind {
    const ALL: [Self; 6] = [
        Self::Ftp,
        Self::WebDav,
        Self::S3,
        Self::Dropbox,
        Self::OneDrive,
        Self::Subversion,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Ftp => "FTP / FTPS / SFTP",
            Self::WebDav => "WebDAV",
            Self::S3 => "S3 compatible storage",
            Self::Dropbox => "Dropbox",
            Self::OneDrive => "OneDrive",
            Self::Subversion => "Subversion (read only)",
            Self::Unknown => "Unknown service",
        }
    }

    fn default_service(self) -> ServiceProfile {
        match self {
            Self::Ftp => ServiceProfile::Ftp(FtpProfile::default()),
            Self::WebDav => ServiceProfile::WebDav(WebDavProfile::default()),
            Self::S3 => ServiceProfile::S3(S3Profile::default()),
            Self::Dropbox => ServiceProfile::Dropbox(CloudProfile::default()),
            Self::OneDrive => ServiceProfile::OneDrive(CloudProfile::default()),
            Self::Subversion => ServiceProfile::Subversion(SubversionProfile::default()),
            Self::Unknown => ServiceProfile::Unknown(ca_vfs::remote::profile::Unknown::new()),
        }
    }
}

fn profile_kind(service: &ServiceProfile) -> ProfileKind {
    match service {
        ServiceProfile::Ftp(_) => ProfileKind::Ftp,
        ServiceProfile::WebDav(_) => ProfileKind::WebDav,
        ServiceProfile::S3(_) => ProfileKind::S3,
        ServiceProfile::Dropbox(_) => ProfileKind::Dropbox,
        ServiceProfile::OneDrive(_) => ProfileKind::OneDrive,
        ServiceProfile::Subversion(_) => ProfileKind::Subversion,
        ServiceProfile::Unknown(_) => ProfileKind::Unknown,
    }
}

fn change_service(service: &mut ServiceProfile, selected: ProfileKind) -> bool {
    if matches!(service, ServiceProfile::Unknown(_)) || selected == profile_kind(service) {
        return false;
    }
    *service = selected.default_service();
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum S3AuthKind {
    Saved,
    CredentialsFile,
    Environment,
    Anonymous,
    Unknown,
}

impl S3AuthKind {
    const ALL: [Self; 4] = [
        Self::Saved,
        Self::CredentialsFile,
        Self::Environment,
        Self::Anonymous,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Saved => "Saved key references",
            Self::CredentialsFile => "Credentials file",
            Self::Environment => "Environment / default file",
            Self::Anonymous => "Anonymous",
            Self::Unknown => "Unknown",
        }
    }

    fn default_auth(self) -> S3Auth {
        match self {
            Self::Saved => S3Auth::Saved {
                access_key_id: String::new(),
                secret_access_key: SecretRef::default(),
                session_token: SecretRef::default(),
                unknown: ca_vfs::remote::profile::Unknown::default(),
            },
            Self::CredentialsFile => S3Auth::CredentialsFile {
                path: String::new(),
                profile_name: String::new(),
                unknown: ca_vfs::remote::profile::Unknown::default(),
            },
            Self::Environment => S3Auth::Environment {
                profile_name: String::new(),
                unknown: ca_vfs::remote::profile::Unknown::default(),
            },
            Self::Anonymous => S3Auth::Anonymous {
                unknown: ca_vfs::remote::profile::Unknown::default(),
            },
            Self::Unknown => S3Auth::Unknown(ca_vfs::remote::profile::Unknown::new()),
        }
    }
}

fn s3_auth_kind(auth: &S3Auth) -> S3AuthKind {
    match auth {
        S3Auth::Saved { .. } => S3AuthKind::Saved,
        S3Auth::CredentialsFile { .. } => S3AuthKind::CredentialsFile,
        S3Auth::Environment { .. } => S3AuthKind::Environment,
        S3Auth::Anonymous { .. } => S3AuthKind::Anonymous,
        S3Auth::Unknown(_) => S3AuthKind::Unknown,
    }
}

fn protocol_label(protocol: &FtpProtocol) -> &'static str {
    match protocol {
        FtpProtocol::Ftp => "FTP",
        FtpProtocol::FtpsExplicit => "FTP over TLS (explicit)",
        FtpProtocol::FtpsImplicit => "FTP over TLS (implicit)",
        FtpProtocol::Sftp => "SFTP",
        FtpProtocol::Unknown(_) => "Unknown",
    }
}

fn auth_label(auth: &HttpAuthScheme) -> &'static str {
    match auth {
        HttpAuthScheme::Negotiate => "Negotiate",
        HttpAuthScheme::Basic => "Basic",
        HttpAuthScheme::Digest => "Digest",
        HttpAuthScheme::None => "None",
        HttpAuthScheme::Unknown(_) => "Unknown",
    }
}

fn service_label(service: &ServiceProfile) -> &'static str {
    profile_kind(service).label()
}

fn load_profiles(folder: &Path) -> (Vec<RemoteProfile>, Option<String>) {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), None),
        Err(error) => {
            return (
                Vec::new(),
                Some(format!("Could not read profiles: {error}")),
            )
        }
    };
    let mut profiles = Vec::new();
    let mut skipped = 0usize;
    for entry in entries {
        let Ok(entry) = entry else {
            skipped += 1;
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            skipped += 1;
            continue;
        };
        let path = entry.path();
        if !kind.is_file() || path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            skipped += 1;
            continue;
        };
        if metadata.len() > MAX_PROFILE_BYTES {
            skipped += 1;
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            skipped += 1;
            continue;
        };
        let Ok(profile) = serde_json::from_str::<RemoteProfile>(&text) else {
            skipped += 1;
            continue;
        };
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            skipped += 1;
            continue;
        };
        if stem != profile.name || validate_name(&profile.name).is_err() {
            skipped += 1;
            continue;
        }
        profiles.push(profile);
    }
    profiles.sort_by_key(|profile| profile.name.to_lowercase());
    let notice = (skipped > 0).then(|| {
        format!(
            "{skipped} profile document(s) could not be loaded; those files were left untouched."
        )
    });
    (profiles, notice)
}

fn write_profile(
    folder: &Path,
    profile: &RemoteProfile,
    original_name: Option<&str>,
) -> Result<(), String> {
    validate_name(&profile.name)?;
    let destination = folder.join(format!("{}.json", profile.name));
    if original_name != Some(profile.name.as_str()) && destination.exists() {
        return Err(
            "A profile document with this name already exists; it was not replaced.".to_owned(),
        );
    }
    fs::create_dir_all(folder)
        .map_err(|error| format!("Could not create the profiles folder: {error}"))?;
    let contents = serde_json::to_vec_pretty(profile)
        .map_err(|error| format!("Could not encode this profile: {error}"))?;
    if contents.len() as u64 > MAX_PROFILE_BYTES {
        return Err("The profile document is larger than the 1 MiB limit.".to_owned());
    }
    ca_io::write_atomic_private(&destination, &contents)
        .map_err(|error| format!("Could not save the profile: {error}"))
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 253
        || name.starts_with('.')
        || name.contains("..")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("Use a profile name of up to 253 letters, digits, dots, underscores or hyphens; it cannot start with a dot or contain two dots in a row.".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "test setup and assertions use unwrap for concise failures"
    )]

    use super::{
        change_service, load_profiles, validate_name, write_profile, ProfileJobKind, ProfileKind,
        ProfileManager, ProfileMessage,
    };
    use ca_vfs::{RemoteProfile, ServiceProfile};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    fn wait_for_job(manager: &mut ProfileManager) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while manager.job.is_some() && Instant::now() < deadline {
            manager.poll_job();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(manager.job.is_none(), "the profile job did not finish");
    }

    #[test]
    fn profile_names_cannot_escape_the_profiles_folder() {
        for name in ["", "..", ".hidden", "a..b", "../outside", "a/b", "a\\b"] {
            assert!(validate_name(name).is_err(), "{name:?}");
        }
        assert!(validate_name("build-server_1.example").is_ok());
    }

    #[test]
    fn profile_documents_round_trip_without_secret_values() {
        let folder = tempfile::tempdir().unwrap();
        let password_reference = ca_vfs::SecretRef::new("ci/build-server/password");
        let profile = RemoteProfile {
            name: "build-server".to_owned(),
            description: "CI server".to_owned(),
            service: ServiceProfile::Ftp(ca_vfs::remote::profile::FtpProfile {
                login: ca_vfs::remote::profile::FtpLogin {
                    password: password_reference.clone(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            unknown: ca_vfs::remote::profile::Unknown::default(),
        };
        let document = serde_json::to_value(&profile).unwrap();
        assert_eq!(
            document.pointer("/service/login/password"),
            Some(&serde_json::json!(password_reference.id()))
        );
        write_profile(folder.path(), &profile, None).unwrap();
        let (loaded, warning) = load_profiles(folder.path());
        assert_eq!(loaded, [profile]);
        assert_eq!(warning, None);
        let text = std::fs::read_to_string(folder.path().join("build-server.json")).unwrap();
        assert!(text.contains(password_reference.id()));
    }

    /// The editor's save worker stores each credential identifier at its exact
    /// field, retains it through known edits, and never needs credential values.
    #[test]
    fn profile_save_worker_preserves_all_credential_reference_fields() {
        let cases = [
            (
                serde_json::json!({"kind":"ftp","login":{"protocol":"ftp","password":"login-ref"},"global":{"ssh_private_key_passphrase":"key-ref"}}),
                vec![
                    ("/service/login/password", "login-ref"),
                    ("/service/global/ssh_private_key_passphrase", "key-ref"),
                ],
            ),
            (
                serde_json::json!({"kind":"ftp","login":{"protocol":"sftp","password":"ssh-ref"},"global":{"ssh_private_key_passphrase":"ssh-key-ref"}}),
                vec![
                    ("/service/login/password", "ssh-ref"),
                    ("/service/global/ssh_private_key_passphrase", "ssh-key-ref"),
                ],
            ),
            (
                serde_json::json!({"kind":"web_dav","password":"web-ref"}),
                vec![("/service/password", "web-ref")],
            ),
            (
                serde_json::json!({"kind":"s3","auth":{"source":"saved","access_key_id":"access-id","secret_access_key":"access-ref","session_token":"token-ref"}}),
                vec![
                    ("/service/auth/secret_access_key", "access-ref"),
                    ("/service/auth/session_token", "token-ref"),
                ],
            ),
            (
                serde_json::json!({"kind":"subversion","password":"svn-ref"}),
                vec![("/service/password", "svn-ref")],
            ),
            (
                serde_json::json!({"kind":"dropbox","refresh_token":"dropbox-ref"}),
                vec![("/service/refresh_token", "dropbox-ref")],
            ),
            (
                serde_json::json!({"kind":"one_drive","refresh_token":"drive-ref"}),
                vec![("/service/refresh_token", "drive-ref")],
            ),
        ];
        let folder = tempfile::tempdir().unwrap();
        let mut manager = ProfileManager::open(folder.path(), Arc::new(|| {}));
        wait_for_job(&mut manager);
        for (index, (service, references)) in cases.into_iter().enumerate() {
            manager.new_draft();
            let name = format!("credential-{index}");
            manager.draft = serde_json::from_value(serde_json::json!({
                "name":name, "description":"before", "service":service,
            }))
            .unwrap();
            assert!(!matches!(manager.draft.service, ServiceProfile::Unknown(_)));
            let expected = manager.draft.clone();
            manager.save();
            wait_for_job(&mut manager);
            assert_eq!(manager.saved_draft.as_ref(), Some(&expected));
            manager.draft.description = "edited without resolving credentials".to_owned();
            manager.save();
            wait_for_job(&mut manager);
            let bytes = std::fs::read(manager.folder.join(format!("{name}.json"))).unwrap();
            let stored: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            for (path, reference) in references {
                assert_eq!(
                    stored.pointer(path).and_then(serde_json::Value::as_str),
                    Some(reference),
                    "{path}"
                );
            }
            assert_eq!(
                stored["description"],
                "edited without resolving credentials"
            );
            let loaded: RemoteProfile = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(loaded.service, expected.service);
        }
    }

    #[test]
    fn create_does_not_replace_an_unrecognized_document() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("build-server.json");
        std::fs::write(&path, "preserve this unknown document").unwrap();
        let profile = RemoteProfile {
            name: "build-server".to_owned(),
            service: ServiceProfile::default(),
            ..RemoteProfile::default()
        };
        assert!(write_profile(folder.path(), &profile, None).is_err());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "preserve this unknown document"
        );
    }

    #[test]
    fn a_profile_with_an_unknown_service_cannot_be_changed_to_a_known_service() {
        let mut service = ServiceProfile::Unknown(ca_vfs::remote::profile::Unknown::new());
        assert!(!change_service(&mut service, ProfileKind::Ftp));
        assert!(matches!(service, ServiceProfile::Unknown(_)));
    }

    #[test]
    fn profile_listing_runs_on_a_worker_before_the_editor_populates() {
        let folder = tempfile::tempdir().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut manager = super::ProfileManager::open_with_loader(
            folder.path(),
            std::sync::Arc::new(|| {}),
            move |_folder| {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                (Vec::new(), None)
            },
        );

        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(manager.job.is_some());
        assert!(manager.profiles.is_empty());
        assert_eq!(manager.notice.as_deref(), Some("Loading saved profiles…"));

        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while manager.job.is_some() && Instant::now() < deadline {
            manager.poll_job();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(manager.job.is_none(), "the profile load job did not finish");
        assert!(manager.notice.is_none());
    }

    #[test]
    fn a_delete_or_discard_confirmation_cannot_replace_a_save_job() {
        use ca_ui::worker::Job;

        let folder = tempfile::tempdir().unwrap();
        let first = RemoteProfile {
            name: "alpha".to_owned(),
            description: "before".to_owned(),
            ..RemoteProfile::default()
        };
        let second = RemoteProfile {
            name: "beta".to_owned(),
            ..RemoteProfile::default()
        };
        let profile_folder = folder.path().join(super::PROFILE_FOLDER);
        write_profile(&profile_folder, &first, None).unwrap();
        write_profile(&profile_folder, &second, None).unwrap();

        let mut manager = ProfileManager::open(folder.path(), Arc::new(|| {}));
        wait_for_job(&mut manager);
        manager.select(0);
        manager.draft.description = "saved while a confirmation is open".to_owned();
        let saving_profile = manager.draft.clone();
        let worker_folder = manager.folder.clone();
        let original_name = manager.saved_name.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        manager.job = Some(Job::spawn_notifying(
            move |emitter, cancel| {
                started_tx.send(()).unwrap();
                if release_rx.recv_timeout(Duration::from_secs(2)).is_err() || cancel.is_cancelled()
                {
                    return;
                }
                let result =
                    write_profile(&worker_folder, &saving_profile, original_name.as_deref())
                        .map(|()| load_profiles(&worker_folder));
                emitter.send(ProfileMessage::Saved(Box::new(saving_profile), result));
            },
            Arc::new(|| {}),
        ));
        manager.job_kind = Some(ProfileJobKind::Saving);
        manager.notice = Some("Saving profile…".to_owned());
        manager.confirm_delete = true;
        manager.confirm_discard = true;
        manager.discard_action = Some(super::DiscardAction::Select("beta".to_owned()));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        manager.delete_selected();
        manager.select(1);
        manager.new_draft();

        assert!(manager.job.is_some());
        assert!(matches!(manager.job_kind, Some(ProfileJobKind::Saving)));
        assert_eq!(manager.selected, Some(0));
        assert_eq!(
            manager.draft.description,
            "saved while a confirmation is open"
        );
        assert_eq!(manager.notice.as_deref(), Some("Saving profile…"));

        release_tx.send(()).unwrap();
        wait_for_job(&mut manager);
        let (profiles, warning) = load_profiles(&profile_folder);
        assert_eq!(warning, None);
        assert_eq!(
            profiles
                .iter()
                .find(|profile| profile.name == "alpha")
                .map(|profile| profile.description.as_str()),
            Some("saved while a confirmation is open")
        );
        assert_eq!(manager.notice.as_deref(), Some("Profile saved."));
        assert!(!manager.confirm_discard);
        assert!(manager.discard_action.is_none());
    }

    #[test]
    fn exit_waits_for_a_profile_save_and_continues_when_it_finishes() {
        use ca_ui::worker::Job;

        let folder = tempfile::tempdir().unwrap();
        let profile_folder = folder.path().join(super::PROFILE_FOLDER);
        let saved = RemoteProfile {
            name: "alpha".to_owned(),
            description: "before".to_owned(),
            ..RemoteProfile::default()
        };
        write_profile(&profile_folder, &saved, None).unwrap();
        let mut manager = ProfileManager::open(folder.path(), Arc::new(|| {}));
        wait_for_job(&mut manager);
        manager.select(0);
        manager.draft.description = "saved".to_owned();
        let saving_profile = manager.draft.clone();
        let worker_folder = manager.folder.clone();
        let original_name = manager.saved_name.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        manager.job = Some(Job::spawn_notifying(
            move |emitter, cancel| {
                started_tx.send(()).unwrap();
                if release_rx.recv_timeout(Duration::from_secs(2)).is_err() || cancel.is_cancelled()
                {
                    return;
                }
                let result =
                    write_profile(&worker_folder, &saving_profile, original_name.as_deref())
                        .map(|()| load_profiles(&worker_folder));
                emitter.send(ProfileMessage::Saved(Box::new(saving_profile), result));
            },
            Arc::new(|| {}),
        ));
        manager.job_kind = Some(ProfileJobKind::Saving);
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        assert!(!manager.request_exit());
        assert_eq!(
            manager.notice.as_deref(),
            Some("Please wait for the profile operation to finish.")
        );
        assert!(!manager.take_exit_allowed());

        release_tx.send(()).unwrap();
        wait_for_job(&mut manager);
        assert!(!manager.is_dirty());
        assert!(manager.take_exit_allowed());
    }

    #[test]
    fn profile_actions_come_back_in_the_poll_that_delivers_the_answer() {
        let folder = tempfile::tempdir().unwrap();
        let mut manager = ProfileManager::open(folder.path(), Arc::new(|| {}));
        wait_for_job(&mut manager);
        let (job, _held) = ca_ui::testing::job_held_after(vec![ProfileMessage::Cancelled]);
        manager.job = Some(job);
        manager.job_kind = Some(ProfileJobKind::Saving);

        manager.poll_job();

        assert!(
            manager.job.is_none(),
            "the profile actions stay off for a job that has answered"
        );
        assert!(manager.job_kind.is_none());
        assert_eq!(
            manager.notice.as_deref(),
            Some("The profile operation was stopped.")
        );
    }

    #[test]
    fn exit_asks_before_dropping_an_unsaved_profile_draft() {
        let folder = tempfile::tempdir().unwrap();
        let mut manager = ProfileManager::open(folder.path(), Arc::new(|| {}));
        wait_for_job(&mut manager);
        manager.draft.name = "draft".to_owned();
        manager.draft.description = "unsaved".to_owned();

        assert!(!manager.request_exit());
        assert!(manager.confirm_discard);
        assert!(matches!(
            manager.discard_action,
            Some(super::DiscardAction::Exit)
        ));
        assert!(!manager.take_exit_allowed());

        manager.discard_changes();
        assert!(manager.take_exit_allowed());
        assert!(!manager.is_dirty());
    }

    #[test]
    fn a_pending_select_tracks_the_profile_name_across_a_list_change() {
        let folder = tempfile::tempdir().unwrap();
        let profile_folder = folder.path().join(super::PROFILE_FOLDER);
        for name in ["b", "c"] {
            write_profile(
                &profile_folder,
                &RemoteProfile {
                    name: name.to_owned(),
                    ..RemoteProfile::default()
                },
                None,
            )
            .unwrap();
        }
        let mut manager = ProfileManager::open(folder.path(), Arc::new(|| {}));
        wait_for_job(&mut manager);
        manager.new_draft();
        manager.draft.name = "a".to_owned();
        manager.request_select(1);
        assert!(matches!(
            &manager.discard_action,
            Some(super::DiscardAction::Select(name)) if name == "c"
        ));

        manager.profiles.insert(
            0,
            RemoteProfile {
                name: "a".to_owned(),
                ..RemoteProfile::default()
            },
        );
        manager.discard_changes();
        assert_eq!(manager.saved_name.as_deref(), Some("c"));
        assert_eq!(manager.selected, Some(2));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn profile_save_preserves_unknown_values_and_applies_known_edits() {
        let folder = tempfile::tempdir().unwrap();
        let fixtures = [
            (
                "future-ftp",
                r#"{"name":"future-ftp","description":"old","service":{"kind":"ftp","login":{"protocol":"ftps_ccc"},"server":{"encoding":"utf32"},"connection":{"min_tls_version":"tls14","max_tls_version":"tls15","address_family":"ipv7"},"proxy":{"kind":"wireguard"},"listing":{"link_resolution":"recursive_probe"},"transfer":{"transfer_type":"chunked"}}}"#,
                &[
                    "/service/login/protocol",
                    "/service/server/encoding",
                    "/service/connection/min_tls_version",
                    "/service/connection/max_tls_version",
                    "/service/connection/address_family",
                    "/service/proxy/kind",
                    "/service/listing/link_resolution",
                    "/service/transfer/transfer_type",
                ][..],
            ),
            (
                "future-webdav",
                r#"{"name":"future-webdav","description":"old","service":{"kind":"web_dav","auth":"sso","tls":{"min_version":"tls14","max_version":"tls15"}}}"#,
                &[
                    "/service/auth",
                    "/service/tls/min_version",
                    "/service/tls/max_version",
                ][..],
            ),
            (
                "future-s3-auth",
                r#"{"name":"future-s3-auth","description":"old","service":{"kind":"s3","auth":{"source":"sso","role":"arn:example","start_url":"https://login.example","options":{"interactive":true}}}}"#,
                &["/service/auth/source"][..],
            ),
            (
                "future-s3-saved",
                r#"{"name":"future-s3-saved","description":"old","service":{"kind":"s3","auth":{"source":"saved","access_key_id":"AKIAEXAMPLE","secret_access_key":"credential/access"},"tls":{"min_version":"tls14","max_version":"tls15"}}}"#,
                &[
                    "/service/auth/source",
                    "/service/tls/min_version",
                    "/service/tls/max_version",
                ][..],
            ),
            (
                "future-s3-file",
                r#"{"name":"future-s3-file","description":"old","service":{"kind":"s3","auth":{"source":"credentials_file","path":"C:/credentials","profile_name":"build"}}}"#,
                &["/service/auth/source"][..],
            ),
            (
                "future-s3-environment",
                r#"{"name":"future-s3-environment","description":"old","service":{"kind":"s3","auth":{"source":"environment","profile_name":"build"}}}"#,
                &["/service/auth/source"][..],
            ),
            (
                "future-s3-anonymous",
                r#"{"name":"future-s3-anonymous","description":"old","service":{"kind":"s3","auth":{"source":"anonymous"}}}"#,
                &["/service/auth/source"][..],
            ),
            (
                "future-dropbox",
                r#"{"name":"future-dropbox","description":"old","service":{"kind":"dropbox","account":"account-7","refresh_token":"secret/dropbox","client_id":"client"}}"#,
                &[][..],
            ),
            (
                "future-one-drive",
                r#"{"name":"future-one-drive","description":"old","service":{"kind":"one_drive","account":"account-8","refresh_token":"secret/onedrive","client_id":"client"}}"#,
                &[][..],
            ),
            (
                "future-subversion",
                r#"{"name":"future-subversion","description":"old","service":{"kind":"subversion","url":"https://svn.example/repository","revision":12,"username":"builder","password":"secret/svn"}}"#,
                &[][..],
            ),
            (
                "future-service",
                r#"{"name":"future-service","description":"old","service":{"kind":"quantum_drive","endpoint":{"region":"moon","replicas":[1,3]},"mode":"future"}}"#,
                &["/service"][..],
            ),
        ];
        let profile_folder = folder.path().join("profiles");
        std::fs::create_dir_all(&profile_folder).unwrap();
        let mut saved_documents = Vec::new();
        for (name, text, enum_paths) in fixtures {
            let path = profile_folder.join(format!("{name}.json"));
            let mut original: serde_json::Value = serde_json::from_str(text).unwrap();
            let mut extra_paths = Vec::new();
            add_forward_fields(&mut original, "", &mut extra_paths);
            std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
            saved_documents.push((name, path, original, enum_paths, extra_paths));
        }
        let mut manager = ProfileManager::open(folder.path(), Arc::new(|| {}));
        wait_for_job(&mut manager);
        let context = egui::Context::default();
        for (name, _, _, _, _) in &saved_documents {
            let index = manager
                .profiles
                .iter()
                .position(|profile| profile.name == *name)
                .unwrap();
            manager.select(index);
            let _ = context.run(egui::RawInput::default(), |ctx| {
                manager.show(ctx);
            });
            manager.draft.description = "edited".to_owned();
            manager.save();
            wait_for_job(&mut manager);
        }
        for (name, path, original, enum_paths, extra_paths) in saved_documents {
            let written: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(
                written.pointer("/description"),
                Some(&serde_json::json!("edited"))
            );
            for pointer in enum_paths
                .iter()
                .copied()
                .chain(extra_paths.iter().map(String::as_str))
            {
                assert_eq!(
                    written.pointer(pointer),
                    original.pointer(pointer),
                    "unknown value at {pointer} changed in {name}"
                );
            }
        }
    }

    fn add_forward_fields(value: &mut serde_json::Value, path: &str, pointers: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(object) => {
                let keys: Vec<String> = object.keys().cloned().collect();
                for key in keys {
                    let escaped = key.replace('~', "~0").replace('/', "~1");
                    if let Some(child) = object.get_mut(&key) {
                        add_forward_fields(child, &format!("{path}/{escaped}"), pointers);
                    }
                }
                object.insert(
                    "future_leaf".to_owned(),
                    serde_json::from_str(
                        r#"{"nested":[1,"x",{"deep":true}],"n":1234567890123456789012345678901234567890}"#,
                    )
                    .unwrap(),
                );
                pointers.push(format!("{path}/future_leaf"));
            }
            serde_json::Value::Array(items) => {
                for (index, child) in items.iter_mut().enumerate() {
                    add_forward_fields(child, &format!("{path}/{index}"), pointers);
                }
            }
            _ => {}
        }
    }
}
