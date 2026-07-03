use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Persisted app settings, stored as JSON in the platform config directory
/// (`~/Library/Application Support/pg-gui/config.json` on macOS).
#[derive(Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub connection_string: String,
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("pg-gui").join("config.json"))
}

pub fn load() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(config: &Config) {
    let Some(path) = config_path() else { return };
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
