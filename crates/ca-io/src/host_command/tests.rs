#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::{clean_environment, host_command, Environment, Image};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

const MOUNT: &str = "/tmp/.mount_comparFcHnCN";

fn unresolved(_: &Path) -> Option<PathBuf> {
    None
}

fn environment(pairs: &[(&str, &str)]) -> Environment {
    pairs
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
}

fn names_inside(environment: &Environment, root: &str) -> Vec<String> {
    environment
        .iter()
        .filter(|(_, value)| value.to_string_lossy().contains(root))
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect()
}

/// A host child of a normal start (`AppRun`, FUSE mount): what the `date`
/// child of a folder compare received.
fn apprun_child() -> Environment {
    environment(&[
        ("USER", "tester"),
        ("SHARUN_DIR", MOUNT),
        ("HOME", "/home/tester"),
        ("APPDIR", MOUNT),
        ("HOST_XDG_CACHE_HOME", "/home/tester/.cache"),
        ("HOST_KERNEL_VERSION", "6.6.87"),
        ("HOST_XDG_STATE_HOME", "/home/tester/.local/state"),
        ("APPIMAGE_ARCH", "x86_64"),
        ("LOGNAME", "tester"),
        ("OWD", "/home/tester"),
        ("HOSTPATH", "/opt/spy:/usr/local/bin:/usr/bin:/bin"),
        (
            "PATH",
            "/tmp/.mount_comparFcHnCN/bin:/opt/spy:/usr/local/bin:/usr/bin:/bin",
        ),
        ("APPIMAGE", "/home/tester/compare-all.AppImage"),
        ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ("DISPLAY", ":99"),
        ("LANG", "C"),
        ("HOST_XDG_CONFIG_HOME", "/home/tester/.config"),
        ("HOST_XDG_DATA_HOME", "/home/tester/.local/share"),
        ("SHELL", "/bin/sh"),
        ("ARGV0", "/home/tester/compare-all.AppImage"),
        ("HOST_HOME", "/home/tester"),
        ("APPIMAGE_UID", "1000"),
        ("PWD", "/home/tester"),
        ("CROSS_LIBC_DLOPEN_ROOT", MOUNT),
        (
            "XDG_DATA_DIRS",
            "/tmp/.mount_comparFcHnCN/share:/home/tester/.local/share:/usr/local/share:/usr/share:/run/opengl-driver/share:/run/current-system/sw/share:/etc",
        ),
        ("TERMINFO", "/tmp/.mount_comparFcHnCN/share/terminfo"),
        (
            "AMDGPU_ASIC_ID_TABLE_PATHS",
            "/usr/local/share/libdrm:/usr/share/libdrm:/tmp/.mount_comparFcHnCN/share/libdrm",
        ),
    ])
}

/// A host child of `bin/ca` or the extracted `bin/compare-all`, where no
/// `APPDIR` is set and the preload removes nothing.
fn sharun_child() -> Environment {
    environment(&[
        (
            "PATH",
            "/tmp/.mount_comparOIliDD/bin:/opt/spy:/usr/local/bin:/usr/bin:/bin",
        ),
        ("HOME", "/home/tester"),
        ("USER", "tester"),
        ("LOGNAME", "tester"),
        ("SHELL", "/bin/sh"),
        ("DISPLAY", ":99"),
        ("LANG", "C"),
        ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ("SHARUN_DIR", "/tmp/.mount_comparOIliDD"),
        ("CROSS_LIBC_DLOPEN_ROOT", "/tmp/.mount_comparOIliDD"),
        (
            "GBM_BACKENDS_PATH",
            "/tmp/.mount_comparOIliDD/lib/gbm:/usr/lib/x86_64-linux-gnu/gbm:/usr/lib64/gbm:/usr/lib/gbm:/run/opengl-driver/lib/gbm",
        ),
        ("LIBGL_DRIVERS_PATH", "/tmp/.mount_comparOIliDD/lib/dri"),
        ("LIBVA_DRIVERS_PATH", "/tmp/.mount_comparOIliDD/lib/dri"),
        ("GCONV_PATH", "/tmp/.mount_comparOIliDD/lib/gconv"),
        (
            "XDG_DATA_DIRS",
            "/tmp/.mount_comparOIliDD/share:/home/tester/.local/share:/usr/local/share:/usr/share:/run/opengl-driver/share:/run/current-system/sw/share:/etc",
        ),
        ("TERMINFO", "/tmp/.mount_comparOIliDD/share/terminfo"),
        (
            "__EGL_VENDOR_LIBRARY_DIRS",
            "/tmp/.mount_comparOIliDD/share/glvnd/egl_vendor.d:/usr/share/glvnd/egl_vendor.d:/etc/glvnd/egl_vendor.d",
        ),
        (
            "AMDGPU_ASIC_ID_TABLE_PATHS",
            "/usr/local/share/libdrm:/usr/share/libdrm:/tmp/.mount_comparOIliDD/share/libdrm",
        ),
        ("LD_LIBRARY_PATH", "/tmp/.mount_comparOIliDD/lib"),
        ("LD_PRELOAD", "anylinux.so"),
    ])
}

