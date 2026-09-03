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
// The bools are independent, persisted on/off UI preferences (panel
// visibility, format-on-save); a state machine or enums would not model them
// any better, so the excessive-bools lint is a false positive here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub connection_string: String,
    /// Default autocommit mode for new editor tabs. ON (each Run commits
    /// immediately); OFF starts a transaction that Commit/Rollback ends.
    /// Each tab keeps its own live toggle; this only seeds a fresh tab.
    #[serde(default = "default_true")]
    pub autocommit: bool,
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
    /// Rows pulled from the server per fetch: a SELECT is run through a
    /// server-side cursor and only this many rows are transferred at a
    /// time; Fetch More pulls the next batch.
    #[serde(default = "default_fetch_size")]
    pub fetch_size: usize,
    /// Anthropic API key for AI completion; when empty, the
    /// `ANTHROPIC_API_KEY` environment variable is used instead.
    #[serde(default)]
    pub ai_api_key: String,
    /// Model used for AI completion; when empty, the `PG_GUI_AI_MODEL`
    /// environment variable is used, falling back to the built-in default.
    #[serde(default)]
    pub ai_model: String,
    /// Extra instructions appended to the AI completion system prompt,
    /// e.g. schema hints or style preferences.
    #[serde(default)]
    pub ai_prompt: String,
    /// Whether cmd-s formats the script through the language server before
    /// writing it to disk. On by default; toggled from the `fmt:` segment
    /// in the status bar or Edit ▸ Format on Save.
    #[serde(default = "default_true")]
    pub format_on_save: bool,
    /// Casing the formatter applies to keywords (SELECT, FROM, WHERE).
    #[serde(default)]
    pub keyword_case: CaseStyle,
    /// Casing the formatter applies to constants (NULL, TRUE, FALSE).
    #[serde(default)]
    pub constant_case: CaseStyle,
    /// Glob matched against `.sql` filenames when a database-browser object
    /// is clicked, deciding which file (if any) to open instead of fetching
    /// the object's definition. `{object}` is replaced with the object name;
    /// `*` matches any run of characters, `?` any single one; matching is
    /// case-insensitive. Default `*_{object}.sql`, which matches both
    /// single- and double-underscore separators (e.g. `R__001_add.sql`
    /// and `01__place_order.sql`). When several files match, the object's
    /// schema picks between them: a file the schema itself qualifies
    /// (`order_utils__place_order.sql`, `order_utils/place_order.sql`)
    /// wins over an unqualified one, which in turn wins over one qualified by
    /// something else (`test__order_utils__place_order.sql`).
    #[serde(default = "default_definition_file_mask")]
    pub definition_file_mask: String,
    /// Directory the file dialogs (Open, Save As, Export) start in; set to
    /// the parent of the last file chosen in any of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_dir: Option<PathBuf>,
    /// Folder shown in the files side panel; set by File ▸ Open Folder….
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<PathBuf>,
    /// Previously opened working folders, most recent first, shown in the
    /// File ▸ Open Recent Folder submenu.
    #[serde(default)]
    pub recent_folders: Vec<PathBuf>,
    /// Whether the files side panel is shown (toggled with cmd-shift-e).
    #[serde(default = "default_true")]
    pub files_panel_visible: bool,
    /// Width of the files side panel in pixels; `None` until its divider
    /// is first dragged.
    #[serde(default)]
    pub files_panel_width: Option<f32>,
    /// Whether the results panel is shown (toggled with cmd-3).
    #[serde(default = "default_true")]
    pub results_panel_visible: bool,
    /// Whether the database object browser (left panel) is shown (toggled
    /// with cmd-1).
    #[serde(default = "default_true")]
    pub db_panel_visible: bool,
    /// Width of the database object browser in pixels; `None` until its
    /// divider is first dragged.
    #[serde(default)]
    pub db_panel_width: Option<f32>,
    /// UI zoom factor (cmd +/-, cmd-0 to reset); 1.0 means 100%.
    #[serde(default = "default_zoom")]
    pub zoom: f32,
    /// Color theme, selected from the View ▸ Theme menu.
    #[serde(default)]
    pub theme: ThemeSelection,
}

/// Which color theme the app uses; stored in config.json as `"light"`,
/// `"dark"`, or `"system"`.
#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeSelection {
    Light,
    Dark,
    /// Follow the OS appearance, switching live when it changes.
    #[default]
    System,
}

impl ThemeSelection {
    /// The menu / status-message label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::System => "System",
        }
    }
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
            autocommit: default_true(),
            recent_connections: Vec::new(),
            script: String::new(),
            script_file: None,
            tabs: Vec::new(),
            active_tab: 0,
            editor_height: None,
            page_size: default_page_size(),
            fetch_size: default_fetch_size(),
            ai_api_key: String::new(),
            ai_model: String::new(),
            ai_prompt: String::new(),
            format_on_save: default_true(),
            keyword_case: CaseStyle::default(),
            constant_case: CaseStyle::default(),
            definition_file_mask: default_definition_file_mask(),
            last_dir: None,
            working_dir: None,
            recent_folders: Vec::new(),
            files_panel_visible: default_true(),
            files_panel_width: None,
            results_panel_visible: default_true(),
            db_panel_visible: default_true(),
            db_panel_width: None,
            zoom: default_zoom(),
            theme: ThemeSelection::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_zoom() -> f32 {
    1.0
}

fn default_page_size() -> usize {
    100
}

fn default_fetch_size() -> usize {
    500
}

fn default_definition_file_mask() -> String {
    "*_{object}.sql".to_string()
}

/// The app's config directory (`~/Library/Application Support/pg-gui` on
/// macOS); `None` when the platform has no config directory. Also hosts
/// the single-instance socket (see [`crate::instance`]).
pub fn dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("pg-gui"))
}

/// Where the config file lives; `None` when the platform has no config
/// directory.
pub fn path() -> Option<PathBuf> {
    dir().map(|dir| dir.join("config.json"))
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
    // Write to a sibling temp file and rename it into place, so a crash
    // mid-write can't leave a truncated config.json (which `try_load`
    // would silently replace with defaults on the next launch).
    let tmp = path.with_extension("json.tmp");
    let result = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::write(
                &tmp,
                serde_json::to_string_pretty(config).unwrap_or_default(),
            )
        })
        .and_then(|()| std::fs::rename(&tmp, &path));
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

    #[test]
    fn format_on_save_defaults_on_when_absent() {
        // The flag used to default off; a config written before the flip
        // (or by anyone who never touched it) must come back formatting.
        let config: Config = serde_json::from_str("{}").unwrap();
        assert!(config.format_on_save);
        // An explicit value is respected.
        let off: Config = serde_json::from_str(r#"{"format_on_save": false}"#).unwrap();
        assert!(!off.format_on_save);
    }

    #[test]
    fn autocommit_defaults_on_when_absent() {
        // A config from before the field existed must come back autocommit ON.
        let config: Config = serde_json::from_str("{}").unwrap();
        assert!(config.autocommit);
        // An explicit value is respected.
        let off: Config = serde_json::from_str(r#"{"autocommit": false}"#).unwrap();
        assert!(!off.autocommit);
    }
}
