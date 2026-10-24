//! View commands: panel toggles, theme, zoom, help, and opening the config
//! file and the project page.

use gpui::{Context, ParentElement as _, Styled as _, Window, div, px};
use gpui_component::{ActiveTheme as _, Theme, WindowExt as _, h_flex, v_flex};

use crate::app::{DbSchemaLoad, PgGuiApp, apply_theme_selection};
use crate::{
    OpenConfig, OpenGitHub, RefreshDbTree, SetTheme, ShowHelp, ToggleDbPanel, ToggleFilesPanel,
    ToggleResultsPanel, ZoomIn, ZoomOut, ZoomReset, config,
};

/// The project's GitHub page, opened from the About application menu.
const REPO_URL: &str = "https://github.com/gintsgints/pg-gui";

const ZOOM_STEP: f32 = 0.1;

const ZOOM_MIN: f32 = 0.5;

const ZOOM_MAX: f32 = 2.0;

/// Every command with its keybinding(s), shown in the help dialog
/// (cmd-h on macOS, F1 elsewhere). Must mirror the bindings in main.rs.
#[cfg(target_os = "macos")]
const COMMANDS: &[(&str, &str)] = &[
    (
        "cmd-enter / ctrl-enter",
        "Run the selection or the statement at the cursor",
    ),
    (
        "cmd-shift-enter",
        "Run the scripts selected in the files panel",
    ),
    (
        "cmd-e / cmd-shift-e",
        "Explain the statement / explain it with ANALYZE (runs it)",
    ),
    ("cmd-i / ctrl-space", "AI-complete SQL at the cursor"),
    ("cmd-shift-f", "Format the script"),
    ("cmd-/", "Comment or uncomment the line / selection"),
    (
        "cmd-j / cmd-k",
        "Next / previous problem (errors first, then warnings)",
    ),
    ("cmd-f", "Find in the script"),
    ("cmd-r", "Find and replace in the script"),
    ("cmd-p", "Go to a file; type > for snippets, : for commands"),
    ("cmd-shift-p", "Run a command"),
    ("cmd-t", "New script tab"),
    ("cmd-w", "Close the tab"),
    ("ctrl-tab / ctrl-shift-tab", "Next / previous tab"),
    ("cmd-o", "Open a SQL script"),
    ("cmd-1", "Show or hide the database browser"),
    ("cmd-b / cmd-2", "Show or hide the files panel"),
    ("cmd-3", "Show or hide the results panel"),
    ("cmd-c", "Copy the selected result cell, row(s) or column"),
    (
        "shift/cmd-click",
        "Select a range of result rows / toggle one row",
    ),
    (
        "cmd-a",
        "Select every result row (all pages; or click the grid's top-left corner)",
    ),
    ("cmd-shift-d", "Start debugging a routine"),
    ("f10 / f11", "Debug: step over / step into"),
    ("f5 / shift-f5", "Debug: continue / stop"),
    ("cmd-s", "Save the script"),
    ("cmd-,", "Open config.json in the system editor"),
    ("cmd-plus / cmd-minus", "Zoom in / out"),
    ("cmd-0", "Reset zoom"),
    ("cmd-h", "Show this help"),
    ("cmd-q", "Quit"),
];

#[cfg(not(target_os = "macos"))]
const COMMANDS: &[(&str, &str)] = &[
    (
        "ctrl-enter",
        "Run the selection or the statement at the cursor",
    ),
    (
        "ctrl-shift-enter",
        "Run the scripts selected in the files panel",
    ),
    (
        "ctrl-e / ctrl-shift-e",
        "Explain the statement / explain it with ANALYZE (runs it)",
    ),
    ("ctrl-i / ctrl-space", "AI-complete SQL at the cursor"),
    ("ctrl-shift-f", "Format the script"),
    ("ctrl-/", "Comment or uncomment the line / selection"),
    (
        "ctrl-j / ctrl-k",
        "Next / previous problem (errors first, then warnings)",
    ),
    ("ctrl-f", "Find in the script"),
    ("ctrl-r", "Find and replace in the script"),
    (
        "ctrl-p",
        "Go to a file; type > for snippets, : for commands",
    ),
    ("ctrl-shift-p", "Run a command"),
    ("ctrl-t", "New script tab"),
    ("ctrl-w", "Close the tab"),
    ("ctrl-tab / ctrl-shift-tab", "Next / previous tab"),
    ("ctrl-o", "Open a SQL script"),
    ("ctrl-1", "Show or hide the database browser"),
    ("ctrl-b / ctrl-2", "Show or hide the files panel"),
    ("ctrl-3", "Show or hide the results panel"),
    ("ctrl-c", "Copy the selected result cell, row(s) or column"),
    (
        "shift/ctrl-click",
        "Select a range of result rows / toggle one row",
    ),
    (
        "ctrl-a",
        "Select every result row (all pages; or click the grid's top-left corner)",
    ),
    ("ctrl-shift-d", "Start debugging a routine"),
    ("f10 / f11", "Debug: step over / step into"),
    ("f5 / shift-f5", "Debug: continue / stop"),
    ("ctrl-s", "Save the script"),
    ("ctrl-,", "Open config.json in the system editor"),
    ("ctrl-plus / ctrl-minus", "Zoom in / out"),
    ("ctrl-0", "Reset zoom"),
    ("f1", "Show this help"),
    ("ctrl-q", "Quit"),
];

