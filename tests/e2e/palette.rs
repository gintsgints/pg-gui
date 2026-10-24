//! The cmd-p palette's files and commands modes; its `>` snippets mode is
//! covered in `snippets`.

use std::path::Path;

use crate::harness::App;

/// Whether the active tab is backed by `path`, whichever spelling of it
/// the config uses.
fn active_tab_is(config: &serde_json::Value, path: &Path) -> bool {
    let Some(active) = config["active_tab"]
        .as_u64()
        .and_then(|ix| usize::try_from(ix).ok())
    else {
        return false;
    };
    config["tabs"][active]["file"]
        .as_str()
        .is_some_and(|file| Path::new(file).canonicalize().ok() == path.canonicalize().ok())
}

#[test]
fn plain_query_opens_matching_file() {
    let mut app = App::launch_with_folder(&["alpha.sql", "reports/", "reports/orders_by_day.sql"]);
    let target = app.work_dir().join("reports/orders_by_day.sql");

    // Scattered characters match, as in any quick-open.
    app.palette("ordday", 0);
    app.wait_for_config("the file opened in the active tab", |c| {
        active_tab_is(c, &target)
    });

    app.quit();
}

#[test]
fn colon_query_runs_command() {
    let mut app = App::launch();
    // Absent from the seed, so the default: shown.
    let shown = |c: &serde_json::Value| c["files_panel_visible"].as_bool().unwrap_or(true);
    let before = shown(&app.config());

    app.palette(":toggle files panel", 0);
    app.wait_for_config("the files panel to flip", |c| shown(c) != before);

    // cmd-shift-p opens the palette with the `:` already typed.
    app.command_palette("toggle files panel");
    app.wait_for_config("the files panel to flip back", |c| shown(c) == before);

    app.quit();
}
