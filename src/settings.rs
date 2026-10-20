//! Persisted settings at `%LOCALAPPDATA%\Musica\settings.json` (the same
//! directory as the log).
//!
//! A missing, unreadable or broken file means defaults: settings must never
//! keep the player from starting. Debug builds with `MUSICA_INSTANCE=<name>`
//! read `settings-<name>.json` instead, so a test resident never overwrites
//! the real file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_LEVEL: i32 = 85;

/// App settings. Add fields with a sensible `Default`; `#[serde(default)]`
/// keeps an older file (or a hand-edited one) loading. Unknown fields from a
/// newer build are ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// App volume level, 0..=100; see `player::volume_for_level`.
    pub level: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            level: DEFAULT_LEVEL,
        }
    }
}

impl Settings {
    /// A hand-edited file must not put playback above full scale.
    fn clamped(self) -> Self {
        Self {
            level: self.level.clamp(0, 100),
        }
    }
}

pub fn dir() -> PathBuf {
    crate::log::dir()
}

pub fn path() -> PathBuf {
    dir().join(file_name())
}

fn file_name() -> String {
    match instance_suffix() {
        Some(suffix) => format!("settings-{suffix}.json"),
        None => "settings.json".into(),
    }
}

/// Debug builds only, like the window class: the test resident writes its own
/// file. Release builds always use `settings.json`.
#[cfg(debug_assertions)]
pub fn instance_suffix() -> Option<String> {
    std::env::var("MUSICA_INSTANCE")
        .ok()
        .as_deref()
        .and_then(sanitize)
}

#[cfg(not(debug_assertions))]
pub fn instance_suffix() -> Option<String> {
    None
}

/// Keep the instance name from escaping the settings directory.
#[cfg(debug_assertions)]
fn sanitize(instance: &str) -> Option<String> {
    let clean: String = instance
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (!clean.is_empty()).then_some(clean)
}

/// The saved settings, or defaults when there is nothing readable.
pub fn load() -> Settings {
    load_from(&path())
}

pub fn load_from(path: &Path) -> Settings {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Settings::default();
    };
    serde_json::from_str::<Settings>(&text)
        .map(Settings::clamped)
        .unwrap_or_default()
}

/// Best effort: a read-only profile must not break the volume keys.
pub fn save(settings: &Settings) {
    let _ = save_to(&path(), settings);
}

/// Write through a temporary file, so a crash mid-write leaves the old
/// settings intact instead of a truncated file.
pub fn save_to(path: &Path, settings: &Settings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A unique directory per test, so parallel tests cannot collide.
    fn scratch(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "musica-settings-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path, file: &str) {
        let path = dir.join(file);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("json.tmp"));
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn default_level_is_85() {
        assert_eq!(Settings::default().level, 85);
        let missing = std::env::temp_dir().join(format!(
            "musica-no-such-settings-{}.json",
            std::process::id()
        ));
        assert_eq!(load_from(&missing).level, 85);
    }

    #[test]
    fn broken_file_means_default() {
        let dir = scratch("broken");
        let path = dir.join("settings.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        assert_eq!(load_from(&path), Settings::default());
        std::fs::write(&path, "").unwrap();
        assert_eq!(load_from(&path), Settings::default());
        cleanup(&dir, "settings.json");
    }

    #[test]
    fn round_trip() {
        let dir = scratch("roundtrip");
        let path = dir.join("settings.json");
        let saved = Settings { level: 40 };
        save_to(&path, &saved).unwrap();
        assert_eq!(load_from(&path), saved);
        // The second save overwrites the first (every key press writes).
        let saved = Settings { level: 62 };
        save_to(&path, &saved).unwrap();
        assert_eq!(load_from(&path), saved);
        // No temporary file left behind.
        assert!(!path.with_extension("json.tmp").exists());
        cleanup(&dir, "settings.json");
    }

    #[test]
    fn out_of_range_is_clamped() {
        let dir = scratch("clamp");
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"level": 900}"#).unwrap();
        assert_eq!(load_from(&path).level, 100);
        std::fs::write(&path, r#"{"level": -5}"#).unwrap();
        assert_eq!(load_from(&path).level, 0);
        cleanup(&dir, "settings.json");
    }

    #[test]
    fn unknown_fields_and_missing_level_are_tolerated() {
        let dir = scratch("generic");
        let path = dir.join("settings.json");
        // A future `library` field and no `level`: the default fills in.
        std::fs::write(&path, r#"{"library": ["a", "b"]}"#).unwrap();
        assert_eq!(load_from(&path).level, 85);
        std::fs::write(&path, r#"{"level": 50, "library": []}"#).unwrap();
        assert_eq!(load_from(&path).level, 50);
        cleanup(&dir, "settings.json");
    }

    #[test]
    fn instance_names_are_sanitized() {
        assert_eq!(sanitize("test"), Some("test".into()));
        assert_eq!(sanitize("a-b_c.1"), Some("a-b_c1".into()));
        assert_eq!(sanitize("..\\x/"), Some("x".into()));
        assert_eq!(sanitize(""), None);
    }
}