impl PgGuiApp {
    /// About ▸ pg-gui on GitHub: open the project page in the browser.
    // &mut self is imposed by the action listener signature.
    #[allow(clippy::unused_self)]
    pub(in crate::app) fn open_github(
        &mut self,
        _: &OpenGitHub,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.open_url(REPO_URL);
    }

    /// Show or hide the files side panel (cmd-shift-e).
    pub(in crate::app) fn toggle_files_panel(
        &mut self,
        _: &ToggleFilesPanel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.config.files_panel_visible = !self.config.files_panel_visible;
        if self.config.files_panel_visible {
            self.load_tree(cx);
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Show or hide the results panel (cmd-3).
    pub(in crate::app) fn toggle_results_panel(
        &mut self,
        _: &ToggleResultsPanel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.config.results_panel_visible = !self.config.results_panel_visible;
        self.schedule_save(cx);
        cx.notify();
    }

    /// Show or hide the database object browser (cmd-1). Loads the schema
    /// list the first time it's revealed for the current connection.
    pub(in crate::app) fn toggle_db_panel(
        &mut self,
        _: &ToggleDbPanel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.config.db_panel_visible = !self.config.db_panel_visible;
        if self.config.db_panel_visible
            && self.db_nodes.is_empty()
            && !matches!(self.db_schema_load, DbSchemaLoad::Loading)
        {
            self.load_db_schemas(cx);
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Reload the object browser from scratch (collapse to the schema list
    /// and refetch), picking up objects created since the last load — and
    /// have the language server reload its schema cache with it.
    pub(in crate::app) fn refresh_db_tree(
        &mut self,
        _: &RefreshDbTree,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.invalidate_lsp_schema();
        self.load_db_schemas(cx);
    }

    /// Toggle whether system schemas are listed, then reload.
    pub(in crate::app) fn toggle_system_schemas(&mut self, cx: &mut Context<Self>) {
        self.show_system_schemas = !self.show_system_schemas;
        self.load_db_schemas(cx);
    }

    /// Open the config file in the system default editor (cmd-,).
    /// Saved edits are picked up live by the config watcher.
    pub(in crate::app) fn open_config(
        &mut self,
        _: &OpenConfig,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = config::path() else {
            self.set_status("No config directory on this platform", cx);
            return;
        };
        // Write the current state first, so the file exists on a fresh
        // install and reflects this session rather than a stale launch.
        self.save_config();
        cx.open_with_system(&path);
        self.set_status(
            format!("Opened {} — saved edits reload live", path.display()),
            cx,
        );
    }

    /// Open a dialog listing every command and its keybinding
    /// (cmd-h on macOS, F1 elsewhere).
    // &mut self is imposed by the action listener signature.
    #[allow(clippy::unused_self)]
    pub(in crate::app) fn show_help(
        &mut self,
        _: &ShowHelp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }

        window.open_dialog(cx, |dialog, _, cx| {
            dialog
                .title("Commands")
                .w(px(520.))
                .child(
                    v_flex()
                        .gap_1()
                        .pb_2()
                        .text_sm()
                        .children(COMMANDS.iter().map(|(keys, description)| {
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .w(px(190.))
                                        .flex_none()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .child(*keys),
                                )
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(*description),
                                )
                        })),
                )
        });
    }

    pub(in crate::app) fn zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(self.config.zoom + ZOOM_STEP, cx);
    }

    pub(in crate::app) fn zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(self.config.zoom - ZOOM_STEP, cx);
    }

    pub(in crate::app) fn zoom_reset(
        &mut self,
        _: &ZoomReset,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_zoom(1.0, cx);
    }

    pub(in crate::app) fn set_theme(
        &mut self,
        action: &SetTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.config.theme == action.0 {
            return;
        }
        self.config.theme = action.0;
        self.apply_theme(window, cx);
        self.set_status(format!("Theme: {}", action.0.label()), cx);
        self.schedule_save(cx);
    }

    /// Apply the configured theme, restore the zoomed font sizes it may
    /// have reset, and re-mark the selected View ▸ Theme entry.
    pub(in crate::app) fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        apply_theme_selection(self.config.theme, window, cx);
        self.apply_zoom(cx);
        self.refresh_menus(cx);
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        // Snap to the step grid so repeated f32 steps don't accumulate drift.
        self.config.zoom = (zoom / ZOOM_STEP).round() * ZOOM_STEP;
        self.apply_zoom(cx);
        self.set_status(format!("Zoom {:.0}%", self.config.zoom * 100.), cx);
        self.schedule_save(cx);
    }

    /// Scale the theme font sizes by the configured zoom factor. All
    /// default-sized text follows `font_size` (the root sets the window rem
    /// size from it each frame); the SQL editor follows `mono_font_size`.
    pub(in crate::app) fn apply_zoom(&mut self, cx: &mut Context<Self>) {
        let zoom = self.config.zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        self.config.zoom = zoom;
        let theme = Theme::global_mut(cx);
        theme.font_size = self.base_font_size * zoom;
        theme.mono_font_size = self.base_mono_font_size * zoom;
        // The result columns were fitted to the old font size.
        self.results.update(cx, |table, cx| {
            table.delegate_mut().measure_columns(false, cx);
            table.refresh(cx);
        });
        cx.refresh_windows();
    }
}
