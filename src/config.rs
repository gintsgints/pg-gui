use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Persisted app settings, stored as JSON in the platform config directory
/// (`~/Library/Application Support/pg-gui/config.json` on macOS).
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub connection_string: String,
    /// The editor buffer, restored on the next launch (unsaved edits included).
    #[serde(default)]
    pub script: String,
    /// Height of the SQL editor panel in pixels; `None` until the divider
    /// is first dragged (both panels then split the window evenly).
    #[serde(default)]
    pub editor_height: Option<f32>,
    /// Whether the toolbar (connection string and action buttons) is shown.
    #[serde(default = "default_true")]
    pub toolbar_visible: bool,
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
            script: String::new(),
            editor_height: None,
            toolbar_visible: true,
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

fn default_true() -> bool {
    true
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
    serde_json::from_str(&text).ok()
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
