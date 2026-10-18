//! Drives a running pg-gui through the macOS window server.
//!
//! GPUI draws its whole window itself and publishes no accessibility tree for
//! it — the window's AX children are the three traffic-light buttons and
//! nothing else — so the tab bar, editor and status bar cannot be read from
//! outside. What *is* native, and therefore observable, is:
//!
//! - the menu bar (an `NSMenu`), including each item's check mark, which gpui
//!   bakes into the item title as a `✓ ` prefix;
//! - the window list and window titles;
//! - `config.json`, which the app rewrites whenever a tab, panel or toggle
//!   changes — the state oracle for everything drawn inside the window;
//! - the app's diagnostics log, `pg-gui.log` beside it, which reports when
//!   the language server has finished a run ([`App::log`]).
//!
//! Input goes through System Events (`keystroke` posts `CGEvent`s to the
//! frontmost app), reads through its AX bridge. Both are reached with
//! `osascript`, so the harness needs no extra crates — but the terminal
//! running `cargo test` needs Accessibility permission (System Settings ▸
//! Privacy & Security ▸ Accessibility), and keystrokes land in whichever app
//! is frontmost, so leave the machine alone while the suite runs.
//!
//! Every launch gets its own `PG_GUI_CONFIG_DIR`, which keeps the user's
//! config.json untouched and keeps the instance socket from finding (and
//! activating) the user's own running pg-gui.
//!
//! Database-backed scenarios are opt-in: they run against a database the
//! user has already started (normally the project's `docker compose`
//! server) named by [`DATABASE_ENV`], and skip themselves when it is unset.
//! The harness never starts, stops or provisions a server.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long to wait for the app to show its window. Generous because the
/// first launch after a build pays for loading a large debug binary.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for a UI reaction (a config write, a menu change).
pub const REACT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(100);
/// How long a failed window lookup is retried ([`App::window_frame`]).
const LOOKUP_RETRY: Duration = Duration::from_secs(2);
/// `PG_GUI_LOG` for every launch: warnings and errors from everything, plus
/// the language-server boundary at `debug`, whose "server diagnostics" line
/// is the only outside sign that a diagnostics run has been published.
const LOG_FILTER: &str = "warn,pg_gui::lsp=debug";

/// GUI tests share one screen, one keyboard focus and one menu bar, so they
/// must never overlap; every [`App`] holds this for its whole life.
static SCREEN: Mutex<()> = Mutex::new(());
/// Held by every [`Schema`] for its whole life, so only one scratch schema
/// exists at a time. A second one would be listed in the database panel
/// too, and could sort ahead of the running scenario's (`_e2e_1_10` before
/// `_e2e_1_4`) and take the first row it addresses. Taken before
/// [`SCREEN`], and never the other way round.
static DATABASE: Mutex<()> = Mutex::new(());
static LAUNCHES: AtomicUsize = AtomicUsize::new(0);

/// Width the files panel is pinned to by [`App::launch_with_folder`].
pub const FILES_PANEL_WIDTH: f64 = 240.;
/// Width the database panel is pinned to by [`App::launch_with_database`].
pub const DB_PANEL_WIDTH: f64 = 260.;
/// Where a side panel's widgets sit, in points from the window's top edge,
/// at 100% zoom. Measured from a capture of the files panel: the window's
/// own title bar takes 34, the panel's header row (folder name and "+")
/// follows, then the filter box, then one tree row per 34. The database
/// panel is built the same way (header row, `xsmall` filter, tree of the
/// same row height), so it shares them.
const PANEL_HEADER_Y: f64 = 50.;
const PANEL_FILTER_Y: f64 = 75.;
const PANEL_FIRST_ROW_Y: f64 = 106.;
const PANEL_ROW_HEIGHT: f64 = 34.;
/// The files panel's "Run N script(s)" bar, shown while two or more scripts
/// are picked: where its button's middle sits, and how far it pushes the
/// tree's rows down. Measured from a capture at 100% zoom.
const RUN_BAR_Y: f64 = 98.;
const RUN_BAR_HEIGHT: f64 = 24.;
/// How long one attempt of [`App::retry_until`] waits for its condition
/// before trying again.
const ATTEMPT_WAIT: Duration = Duration::from_millis(1500);
/// Height the editor panel is pinned to by [`App::launch_for_results`], so
/// the results panel below it starts at a known offset.
pub const EDITOR_HEIGHT: f64 = 200.;
/// The window's own title bar, which the editor panel sits under.
const TITLE_BAR_HEIGHT: f64 = 34.;
/// Padding around the results panel's views (the table, the log): `p_2`.
const RESULTS_PADDING: f64 = 8.;
/// Height of one results-grid row, the header row included: gpui-component's
/// `table_row_height` at the default size.
const TABLE_ROW_HEIGHT: f64 = 32.;
/// Height of one Log view line: the theme's mono font size (13) at gpui's
/// default line height.
const LOG_LINE_HEIGHT: f64 = 20.;

/// Names the pre-created database the database-backed scenarios run
/// against: a connection string, or `docker` for the project's own
/// `docker compose` server ([`DOCKER_DATABASE_URL`]). Unset or empty, those
/// scenarios skip.
pub const DATABASE_ENV: &str = "PG_GUI_E2E_DATABASE";
/// The server `docker-compose.yml` starts: host port 5433, user, password
/// and database `pgui`/`pgui`/`pgui_test`.
pub const DOCKER_DATABASE_URL: &str = "postgres://pgui:pgui@127.0.0.1:5433/pgui_test";
/// How long to wait for something that has to reach the database: the
/// app's first Run opens its session then.
pub const DB_TIMEOUT: Duration = Duration::from_secs(15);

/// The connection string [`DATABASE_ENV`] selects, or `None` when the
/// database-backed scenarios are off.
pub fn database_url() -> Option<String> {
    let value = std::env::var(DATABASE_ENV).ok()?;
    match value.trim() {
        "" => None,
        "docker" => Some(DOCKER_DATABASE_URL.to_string()),
        url => Some(url.to_string()),
    }
}

/// The connection string [`database_url`] names, or `None` after printing
/// that `test` is skipped. For scenarios that need a server but no objects
/// of their own (a query over `generate_series`); the rest use
/// [`Schema::create`].
pub fn require_database(test: &str) -> Option<String> {
    let url = database_url();
    if url.is_none() {
        eprintln!("e2e: {test} skipped; set {DATABASE_ENV}=docker (or a URL) to run it");
    }
    url
}

/// A scratch schema a database scenario owns, on a connection of the
/// test's own to the database [`database_url`] names. Everything the
/// scenario creates goes inside it, and dropping it drops the schema with
/// `CASCADE`, pass or fail, so the shared database is left as it was.
///
/// Its name, `_e2e_<pid>_<n>`, is unique per run and sorts before every
/// lowercase schema name (`nspname` compares bytewise), so it is the first
/// row of the database panel — the one row a scenario can address without
/// knowing what else the database holds.
///
/// Creating one fails the test outright when the server cannot be reached:
/// the user asked for the database scenarios, so that is an error, not a
/// skip.
pub struct Schema {
    client: postgres::Client,
    name: String,
    url: String,
    _database: MutexGuard<'static, ()>,
}

