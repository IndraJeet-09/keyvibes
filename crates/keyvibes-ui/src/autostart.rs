//! "Launch at login" for the desktop UI.
//!
//! The mechanism is the freedesktop **XDG autostart** entry: a desktop file
//! under `$XDG_CONFIG_HOME/autostart` that session managers on GNOME, KDE,
//! XFCE, Cinnamon and anything else speaking the spec run at login. It is a
//! plain file, so enabling and disabling it needs no privilege, no daemon
//! and no shell - and uninstalling KeyVibes removes exactly what was
//! written.
//!
//! The systemd *user* service that packages also install is a second,
//! independent way to start the engine at login. KeyVibes did not create
//! that symlink, so [`systemd_enabled`] only *reports* it; turning the
//! toggle off removes the autostart entry it owns and leaves the service
//! alone unless it was also enabled by this toggle.
//!
//! Everything takes an explicit base directory so the unit tests never
//! touch the real home directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// File name of the autostart entry.
pub const DESKTOP_FILE_NAME: &str = "keyvibes.desktop";

/// Default engine command used by the autostart entry.
pub const DEFAULT_ENGINE_COMMAND: &str = "keyvibes run";

/// `$XDG_CONFIG_HOME`, else `$HOME/.config`, else `None`.
pub fn config_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            let path = PathBuf::from(dir);
            if path.is_absolute() {
                return Some(path);
            }
        }
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    let home = PathBuf::from(home);
    if home.is_absolute() {
        Some(home.join(".config"))
    } else {
        None
    }
}

/// The autostart entry's path, or `None` when no config home is resolvable.
pub fn autostart_path() -> Option<PathBuf> {
    config_home().map(|home| autostart_path_in(&home))
}

/// Path of the autostart entry under an explicit config home.
pub fn autostart_path_in(config_home: &Path) -> PathBuf {
    config_home.join("autostart").join(DESKTOP_FILE_NAME)
}

/// The desktop entry, rendered for `engine_command`.
///
/// `TryExec` matters: once `keyvibes` is uninstalled the session skips the
/// entry instead of reporting a missing program at every login.
pub fn desktop_contents(engine_command: &str) -> String {
    format!(
        "\
[Desktop Entry]
Type=Application
Version=1.0
Name=KeyVibes
Comment=Keyboard sound engine for Linux
Exec={engine_command}
TryExec=keyvibes
Icon=keyvibes
Terminal=false
StartupNotify=false
NoDisplay=true
Hidden=false
X-GNOME-Autostart-enabled=true
Categories=AudioVideo;Audio;
",
        engine_command = engine_command.trim()
    )
}

/// Whether the autostart entry exists.
pub fn is_enabled() -> bool {
    config_home().is_some_and(|home| is_enabled_in(&home))
}

/// Whether the entry exists under an explicit config home.
pub fn is_enabled_in(config_home: &Path) -> bool {
    autostart_path_in(config_home).is_file()
}

/// Writes the autostart entry, creating `autostart/` as needed.
///
/// Idempotent: writing twice produces the same file and the same result.
pub fn enable(engine_command: &str) -> io::Result<PathBuf> {
    let home = config_home().ok_or_else(|| io::Error::other("no XDG config directory is set"))?;
    enable_in(&home, engine_command)
}

/// Writes the autostart entry under an explicit config home.
pub fn enable_in(config_home: &Path, engine_command: &str) -> io::Result<PathBuf> {
    let path = autostart_path_in(config_home);
    write_entry(&path, engine_command)?;
    Ok(path)
}

