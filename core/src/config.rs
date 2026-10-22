//! User settings, kept in a plain TOML file so they can be backed up or shared (SPEC 8):
//! `%APPDATA%\Flick\config.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hotkey::Shortcut;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The shortcut that opens the wheel, e.g. "Ctrl+Alt+Space".
    pub shortcut: String,
    pub slices: SliceConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SliceConfig {
    /// Slice ids ("pack.slice") in the order the user wants them. Slices not listed keep their
    /// pack order after these.
    pub order: Vec<String>,
    /// Slice ids the user turned off.
    pub disabled: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shortcut: Shortcut::default().to_string(),
            slices: SliceConfig::default(),
        }
    }
}

impl Config {
    /// The configured shortcut, or the default if the file holds something invalid.
    pub fn shortcut(&self) -> Shortcut {
        self.shortcut.parse().unwrap_or_default()
    }
}

/// `%APPDATA%\Flick\config.toml`.
pub fn config_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("Flick").join("config.toml"))
}

/// Reads the config. A missing file gives the defaults; an unreadable or invalid one gives the
/// defaults plus a message saying why, so a broken file never stops Flick from starting.
pub fn load(path: &Path) -> (Config, Option<String>) {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), None),
        Err(e) => (
            Config::default(),
            Some(format!(
                "Could not read {} ({e}); using defaults.",
                path.display()
            )),
        ),
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(config) => (config, None),
            Err(e) => (
                Config::default(),
                Some(format!(
                    "{} is not valid ({e}); using defaults.",
                    path.display()
                )),
            ),
        },
    }
}

/// Writes the config through a temp file, so a crash mid-write never leaves half a file.
pub fn save(path: &Path, config: &Config) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string_pretty(config).map_err(std::io::Error::other)?;
    let temp = path.with_extension("toml.tmp");
    std::fs::write(&temp, text)?;
    // This is Flick's own file, so replacing it is intended.
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults_without_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let (config, warning) = load(&dir.path().join("config.toml"));
        assert_eq!(config, Config::default());
        assert_eq!(config.shortcut, "Ctrl+Alt+Space");
        assert!(warning.is_none());
    }

    #[test]
    fn round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let config = Config {
            shortcut: "Ctrl+Shift+F12".into(),
            slices: SliceConfig {
                order: vec!["pdf-light.merge".into()],
                disabled: vec!["pdf-light.split".into()],
            },
        };
        save(&path, &config).unwrap();
        assert_eq!(load(&path), (config, None));
    }

    #[test]
    fn partial_file_fills_in_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[slices]\ndisabled = [\"a.b\"]\n").unwrap();
        let (config, warning) = load(&path);
        assert!(warning.is_none());
        assert_eq!(config.shortcut, "Ctrl+Alt+Space");
        assert_eq!(config.slices.disabled, ["a.b"]);
    }

    #[test]
    fn broken_file_falls_back_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "shortcut = [not toml").unwrap();
        let (config, warning) = load(&path);
        assert_eq!(config, Config::default());
        assert!(warning.unwrap().contains("not valid"));
    }

    #[test]
    fn invalid_shortcut_text_uses_the_default() {
        let config = Config {
            shortcut: "Banana".into(),
            ..Config::default()
        };
        assert_eq!(config.shortcut(), Shortcut::default());
    }
}