impl Schema {
    /// Connect and create the schema, or return `None` (after saying so)
    /// when [`DATABASE_ENV`] is unset, so the calling scenario can skip.
    pub fn create(test: &str) -> Option<Self> {
        let url = require_database(test)?;
        let database = DATABASE.lock().unwrap_or_else(PoisonError::into_inner);
        let client = postgres::Client::connect(&url, postgres::NoTls).unwrap_or_else(|err| {
            panic!(
                "{DATABASE_ENV} is set but the database is unreachable: {err}\n\
                 hint: start the project database with `docker compose up -d`"
            )
        });
        let name = format!(
            "_e2e_{}_{}",
            std::process::id(),
            LAUNCHES.fetch_add(1, Ordering::Relaxed)
        );
        let mut schema = Self {
            client,
            name,
            url,
            _database: database,
        };
        schema.drop_leftovers();
        schema.execute(&format!("create schema {}", schema.name));
        Some(schema)
    }

    /// Drop the scratch schemas of earlier runs. A run cut short (ctrl-c, a
    /// killed process) never reaches [`Drop`], and its schema would sort
    /// ahead of this one in the database panel.
    fn drop_leftovers(&mut self) {
        let leftovers: Vec<String> = self
            .client
            .query(
                "select nspname::text from pg_namespace where nspname like '\\_e2e\\_%'",
                &[],
            )
            .unwrap_or_else(|err| panic!("e2e database: {err}"))
            .iter()
            .map(|row| row.get(0))
            .collect();
        if !leftovers.is_empty() {
            self.end_backends_naming("_e2e_");
        }
        for name in leftovers {
            eprintln!("e2e: dropping leftover schema {name}");
            self.execute(&format!("drop schema if exists \"{name}\" cascade"));
        }
    }

    /// Terminate every other backend whose current statement names
    /// `needle`. A debug scenario's target backend stays trapped in the
    /// routine it runs (`CALL <schema>.…`) for as long as nothing continues
    /// it — after a failure, or once the app is killed — and holds locks in
    /// the schema that would make dropping it wait forever.
    fn end_backends_naming(&mut self, needle: &str) {
        let result = self.client.execute(
            "select pg_terminate_backend(pid) from pg_stat_activity \
             where pid <> pg_backend_pid() and datname = current_database() \
             and strpos(query, $1) > 0",
            &[&needle],
        );
        if let Err(err) = result {
            eprintln!("e2e: could not end backends using {needle}: {err}");
        }
    }

    /// The schema's name, safe to splice into SQL unquoted.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The connection string, for [`App::launch_with_database`].
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Run `sql` (one or more statements), panicking on failure. Every
    /// `{schema}` in it is replaced by the schema's name.
    pub fn execute(&mut self, sql: &str) {
        let sql = sql.replace("{schema}", &self.name);
        self.client
            .batch_execute(&sql)
            .unwrap_or_else(|err| panic!("e2e database: {err}\nsql: {sql}"));
    }

    /// Whether a table named `table` exists in the schema.
    pub fn table_exists(&mut self, table: &str) -> bool {
        self.client
            .query_one(
                "select exists (select 1 from pg_tables \
                 where schemaname = $1 and tablename = $2)",
                &[&self.name, &table],
            )
            .and_then(|row| row.try_get(0))
            .unwrap_or_else(|err| panic!("e2e database: {err}"))
    }

    /// Number of rows in the schema's table `table`.
    pub fn row_count(&mut self, table: &str) -> i64 {
        let sql = format!("select count(*) from {}.{table}", self.name);
        self.client
            .query_one(&sql, &[])
            .and_then(|row| row.try_get(0))
            .unwrap_or_else(|err| panic!("e2e database: {err}\nsql: {sql}"))
    }

    /// How many other backends sit in an open transaction whose last
    /// statement named the schema — a tab with autocommit off that has run
    /// something and not yet committed or rolled back.
    pub fn open_transactions(&mut self) -> i64 {
        self.client
            .query_one(
                "select count(*) from pg_stat_activity \
                 where pid <> pg_backend_pid() and datname = current_database() \
                 and state like 'idle in transaction%' and strpos(query, $1) > 0",
                &[&self.name],
            )
            .and_then(|row| row.try_get(0))
            .unwrap_or_else(|err| panic!("e2e database: {err}"))
    }

    /// The state (`idle`, `idle in transaction`, `active`, …) and last
    /// statement of every other backend whose last statement named the
    /// schema — the app's sessions, seen from outside.
    pub fn backends(&mut self) -> Vec<(String, String)> {
        self.client
            .query(
                "select coalesce(state, ''), query from pg_stat_activity \
                 where pid <> pg_backend_pid() and datname = current_database() \
                 and strpos(query, $1) > 0",
                &[&self.name],
            )
            .unwrap_or_else(|err| panic!("e2e database: {err}"))
            .iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect()
    }

    /// How many times `nextval` has been called on the schema's sequence
    /// `sequence`: its value once called, 0 before. Sequences are not
    /// transactional, so this sees a call the moment it is made, even inside
    /// a transaction that has not committed.
    pub fn sequence_calls(&mut self, sequence: &str) -> i64 {
        let sql = format!(
            "select case when is_called then last_value else 0 end from {}.{sequence}",
            self.name
        );
        self.client
            .query_one(&sql, &[])
            .and_then(|row| row.try_get(0))
            .unwrap_or_else(|err| panic!("e2e database: {err}\nsql: {sql}"))
    }
}

impl Drop for Schema {
    fn drop(&mut self) {
        let name = self.name.clone();
        self.end_backends_naming(&name);
        let sql = format!("drop schema if exists {} cascade", self.name);
        if let Err(err) = self.client.batch_execute(&sql) {
            eprintln!("e2e: could not drop schema {}: {err}", self.name);
        }
    }
}

/// A unique, empty scratch directory for one launch.
fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pg-gui-e2e-{}-{}",
        std::process::id(),
        LAUNCHES.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create e2e config dir");
    dir
}

/// A modifier held while a key is typed.
#[derive(Clone, Copy)]
pub enum Mod {
    Cmd,
    Shift,
}

impl Mod {
    fn applescript(self) -> &'static str {
        match self {
            Mod::Cmd => "command down",
            Mod::Shift => "shift down",
        }
    }

    /// The modifier's `CGEventFlags` bit, for a mouse event posted with it
    /// held.
    fn event_flag(self) -> u64 {
        match self {
            Mod::Cmd => 0x10_0000,
            Mod::Shift => 0x2_0000,
        }
    }
}

/// The config every launch starts from unless a test seeds its own: one
/// blank untitled tab, and a connection to a port nothing listens on, so the
/// app's connection attempts fail fast and never reach a real server.
pub fn default_seed() -> Value {
    serde_json::json!({
        "connection_string": "postgres://e2e@127.0.0.1:1/e2e",
        "recent_connections": [],
        "tabs": [{ "script": "", "file": null }],
        "active_tab": 0,
        "format_on_save": true,
    })
}

/// A key without a printable character, typed by its macOS virtual key code.
#[derive(Clone, Copy)]
pub enum Key {
    Return,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    F5,
    F10,
}

impl Key {
    fn code(self) -> u8 {
        match self {
            Key::Return => 36,
            Key::Tab => 48,
            Key::Escape => 53,
            Key::Up => 126,
            Key::Down => 125,
            Key::Left => 123,
            Key::Right => 124,
            Key::F5 => 96,
            Key::F10 => 109,
        }
    }
}

