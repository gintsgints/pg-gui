//! The application menus: built from the config (recent connections and
//! folders, theme and format-on-save check marks) and rebuilt on change.

use std::path::{Path, PathBuf};

use gpui::{Context, Menu, MenuItem, NoAction};
// Linux and Windows have no OS-native menu bar, so an in-window one is drawn
// in the title bar. macOS uses the real menu bar and skips all of this.
#[cfg(not(target_os = "macos"))]
use gpui_component::{GlobalState, menu::AppMenuBar};

use crate::app::{PgGuiApp, mask_credentials};
use crate::{
    AiComplete, CancelQuery, CloseTab, Commit, Connect, DebugContinue, DebugStepInto,
    DebugStepOver, DebugStop, EditConnection, ExplainAnalyze, ExplainPlan, ExportCsv,
    ExportInserts, FormatScript, NewConnection, NewFile, NextDiagnostic, NextTab, OpenConfig,
    OpenFile, OpenFolder, OpenGitHub, OpenRecentFolder, OpenSnippets, PrevDiagnostic, PrevTab,
    Quit, RefreshDbTree, Rollback, RunQuery, RunScripts, SaveFile, SetTheme, ShowHelp, StartDebug,
    ToggleAutocommit, ToggleComment, ToggleDbPanel, ToggleFilesPanel, ToggleFormatOnSave,
    ToggleResultsPanel, ZoomIn, ZoomOut, ZoomReset, config,
};

/// Menu label for a recent folder: the path with the home directory
/// shortened to `~`, since full paths are long and the menu is narrow.
fn folder_menu_label(path: &Path) -> String {
    let stripped = dirs::home_dir()
        .and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf))
        .map(|rest| format!("~/{}", rest.display()));
    stripped.unwrap_or_else(|| path.display().to_string())
}

/// One View ▸ Theme entry. Native menu items have no checked state
/// reachable from here, so the selected theme is marked with a check-mark
/// prefix and the menu is rebuilt whenever the selection changes.
fn theme_menu_item(theme: config::ThemeSelection, current: config::ThemeSelection) -> MenuItem {
    let label = if theme == current {
        format!("✓ {}", theme.label())
    } else {
        theme.label().to_string()
    };
    MenuItem::action(label, SetTheme(theme))
}

/// The Edit ▸ Format on Save entry, check-marked while the setting is on —
/// same trick (and same reason) as [`theme_menu_item`].
fn format_on_save_menu_item(format_on_save: bool) -> MenuItem {
    let label = if format_on_save {
        "✓ Format on Save"
    } else {
        "Format on Save"
    };
    MenuItem::action(label, ToggleFormatOnSave)
}

/// The application menu bar. Every command lives here now that the toolbar
/// is gone; the OS fills in each item's shortcut from the keybindings in
/// `main.rs`. `recents` becomes the Connection ▸ Recent submenu, each entry
/// carrying its (unmasked) connection string in a [`Connect`] action while
/// showing the user-given name (or the masked connection string when
/// unnamed). `theme` marks the selected View ▸ Theme entry.
fn connection_menu(recents: &[config::RecentConnection]) -> Menu {
    let recent_items = if recents.is_empty() {
        vec![MenuItem::action("No recent connections", NoAction)]
    } else {
        recents
            .iter()
            .map(|c| {
                let label = if c.name.is_empty() {
                    mask_credentials(&c.url)
                } else {
                    c.name.clone()
                };
                MenuItem::action(
                    label,
                    Connect {
                        url: c.url.clone(),
                        name: c.name.clone(),
                    },
                )
            })
            .collect()
    };

    Menu {
        name: "Connection".into(),
        disabled: false,
        items: vec![
            MenuItem::action("New Connection…", NewConnection),
            MenuItem::action("Edit Connection…", EditConnection),
            MenuItem::submenu(Menu {
                name: "Recent".into(),
                disabled: false,
                items: recent_items,
            }),
            MenuItem::separator(),
            MenuItem::action("Run Query", RunQuery),
            MenuItem::action("Run Selected Scripts", RunScripts),
            MenuItem::separator(),
            MenuItem::action("Explain Plan", ExplainPlan),
            MenuItem::action("Explain Analyze", ExplainAnalyze),
            MenuItem::separator(),
            MenuItem::action("Export as CSV…", ExportCsv),
            MenuItem::action("Export as INSERT…", ExportInserts),
        ],
    }
}

/// The File ▸ Open Recent Folder submenu, one entry per remembered
/// working folder, each carrying its path in an [`OpenRecentFolder`]
/// action.
fn recent_folders_menu(recent_folders: &[PathBuf]) -> MenuItem {
    let items = if recent_folders.is_empty() {
        vec![MenuItem::action("No recent folders", NoAction)]
    } else {
        recent_folders
            .iter()
            .map(|dir| MenuItem::action(folder_menu_label(dir), OpenRecentFolder(dir.clone())))
            .collect()
    };
    MenuItem::submenu(Menu {
        name: "Open Recent Folder".into(),
        disabled: false,
        items,
    })
}

