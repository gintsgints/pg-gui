//! The results panel: picking and copying from the grid, and the message
//! log. The grid and the log cannot be read from outside the window, so
//! every scenario copies what it selected (cmd-c) and reads the clipboard.
//!
//! The grid scenarios and `log_shows_server_notices` need
//! `PG_GUI_E2E_DATABASE` (they are skipped without it) but no objects of
//! their own: their SQL is over `generate_series` or a `DO` block.
//! `log_shows_failed_run_and_copies` needs no database at all — a run
//! against the seeded dead connection fails, and the failure is what lands
//! in the log.
//!
//! The paging scenarios seed a `page_size` of [`PAGE_SIZE`] (and, for the
//! cursor, a small `fetch_size`) so a short `generate_series` spans pages,
//! and drive the pager under the grid by its buttons, which have no
//! shortcut. Which page is showing is read off its first cell.
//!
//! Each launch goes through [`App::launch_for_results`], which pins the
//! layout so the grid's rows and the log's lines sit at known offsets.
//! Within the grid the scenarios click only cell (0, 0) — the first column
//! starts right of the row-header gutter whatever the columns' widths — and
//! walk from there with the arrows. (Escape then Right is no way to get
//! there: clearing the selection puts the table back in row mode, where
//! Right moves a column selection instead.)

use std::time::{Duration, Instant};

use crate::harness::{self, App, Button, Key, Mod, Pager};

/// What the clipboard is set to before each copy, so a copy that never
/// happened cannot pass for one that did.
const SENTINEL: &str = "e2e: nothing copied";

/// A five-row, three-column result: `r<row>c<col>`, 1-based, with a NULL in
/// the third column of every even row.
const GRID_QUERY: &str = "select 'r' || i || 'c1' as c1, 'r' || i || 'c2' as c2, \
     case when i % 2 = 1 then 'r' || i || 'c3' end as c3 \
     from generate_series(1, 5) as i;";

/// The grid's row `row` (1-based) as the grid copies it: cells joined by
/// tabs, a NULL as the empty string.
fn grid_line(row: u32) -> String {
    let c3 = if row % 2 == 1 {
        format!("r{row}c3")
    } else {
        String::new()
    };
    format!("r{row}c1\tr{row}c2\t{c3}")
}

/// Launch on the database with [`GRID_QUERY`] in the tab and run it, or
/// `None` when the database scenarios are off.
fn launch_grid(test: &str) -> Option<App> {
    let url = harness::require_database(test)?;
    let mut app = App::launch_for_results(Some(&url), GRID_QUERY);
    app.press(&[Mod::Cmd], Key::Return);
    Some(app)
}

/// Middle of the grid horizontally: inside some column whatever their
/// widths, never on the row-header gutter at the left.
fn grid_x(app: &App) -> f64 {
    let (_, _, width, _) = app.window_frame();
    width / 2.
}

/// Inside the grid's first column: past the row-header gutter (about 25
/// points) and well short of the second column, however the columns are
/// stretched over the window's width.
const FIRST_COLUMN_X: f64 = harness::RESULTS_LEFT + 60.;