/// Which mouse button a click presses.
#[derive(Clone, Copy)]
pub enum Button {
    Left,
    Right,
}

/// A launched pg-gui. Killed on drop; when dropped during a failing test it
/// first saves a screenshot and keeps its scratch directory (config.json,
/// app log) for inspection.
pub struct App {
    child: Child,
    dir: PathBuf,
    _screen: MutexGuard<'static, ()>,
}

impl App {
    /// Start the app with [`default_seed`].
    pub fn launch() -> Self {
        Self::launch_with(&default_seed())
    }

    /// Start the app on its own config directory holding `seed` as
    /// config.json, and wait for its main window.
    pub fn launch_with(seed: &Value) -> Self {
        let screen = SCREEN.lock().unwrap_or_else(PoisonError::into_inner);
        Self::start(screen, scratch_dir(), seed)
    }

    /// Start the app on a scratch directory `prepare` gets first: it may
    /// write files there (a script to open, user snippets under
    /// `snippets/`) and edit `seed`, which starts as [`default_seed`].
    pub fn launch_prepared(prepare: impl FnOnce(&Path, &mut Value)) -> Self {
        let screen = SCREEN.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = scratch_dir();
        let mut seed = default_seed();
        prepare(&dir, &mut seed);
        Self::start(screen, dir, &seed)
    }

    /// Start the app with the files panel open on a fresh working folder
    /// holding `files` (paths relative to it, created empty — a path ending
    /// in `/` is a folder). The database panel is hidden and the files
    /// panel pinned to [`FILES_PANEL_WIDTH`], so its rows sit where
    /// [`App::click_files_row`] expects them. The folder lives in the scratch
    /// directory, kept with it when the test fails.
    pub fn launch_with_folder(files: &[&str]) -> Self {
        let files: Vec<_> = files.iter().map(|file| (*file, "select 1;\n")).collect();
        Self::launch_folder(default_seed(), &files)
    }

    /// [`Self::launch_with_folder`] connected to `schema`'s database (saved
    /// under the name `e2e`), each file holding the given script, for
    /// scenarios that run scripts from the files panel.
    pub fn launch_with_scripts(schema: &Schema, files: &[(&str, &str)]) -> Self {
        Self::launch_folder(database_seed(schema.url(), ""), files)
    }

    /// Seed a working folder with `files` (path and content; a path ending
    /// in `/` is a folder) and start on `seed` with the files panel laid
    /// out as [`Self::launch_with_folder`] describes.
    fn launch_folder(mut seed: Value, files: &[(&str, &str)]) -> Self {
        let screen = SCREEN.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = scratch_dir();
        let work = dir.join("work");
        std::fs::create_dir_all(&work).expect("create e2e working folder");
        for (file, content) in files {
            let path = work.join(file);
            if file.ends_with('/') {
                std::fs::create_dir_all(&path).expect("create seeded folder");
                continue;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create seeded folder");
            }
            std::fs::write(&path, content).expect("write seeded file");
        }
        // Canonical, as File ▸ Open Folder stores it: the tree's row ids
        // (and so the paths tabs open) derive from it.
        let work = work.canonicalize().expect("canonicalize working folder");
        seed["working_dir"] = Value::from(work.to_str().expect("utf-8 temp path"));
        seed["files_panel_visible"] = Value::from(true);
        seed["files_panel_width"] = Value::from(FILES_PANEL_WIDTH);
        seed["db_panel_visible"] = Value::from(false);
        Self::start(screen, dir, &seed)
    }

    /// Start the app connected to `schema`'s database with one tab holding
    /// `script`. The connection is saved under the name `e2e`. The database
    /// panel is shown and pinned to [`DB_PANEL_WIDTH`] and the files panel
    /// hidden, so the panel's rows sit where [`App::click_db_row`] expects
    /// them.
    pub fn launch_with_database(schema: &Schema, script: &str) -> Self {
        let mut seed = database_seed(schema.url(), script);
        seed["db_panel_visible"] = Value::from(true);
        seed["db_panel_width"] = Value::from(DB_PANEL_WIDTH);
        Self::launch_with(&seed)
    }

    /// Start the app on `url` (the dead seeded connection when `None`) with
    /// one tab holding `script`, laid out for the results panel: both side
    /// panels hidden, so the panel spans the window's width, and the editor
    /// pinned to [`EDITOR_HEIGHT`], so the panel's rows sit where
    /// [`results_row_y`] and [`log_line_y`] expect them.
    pub fn launch_for_results(url: Option<&str>, script: &str) -> Self {
        Self::launch_for_results_with(url, script, |_| {})
    }

    /// [`Self::launch_for_results`] with `configure` given the seed last, to
    /// set what the scenario needs on top of the pinned layout (a small
    /// `page_size` or `fetch_size`, say).
    pub fn launch_for_results_with(
        url: Option<&str>,
        script: &str,
        configure: impl FnOnce(&mut Value),
    ) -> Self {
        let mut seed = results_seed(url, script);
        configure(&mut seed);
        Self::launch_with(&seed)
    }

    /// [`Self::launch_for_results_with`] with `prepare` also given the
    /// scratch directory before launch, to write files there (a working
    /// folder the seed then points at).
    pub fn launch_for_results_prepared(
        url: Option<&str>,
        script: &str,
        prepare: impl FnOnce(&Path, &mut Value),
    ) -> Self {
        let screen = SCREEN.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = scratch_dir();
        let mut seed = results_seed(url, script);
        prepare(&dir, &mut seed);
        Self::start(screen, dir, &seed)
    }

    /// Click `button` of the results pager, for a launch through
    /// [`Self::launch_for_results`]. The pager sits on the results panel's
    /// bottom row, so its height follows the window's.
    pub fn click_pager(&mut self, button: Pager) {
        let (_, _, _, height) = self.window_frame();
        self.click_at(Button::Left, button.x(), height - PAGER_FROM_BOTTOM);
    }

    /// The working folder [`Self::launch_with_folder`] created.
    pub fn work_dir(&self) -> PathBuf {
        self.dir
            .join("work")
            .canonicalize()
            .expect("launched without a working folder")
    }