fn build_menus(
    recents: &[config::RecentConnection],
    recent_folders: &[PathBuf],
    theme: config::ThemeSelection,
    format_on_save: bool,
) -> Vec<Menu> {
    vec![
        Menu {
            name: "pg-gui".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Preferences…", OpenConfig),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ],
        },
        connection_menu(recents),
        Menu {
            name: "File".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Open…", OpenFile),
                MenuItem::action("Open Folder…", OpenFolder),
                recent_folders_menu(recent_folders),
                MenuItem::action("Save", SaveFile),
                MenuItem::separator(),
                MenuItem::action("New Tab", NewFile),
                MenuItem::action("Close Tab", CloseTab),
                MenuItem::action("Next Tab", NextTab),
                MenuItem::action("Previous Tab", PrevTab),
            ],
        },
        Menu {
            name: "Edit".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Format", FormatScript),
                format_on_save_menu_item(format_on_save),
                MenuItem::separator(),
                MenuItem::action("Snippets", OpenSnippets),
                MenuItem::action("AI Complete", AiComplete),
                MenuItem::separator(),
                MenuItem::action("Toggle Comment", ToggleComment),
                MenuItem::separator(),
                MenuItem::action("Next Problem", NextDiagnostic),
                MenuItem::action("Previous Problem", PrevDiagnostic),
            ],
        },
        Menu {
            name: "Session".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Toggle Autocommit", ToggleAutocommit),
                MenuItem::separator(),
                MenuItem::action("Commit", Commit),
                MenuItem::action("Rollback", Rollback),
                MenuItem::separator(),
                MenuItem::action("Cancel Query", CancelQuery),
            ],
        },
        Menu {
            name: "Debug".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Start Debug", StartDebug),
                MenuItem::separator(),
                MenuItem::action("Step Over", DebugStepOver),
                MenuItem::action("Step Into", DebugStepInto),
                MenuItem::action("Continue", DebugContinue),
                MenuItem::separator(),
                MenuItem::action("Stop", DebugStop),
            ],
        },
        Menu {
            name: "View".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Toggle Database Browser", ToggleDbPanel),
                MenuItem::action("Toggle Files Panel", ToggleFilesPanel),
                MenuItem::action("Toggle Results Panel", ToggleResultsPanel),
                MenuItem::action("Refresh Database Browser", RefreshDbTree),
                MenuItem::separator(),
                MenuItem::action("Zoom In", ZoomIn),
                MenuItem::action("Zoom Out", ZoomOut),
                MenuItem::action("Actual Size", ZoomReset),
                MenuItem::separator(),
                MenuItem::submenu(Menu {
                    name: "Theme".into(),
                    disabled: false,
                    items: vec![
                        theme_menu_item(config::ThemeSelection::Light, theme),
                        theme_menu_item(config::ThemeSelection::Dark, theme),
                        theme_menu_item(config::ThemeSelection::System, theme),
                    ],
                }),
            ],
        },
        Menu {
            name: "About".into(),
            disabled: false,
            items: vec![
                MenuItem::action("pg-gui on GitHub", OpenGitHub),
                MenuItem::action("Keyboard Shortcuts", ShowHelp),
            ],
        },
    ]
}

impl PgGuiApp {
    /// Rebuild the application menu bar, e.g. after the recent-connections
    /// list changes. `set_menus` is on `App`, reached through `Context`.
    pub(in crate::app) fn refresh_menus(&self, cx: &mut Context<Self>) {
        cx.set_menus(build_menus(
            &self.config.recent_connections,
            &self.config.recent_folders,
            self.config.theme,
            self.config.format_on_save,
        ));
        // Feed the same menus to the in-window bar (Linux/Windows) and rebuild
        // it; macOS renders the native bar from `set_menus` alone.
        #[cfg(not(target_os = "macos"))]
        {
            let menus = build_menus(
                &self.config.recent_connections,
                &self.config.recent_folders,
                self.config.theme,
                self.config.format_on_save,
            )
            .into_iter()
            .map(gpui::Menu::owned)
            .collect();
            GlobalState::global_mut(cx).set_app_menus(menus);
            self.app_menu_bar.update(cx, AppMenuBar::reload);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::folder_menu_label;

    #[test]
    fn folder_label_shortens_home() {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        assert_eq!(folder_menu_label(&home.join("sql")), "~/sql");
        assert_eq!(folder_menu_label(Path::new("/opt/sql")), "/opt/sql");
    }
}
