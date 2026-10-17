# UI end-to-end tests

These tests launch the real `pg-gui` binary and drive it the way a user would.
They press shortcuts, click menu items and quit with cmd-q, then check what the
app did. They run on macOS only and need no database.

```sh
cargo test --features e2e --test e2e
```

The test target is built only with the `e2e` Cargo feature (`required-features`
in `Cargo.toml`), because the tests take over the keyboard focus. Without the
feature, `cargo test` skips the target completely: it is not built, not run and
not listed. That includes `cargo test -- --ignored`, so these tests never run
alongside the database-backed tests in `src/`. The feature changes nothing in
the app itself.

## Before the first run

- **Grant Accessibility permission** to the app you run `cargo test` from
  (Terminal, iTerm, your IDE). It is under System Settings ▸ Privacy & Security
  ▸ Accessibility. Without it, every test fails with an `osascript` error that
  includes a hint about this permission. After you grant it, restart the
  terminal app.
- **Leave the machine alone while the tests run** (about 20 s for the smoke
  set). Keystrokes go to whatever app is frontmost, so if you type or click
  somewhere else, the keystrokes land there.

## Running

```sh
cargo test --features e2e --test e2e                          # all scenarios
cargo test --features e2e --test e2e new_tab_and_close_tab    # one scenario
cargo test --features e2e --test e2e -- --nocapture           # show harness output
cargo test --features e2e --test e2e -- --list                # list scenarios
```

Cargo builds the binary before the tests run, so the tests always drive the
current code. The tests run one at a time even without `--test-threads=1`,
because they share one screen and one menu bar. A mutex in the harness takes
care of that.

Leaving out `--test e2e` (`cargo test --features e2e`) also runs the unit
tests, which is harmless but slower.

## Scenarios

| Test | What it checks |
|---|---|
| `launches_with_main_window_and_menus` | The window titled `pg-gui` appears, the main menus are present, and cmd-q exits cleanly. |
| `new_tab_and_close_tab` | cmd-t adds a tab and makes it active; cmd-w on that blank tab closes it with no prompt. |
| `panel_shortcuts_toggle_panels` | cmd-1, cmd-2 and cmd-3 each flip the database, files and results panels, and pressing again flips them back. |
| `format_on_save_menu_toggles_check_mark` | Clicking Edit ▸ Format on Save flips the menu's check mark and the stored `format_on_save` setting. |

## Isolation

Each launch gets its own scratch config directory at
`$TMPDIR/pg-gui-e2e-<pid>-<n>`, passed to the app as `PG_GUI_CONFIG_DIR`. As a
result:

- Your own `config.json`, snippets and log are never read or changed.
- A pg-gui you already have open is left alone. Its single-instance socket lives
  in the real config directory, so the test instance does not find it.
- No database server is contacted. The seeded `config.json` points at
  `127.0.0.1:1`, where nothing listens, so connection attempts fail
  immediately.

When a test passes, its directory is removed.

## When a test fails

The failure message names the condition that timed out and shows the last
`config.json` it read. The test output also gives the path of the scratch
directory, which is kept and holds:

| File | Contents |
|---|---|
| `failure.png` | A capture of the app window at the moment of failure. |
| `stdio.log` | The app's stdout and stderr. |
| `pg-gui.log` | The app's diagnostics log (`PG_GUI_LOG=warn`). |
| `config.json` | The last state the app saved. |

Delete these directories when you are done with them; nothing else cleans them
up.

## How it works

The harness (`harness.rs`) talks to the app through `osascript` and System
Events. `keystroke` sends key presses to the frontmost app, and System Events'
accessibility bridge reads and clicks the menu bar. It needs no extra crates.

GPUI draws everything inside the window itself and publishes no accessibility
tree for it. To macOS Accessibility, the window contains only the three
traffic-light buttons. The tab bar, editor, status bar and results grid
**cannot be read from outside**, so the tests check these instead:

- **The menu bar.** It is a native `NSMenu`. gpui puts a check mark into the
  item title as a `✓ ` prefix, which `menu_checked` looks for.
- **Window titles.**
- **`config.json`.** The app rewrites it whenever a tab, panel or toggle
  changes, so it shows the state of everything drawn inside the window. Tests
  wait on it with `wait_for_config`.

## Writing a test

Add a function to `main.rs`:

```rust
#[test]
fn my_scenario() {
    let mut app = App::launch();               // or App::launch_with(&seed)

    app.send_keys(&[Mod::Cmd], 't');           // press a shortcut
    app.wait_for_config("a second tab", |c| {  // poll until the state shows it
        c["tabs"].as_array().map_or(0, Vec::len) == 2
    });

    app.click_menu("Edit", "Format on Save");  // click a menu item
    assert!(!app.menu_checked("Edit", "Format on Save"));

    assert!(app.quit().success());             // cmd-q and wait for exit
}
```

Harness API:

| Call | Purpose |
|---|---|
| `App::launch()` / `App::launch_with(&Value)` | Starts the app on a scratch config seeded with `default_seed()` or your own JSON, and waits for the window. |
| `send_keys(&[Mod], char)` | Brings the app to the front and types the key with modifiers held. |
| `click_menu(menu, item)` | Clicks a menu item. The item matches with or without its check mark. |
| `menu_checked(menu, item)` | Returns whether the item carries a check mark. |
| `menu_bar()`, `menu_items(menu)`, `window_titles()` | Return the titles shown by the native UI. |
| `config()` | Returns the `config.json` the app last wrote, as `serde_json::Value`. |
| `wait_for_config(what, pred)` / `wait_for(what, timeout, cond)` | Polls until the condition holds. On timeout it panics with `what` and the last config. |
| `screenshot(path)` | Captures the window as a PNG. |
| `quit()` | Sends cmd-q and returns the exit status. |

Guidelines:

- **Always wait; never sleep.** The app reacts asynchronously, so assert with
  `wait_for_config` or `wait_for`. The default timeout is `REACT_TIMEOUT` (5 s).
- **Seed the state you need** with `launch_with` (tabs, panel flags, settings)
  instead of building it up with key presses.
- **End with `quit()`** when the scenario should also prove a clean exit.
  Otherwise the drop handler kills the process.
- **Only `Mod::Cmd` exists for now.** When a scenario needs another modifier,
  add the variant and its System Events name in `Mod::applescript`. Clippy
  rejects a variant that nothing uses.
- `#![cfg(target_os = "macos")]` in `main.rs` makes the target empty on other
  platforms.
- **Lint with the feature on.** The project's clippy command does not see this
  target unless the feature is enabled:
  `cargo clippy --all-targets --features e2e -- -W clippy::pedantic -D warnings`.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `osascript failed: … not allowed assistive access` (`-25211`) | The terminal has no Accessibility permission. Grant it, then restart the terminal. |
| `timed out … waiting for app to become frontmost` | Another app keeps focus, such as a modal dialog or a full-screen app. Close it and rerun. |
| `pg-gui exited early with …` | The app crashed or refused to start. Check `stdio.log` in the kept directory. |
| Keys typed into another window | You used the keyboard or mouse during the run. Rerun and leave the machine alone. |

## Not covered yet

- **Database-backed scenarios** (running a query, viewing results or the plan).
  The results grid cannot be read from outside the window. The options are OCR
  on `screenshot`, or a debug-only state dump the app writes on request.
- **The theme-switch scenario.**
- **CI.** A runner would need Accessibility permission for the terminal process.
  GitHub-hosted macOS runners do not grant it by default, so check whether they
  allow it before adding a workflow.