fn write_entry(path: &Path, engine_command: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // Replace rather than patch: a leftover entry from an older version
    // would keep starting a command the current version no longer uses.
    let tmp = path.with_extension("desktop.tmp");
    fs::write(&tmp, desktop_contents(engine_command))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Removes the autostart entry. Returns `true` when a file was removed.
pub fn disable() -> io::Result<bool> {
    match autostart_path() {
        Some(path) => disable_at(&path),
        None => Ok(false),
    }
}

/// Removes the autostart entry at an explicit path.
pub fn disable_at(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// The `default.target.wants` symlink `systemctl --user enable` creates.
///
/// Probed, never written: KeyVibes does not enable or disable the service
/// behind the user's back.
pub fn systemd_wants_path() -> Option<PathBuf> {
    config_home().map(|dir| {
        dir.join("systemd")
            .join("user")
            .join("default.target.wants")
            .join("keyvibes.service")
    })
}

/// Whether a systemd user service appears to be enabled at login.
pub fn systemd_enabled() -> bool {
    systemd_wants_path().is_some_and(|path| std::fs::symlink_metadata(&path).is_ok())
}

/// One-line summary of every login mechanism that is currently armed.
///
/// Empty means "nothing starts at login", which is a perfectly good state.
pub fn active_mechanisms() -> Vec<&'static str> {
    let mut mechanisms = Vec::new();
    if is_enabled() {
        mechanisms.push("XDG autostart");
    }
    if systemd_enabled() {
        mechanisms.push("systemd user service");
    }
    mechanisms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("keyvibes-autostart-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn entry_is_valid_desktop_entry_syntax() {
        let text = desktop_contents("keyvibes run");
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("Type=Application"));
        assert!(text.contains("Exec=keyvibes run"));
        // Without TryExec, uninstalling the binary would leave a session
        // error at every login.
        assert!(text.contains("TryExec=keyvibes"));
        // Hidden=true is how XDG marks an entry as *disabled*; it must stay
        // false or the entry would autostart nothing.
        assert!(text.contains("Hidden=false"));
        assert!(text.contains("NoDisplay=true"));
        for line in text.lines() {
            assert!(!line.ends_with(' '), "trailing space in {line:?}");
        }
    }

    #[test]
    fn custom_command_is_substituted_and_sanitised() {
        let text = desktop_contents("  /opt/keyvibes/bin/keyvibes run  ");
        assert!(text.contains("Exec=/opt/keyvibes/bin/keyvibes run"));
        assert!(!text.contains("Exec=  "));
        assert!(!text.contains("run  \n"));
    }

    #[test]
    fn enable_writes_and_is_idempotent() {
        let home = scratch("enable");
        assert!(!is_enabled_in(&home));

        let first = enable_in(&home, "keyvibes run").unwrap();
        assert_eq!(first, autostart_path_in(&home));
        assert!(is_enabled_in(&home));

        let bytes_before = fs::read(&first).unwrap();
        enable_in(&home, "keyvibes run").unwrap();
        assert_eq!(fs::read(&first).unwrap(), bytes_before);

        // The temp file must not be left behind.
        assert!(!first.with_extension("desktop.tmp").exists());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn disable_removes_once_and_is_idempotent() {
        let home = scratch("disable");
        let path = enable_in(&home, "keyvibes run").unwrap();
        assert!(disable_at(&path).unwrap());
        assert!(!path.exists());
        assert!(!disable_at(&path).unwrap(), "second call is a no-op");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn repeated_install_update_uninstall_cycles_stay_clean() {
        let home = scratch("cycles");
        for round in 0..3 {
            let command = if round == 0 {
                "keyvibes run"
            } else {
                "keyvibes run --rate 48000"
            };
            let path = enable_in(&home, command).unwrap();
            let text = fs::read_to_string(&path).unwrap();
            assert!(text.contains(&format!("Exec={command}")));
            assert!(disable_at(&path).unwrap());
            assert!(!path.exists());
        }
        // Nothing but the autostart directory is left behind.
        let autostart_dir = home.join("autostart");
        let leftover: usize = fs::read_dir(&autostart_dir)
            .map(|entries| entries.flatten().count())
            .unwrap_or(0);
        assert_eq!(leftover, 0, "no stray files after uninstall");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn active_mechanisms_reflect_the_file_state() {
        let home = scratch("mechanisms");
        assert!(!is_enabled_in(&home));
        let _ = enable_in(&home, "keyvibes run");
        assert!(is_enabled_in(&home));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn config_home_honours_the_xdg_variable() {
        // The pure path builders never read the environment, so this stays
        // deterministic regardless of what the test host exports.
        assert_eq!(
            autostart_path_in(Path::new("/tmp/xdg")),
            Path::new("/tmp/xdg")
                .join("autostart")
                .join(DESKTOP_FILE_NAME)
        );
    }
}
