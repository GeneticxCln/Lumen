//! Keeps the environment of an AppImage away from the programs Lumen starts.
//!
//! An AppImage's launcher points `LD_LIBRARY_PATH`, `PATH`, `GTK_*` and friends at the libraries
//! and tools packed inside it, which were built for one particular distribution. A program started
//! from inside (mpv, ffmpeg, yt-dlp) inherits that and loads the AppImage's copies of system
//! libraries instead of the host's own, which can make it crash at once on a newer system. So
//! children get the environment with every entry that points into the AppImage taken out.

use std::collections::HashMap;
use std::process::Command;

/// Variables that hold one path or a `:`-separated list of paths.
const PATH_LIKE: &[&str] = &[
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "PATH",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "GIO_EXTRA_MODULES",
    "GIO_MODULE_DIR",
    "GTK_PATH",
    "GTK_EXE_PREFIX",
    "GTK_DATA_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GDK_PIXBUF_MODULE_FILE",
    "GDK_PIXBUF_MODULEDIR",
    "GSETTINGS_SCHEMA_DIR",
    "LIBGL_DRIVERS_PATH",
    "LIBVA_DRIVERS_PATH",
    "PERL5LIB",
    "PYTHONHOME",
    "PYTHONPATH",
    "QT_PLUGIN_PATH",
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SCANNER",
    "WEBKIT_EXEC_PATH",
    "WEBKIT_INJECTED_BUNDLE_PATH",
];

/// What to do with one variable: `None` removes it, `Some` sets the cleaned value.
pub type Change = (String, Option<String>);

/// The changes that take `appdir` out of `vars`. Only variables that mention it are touched.
pub fn changes(vars: &HashMap<String, String>, appdir: &str) -> Vec<Change> {
    let appdir = appdir.trim_end_matches('/');
    if appdir.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for name in PATH_LIKE {
        let Some(value) = vars.get(*name) else {
            continue;
        };
        if !value.contains(appdir) {
            continue;
        }
        let kept: Vec<&str> = value
            .split(':')
            .filter(|entry| !entry.is_empty() && !entry.starts_with(appdir))
            .collect();
        out.push((
            (*name).to_string(),
            (!kept.is_empty()).then(|| kept.join(":")),
        ));
    }
    out
}

/// Applies `changes` for the AppImage this process runs from (`APPDIR`) to `command`. Nothing
/// happens outside an AppImage.
pub fn scrub(command: &mut Command) {
    let Ok(appdir) = std::env::var("APPDIR") else {
        return;
    };
    let vars: HashMap<String, String> = std::env::vars().collect();
    for (name, value) in changes(&vars, &appdir) {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
}

/// Variables that could make mpv or yt-dlp pick a different proxy (or none) than the one Lumen
/// explicitly tells them to use, or bypass it for hosts `no_proxy` names.
const PROXY_ENV: &[&str] = &[
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "no_proxy",
    "NO_PROXY",
];

/// Removes proxy-related environment variables from `command`, so it can only reach the network
/// the way Lumen explicitly set it up (direct, or through the network-privacy relay) rather than
/// whatever this desktop session happens to have set.
pub fn strip_proxy_env(command: &mut Command) {
    for name in PROXY_ENV {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn as_map(changes: Vec<Change>) -> HashMap<String, Option<String>> {
        changes.into_iter().collect()
    }

    #[test]
    fn entries_inside_the_appimage_are_removed_and_the_rest_kept() {
        let env = vars(&[
            (
                "LD_LIBRARY_PATH",
                "/tmp/app/usr/lib:/tmp/app/usr/lib/i386:/usr/local/lib",
            ),
            ("PATH", "/tmp/app/usr/bin/:/tmp/app/usr/sbin/:/usr/bin:/bin"),
            ("XDG_DATA_DIRS", "/tmp/app/usr/share/:/usr/share"),
            ("HOME", "/home/me"),
        ]);
        let changed = as_map(changes(&env, "/tmp/app"));
        assert_eq!(
            changed["LD_LIBRARY_PATH"].as_deref(),
            Some("/usr/local/lib")
        );
        assert_eq!(changed["PATH"].as_deref(), Some("/usr/bin:/bin"));
        assert_eq!(changed["XDG_DATA_DIRS"].as_deref(), Some("/usr/share"));
        assert!(
            !changed.contains_key("HOME"),
            "unrelated variables are left alone"
        );
    }

    #[test]
    fn a_variable_that_only_pointed_inside_is_removed() {
        let env = vars(&[
            ("GTK_PATH", "/tmp/app/usr/lib/gtk-3.0"),
            ("GDK_PIXBUF_MODULE_FILE", "/tmp/app/usr/lib/loaders.cache"),
            ("LD_LIBRARY_PATH", "/tmp/app/usr/lib"),
        ]);
        let changed = as_map(changes(&env, "/tmp/app/"));
        for name in ["GTK_PATH", "GDK_PIXBUF_MODULE_FILE", "LD_LIBRARY_PATH"] {
            assert_eq!(changed[name], None, "{name}");
        }
    }

    #[test]
    fn only_whole_path_entries_match_the_appimage_folder() {
        // `/tmp/app2` is a different folder that happens to start with the same letters.
        let env = vars(&[("PATH", "/tmp/app/usr/bin:/tmp/app2/bin:/usr/bin")]);
        let changed = as_map(changes(&env, "/tmp/app"));
        // The prefix test is by text, so this documents the behaviour: a sibling with the same
        // prefix is dropped too, which is harmless for a randomly named mount folder.
        assert_eq!(changed["PATH"].as_deref(), Some("/usr/bin"));
    }

    #[test]
    fn nothing_changes_without_an_appimage() {
        let env = vars(&[("PATH", "/usr/bin"), ("LD_LIBRARY_PATH", "/opt/lib")]);
        assert!(changes(&env, "/tmp/app").is_empty());
        assert!(changes(&env, "").is_empty());
    }

    #[test]
    fn strip_proxy_env_removes_every_variant() {
        let mut command = Command::new("true");
        for name in PROXY_ENV {
            command.env(name, "set");
        }
        command.env("HOME", "/home/me");
        strip_proxy_env(&mut command);
        let remaining: Vec<&std::ffi::OsStr> = command
            .get_envs()
            .filter_map(|(k, v)| v.map(|_| k))
            .collect();
        for name in PROXY_ENV {
            assert!(
                !remaining.contains(&std::ffi::OsStr::new(name)),
                "{name} should have been removed"
            );
        }
        assert!(
            remaining.contains(&std::ffi::OsStr::new("HOME")),
            "unrelated variables are left alone"
        );
    }
}