/// Repeat `attempt`, then cmd-c, until the clipboard holds text `accept`
/// takes; panics after `timeout`. The first attempts may run before the
/// run's output has arrived — nothing outside the window says when it has —
/// so each attempt must be safe to repeat.
fn copy_until(
    app: &mut App,
    what: &str,
    timeout: Duration,
    mut attempt: impl FnMut(&mut App),
    accept: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        harness::set_clipboard(SENTINEL);
        attempt(app);
        app.send_keys(&[Mod::Cmd], 'c');
        // The copy runs on the app's next frame; give it a moment before
        // reading the clipboard back.
        std::thread::sleep(Duration::from_millis(300));
        let copied = harness::clipboard();
        if accept(&copied) {
            return copied;
        }
        assert!(
            Instant::now() < deadline,
            "timed out copying {what}; clipboard holds {copied:?}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// [`copy_until`] the clipboard holds exactly `expected`.
fn copy_exact(app: &mut App, what: &str, expected: &str, attempt: impl FnMut(&mut App)) {
    let what = format!("{what} ({expected:?})");
    copy_until(app, &what, harness::DB_TIMEOUT, attempt, |copied| {
        copied == expected
    });
}

/// Select the Log view's text the way a user does: Escape first (it shuts
/// an error dialog, and is harmless once there is none), then a sweep from
/// the first character to the far right of the panel a few lines down.
/// The log's lines are one selectable run, so the sweep takes them all.
fn sweep_log(app: &mut App) {
    let (_, _, width, _) = app.window_frame();
    app.press(&[], Key::Escape);
    app.drag(
        (harness::RESULTS_LEFT + 1., harness::log_line_y(0)),
        (width - harness::RESULTS_LEFT, harness::log_line_y(4)),
    );
}

/// Rows per page the paging scenarios seed.
const PAGE_SIZE: u32 = 3;

/// Launch on the database with a one-column `generate_series(1, rows)` in
/// the tab and run it, `PAGE_SIZE` rows to a page and `fetch_size` rows
/// fetched at a time, or `None` when the database scenarios are off.
fn launch_paged(test: &str, rows: u32, fetch_size: u32) -> Option<App> {
    let url = harness::require_database(test)?;
    let script = format!("select i from generate_series(1, {rows}) as i;");
    let mut app = App::launch_for_results_with(Some(&url), &script, |seed| {
        seed["page_size"] = PAGE_SIZE.into();
        seed["fetch_size"] = fetch_size.into();
    });
    app.press(&[Mod::Cmd], Key::Return);
    Some(app)
}

/// The values `1..=last`, one per line, as the one-column grid copies them.
fn series(last: u32) -> String {
    (1..=last)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// [`copy_exact`] the first cell of the page on show: the page starting at
/// row `first` (1-based) of the series.
fn expect_page_from(app: &mut App, first: u32) {
    copy_exact(
        app,
        &format!("the first cell of the page from row {first}"),
        &first.to_string(),
        select_first_cell,
    );
}

/// [`copy_exact`] every row the grid holds, across pages: the series up to
/// `last`.
fn expect_rows_to(app: &mut App, last: u32) {
    copy_exact(app, &format!("rows 1–{last}"), &series(last), |app| {
        select_first_cell(app);
        app.send_keys(&[Mod::Cmd], 'a');
    });
}

/// Click cell (0, 0), which gives the grid the keyboard focus and puts the
/// table in cell mode, where the arrows move a cell selection.
fn select_first_cell(app: &mut App) {
    app.click_at(Button::Left, FIRST_COLUMN_X, harness::results_row_y(0));
}

#[test]
fn grid_copies_selected_cell() {
    let Some(mut app) = launch_grid("results::grid_copies_selected_cell") else {
        return;
    };

    copy_exact(&mut app, "the first cell", "r1c1", select_first_cell);

    // The arrows walk from there: one right, two down.
    copy_exact(&mut app, "cell (2, 1)", "r3c2", |app| {
        select_first_cell(app);
        app.press(&[], Key::Right);
        app.press(&[], Key::Down);
        app.press(&[], Key::Down);
    });

    // Left from the first column wraps to the last. A NULL copies as
    // nothing at all, never as the word NULL.
    copy_exact(&mut app, "a NULL cell", "", |app| {
        select_first_cell(app);
        app.press(&[], Key::Left);
        app.press(&[], Key::Down);
    });

    app.quit();
}

#[test]
fn grid_select_all_copies_every_row() {
    let Some(mut app) = launch_grid("results::grid_select_all_copies_every_row") else {
        return;
    };

    let expected = (1..=5).map(grid_line).collect::<Vec<_>>().join("\n");
    copy_exact(&mut app, "every row", &expected, |app| {
        select_first_cell(app);
        app.send_keys(&[Mod::Cmd], 'a');
    });

    // Escape drops the row selection along with the table's own, so cmd-c
    // has nothing in the grid to copy and the rows stay off the clipboard.
    copy_until(
        &mut app,
        "nothing from the grid after Escape",
        harness::REACT_TIMEOUT,
        |app| app.press(&[], Key::Escape),
        |copied| !copied.contains("r1c1"),
    );

    app.quit();
}

#[test]
fn grid_shift_and_cmd_click_select_rows() {
    let Some(mut app) = launch_grid("results::grid_shift_and_cmd_click_select_rows") else {
        return;
    };
    let x = grid_x(&app);

    // A plain click anchors the second row, a shift-click extends to the
    // fourth.
    let range = [2, 3, 4].map(grid_line).join("\n");
    copy_exact(&mut app, "rows 2–4", &range, |app| {
        app.click_at(Button::Left, x, harness::results_row_y(1));
        app.click_with(Button::Left, &[Mod::Shift], x, harness::results_row_y(3));
    });

    // Cmd-click toggles the third row out of that range and the fifth in.
    let expected = [2, 4, 5].map(grid_line).join("\n");
    copy_exact(&mut app, "rows 2, 4 and 5", &expected, |app| {
        app.click_at(Button::Left, x, harness::results_row_y(1));
        app.click_with(Button::Left, &[Mod::Shift], x, harness::results_row_y(3));
        app.click_with(Button::Left, &[Mod::Cmd], x, harness::results_row_y(2));
        app.click_with(Button::Left, &[Mod::Cmd], x, harness::results_row_y(4));
    });

    app.quit();
}

#[test]
fn log_shows_failed_run_and_copies() {
    // The seeded connection points at a port nothing listens on, so the run
    // fails at once. The error goes to the log, which the panel switches
    // to, and to a dialog over the window.
    let mut app = App::launch_for_results(None, "select 1;");
    app.press(&[Mod::Cmd], Key::Return);

    copy_until(
        &mut app,
        "the log's error line",
        harness::REACT_TIMEOUT,
        sweep_log,
        |copied| copied.contains("Connection refused"),
    );

    app.quit();
}

#[test]
fn log_shows_server_notices() {
    let Some(url) = harness::require_database("results::log_shows_server_notices") else {
        return;
    };
    // A DO block returns no result set, so the run's only output is the
    // log, which the panel switches to by itself. The notices come out of
    // band and land there before the statement's own line.
    let script = "do $$ begin raise notice 'e2e first notice'; \
                  raise warning 'e2e second notice'; end $$;";
    let mut app = App::launch_for_results(Some(&url), script);
    app.press(&[Mod::Cmd], Key::Return);

    let copied = copy_until(
        &mut app,
        "the log's notices",
        harness::DB_TIMEOUT,
        sweep_log,
        |copied| copied.contains("e2e second notice"),
    );
    let first = copied.find("e2e first notice");
    let second = copied.find("e2e second notice");
    assert!(
        first.is_some() && first < second,
        "the notices in the order raised: {copied:?}"
    );

    app.quit();
}

#[test]
fn grid_pages_through_long_result() {
    // Eight rows in one fetch: pages of three, the last one short.
    let Some(mut app) = launch_paged("results::grid_pages_through_long_result", 8, 500) else {
        return;
    };

    expect_page_from(&mut app, 1);

    app.click_pager(Pager::Next);
    expect_page_from(&mut app, 4);

    app.click_pager(Pager::Next);
    expect_page_from(&mut app, 7);
    // The last page holds the two rows left over, and only those: one Down
    // reaches the eighth, and a second wraps back to the page's first row
    // rather than finding a third.
    copy_exact(&mut app, "the last row", "8", |app| {
        select_first_cell(app);
        app.press(&[], Key::Down);
    });
    copy_exact(&mut app, "the wrap past the last row", "7", |app| {
        select_first_cell(app);
        app.press(&[], Key::Down);
        app.press(&[], Key::Down);
    });

    // Next is disabled on the last page, so the page stays put.
    app.click_pager(Pager::Next);
    expect_page_from(&mut app, 7);

    app.click_pager(Pager::Prev);
    expect_page_from(&mut app, 4);

    // Select-all takes the whole result set, not just the page on show.
    expect_rows_to(&mut app, 8);

    app.quit();
}

#[test]
fn grid_fetch_more_appends_rows() {
    // Twelve rows five at a time: the run leaves the cursor open after the
    // first five, and it takes two Fetch Mores to drain it.
    let Some(mut app) = launch_paged("results::grid_fetch_more_appends_rows", 12, 5) else {
        return;
    };

    expect_rows_to(&mut app, 5);

    // Fetching keeps the page the user is on.
    app.click_pager(Pager::Next);
    expect_page_from(&mut app, 4);
    app.click_pager(Pager::FetchMore);
    expect_rows_to(&mut app, 10);
    expect_page_from(&mut app, 4);

    // The fetched rows page like the first batch: the third page starts on
    // one of them.
    app.click_pager(Pager::Next);
    expect_page_from(&mut app, 7);

    app.click_pager(Pager::FetchMore);
    expect_rows_to(&mut app, 12);

    app.quit();
}
