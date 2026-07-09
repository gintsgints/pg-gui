use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// One editor tab: the buffer text (unsaved edits included) and the file
/// it was opened from or saved to, if any.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ScriptTab {
    #[serde(default)]
    pub script: String,
    #[serde(default)]
    pub file: Option<PathBuf>,
    /// The file's mtime when we last read or wrote it, persisted so the next
    /// launch can tell whether the file changed while the app was closed
    /// (which, combined with unsaved edits, means a save would clobber it).
    /// Excluded from `PartialEq` so a bare mtime change doesn't count as a
    /// tab edit for the config watcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_time: Option<SystemTime>,
}

impl PartialEq for ScriptTab {
    fn eq(&self, other: &Self) -> bool {
        self.script == other.script && self.file == other.file
    }
}

impl Eq for ScriptTab {}

/// A remembered connection: an optional user-given name and the connection
/// string it maps to. Shown in the Connection ▸ Recent menu by name, or by
/// the masked connection string when unnamed.
#[derive(Clone, Serialize, PartialEq, Eq)]
pub struct RecentConnection {
    pub name: String,
    pub url: String,
}

impl<'de> Deserialize<'de> for RecentConnection {
    /// Accept both the current object form (`{ "name": …, "url": … }`) and
    /// the legacy bare-string form — older configs stored
    /// `recent_connections` as a plain array of URLs.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Url(String),
            Named {
                #[serde(default)]
                name: String,
                url: String,
            },
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Url(url) => Self {
                name: String::new(),
                url,
            },
            Raw::Named { name, url } => Self { name, url },
        })
    }
}

/// Persisted app settings, stored as JSON in the platform config directory
/// (`~/Library/Application Support/pg-gui/config.json` on macOS).
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub connection_string: String,
    /// Previously used connections, most recent first, shown in the
    /// Connection ▸ Recent application menu.
    #[serde(default)]
    pub recent_connections: Vec<RecentConnection>,
    /// Pre-tabs single-script fields; read once to seed `tabs` in
    /// [`try_load`] and no longer written.
    #[serde(default, skip_serializing)]
    script: String,
    #[serde(default, skip_serializing)]
    script_file: Option<PathBuf>,
    /// The open editor tabs, restored on the next launch.
    #[serde(default)]
    pub tabs: Vec<ScriptTab>,
    /// Index of the selected tab.
    #[serde(default)]
    pub active_tab: usize,
    /// Height of the SQL editor panel in pixels; `None` until the divider
    /// is first dragged (both panels then split the window evenly).
    #[serde(default)]
    pub editor_height: Option<f32>,
    /// Rows shown per results page.
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    /// Anthropic API key for AI completion; when empty, the
    /// `ANTHROPIC_API_KEY` environment variable is used instead.
    #[serde(default)]
    pub ai_api_key: String,
    /// Whether cmd-s formats the script through the language server
    /// before writing it to disk. Needs postgrestools ≥ 0.22.
    #[serde(default)]
    pub format_on_save: bool,
    /// Casing the formatter applies to keywords (SELECT, FROM, WHERE).
    #[serde(default)]
    pub keyword_case: CaseStyle,
    /// Casing the formatter applies to constants (NULL, TRUE, FALSE).
    #[serde(default)]
    pub constant_case: CaseStyle,
    /// UI zoom factor (cmd +/-, cmd-0 to reset); 1.0 means 100%.
    #[serde(default = "default_zoom")]
    pub zoom: f32,
}

/// Casing style used by the language-server formatter; stored in
/// config.json as `"lower"` or `"upper"`.
#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaseStyle {
    /// The server's default.
    #[default]
    Lower,
    Upper,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connection_string: String::new(),
            recent_connections: Vec::new(),
            script: String::new(),
            script_file: None,
            tabs: Vec::new(),
            active_tab: 0,
            editor_height: None,
            page_size: default_page_size(),
            ai_api_key: String::new(),
            format_on_save: false,
            keyword_case: CaseStyle::default(),
            constant_case: CaseStyle::default(),
            zoom: default_zoom(),
        }
    }
}

fn default_zoom() -> f32 {
    1.0
}

fn default_page_size() -> usize {
    100
}

/// Where the config file lives; `None` when the platform has no config
/// directory.
pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("pg-gui").join("config.json"))
}

pub fn load() -> Config {
    try_load().unwrap_or_default()
}

/// Like [`load`], but `None` when the file is missing or doesn't parse
/// (e.g. a half-saved external edit), so the caller can keep the current
/// in-memory config instead of falling back to defaults.
pub fn try_load() -> Option<Config> {
    let text = std::fs::read_to_string(path()?).ok()?;
    let mut config: Config = serde_json::from_str(&text).ok()?;
    // Migrate a pre-tabs config: its single script becomes the only tab.
    if config.tabs.is_empty() && (!config.script.is_empty() || config.script_file.is_some()) {
        config.tabs.push(ScriptTab {
            script: std::mem::take(&mut config.script),
            file: config.script_file.take(),
            disk_time: None,
        });
    }
    Some(config)
}

/// The config file's last modification time; `None` when it doesn't
/// exist. Used to detect external edits.
pub fn modified_time() -> Option<std::time::SystemTime> {
    path()?.metadata().ok()?.modified().ok()
}

pub fn save(config: &Config) {
    let Some(path) = path() else { return };
    let result = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::write(
                &path,
                serde_json::to_string_pretty(config).unwrap_or_default(),
            )
        });
    if let Err(err) = result {
        eprintln!("pg-gui: failed to save config to {}: {err}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_connections_accept_legacy_strings_and_named_objects() {
        let json = r#"{
            "recent_connections": [
                "postgres://a@localhost/db",
                { "name": "Prod", "url": "postgres://b@remote/db" }
            ]
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.recent_connections.len(), 2);
        assert_eq!(config.recent_connections[0].name, "");
        assert_eq!(
            config.recent_connections[0].url,
            "postgres://a@localhost/db"
        );
        assert_eq!(config.recent_connections[1].name, "Prod");
        assert_eq!(config.recent_connections[1].url, "postgres://b@remote/db");
    }
}
