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
//!   changes — the state oracle for everything drawn inside the window.
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

/// GUI tests share one screen, one keyboard focus and one menu bar, so they
/// must never overlap; every [`App`] holds this for its whole life.
static SCREEN: Mutex<()> = Mutex::new(());
static LAUNCHES: AtomicUsize = AtomicUsize::new(0);

/// Width the files panel is pinned to by [`App::launch_with_folder`].
pub const FILES_PANEL_WIDTH: f64 = 240.;
/// Where the files panel's widgets sit, in points from the window's top
/// edge, at 100% zoom. Measured from a capture: the window's own title bar
/// takes 34, the panel's header row (folder name and "+") follows, then
/// the filter box, then one tree row per 34.
const FILES_HEADER_Y: f64 = 50.;
const FILES_FILTER_Y: f64 = 75.;
const FILES_FIRST_ROW_Y: f64 = 106.;
const FILES_ROW_HEIGHT: f64 = 34.;

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
}

impl Mod {
    fn applescript(self) -> &'static str {
        match self {
            Mod::Cmd => "command down",
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
    Up,
}

impl Key {
    fn code(self) -> u8 {
        match self {
            Key::Return => 36,
            Key::Up => 126,
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

    /// Start the app with the files panel open on a fresh working folder
    /// holding `files` (paths relative to it, created empty — a path ending
    /// in `/` is a folder). The database panel is hidden and the files
    /// panel pinned to [`FILES_PANEL_WIDTH`], so its rows sit where
    /// [`App::click_files_row`] expects them. The folder lives in the scratch
    /// directory, kept with it when the test fails.
    pub fn launch_with_folder(files: &[&str]) -> Self {
        let screen = SCREEN.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = scratch_dir();
        let work = dir.join("work");
        std::fs::create_dir_all(&work).expect("create e2e working folder");
        for file in files {
            let path = work.join(file);
            if file.ends_with('/') {
                std::fs::create_dir_all(&path).expect("create seeded folder");
                continue;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create seeded folder");
            }
            std::fs::write(&path, "select 1;\n").expect("write seeded file");
        }
        // Canonical, as File ▸ Open Folder stores it: the tree's row ids
        // (and so the paths tabs open) derive from it.
        let work = work.canonicalize().expect("canonicalize working folder");
        let mut seed = default_seed();
        seed["working_dir"] = Value::from(work.to_str().expect("utf-8 temp path"));
        seed["files_panel_visible"] = Value::from(true);
        seed["files_panel_width"] = Value::from(FILES_PANEL_WIDTH);
        seed["db_panel_visible"] = Value::from(false);
        Self::start(screen, dir, &seed)
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
            .env("PG_GUI_LOG", "warn")
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
            .is_ok_and(|titles| list(&titles).iter().any(|title| title == "pg-gui"))
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
    pub fn window_frame(&self) -> (f64, f64, f64, f64) {
        let process = self.process();
        let frame = osascript(&format!(
            "tell application \"System Events\" to tell window \"pg-gui\" of {process}\n\
             set {{x, y}} to position\n\
             set {{w, h}} to size\n\
             return (x as text) & \",\" & (y as text) & \",\" & (w as text) & \",\" & (h as text)\n\
             end tell"
        ));
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
        self.focus();
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
             const post = (type, button) => $.CGEventPost($.kCGHIDEventTap,\n\
                 $.CGEventCreateMouseEvent($(), type, at, button));\n\
             post($.kCGEventMouseMoved, $.kCGMouseButtonLeft);\n\
             delay(0.2);\n\
             post($.{down}, $.{which});\n\
             delay(0.05);\n\
             post($.{up}, $.{which});"
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
        self.click_at(Button::Left, width - 17., FILES_HEADER_Y);
    }

    /// Replace the files panel's filter text with `text`.
    pub fn filter_files(&mut self, text: &str) {
        let (_, _, width, _) = self.window_frame();
        self.click_at(Button::Left, width - FILES_PANEL_WIDTH / 2., FILES_FILTER_Y);
        self.send_keys(&[Mod::Cmd], 'a');
        self.type_text(text);
    }

    /// Click the files panel's tree row `row` (0 = top) with `button`.
    pub fn click_files_row(&mut self, button: Button, row: u32) {
        let (_, _, width, _) = self.window_frame();
        let y = FILES_FIRST_ROW_Y + f64::from(row) * FILES_ROW_HEIGHT;
        self.click_at(button, width - FILES_PANEL_WIDTH / 2., y);
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

    /// The config.json the app last wrote (or the seed, before its first
    /// write). A half-written file reads as `Null`; callers poll anyway.
    pub fn config(&self) -> Value {
        std::fs::read_to_string(self.dir.join("config.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null)
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
