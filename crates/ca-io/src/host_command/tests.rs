#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::{clean_environment, host_command, Environment, Image};
use std::ffi::{OsStr, OsString};

const MOUNT: &str = "/tmp/.mount_comparFcHnCN";

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
    let image = Image::detect(environment).expect("an image environment");
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
            (
                "XDG_DATA_DIRS",
                "/home/tester/.local/share:/usr/local/share:/usr/share:/run/opengl-driver/share:/run/current-system/sw/share:/etc",
            ),
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
        "GBM_BACKENDS_PATH",
        "__EGL_VENDOR_LIBRARY_DIRS",
        "TERMINFO",
        "AMDGPU_ASIC_ID_TABLE_PATHS",
        "CROSS_LIBC_DLOPEN_ROOT",
        "SHARUN_DIR",
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
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
    assert_eq!(Image::detect(&plain), None);

    let own_appdir = environment(&[("APPDIR", "/home/tester/apps"), ("PATH", "/usr/bin")]);
    assert_eq!(Image::detect(&own_appdir), None);

    let file_system_root = environment(&[("SHARUN_DIR", "/"), ("APPDIR", "relative")]);
    assert_eq!(Image::detect(&file_system_root), None);
}

#[test]
fn outside_an_image_a_host_command_inherits_the_environment() {
    if super::host_environment().is_some() {
        return;
    }
    let command = host_command("a-program");
    assert_eq!(command.get_envs().count(), 0);
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
    let image = Image::detect(&input).unwrap();
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
    let image = Image::detect(&input).unwrap();
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
    let image = Image::detect(&input).unwrap();
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
    let image = Image::detect(&input).unwrap();
    assert!(!image.needs_account_home(&input));
    let image = image.with_account_home(Some("/home/tester".into()));
    assert_eq!(
        clean_environment(&input, &image).get(OsStr::new("HOME")),
        Some(&OsString::from("/srv/elsewhere"))
    );
}

#[test]
fn the_account_home_is_read_from_a_password_line() {
    let database = "root:x:0:0:root:/root:/bin/bash\n\
                    # comment\n\
                    tester:x:1000:1000:Tester,,,:/home/tester:/bin/sh\n\
                    broken\n";
    assert_eq!(
        super::home_of(database, 1000),
        Some(OsString::from("/home/tester"))
    );
    assert_eq!(super::home_of(database, 0), Some(OsString::from("/root")));
    assert_eq!(super::home_of(database, 4242), None);
    assert_eq!(super::home_of("nohome:x:7:7:::/bin/sh\n", 7), None);
}