fn clean(environment: &Environment) -> Environment {
    let image = Image::detect(environment, None, &unresolved)
        .expect("an image environment")
        .with_triplet(Some("x86_64-linux-gnu"));
    clean_environment(environment, &image)
}

#[test]
fn a_host_child_of_a_normal_start_gets_no_image_variable() {
    let cleaned = clean(&apprun_child());
    assert_eq!(names_inside(&cleaned, MOUNT), Vec::<String>::new());
    assert_eq!(
        cleaned,
        environment(&[
            ("USER", "tester"),
            ("HOME", "/home/tester"),
            ("LOGNAME", "tester"),
            ("PATH", "/opt/spy:/usr/local/bin:/usr/bin:/bin"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("DISPLAY", ":99"),
            ("LANG", "C"),
            ("SHELL", "/bin/sh"),
            ("PWD", "/home/tester"),
        ])
    );
}

#[test]
fn a_host_child_of_the_command_line_entry_gets_no_image_variable() {
    let cleaned = clean(&sharun_child());
    assert_eq!(
        names_inside(&cleaned, "/tmp/.mount_comparOIliDD"),
        Vec::<String>::new()
    );
    for removed in [
        "GCONV_PATH",
        "LIBGL_DRIVERS_PATH",
        "LIBVA_DRIVERS_PATH",
        "TERMINFO",
        "CROSS_LIBC_DLOPEN_ROOT",
        "SHARUN_DIR",
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
        "GBM_BACKENDS_PATH",
        "__EGL_VENDOR_LIBRARY_DIRS",
        "AMDGPU_ASIC_ID_TABLE_PATHS",
        "XDG_DATA_DIRS",
    ] {
        assert!(!cleaned.contains_key(OsStr::new(removed)), "{removed}");
    }
    assert_eq!(
        cleaned.get(OsStr::new("PATH")),
        Some(&OsString::from("/opt/spy:/usr/local/bin:/usr/bin:/bin"))
    );
    for kept in [
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "DISPLAY",
        "LANG",
        "XDG_RUNTIME_DIR",
    ] {
        assert_eq!(
            cleaned.get(OsStr::new(kept)),
            sharun_child().get(OsStr::new(kept)),
            "{kept}"
        );
    }
}

#[test]
fn list_variables_lose_only_their_image_entries_in_order() {
    let mut input = apprun_child();
    input.insert(
        "PATH".into(),
        format!("/home/tester/bin:{MOUNT}/bin::{MOUNT}:/usr/bin:{MOUNT}/usr/bin:relative").into(),
    );
    input.insert(
        "XDG_CONFIG_DIRS".into(),
        format!("{MOUNT}/etc/xdg:/etc/xdg").into(),
    );
    input.insert("XDG_DATA_DIRS".into(), format!("{MOUNT}/share").into());
    let cleaned = clean(&input);
    assert_eq!(
        cleaned.get(OsStr::new("PATH")),
        Some(&OsString::from("/home/tester/bin::/usr/bin:relative"))
    );
    assert_eq!(
        cleaned.get(OsStr::new("XDG_CONFIG_DIRS")),
        Some(&OsString::from("/etc/xdg"))
    );
    assert!(!cleaned.contains_key(OsStr::new("XDG_DATA_DIRS")));
}

#[test]
fn a_user_loader_path_survives_for_a_host_child() {
    let mut input = apprun_child();
    input.insert(
        "LD_LIBRARY_PATH".into(),
        format!("/home/tester/userlib:{MOUNT}/lib;/opt/vendor/lib").into(),
    );
    input.insert(
        "LD_PRELOAD".into(),
        format!("/usr/lib/libgtk3-nocsd.so.0 {MOUNT}/lib/anylinux.so:anylinux.so /opt/libfoo.so")
            .into(),
    );
    let cleaned = clean(&input);
    assert_eq!(
        cleaned.get(OsStr::new("LD_LIBRARY_PATH")),
        Some(&OsString::from("/home/tester/userlib;/opt/vendor/lib"))
    );
    assert_eq!(
        cleaned.get(OsStr::new("LD_PRELOAD")),
        Some(&OsString::from(
            "/usr/lib/libgtk3-nocsd.so.0 /opt/libfoo.so"
        ))
    );

    input.insert("LD_LIBRARY_PATH".into(), format!("{MOUNT}/lib:").into());
    input.insert("LD_PRELOAD".into(), "cross-libc-dlopen.so".into());
    let cleaned = clean(&input);
    assert!(!cleaned.contains_key(OsStr::new("LD_LIBRARY_PATH")));
    assert!(!cleaned.contains_key(OsStr::new("LD_PRELOAD")));
}

#[test]
fn a_value_that_is_not_a_path_list_is_kept_byte_for_byte() {
    let opaque = [
        ("MY_URL", format!("file:{MOUNT}/share/doc")),
        ("GIT_SSH_COMMAND", format!("ssh -i {MOUNT}/key")),
        ("http_proxy", "http://proxy.invalid:3128".to_owned()),
        ("DISPLAY", ":0".to_owned()),
        (
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/run/user/1000/bus".to_owned(),
        ),
        (
            "LS_COLORS",
            "rs=0:di=01;34:ln=01;36:*.tar=01;31:".to_owned(),
        ),
        ("MIXED", format!("relative:{MOUNT}/share")),
    ];
    let mut input = apprun_child();
    for (name, value) in &opaque {
        input.insert((*name).into(), value.into());
    }
    let cleaned = clean(&input);
    for (name, value) in &opaque {
        assert_eq!(
            cleaned.get(OsStr::new(name)),
            Some(&OsString::from(value)),
            "{name}"
        );
    }
}

#[test]
fn a_resource_variable_keeps_the_user_entries() {
    let mut input = apprun_child();
    input.insert(
        "GST_PLUGIN_PATH".into(),
        format!("/home/tester/gst:{MOUNT}/lib/gstreamer-1.0").into(),
    );
    input.insert(
        "TERMINFO_DIRS".into(),
        format!("/home/tester/.terminfo:{MOUNT}/share/terminfo").into(),
    );
    let cleaned = clean(&input);
    assert_eq!(
        cleaned.get(OsStr::new("GST_PLUGIN_PATH")),
        Some(&OsString::from("/home/tester/gst"))
    );
    assert_eq!(
        cleaned.get(OsStr::new("TERMINFO_DIRS")),
        Some(&OsString::from("/home/tester/.terminfo"))
    );
    assert!(!cleaned.contains_key(OsStr::new("TERMINFO")));
}

#[test]
fn a_new_variable_that_points_into_the_image_is_removed() {
    let mut input = apprun_child();
    input.insert(
        "SOME_FUTURE_DATA".into(),
        format!("{MOUNT}/share/future").into(),
    );
    input.insert(
        "SOME_FUTURE_LIST".into(),
        format!("/usr/share/future:{MOUNT}/share/future/").into(),
    );
    input.insert("A_SIBLING_FOLDER".into(), format!("{MOUNT}X/share").into());
    let cleaned = clean(&input);
    assert!(!cleaned.contains_key(OsStr::new("SOME_FUTURE_DATA")));
    assert_eq!(
        cleaned.get(OsStr::new("SOME_FUTURE_LIST")),
        Some(&OsString::from("/usr/share/future"))
    );
    assert_eq!(
        cleaned.get(OsStr::new("A_SIBLING_FOLDER")),
        Some(&OsString::from(format!("{MOUNT}X/share")))
    );
}

#[test]
fn user_settings_are_kept_exactly() {
    let user = [
        ("TZ", "Asia/Kolkata"),
        ("LC_ALL", "de_DE.UTF-8"),
        ("http_proxy", "http://proxy.invalid:3128"),
        ("WAYLAND_DISPLAY", "wayland-0"),
        ("XDG_CONFIG_HOME", "/home/tester/.cfg"),
        ("XDG_CURRENT_DESKTOP", "GNOME"),
        ("SSH_AUTH_SOCK", "/run/user/1000/ssh"),
        ("GST_PLUGIN_PATH", "/opt/gstreamer/lib"),
        ("TERMINFO", "/home/tester/.terminfo"),
        ("APPIMAGE_EXTRACT_AND_RUN", "1"),
        ("MY_OWN_SETTING", "a:b:c"),
    ];
    let mut input = apprun_child();
    input.remove(OsStr::new("TERMINFO"));
    for (name, value) in user {
        input.insert(name.into(), value.into());
    }
    let cleaned = clean(&input);
    for (name, value) in user {
        assert_eq!(
            cleaned.get(OsStr::new(name)),
            Some(&OsString::from(value)),
            "{name}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_value_that_is_not_text_is_kept_or_filtered_byte_for_byte() {
    use std::os::unix::ffi::OsStrExt;
    let mut input = apprun_child();
    let foreign = OsStr::from_bytes(b"/opt/\xff/bin").to_owned();
    let mut path = OsString::from(format!("{MOUNT}/bin:"));
    path.push(&foreign);
    input.insert("PATH".into(), path);
    input.insert("RAW".into(), foreign.clone());
    let cleaned = clean(&input);
    assert_eq!(cleaned.get(OsStr::new("PATH")), Some(&foreign));
    assert_eq!(cleaned.get(OsStr::new("RAW")), Some(&foreign));
}

#[test]
fn outside_an_image_nothing_is_detected() {
    let plain = environment(&[
        ("HOME", "/home/tester"),
        ("PATH", "/usr/bin:/bin"),
        ("GCONV_PATH", "/usr/lib/gconv"),
    ]);
    assert_eq!(Image::detect(&plain, None, &unresolved), None);

    let own_appdir = environment(&[("APPDIR", "/home/tester/apps"), ("PATH", "/usr/bin")]);
    assert_eq!(Image::detect(&own_appdir, None, &unresolved), None);

    let file_system_root = environment(&[("SHARUN_DIR", "/"), ("APPDIR", "relative")]);
    assert_eq!(Image::detect(&file_system_root, None, &unresolved), None);
}

fn detect_from(environment: &Environment, executable: &str) -> Option<Image> {
    Image::detect(environment, Some(Path::new(executable)), &unresolved)
}

/// What the library path keeps of `value` for a host program started from
/// the image at `/opt/ca-image`.
fn library_path_kept_of(value: &str) -> Option<String> {
    let input = environment(&[("SHARUN_DIR", "/opt/ca-image"), ("LD_LIBRARY_PATH", value)]);
    let image = detect_from(&input, "/opt/ca-image/bin/ca").unwrap();
    clean_environment(&input, &image)
        .get(OsStr::new("LD_LIBRARY_PATH"))
        .map(|value| value.to_string_lossy().into_owned())
}

#[test]
fn an_image_entry_is_removed_and_an_outside_entry_kept_for_any_spelling() {
    for inside in [
        "/opt/./ca-image/lib",
        "/opt//ca-image/lib",
        "/opt/ca-image/lib",
    ] {
        assert_eq!(library_path_kept_of(inside), None, "{inside}");
    }
    for outside in ["/opt/ca-image/../vendor/lib", "/opt/vendor/lib"] {
        assert_eq!(
            library_path_kept_of(outside).as_deref(),
            Some(outside),
            "{outside}"
        );
    }
}

#[test]
fn a_plain_binary_under_another_image_keeps_the_environment() {
    let terminal = environment(&[
        ("APPDIR", "/tmp/.mount_TermAB"),
        ("APPIMAGE", "/home/u/Term.AppImage"),
        ("HOME", "/home/u/Term.AppImage.home"),
        ("XDG_CONFIG_HOME", "/home/u/Term.AppImage.config"),
        ("LD_LIBRARY_PATH", "/opt/vendor/lib"),
        ("PATH", "/tmp/.mount_TermAB/bin:/usr/bin"),
    ]);
    assert_eq!(detect_from(&terminal, "/usr/bin/compare-all"), None);
    assert!(detect_from(
        &terminal,
        "/tmp/.mount_TermAB/shared/lib/ld-linux-x86-64.so.2"
    )
    .is_some());

    let extract_and_run = environment(&[
        ("APPDIR", "/tmp/appimage_extracted_7112f36"),
        ("SHARUN_DIR", "/tmp/appimage_extracted_7112f36"),
        ("APPIMAGE", "/home/u/compare-all.AppImage"),
    ]);
    assert!(detect_from(
        &extract_and_run,
        "/tmp/appimage_extracted_7112f36/shared/lib/ld-linux-x86-64.so.2"
    )
    .is_some());

    let extracted_tree = environment(&[("SHARUN_DIR", "/home/u/squashfs-root")]);
    assert!(detect_from(
        &extracted_tree,
        "/home/u/squashfs-root/shared/lib/ld-linux-x86-64.so.2"
    )
    .is_some());

    let inside_the_terminal = environment(&[
        ("SHARUN_DIR", "/tmp/.mount_comparAB"),
        ("APPDIR", "/tmp/.mount_TermAB"),
        ("APPIMAGE", "/home/u/Term.AppImage"),
        ("HOME", "/home/u/Term.AppImage.home"),
        (
            "PATH",
            "/tmp/.mount_comparAB/bin:/tmp/.mount_TermAB/bin:/usr/bin",
        ),
    ]);
    let image = detect_from(
        &inside_the_terminal,
        "/tmp/.mount_comparAB/shared/lib/ld-linux-x86-64.so.2",
    )
    .unwrap();
    assert!(!image.needs_account_home(&inside_the_terminal));
    let cleaned = clean_environment(&inside_the_terminal, &image);
    assert_eq!(
        cleaned.get(OsStr::new("PATH")),
        Some(&OsString::from("/tmp/.mount_TermAB/bin:/usr/bin"))
    );
    assert_eq!(
        cleaned.get(OsStr::new("HOME")),
        Some(&OsString::from("/home/u/Term.AppImage.home"))
    );
}

fn detect_resolving(
    environment: &Environment,
    executable: &str,
    links: &[(&str, &str)],
) -> Option<Image> {
    let resolve = |folder: &Path| {
        links
            .iter()
            .find(|(link, _)| Path::new(link) == folder)
            .map(|(_, target)| PathBuf::from(target))
    };
    Image::detect(environment, Some(Path::new(executable)), &resolve)
}

#[test]
fn a_root_given_through_a_link_or_relative_path_matches() {
    let linked = environment(&[
        ("SHARUN_DIR", "/tmp/link-to-mount"),
        (
            "PATH",
            "/tmp/real-mount/bin:/tmp/link-to-mount/bin:/usr/bin",
        ),
        ("GCONV_PATH", "/tmp/real-mount/lib/gconv"),
    ]);
    let image = detect_resolving(
        &linked,
        "/tmp/real-mount/shared/lib/ld-linux-x86-64.so.2",
        &[("/tmp/link-to-mount", "/tmp/real-mount")],
    )
    .expect("the image through its link");
    let cleaned = clean_environment(&linked, &image);
    assert_eq!(
        cleaned.get(OsStr::new("PATH")),
        Some(&OsString::from("/usr/bin"))
    );
    assert!(!cleaned.contains_key(OsStr::new("GCONV_PATH")));

    let relative = environment(&[
        ("SHARUN_DIR", "squashfs-root"),
        ("APPDIR", "squashfs-root"),
        ("APPIMAGE", "/home/u/compare-all.AppImage"),
        ("PATH", "/home/u/squashfs-root/bin:/usr/bin"),
    ]);
    let image = detect_resolving(
        &relative,
        "/home/u/squashfs-root/shared/lib/ld-linux-x86-64.so.2",
        &[("squashfs-root", "/home/u/squashfs-root")],
    )
    .expect("the image through its relative folder");
    assert_eq!(
        clean_environment(&relative, &image).get(OsStr::new("PATH")),
        Some(&OsString::from("/usr/bin"))
    );
}

#[test]
fn outside_an_image_a_host_command_inherits_the_environment() {
    let command = super::command_with("a-program", None);
    assert_eq!(command.get_envs().count(), 0);
    assert_eq!(command.get_program(), OsStr::new("a-program"));

    if super::host_environment().is_some() {
        println!("this process runs from an image; the process-environment case is skipped");
    } else {
        let command = host_command("a-program");
        assert_eq!(command.get_envs().count(), 0);
        assert_eq!(command.get_program(), OsStr::new("a-program"));
    }
}

#[test]
fn inside_an_image_a_host_command_gets_exactly_the_cleaned_environment() {
    let cleaned = clean(&apprun_child());
    let command = super::command_with("a-program", Some(cleaned.clone()));
    let given: Environment = command
        .get_envs()
        .filter_map(|(name, value)| Some((name.to_owned(), value?.to_owned())))
        .collect();
    assert_eq!(given, cleaned);
    assert_eq!(command.get_program(), OsStr::new("a-program"));
}

fn portable_child() -> Environment {
    let mut portable = apprun_child();
    for (name, value) in [
        ("HOME", "/home/tester/compare-all.AppImage.home"),
        (
            "XDG_CONFIG_HOME",
            "/home/tester/compare-all.AppImage.config",
        ),
        ("HOST_HOME", "/home/tester/compare-all.AppImage.home"),
        (
            "HOST_XDG_CONFIG_HOME",
            "/home/tester/compare-all.AppImage.config",
        ),
    ] {
        portable.insert(name.into(), value.into());
    }
    portable
}

#[test]
fn portable_mode_gives_a_host_child_the_account_home() {
    let input = portable_child();
    let image = Image::detect(&input, None, &unresolved).unwrap();
    assert!(image.needs_account_home(&input));
    let image = image.with_account_home(Some("/home/tester".into()));
    let cleaned = clean_environment(&input, &image);
    assert_eq!(
        cleaned.get(OsStr::new("HOME")),
        Some(&OsString::from("/home/tester"))
    );
    assert!(!cleaned.contains_key(OsStr::new("XDG_CONFIG_HOME")));
    assert_eq!(
        names_inside(&cleaned, "compare-all.AppImage.config"),
        Vec::<String>::new()
    );
}

#[test]
fn portable_mode_with_no_known_account_home_keeps_home() {
    let input = portable_child();
    let image = Image::detect(&input, None, &unresolved).unwrap();
    let cleaned = clean_environment(&input, &image);
    assert_eq!(
        cleaned.get(OsStr::new("HOME")),
        Some(&OsString::from("/home/tester/compare-all.AppImage.home"))
    );
    assert!(!cleaned.contains_key(OsStr::new("XDG_CONFIG_HOME")));
}

#[test]
fn saved_originals_are_restored_rather_than_removed() {
    let mut input = portable_child();
    for (name, value) in [
        ("REAL_HOME", "/home/tester"),
        ("REAL_XDG_CONFIG_HOME", "/home/tester/.cfg"),
        ("REAL_XDG_DATA_HOME", "/home/tester/.data"),
        (
            "XDG_DATA_HOME",
            "/home/tester/compare-all.AppImage.home/.local/share",
        ),
    ] {
        input.insert(name.into(), value.into());
    }
    let image = Image::detect(&input, None, &unresolved).unwrap();
    assert!(!image.needs_account_home(&input));
    let cleaned = clean_environment(&input, &image);
    for (name, value) in [
        ("HOME", "/home/tester"),
        ("XDG_CONFIG_HOME", "/home/tester/.cfg"),
        ("XDG_DATA_HOME", "/home/tester/.data"),
    ] {
        assert_eq!(
            cleaned.get(OsStr::new(name)),
            Some(&OsString::from(value)),
            "{name}"
        );
    }
    for gone in ["REAL_HOME", "REAL_XDG_CONFIG_HOME", "REAL_XDG_DATA_HOME"] {
        assert!(!cleaned.contains_key(OsStr::new(gone)), "{gone}");
    }
}

#[test]
fn a_home_that_is_not_the_portable_folder_is_kept() {
    let mut input = apprun_child();
    input.insert("HOME".into(), "/srv/elsewhere".into());
    let image = Image::detect(&input, None, &unresolved).unwrap();
    assert!(!image.needs_account_home(&input));
    let image = image.with_account_home(Some("/home/tester".into()));
    assert_eq!(
        clean_environment(&input, &image).get(OsStr::new("HOME")),
        Some(&OsString::from("/srv/elsewhere"))
    );
}

/// The lists that sharun builds in this image's start.
const BUILT: &[&str] = &[
    "XDG_DATA_DIRS",
    "GBM_BACKENDS_PATH",
    "LIBVA_DRIVERS_PATH",
    "AMDGPU_ASIC_ID_TABLE_PATHS",
    "__EGL_VENDOR_LIBRARY_DIRS",
];

/// sharun's `add_to_env`: `folder` is prepended unless the value holds it.
fn sharun_add(environment: &mut Environment, name: &str, folder: &str) {
    let old = environment
        .get(OsStr::new(name))
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_owned();
    if old.is_empty() {
        environment.insert(name.into(), folder.into());
    } else if old != folder
        && !old.starts_with(&format!("{folder}:"))
        && !old.ends_with(&format!(":{folder}"))
        && !old.contains(&format!(":{folder}:"))
    {
        environment.insert(name.into(), format!("{folder}:{old}").into());
    }
}

/// The environment of this image's process after sharun started it from
/// `host`, on an `x86_64` host without the NVIDIA module, where the vendor
/// folders `vendor_folders` exist besides the image's own.
fn sharun_start(host: &Environment, root: &str, vendor_folders: &[&str]) -> Environment {
    let mut started = host.clone();
    started.insert("SHARUN_DIR".into(), root.into());
    started.insert("CROSS_LIBC_DLOPEN_ROOT".into(), root.into());
    sharun_add(&mut started, "PATH", &format!("{root}/bin"));
    sharun_add(&mut started, "GCONV_PATH", &format!("{root}/lib/gconv"));
    started.insert(
        "LIBGL_DRIVERS_PATH".into(),
        format!("{root}/lib/dri").into(),
    );
    sharun_add(
        &mut started,
        "LIBVA_DRIVERS_PATH",
        &format!("{root}/lib/dri"),
    );
    for folder in [
        "/run/opengl-driver/lib/gbm",
        "/usr/lib/gbm",
        "/usr/lib64/gbm",
        "/usr/lib/x86_64-linux-gnu/gbm",
        &format!("{root}/lib/gbm"),
    ] {
        sharun_add(&mut started, "GBM_BACKENDS_PATH", folder);
    }
    let home = host
        .get(OsStr::new("HOME"))
        .and_then(|home| home.to_str())
        .unwrap_or_default();
    for folder in [
        "/etc",
        "/run/current-system/sw/share",
        "/run/opengl-driver/share",
        "/usr/share",
        "/usr/local/share",
        &format!("{home}/.local/share"),
        &format!("{root}/share"),
    ] {
        sharun_add(&mut started, "XDG_DATA_DIRS", folder);
    }
    let data_folders = started
        .get(OsStr::new("XDG_DATA_DIRS"))
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_owned();
    for folder in data_folders.rsplit(':') {
        let vendor = Path::new(folder)
            .join("glvnd/egl_vendor.d")
            .to_string_lossy()
            .replace('\\', "/");
        if vendor.starts_with(root) || vendor_folders.contains(&vendor.as_str()) {
            sharun_add(&mut started, "__EGL_VENDOR_LIBRARY_DIRS", &vendor);
        }
    }
    started.insert("TERMINFO".into(), format!("{root}/share/terminfo").into());
    for folder in [
        &format!("{root}/share/libdrm"),
        "/usr/share/libdrm",
        "/usr/local/share/libdrm",
    ] {
        sharun_add(&mut started, "AMDGPU_ASIC_ID_TABLE_PATHS", folder);
    }
    started
}

const HOST_VENDOR_FOLDERS: &[&str] = &["/usr/share/glvnd/egl_vendor.d", "/etc/glvnd/egl_vendor.d"];

fn host_session(pairs: &[(&str, &str)]) -> Environment {
    let mut host = environment(&[
        ("HOME", "/tmp/home"),
        ("PATH", "/usr/local/bin:/usr/bin:/bin"),
        ("LANG", "C.UTF-8"),
    ]);
    host.extend(environment(pairs));
    host
}

#[test]
fn a_host_child_gets_no_list_that_sharun_built_from_nothing() {
    let root = "/tmp/.mount_comparOIliDD";
    let input = environment(&[
        ("SHARUN_DIR", root),
        ("HOME", "/tmp/home"),
        (
            "GBM_BACKENDS_PATH",
            "/tmp/.mount_comparOIliDD/lib/gbm:/usr/lib/x86_64-linux-gnu/gbm:/usr/lib64/gbm:/usr/lib/gbm:/run/opengl-driver/lib/gbm",
        ),
        (
            "AMDGPU_ASIC_ID_TABLE_PATHS",
            "/usr/local/share/libdrm:/usr/share/libdrm:/tmp/.mount_comparOIliDD/share/libdrm",
        ),
        (
            "XDG_DATA_DIRS",
            "/tmp/.mount_comparOIliDD/share:/tmp/home/.local/share:/usr/local/share:/usr/share:/run/opengl-driver/share:/run/current-system/sw/share:/etc",
        ),
        (
            "__EGL_VENDOR_LIBRARY_DIRS",
            "/tmp/.mount_comparOIliDD/share/glvnd/egl_vendor.d:/usr/share/glvnd/egl_vendor.d:/etc/glvnd/egl_vendor.d",
        ),
    ]);
    let cleaned = clean(&input);
    for name in BUILT {
        assert_eq!(cleaned.get(OsStr::new(name)), None, "{name}");
    }
    assert_eq!(
        cleaned.get(OsStr::new("HOME")),
        Some(&OsString::from("/tmp/home"))
    );
}

#[test]
fn a_host_child_gets_the_lists_of_the_host_session() {
    let root = "/tmp/.mount_comparOIliDD";
    for host in [
        host_session(&[]),
        host_session(&[("GBM_BACKENDS_PATH", "/opt/gbm")]),
        host_session(&[("GBM_BACKENDS_PATH", "/usr/lib/gbm:/opt/gbm")]),
        host_session(&[("XDG_DATA_DIRS", "/usr/local/share:/usr/share:/opt/share")]),
        host_session(&[("XDG_DATA_DIRS", "/usr/share")]),
        host_session(&[("XDG_DATA_DIRS", "/opt/share/:/usr/share")]),
        host_session(&[("AMDGPU_ASIC_ID_TABLE_PATHS", "/usr/share/libdrm")]),
        host_session(&[("LIBVA_DRIVERS_PATH", "/opt/va:/usr/lib/dri")]),
        host_session(&[("__EGL_VENDOR_LIBRARY_DIRS", "/usr/share/glvnd/egl_vendor.d")]),
        host_session(&[
            ("XDG_DATA_DIRS", "/opt/share"),
            ("__EGL_VENDOR_LIBRARY_DIRS", "/home/u/egl"),
        ]),
    ] {
        let mut vendor_folders = HOST_VENDOR_FOLDERS.to_vec();
        vendor_folders.push("/opt/share/glvnd/egl_vendor.d");
        let started = sharun_start(&host, root, &vendor_folders);
        let cleaned = clean(&started);
        for name in BUILT {
            assert_eq!(
                cleaned.get(OsStr::new(name)),
                host.get(OsStr::new(name)),
                "{name} of {host:?} from {:?}",
                started.get(OsStr::new(name))
            );
        }
        assert_eq!(names_inside(&cleaned, root), Vec::<String>::new());
    }
}

#[test]
fn a_user_list_in_the_form_sharun_gives_it_keeps_the_user_entries() {
    let mut input = sharun_child();
    input.insert(
        "GBM_BACKENDS_PATH".into(),
        "/tmp/.mount_comparOIliDD/lib/gbm:/usr/lib/x86_64-linux-gnu/gbm:/usr/lib64/gbm:/usr/lib/gbm:/run/opengl-driver/lib/gbm:/opt/gbm"
            .into(),
    );
    input.insert("HOME".into(), "/tmp/home".into());
    input.insert(
        "XDG_DATA_DIRS".into(),
        "/tmp/.mount_comparOIliDD/share:/tmp/home/.local/share:/run/opengl-driver/share:/run/current-system/sw/share:/etc:/usr/local/share:/usr/share:/opt/share"
            .into(),
    );
    let cleaned = clean(&input);
    assert_eq!(
        cleaned.get(OsStr::new("GBM_BACKENDS_PATH")),
        Some(&OsString::from("/opt/gbm"))
    );
    assert_eq!(
        cleaned.get(OsStr::new("XDG_DATA_DIRS")),
        Some(&OsString::from("/usr/local/share:/usr/share:/opt/share"))
    );
}

#[test]
fn a_user_list_that_sharun_leaves_byte_identical_reads_as_unset() {
    let root = "/tmp/.mount_comparOIliDD";
    let unset = sharun_start(&host_session(&[]), root, HOST_VENDOR_FOLDERS);
    let user_set = sharun_start(
        &host_session(&[("GBM_BACKENDS_PATH", "/run/opengl-driver/lib/gbm")]),
        root,
        HOST_VENDOR_FOLDERS,
    );
    assert_eq!(
        unset.get(OsStr::new("GBM_BACKENDS_PATH")),
        user_set.get(OsStr::new("GBM_BACKENDS_PATH"))
    );
    assert_eq!(clean(&user_set).get(OsStr::new("GBM_BACKENDS_PATH")), None);
}

#[test]
fn a_list_that_sharun_did_not_build_loses_only_its_image_entries() {
    let mut input = sharun_child();
    input.insert(
        "XDG_DATA_DIRS".into(),
        "/tmp/.mount_comparOIliDD/share:/usr/local/share:/usr/share".into(),
    );
    input.insert(
        "GBM_BACKENDS_PATH".into(),
        "/usr/lib/x86_64-linux-gnu/gbm:/usr/lib64/gbm".into(),
    );
    input.insert(
        "LIBVA_DRIVERS_PATH".into(),
        "/tmp/.mount_comparOIliDD/lib/dri:/usr/lib/x86_64-linux-gnu/dri".into(),
    );
    let cleaned = clean(&input);
    for (name, value) in [
        ("XDG_DATA_DIRS", "/usr/local/share:/usr/share"),
        (
            "GBM_BACKENDS_PATH",
            "/usr/lib/x86_64-linux-gnu/gbm:/usr/lib64/gbm",
        ),
        ("LIBVA_DRIVERS_PATH", "/usr/lib/x86_64-linux-gnu/dri"),
    ] {
        assert_eq!(
            cleaned.get(OsStr::new(name)),
            Some(&OsString::from(value)),
            "{name}"
        );
    }
}

#[test]
fn the_driver_folders_sharun_adds_for_nvidia_are_removed() {
    let mut input = sharun_child();
    input.insert(
        "LIBVA_DRIVERS_PATH".into(),
        "/tmp/.mount_comparOIliDD/lib/dri:/usr/lib/x86_64-linux-gnu/dri:/usr/lib64/dri:/usr/lib/dri:/run/opengl-driver/lib/dri:/opt/va"
            .into(),
    );
    input.insert(
        "__EGL_VENDOR_LIBRARY_FILENAMES".into(),
        "/usr/share/glvnd/egl_vendor.d/10_nvidia.json:/tmp/.mount_comparOIliDD/share/glvnd/egl_vendor.d/50_mesa.json"
            .into(),
    );
    let cleaned = clean(&input);
    assert_eq!(
        cleaned.get(OsStr::new("LIBVA_DRIVERS_PATH")),
        Some(&OsString::from("/opt/va"))
    );
    assert!(!cleaned.contains_key(OsStr::new("__EGL_VENDOR_LIBRARY_FILENAMES")));

    input.insert(
        "__EGL_VENDOR_LIBRARY_FILENAMES".into(),
        "/usr/share/glvnd/egl_vendor.d/10_nvidia.json".into(),
    );
    assert_eq!(
        clean(&input).get(OsStr::new("__EGL_VENDOR_LIBRARY_FILENAMES")),
        Some(&OsString::from(
            "/usr/share/glvnd/egl_vendor.d/10_nvidia.json"
        ))
    );
}

#[test]
fn the_lists_sharun_derives_from_the_data_folders_are_removed() {
    let mut input = sharun_child();
    input.insert(
        "XDG_DATA_DIRS".into(),
        "/tmp/.mount_comparOIliDD/share:/home/tester/.local/share:/usr/local/share:/usr/share:/run/opengl-driver/share:/run/current-system/sw/share:/etc:/opt/share"
            .into(),
    );
    input.insert(
        "GSETTINGS_SCHEMA_DIR".into(),
        "/tmp/.mount_comparOIliDD/share/glib-2.0/schemas:/usr/share/glib-2.0/schemas:/opt/share/glib-2.0/schemas:/home/tester/schemas"
            .into(),
    );
    input.insert(
        "VK_DRIVER_FILES".into(),
        "/tmp/.mount_comparOIliDD/share/vulkan/icd.d:/usr/share/vulkan/icd.d/nvidia_icd.json:/usr/share/vulkan/icd.d/nouveau_icd.x86_64.json"
            .into(),
    );
    let cleaned = clean(&input);
    assert_eq!(
        cleaned.get(OsStr::new("GSETTINGS_SCHEMA_DIR")),
        Some(&OsString::from("/home/tester/schemas"))
    );
    assert!(!cleaned.contains_key(OsStr::new("VK_DRIVER_FILES")));
    assert_eq!(
        cleaned.get(OsStr::new("XDG_DATA_DIRS")),
        Some(&OsString::from("/opt/share"))
    );
}

fn home_in(database: &[u8], uid: u32) -> Option<OsString> {
    super::home_of(database, uid)
}

#[test]
fn a_password_file_that_is_not_text_still_gives_the_home() {
    let database = b"root:x:0:0:root:/root:/bin/bash\n\
                     j\xf6rg:x:1001:1001:J\xf6rg M\xfcller:/home/j\xf6rg:/bin/sh\n\
                     tester:x:1000:1000:Tester:/home/tester:/bin/sh\n";
    assert_eq!(
        home_in(database, 1000),
        Some(OsString::from("/home/tester"))
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            home_in(database, 1001),
            Some(OsStr::from_bytes(b"/home/j\xf6rg").to_owned())
        );
    }
}

#[test]
fn the_account_database_is_read_once() {
    use std::sync::atomic::Ordering;
    let first = super::account_home();
    let second = super::account_home();
    assert_eq!(first, second);
    assert_eq!(super::ACCOUNT_READS.load(Ordering::SeqCst), 1);
}

#[test]
fn the_account_home_is_read_from_a_password_line() {
    let database = b"root:x:0:0:root:/root:/bin/bash\n\
                    # comment\n\
                    tester:x:1000:1000:Tester,,,:/home/tester:/bin/sh\n\
                    broken\n";
    assert_eq!(
        super::home_of(database, 1000),
        Some(OsString::from("/home/tester"))
    );
    assert_eq!(super::home_of(database, 0), Some(OsString::from("/root")));
    assert_eq!(super::home_of(database, 4242), None);
    assert_eq!(super::home_of(b"nohome:x:7:7:::/bin/sh\n", 7), None);
}