    fn start(screen: MutexGuard<'static, ()>, dir: PathBuf, seed: &Value) -> Self {
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_string_pretty(seed).expect("serialize seed"),
        )
        .expect("write seed config.json");
        let log = std::fs::File::create(dir.join("stdio.log")).expect("create stdio log");
        let child = Command::new(env!("CARGO_BIN_EXE_pg-gui"))
            .env("PG_GUI_CONFIG_DIR", &dir)
            .env("PG_GUI_LOG", LOG_FILTER)
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("clone stdio log"))
            .stderr(log)
            .spawn()
            .expect("spawn pg-gui");
        let mut app = App {
            child,
            dir,
            _screen: screen,
        };
        app.wait_for("main window", LAUNCH_TIMEOUT, |app| {
            app.assert_running();
            // System Events does not know the process for a moment after
            // the spawn; until it does, the lookup fails rather than
            // answering "no windows".
            let process = app.process();
            osascript_result(&format!(
                "tell application \"System Events\" to return name of every window of {process}"
            ))
            .is_ok_and(|titles| list(&titles).iter().any(|title| is_main_title(title)))
        });
        app.focus();
        app
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// `AppleScript` addressing this process, unambiguous even when the
    /// user's own pg-gui is running alongside.
    fn process(&self) -> String {
        format!(
            "(first application process whose unix id is {})",
            self.pid()
        )
    }

    /// Panic if the app has exited (crashed, or refused to start).
    fn assert_running(&mut self) {
        if let Some(status) = self.child.try_wait().expect("poll pg-gui") {
            panic!("pg-gui exited early with {status}");
        }
    }

    /// Bring the app to the front, so keystrokes reach it.
    pub fn focus(&mut self) {
        let process = self.process();
        osascript(&format!(
            "tell application \"System Events\" to set frontmost of {process} to true"
        ));
        self.wait_for("app to become frontmost", REACT_TIMEOUT, |app| {
            let process = app.process();
            osascript(&format!(
                "tell application \"System Events\" to return frontmost of {process}"
            )) == "true"
        });
    }

    /// Type `key` (a single character) with `mods` held, the way a user
    /// presses a shortcut.
    pub fn send_keys(&mut self, mods: &[Mod], key: char) {
        self.focus();
        let using = if mods.is_empty() {
            String::new()
        } else {
            let mods: Vec<_> = mods.iter().map(|m| m.applescript()).collect();
            format!(" using {{{}}}", mods.join(", "))
        };
        let key = applescript_string(&key.to_string());
        osascript(&format!(
            "tell application \"System Events\" to keystroke {key}{using}"
        ));
    }

    /// Type `text` into whatever has the keyboard focus.
    pub fn type_text(&mut self, text: &str) {
        self.focus();
        let text = applescript_string(text);
        osascript(&format!(
            "tell application \"System Events\" to keystroke {text}"
        ));
    }

    /// Press a non-printing key with `mods` held.
    pub fn press(&mut self, mods: &[Mod], key: Key) {
        self.focus();
        let using = if mods.is_empty() {
            String::new()
        } else {
            let mods: Vec<_> = mods.iter().map(|m| m.applescript()).collect();
            format!(" using {{{}}}", mods.join(", "))
        };
        osascript(&format!(
            "tell application \"System Events\" to key code {}{using}",
            key.code()
        ));
    }

    /// The main window's frame in screen points: `(x, y, width, height)`,
    /// origin at the top left of the main display.
    ///
    /// System Events now and then fails to find a window that is plainly
    /// there ("Invalid index") — seen under a tight loop of captures — so a
    /// failed lookup is retried for a moment before it counts.
    pub fn window_frame(&self) -> (f64, f64, f64, f64) {
        let process = self.process();
        let script = format!(
            "tell application \"System Events\" to tell (first window of {process} whose name starts with \"pg-gui\")\n\
             set {{x, y}} to position\n\
             set {{w, h}} to size\n\
             return (x as text) & \",\" & (y as text) & \",\" & (w as text) & \",\" & (h as text)\n\
             end tell"
        );
        let start = Instant::now();
        let frame = loop {
            match osascript_result(&script) {
                Ok(frame) => break frame,
                Err(err) if start.elapsed() > LOOKUP_RETRY => panic!("{err}"),
                Err(_) => std::thread::sleep(POLL),
            }
        };
        let parts: Vec<f64> = frame
            .split(',')
            .map(|part| part.trim().parse().expect("window frame number"))
            .collect();
        (parts[0], parts[1], parts[2], parts[3])
    }

    /// Click at `(x, y)` points from the main window's top-left corner.
    ///
    /// GPUI draws the whole window itself, so there is nothing to address
    /// but a position; System Events' own `click at` can only press the
    /// left button, so the events are posted through `CoreGraphics` from
    /// JXA. The pointer moves there first, so hover state (a row's hidden
    /// action, a tooltip) settles before the press.
    pub fn click_at(&mut self, button: Button, x: f64, y: f64) {
        self.click_with(button, &[], x, y);
    }

    /// [`Self::click_at`] with `mods` held, the way a shift- or cmd-click
    /// extends a selection. The flags go on the button events themselves:
    /// gpui reads a click's modifiers from the event, not from the keyboard.
    pub fn click_with(&mut self, button: Button, mods: &[Mod], x: f64, y: f64) {
        self.focus();
        let flags: u64 = mods.iter().map(|m| m.event_flag()).sum();
        let (left, top, _, _) = self.window_frame();
        let (x, y) = (left + x, top + y);
        let (down, up, which) = match button {
            Button::Left => (
                "kCGEventLeftMouseDown",
                "kCGEventLeftMouseUp",
                "kCGMouseButtonLeft",
            ),
            Button::Right => (
                "kCGEventRightMouseDown",
                "kCGEventRightMouseUp",
                "kCGMouseButtonRight",
            ),
        };
        jxa(&format!(
            "ObjC.import('CoreGraphics');\n\
             const at = $.CGPointMake({x}, {y});\n\
             const post = (type, button, flags) => {{\n\
                 const event = $.CGEventCreateMouseEvent($(), type, at, button);\n\
                 $.CGEventSetFlags(event, flags);\n\
                 $.CGEventPost($.kCGHIDEventTap, event);\n\
             }};\n\
             post($.kCGEventMouseMoved, $.kCGMouseButtonLeft, 0);\n\
             delay(0.2);\n\
             post($.{down}, $.{which}, {flags});\n\
             delay(0.05);\n\
             post($.{up}, $.{which}, {flags});"
        ));
    }

    /// Press the left button at `from` and release it at `to`, both in
    /// points from the main window's top-left corner, moving through a few
    /// drag events in between, the way a text selection is swept.
    pub fn drag(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.focus();
        let (left, top, _, _) = self.window_frame();
        let (x0, y0) = (left + from.0, top + from.1);
        let (x1, y1) = (left + to.0, top + to.1);
        jxa(&format!(
            "ObjC.import('CoreGraphics');\n\
             const post = (type, x, y) => $.CGEventPost($.kCGHIDEventTap,\n\
                 $.CGEventCreateMouseEvent($(), type, $.CGPointMake(x, y), $.kCGMouseButtonLeft));\n\
             post($.kCGEventMouseMoved, {x0}, {y0});\n\
             delay(0.2);\n\
             post($.kCGEventLeftMouseDown, {x0}, {y0});\n\
             const steps = 10;\n\
             for (let i = 1; i <= steps; i++) {{\n\
                 delay(0.03);\n\
                 post($.kCGEventLeftMouseDragged,\n\
                     {x0} + ({x1} - {x0}) * i / steps, {y0} + ({y1} - {y0}) * i / steps);\n\
             }}\n\
             delay(0.05);\n\
             post($.kCGEventLeftMouseUp, {x1}, {y1});"
        ));
    }

    /// Wait for the app to show a window besides the main one (a save
    /// panel).
    pub fn wait_for_extra_window(&mut self, what: &str) {
        self.wait_for(what, REACT_TIMEOUT, |app| app.window_titles().len() > 1);
    }

    /// Click the files panel's header "+" (new script in the working
    /// folder).
    pub fn click_files_new_script(&mut self) {
        let (_, _, width, _) = self.window_frame();
        self.click_at(Button::Left, width - 17., PANEL_HEADER_Y);
    }

    /// Replace the files panel's filter text with `text`.
    pub fn filter_files(&mut self, text: &str) {
        let (_, _, width, _) = self.window_frame();
        self.click_at(Button::Left, width - FILES_PANEL_WIDTH / 2., PANEL_FILTER_Y);
        self.send_keys(&[Mod::Cmd], 'a');
        self.type_text(text);
    }

    /// Click the files panel's tree row `row` (0 = top) with `button`.
    pub fn click_files_row(&mut self, button: Button, row: u32) {
        let (_, _, width, _) = self.window_frame();
        self.click_at(button, width - FILES_PANEL_WIDTH / 2., row_y(row));
    }

    /// Left-click the files panel's tree row `row` with `mods` held: shift
    /// extends the selection from the last plain click, cmd toggles the row.
    /// `picked` says whether two or more scripts are picked before the
    /// click: the panel then shows its "Run N script(s)" bar above the tree,
    /// which pushes every row down by [`RUN_BAR_HEIGHT`].
    pub fn click_files_row_with(&mut self, mods: &[Mod], row: u32, picked: bool) {
        let (_, _, width, _) = self.window_frame();
        let bar = if picked { RUN_BAR_HEIGHT } else { 0. };
        self.click_with(
            Button::Left,
            mods,
            width - FILES_PANEL_WIDTH / 2.,
            row_y(row) + bar,
        );
    }

    /// Click the files panel's "Run N script(s)" bar, shown while two or
    /// more scripts are picked.
    pub fn click_files_run_bar(&mut self) {
        let (_, _, width, _) = self.window_frame();
        self.click_at(Button::Left, width - FILES_PANEL_WIDTH / 2., RUN_BAR_Y);
    }

    /// Click the database panel's tree row `row` (0 = top) with `button`.
    /// A folder row toggles open or shut; an object row opens its
    /// definition.
    pub fn click_db_row(&mut self, button: Button, row: u32) {
        self.click_at(button, DB_PANEL_WIDTH / 2., row_y(row));
    }

    /// Make sure the database panel's folder row `row` is expanded, whatever
    /// state it was in: the click toggles it and focuses the tree, then
    /// Right expands it only if the click shut it. Safe to repeat, and a
    /// no-op on an empty row or a disabled placeholder.
    pub fn expand_db_row(&mut self, row: u32) {
        self.click_db_row(Button::Left, row);
        self.press(&[], Key::Right);
    }

    /// Repeat `attempt` until `config.json` satisfies `pred`, giving each
    /// attempt [`ATTEMPT_WAIT`] to show; panics after `timeout`.
    ///
    /// For steps that wait on something nothing outside the window reports —
    /// the database panel's lazy folder loads — so the scenario cannot wait
    /// for it before the next click. Each attempt must be safe to repeat
    /// (see [`App::expand_db_row`]).
    pub fn retry_until(
        &mut self,
        what: &str,
        timeout: Duration,
        mut attempt: impl FnMut(&mut Self),
        pred: impl Fn(&Value) -> bool,
    ) -> Value {
        let start = Instant::now();
        loop {
            attempt(self);
            let tried = Instant::now();
            while tried.elapsed() < ATTEMPT_WAIT {
                let config = self.config();
                if pred(&config) {
                    return config;
                }
                std::thread::sleep(POLL);
            }
            assert!(
                start.elapsed() < timeout,
                "timed out after {timeout:?} retrying {what}; last config.json: {}",
                self.config()
            );
        }
    }

    /// Resize the main window to `width` × `height` points, keeping its
    /// position.
    pub fn set_window_size(&mut self, width: f64, height: f64) {
        let process = self.process();
        osascript(&format!(
            "tell application \"System Events\" to tell (first window of {process} whose name starts with \"pg-gui\") to set size to {{{width}, {height}}}"
        ));
    }

    /// Open the tab bar's "» N" menu of clipped tabs and pick its `item`
    /// (0 = top), for a launch with both side panels hidden, so the tab bar
    /// ends at the window's right edge. Escape first closes a menu a previous
    /// attempt left open, so a retry never toggles it shut.
    pub fn pick_hidden_tab(&mut self, item: u32) {
        let (_, _, width, _) = self.window_frame();
        let button_x = width - HIDDEN_TABS_FROM_RIGHT;
        self.press(&[], Key::Escape);
        self.click_at(Button::Left, button_x, TAB_BAR_Y);
        std::thread::sleep(DIALOG_SETTLE);
        self.click_at(
            Button::Left,
            button_x - HIDDEN_TABS_MENU_INSET,
            HIDDEN_TABS_FIRST_ITEM_Y + HIDDEN_TABS_ITEM_HEIGHT * f64::from(item),
        );
    }

    /// Titles of the app's windows.
    pub fn window_titles(&self) -> Vec<String> {
        let process = self.process();
        list(&osascript(&format!(
            "tell application \"System Events\" to return name of every window of {process}"
        )))
    }

    /// Titles of the menu bar's top-level menus.
    pub fn menu_bar(&self) -> Vec<String> {
        let process = self.process();
        list(&osascript(&format!(
            "tell application \"System Events\" to return name of every menu bar item of menu bar 1 of {process}"
        )))
    }

    /// Titles of the items in one top-level menu, separators omitted. A
    /// checked item keeps gpui's `✓ ` prefix; see [`unchecked`].
    pub fn menu_items(&self, menu: &str) -> Vec<String> {
        let process = self.process();
        let menu = applescript_string(menu);
        list(&osascript(&format!(
            "tell application \"System Events\" to return name of every menu item of menu 1 of menu bar item {menu} of menu bar 1 of {process}"
        )))
        .into_iter()
        .filter(|title| title != "missing value")
        .collect()
    }

    /// The live title of `item` in `menu`, matched with or without its check
    /// mark.
    fn menu_item_title(&self, menu: &str, item: &str) -> String {
        let items = self.menu_items(menu);
        items
            .iter()
            .find(|title| unchecked(title) == item)
            .cloned()
            .unwrap_or_else(|| panic!("no {item:?} in the {menu:?} menu: {items:?}"))
    }

    /// Whether `item` in `menu` carries a check mark.
    pub fn menu_checked(&self, menu: &str, item: &str) -> bool {
        self.menu_item_title(menu, item) != item
    }

    /// Click `item` in the top-level `menu`, as a user would.
    pub fn click_menu(&mut self, menu: &str, item: &str) {
        self.focus();
        let title = applescript_string(&self.menu_item_title(menu, item));
        let menu = applescript_string(menu);
        let process = self.process();
        osascript(&format!(
            "tell application \"System Events\" to click menu item {title} of menu 1 of menu bar item {menu} of menu bar 1 of {process}"
        ));
    }

    /// Titles of the items in the `submenu` of the top-level `menu`,
    /// separators omitted. A checked item keeps gpui's `✓ ` prefix.
    pub fn submenu_items(&self, menu: &str, submenu: &str) -> Vec<String> {
        let (menu, submenu) = (applescript_string(menu), applescript_string(submenu));
        let process = self.process();
        list(&osascript(&format!(
            "tell application \"System Events\" to return name of every menu item of menu 1 of menu item {submenu} of menu 1 of menu bar item {menu} of menu bar 1 of {process}"
        )))
        .into_iter()
        .filter(|title| title != "missing value")
        .collect()
    }

    /// The live title of `item` in a submenu, matched with or without its
    /// check mark.
    fn submenu_item_title(&self, menu: &str, submenu: &str, item: &str) -> String {
        let items = self.submenu_items(menu, submenu);
        items
            .iter()
            .find(|title| unchecked(title) == item)
            .cloned()
            .unwrap_or_else(|| panic!("no {item:?} in {menu} ▸ {submenu}: {items:?}"))
    }

    /// Whether `item` in the `submenu` of `menu` carries a check mark.
    pub fn submenu_checked(&self, menu: &str, submenu: &str, item: &str) -> bool {
        self.submenu_item_title(menu, submenu, item) != item
    }

    /// Click `item` in the `submenu` of the top-level `menu` (Connection ▸
    /// Recent ▸ a saved connection), matched with or without its check mark.
    pub fn click_submenu(&mut self, menu: &str, submenu: &str, item: &str) {
        self.focus();
        let item = applescript_string(&self.submenu_item_title(menu, submenu, item));
        let (menu, submenu) = (applescript_string(menu), applescript_string(submenu));
        let process = self.process();
        osascript(&format!(
            "tell application \"System Events\" to click menu item {item} of menu 1 of menu item {submenu} of menu 1 of menu bar item {menu} of menu bar 1 of {process}"
        ));
    }

    /// The status bar's text, read off a capture of the window with the
    /// Vision framework's text recognition (from JXA, so no extra crates):
    /// the status bar is drawn by GPUI and has no other way out. The whole
    /// window is recognised and the runs whose box sits within
    /// [`STATUS_BAR_HEIGHT`] of its bottom edge are kept, left to right,
    /// joined by single spaces — recognising the thin strip alone loses
    /// whole runs of it. OCR is good but not exact (`AI` reads as `Al`, a
    /// `·` as `•` or `-`), so callers match on distinctive words.
    pub fn status_bar_text(&self) -> String {
        let (_, _, _, height) = self.window_frame();
        let shot = self.dir.join("status-bar.png");
        assert!(self.screenshot(&shot), "capture the window");
        let path = shot.to_str().expect("utf-8 temp path");
        // Vision's boxes are normalised, origin at the bottom left.
        let band = STATUS_BAR_HEIGHT / height;
        let text = run_osascript(
            &["-l", "JavaScript", "-"],
            &format!(
                "ObjC.import('Vision');\n\
                 const request = $.VNRecognizeTextRequest.alloc.init;\n\
                 request.usesLanguageCorrection = false;\n\
                 const handler = $.VNImageRequestHandler.alloc\n\
                     .initWithURLOptions($.NSURL.fileURLWithPath({path:?}), $({{}}));\n\
                 handler.performRequestsError($([request]), null);\n\
                 const found = [];\n\
                 const results = request.results;\n\
                 for (let i = 0; i < results.count; i++) {{\n\
                     const observation = results.objectAtIndex(i);\n\
                     const box = observation.boundingBox;\n\
                     if (box.origin.y + box.size.height / 2 > {band}) continue;\n\
                     found.push([box.origin.x,\n\
                         observation.topCandidates(1).objectAtIndex(0).string.js]);\n\
                 }}\n\
                 found.sort((a, b) => a[0] - b[0]).map(f => f[1]).join(' ');"
            ),
        )
        .unwrap_or_else(|err| panic!("{err}"));
        let _ = std::fs::remove_file(&shot);
        text
    }

    /// Brightness (0 = black, 1 = white) of the pixel drawn at `(x, y)`
    /// points from the main window's top-left corner, read off a capture of
    /// the window — the one way to see what GPUI actually painted.
    pub fn brightness_at(&self, x: f64, y: f64) -> f64 {
        let shot = self.dir.join("brightness.png");
        assert!(self.screenshot(&shot), "capture the window");
        let path = shot.to_str().expect("utf-8 temp path");
        // The capture is in pixels, the window in points: scale by their
        // ratio (2 on a Retina display).
        let out = run_osascript(
            &["-l", "JavaScript", "-"],
            &format!(
                "ObjC.import('AppKit');\n\
                 const rep = $.NSBitmapImageRep.imageRepWithContentsOfFile({path:?});\n\
                 const scale = rep.pixelsWide / rep.size.width;\n\
                 const color = rep.colorAtXY(Math.round({x} * scale), Math.round({y} * scale))\n\
                     .colorUsingColorSpace($.NSColorSpace.sRGBColorSpace);\n\
                 (color.redComponent + color.greenComponent + color.blueComponent) / 3;"
            ),
        )
        .unwrap_or_else(|err| panic!("{err}"));
        let _ = std::fs::remove_file(&shot);
        out.parse().expect("brightness number")
    }

    /// Fill the connection dialog (Connection ▸ New Connection…, already
    /// opened) with `name` and the connection string `url`, then press its
    /// Connect button. The dialog's fields have no tab order, so each is
    /// clicked at its offset ([`ConnectionDialog`]) and typed into; the
    /// connection string drives the host, port, database and user fields.
    pub fn fill_connection_dialog(&mut self, name: &str, url: &str) {
        // Nothing outside the window reports the dialog opening; it shows on
        // the frame after the menu action, so give it a moment rather than
        // clicking through to the editor below.
        std::thread::sleep(DIALOG_SETTLE);
        self.click_connection_dialog(ConnectionDialog::Name);
        self.send_keys(&[Mod::Cmd], 'a');
        self.type_text(name);
        self.click_connection_dialog(ConnectionDialog::ConnectionString);
        self.send_keys(&[Mod::Cmd], 'a');
        self.type_text(url);
        self.click_connection_dialog(ConnectionDialog::Connect);
    }

    /// Click `target` in the open connection dialog.
    fn click_connection_dialog(&mut self, target: ConnectionDialog) {
        self.click_in_dialog(target.offset());
    }

    /// Click at `(dx, dy)` in the open dialog: points from the window's
    /// centre horizontally, and from the dialog's top edge vertically.
    fn click_in_dialog(&mut self, (dx, dy): (f64, f64)) {
        let (_, _, width, height) = self.window_frame();
        // gpui-component centres a dialog horizontally and puts its top a
        // tenth of the window's height down.
        self.click_at(Button::Left, width / 2. + dx, height / 10. + dy);
    }

    /// Start a debug session on the routine whose `CREATE` is under the
    /// cursor: cmd-shift-d opens the launch dialog with its name filled in,
    /// the "Stop on entry" box (checked by default) is unticked unless
    /// `stop_on_entry`, and Enter starts the session.
    pub fn start_debug(&mut self, stop_on_entry: bool) {
        self.send_keys(&[Mod::Cmd, Mod::Shift], 'd');
        // As in `fill_connection_dialog`: nothing outside the window says
        // when the dialog has drawn.
        std::thread::sleep(DIALOG_SETTLE);
        if !stop_on_entry {
            self.click_in_dialog(STOP_ON_ENTRY_OFFSET);
        }
        self.press(&[], Key::Return);
    }

    /// Insert a snippet through the Edit ▸ Snippets picker (cmd-p): its
    /// search box starts focused, `filter` narrows the list, and Enter
    /// inserts the first match at the cursor.
    pub fn pick_snippet(&mut self, filter: &str) {
        self.send_keys(&[Mod::Cmd], 'p');
        // As in `fill_connection_dialog`: nothing outside the window says
        // when the picker has drawn, nor when the filter has applied.
        std::thread::sleep(DIALOG_SETTLE);
        self.type_text(filter);
        std::thread::sleep(DIALOG_SETTLE);
        self.press(&[], Key::Return);
    }

    /// Click the breakpoint dot of editor line `row` (0 = first buffer
    /// line), setting or clearing its breakpoint, for a launch through
    /// [`Self::launch_for_results`] (or `_with`): the editor then starts at
    /// the window's left edge right under the tab bar, scrolled to the top,
    /// so `row` must fit inside the editor's height — [`EDITOR_HEIGHT`]
    /// unless the launch set another.
    pub fn toggle_breakpoint(&mut self, row: u32) {
        self.click_at(
            Button::Left,
            BREAKPOINT_X,
            EDITOR_FIRST_LINE_Y + EDITOR_LINE_HEIGHT * f64::from(row),
        );
    }

    /// Hold cmd over editor text at buffer `row`, column `col` (0-based,
    /// in characters), wait `dwell` for the editor to resolve what is
    /// under the pointer, and click there — Go to Definition. For a launch
    /// through [`Self::launch_for_results`]: the editor's text starts at a
    /// fixed offset and is monospaced. The pointer moves with cmd already
    /// in the event's flags, since gpui reads a hover's modifiers from the
    /// mouse event, and the link only resolves on a cmd-hover.
    pub fn cmd_click_text(&mut self, row: u32, col: u32, dwell: Duration) {
        self.focus();
        let (left, top, _, _) = self.window_frame();
        let (x, y) = text_point(row, col);
        let (x, y) = (left + x, top + y);
        let cmd = Mod::Cmd.event_flag();
        let dwell = dwell.as_secs_f64();
        jxa(&format!(
            "ObjC.import('CoreGraphics');\n\
             const at = $.CGPointMake({x}, {y});\n\
             const post = (type) => {{\n\
                 const event = $.CGEventCreateMouseEvent($(), type, at, $.kCGMouseButtonLeft);\n\
                 $.CGEventSetFlags(event, {cmd});\n\
                 $.CGEventPost($.kCGHIDEventTap, event);\n\
             }};\n\
             post($.kCGEventMouseMoved);\n\
             delay({dwell});\n\
             post($.kCGEventMouseMoved);\n\
             delay(0.1);\n\
             post($.kCGEventLeftMouseDown);\n\
             delay(0.05);\n\
             post($.kCGEventLeftMouseUp);"
        ));
    }

    /// Rest the pointer on editor text at buffer `row`, column `col`, as
    /// [`Self::cmd_click_text`] addresses it, with no button or modifier:
    /// a hover. It comes in from a blank spot further right, so the editor
    /// sees a fresh move onto the text even when the pointer was already
    /// there.
    pub fn hover_text(&mut self, row: u32, col: u32) {
        self.focus();
        let (left, top, width, _) = self.window_frame();
        let (x, y) = text_point(row, col);
        let (x, y) = (left + x, top + y);
        let away = left + width - 40.;
        jxa(&format!(
            "ObjC.import('CoreGraphics');\n\
             const move = (x) => $.CGEventPost($.kCGHIDEventTap,\n\
                 $.CGEventCreateMouseEvent($(), $.kCGEventMouseMoved, $.CGPointMake(x, {y}), $.kCGMouseButtonLeft));\n\
             move({away});\n\
             delay(0.2);\n\
             move({x});"
        ));
    }

    /// The config.json the app last wrote (or the seed, before its first
    /// write). A half-written file reads as `Null`; callers poll anyway.
    pub fn config(&self) -> Value {
        std::fs::read_to_string(self.dir.join("config.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null)
    }

    /// The app's diagnostics log so far (`pg-gui.log`, filtered by
    /// [`LOG_FILTER`]); empty before the app has written it.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("pg-gui.log")).unwrap_or_default()
    }

    /// Poll until `config.json` satisfies `pred`; panics with the last config
    /// read on timeout.
    pub fn wait_for_config(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        self.wait_for(what, REACT_TIMEOUT, |app| pred(&app.config()));
        self.config()
    }

    /// Poll `cond` until it holds, panicking after `timeout`.
    pub fn wait_for(
        &mut self,
        what: &str,
        timeout: Duration,
        mut cond: impl FnMut(&mut Self) -> bool,
    ) {
        let start = Instant::now();
        loop {
            if cond(self) {
                return;
            }
            assert!(
                start.elapsed() < timeout,
                "timed out after {timeout:?} waiting for {what}; last config.json: {}",
                self.config()
            );
            std::thread::sleep(POLL);
        }
    }

    /// Capture the app's main window into `path` (PNG).
    pub fn screenshot(&self, path: &Path) -> bool {
        let process = self.process();
        let frame = osascript_result(&format!(
            "tell application \"System Events\" to tell window 1 of {process}\n\
             set {{x, y}} to position\n\
             set {{w, h}} to size\n\
             return (x as text) & \",\" & (y as text) & \",\" & (w as text) & \",\" & (h as text)\n\
             end tell"
        ));
        let Ok(frame) = frame else {
            return false;
        };
        Command::new("screencapture")
            .args(["-x", "-R", &frame])
            .arg(path)
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Quit with cmd-q and wait for the process to exit.
    pub fn quit(mut self) -> ExitStatus {
        self.send_keys(&[Mod::Cmd], 'q');
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("poll pg-gui") {
                return status;
            }
            assert!(
                start.elapsed() < REACT_TIMEOUT,
                "pg-gui still running {REACT_TIMEOUT:?} after cmd-q"
            );
            std::thread::sleep(POLL);
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let failed = std::thread::panicking();
        if failed && self.child.try_wait().ok().flatten().is_none() {
            let shot = self.dir.join("failure.png");
            if self.screenshot(&shot) {
                eprintln!("e2e: screenshot saved to {}", shot.display());
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if failed {
            eprintln!("e2e: config and app log kept in {}", self.dir.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// The connection-string seed shared by the database launches: `url` saved
/// under the name `e2e`, one tab holding `script`, the files panel hidden.
pub fn database_seed(url: &str, script: &str) -> Value {
    let mut seed = default_seed();
    seed["connection_string"] = Value::from(url);
    seed["recent_connections"] = serde_json::json!([{ "name": "e2e", "url": url }]);
    seed["tabs"] = serde_json::json!([{ "script": script, "file": null }]);
    seed["files_panel_visible"] = Value::from(false);
    seed
}

/// Height of the band at the window's bottom that [`App::status_bar_text`]
/// reads: the status bar (a small line with its padding) and nothing above
/// it. Measured from a capture at 100% zoom.
const STATUS_BAR_HEIGHT: f64 = 28.;
/// The tab bar's "» N" button of clipped tabs: its vertical middle, and how
/// far left of the window's right edge its middle sits when the tab bar
/// spans the window (the session toolbar takes the rest). Its menu hangs
/// from the button's right edge; the inset reaches into an item's label
/// from the button's middle. Measured from a capture at 100% zoom.
const TAB_BAR_Y: f64 = 46.;
const HIDDEN_TABS_FROM_RIGHT: f64 = 215.;
const HIDDEN_TABS_MENU_INSET: f64 = 40.;
const HIDDEN_TABS_FIRST_ITEM_Y: f64 = 79.;
const HIDDEN_TABS_ITEM_HEIGHT: f64 = 27.;
/// How long [`App::fill_connection_dialog`] waits for the dialog to draw.
const DIALOG_SETTLE: Duration = Duration::from_millis(500);

/// Where the debug launch dialog's "Stop on entry" checkbox sits, as
/// [`App::click_in_dialog`] takes it. Measured from a capture at 100% zoom.
const STOP_ON_ENTRY_OFFSET: (f64, f64) = (-215., 204.);
/// The editor's breakpoint dots, for a launch through
/// [`App::launch_for_results`]: their horizontal middle, the vertical middle
/// of the first line's, and the line height. Measured from a capture at
/// 100% zoom.
const BREAKPOINT_X: f64 = 22.;
const EDITOR_FIRST_LINE_Y: f64 = 85.;
const EDITOR_LINE_HEIGHT: f64 = 20.;
/// Left edge of the editor's text and the width of one character of its
/// monospaced font, for the same launch. Measured from a capture at 100%
/// zoom.
const EDITOR_TEXT_X: f64 = 76.8;
const EDITOR_CHAR_WIDTH: f64 = 7.8;

/// A widget of the connection dialog.
#[derive(Clone, Copy)]
enum ConnectionDialog {
    Name,
    ConnectionString,
    Connect,
}

impl ConnectionDialog {
    /// Where the widget's middle sits, in points: horizontally from the
    /// window's centre, vertically from the dialog's top edge. Measured from
    /// a capture at 100% zoom; the dialog is a fixed 520 wide.
    fn offset(self) -> (f64, f64) {
        match self {
            ConnectionDialog::Name => (0., 87.),
            ConnectionDialog::ConnectionString => (0., 459.),
            ConnectionDialog::Connect => (202., 523.),
        }
    }
}

/// The middle of the editor character at buffer `row`, column `col`, in
/// window points, for a launch through [`App::launch_for_results`].
fn text_point(row: u32, col: u32) -> (f64, f64) {
    (
        EDITOR_TEXT_X + EDITOR_CHAR_WIDTH * (f64::from(col) + 0.5),
        EDITOR_FIRST_LINE_Y + EDITOR_LINE_HEIGHT * f64::from(row),
    )
}

/// The seed of [`App::launch_for_results`]: `url` (the dead seeded
/// connection when `None`) with one tab holding `script`, both side panels
/// hidden and the editor pinned to [`EDITOR_HEIGHT`].
fn results_seed(url: Option<&str>, script: &str) -> Value {
    let mut seed = url.map_or_else(
        || {
            let mut seed = default_seed();
            seed["tabs"] = serde_json::json!([{ "script": script, "file": null }]);
            seed
        },
        |url| database_seed(url, script),
    );
    seed["db_panel_visible"] = Value::from(false);
    seed["files_panel_visible"] = Value::from(false);
    seed["results_panel_visible"] = Value::from(true);
    seed["editor_height"] = Value::from(EDITOR_HEIGHT);
    seed
}

/// Top edge of the results panel's content (inside its padding), in window
/// points, for a launch through [`App::launch_for_results`].
fn results_top() -> f64 {
    TITLE_BAR_HEIGHT + EDITOR_HEIGHT + RESULTS_PADDING
}

/// Vertical middle of the results grid's data row `row` (0 = top).
pub fn results_row_y(row: u32) -> f64 {
    results_top() + TABLE_ROW_HEIGHT * (f64::from(row) + 1.5)
}

/// A point inside the Log view's line `line` (0 = top), as far left as
/// the text starts.
pub fn log_line_y(line: u32) -> f64 {
    results_top() + LOG_LINE_HEIGHT * (f64::from(line) + 0.5)
}

/// A button of the pager under the results grid, which only shows when the
/// result spans more than one page or the cursor has more rows to fetch.
#[derive(Clone, Copy)]
pub enum Pager {
    Prev,
    Next,
    FetchMore,
}

impl Pager {
    /// Horizontal middle of the button, in window points. The pager starts
    /// at the panel's left padding and its buttons keep their order and
    /// width whatever the page, so each one sits at a fixed offset.
    /// Measured from a capture at 100% zoom.
    fn x(self) -> f64 {
        match self {
            Pager::Prev => 36.,
            Pager::Next => 102.,
            Pager::FetchMore => 185.,
        }
    }
}

/// How far above the window's bottom edge the pager's buttons are
/// centred: the status bar, the results panel's bottom padding and half a
/// small button. Measured from a capture at 100% zoom.
const PAGER_FROM_BOTTOM: f64 = 52.;

/// Left edge of the results panel's content, for a launch through
/// [`App::launch_for_results`] (no database panel to its left).
pub const RESULTS_LEFT: f64 = RESULTS_PADDING;

/// The clipboard's plain text, as `pbpaste` prints it.
pub fn clipboard() -> String {
    let out = Command::new("pbpaste").output().expect("run pbpaste");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Replace the clipboard's contents with `text`, so a test can tell a
/// copy that happened from one that did not.
pub fn set_clipboard(text: &str) {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn pbcopy");
    child
        .stdin
        .take()
        .expect("pbcopy stdin")
        .write_all(text.as_bytes())
        .expect("write pbcopy");
    child.wait().expect("run pbcopy");
}

/// Top edge offset of side-panel tree row `row`, in window points.
fn row_y(row: u32) -> f64 {
    PANEL_FIRST_ROW_Y + f64::from(row) * PANEL_ROW_HEIGHT
}

/// The text of the active tab, if any.
pub fn active_script(config: &Value) -> Option<&str> {
    let active = usize::try_from(config["active_tab"].as_u64()?).ok()?;
    config["tabs"][active]["script"].as_str()
}

/// Whether `title` is the main window's: `pg-gui`, or `pg-gui — <path>`
/// while the active tab is backed by a file.
fn is_main_title(title: &str) -> bool {
    title == "pg-gui" || title.starts_with("pg-gui — ")
}

/// An item title without gpui's check-mark prefix.
pub fn unchecked(title: &str) -> &str {
    title.strip_prefix("✓ ").unwrap_or(title)
}

/// Run an `AppleScript` and return its trimmed stdout, panicking on failure.
fn osascript(script: &str) -> String {
    osascript_result(script).unwrap_or_else(|err| panic!("{err}"))
}

/// Run a JavaScript for Automation script, panicking on failure.
fn jxa(script: &str) {
    run_osascript(&["-l", "JavaScript", "-"], script).unwrap_or_else(|err| panic!("{err}"));
}

fn osascript_result(script: &str) -> Result<String, String> {
    run_osascript(&["-"], script)
}

fn run_osascript(args: &[&str], script: &str) -> Result<String, String> {
    let mut child = Command::new("osascript")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("spawn osascript: {err}"))?;
    child
        .stdin
        .take()
        .expect("osascript stdin")
        .write_all(script.as_bytes())
        .map_err(|err| format!("write osascript: {err}"))?;
    let out = child
        .wait_with_output()
        .map_err(|err| format!("run osascript: {err}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let hint = if stderr.contains("-25211") || stderr.contains("assistive") {
        "\nhint: grant the terminal running `cargo test` Accessibility permission \
         (System Settings ▸ Privacy & Security ▸ Accessibility)"
    } else {
        ""
    };
    Err(format!(
        "osascript failed: {}{hint}\nscript:\n{script}",
        stderr.trim()
    ))
}

/// Split an `AppleScript` list as `osascript` prints it (`a, b, c`). Good
/// enough for menu and window titles, none of which hold `", "`.
fn list(output: &str) -> Vec<String> {
    if output.is_empty() {
        return Vec::new();
    }
    output.split(", ").map(str::to_string).collect()
}

/// Quote `text` as an `AppleScript` string literal.
fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}
