//! End-to-end UI tests: launch the real pg-gui binary and drive it through
//! the macOS window server. See `harness.rs` for what can and cannot be
//! observed from outside a GPUI window.
//!
//! These take over the keyboard focus and need Accessibility permission for
//! the terminal, so the target is only built behind the `e2e` feature and
//! runs on request:
//!
//! ```sh
//! cargo e2e   # alias: cargo test --features e2e --test e2e -- --test-threads=1
//! ```
//!
//! One module per kind of scenario. `general`, `connections`, `editor`,
//! `files_panel` and `format` need no database, nor do all but the last of
//! `snippets`, `results`' failed-run log scenario, the snippet one of
//! `completion`, the first four of `status_bar` and all but the last of
//! `editor_syntax`'s. The rest of `completion`, `query`, `db_viewer`,
//! `debugger`, `definitions`, `explain`, `export`, `hover`, `transactions`
//! and the rest of `snippets`, `results`, `status_bar` and `editor_syntax`
//! run only when
//! `PG_GUI_E2E_DATABASE` names one that is already running — `docker` for
//! the project's `docker compose` server — and skip otherwise:
//!
//! ```sh
//! PG_GUI_E2E_DATABASE=docker cargo e2e
//! ```

#![cfg(target_os = "macos")]

mod harness;

mod completion;
mod connections;
mod db_viewer;
mod debugger;
mod definitions;
mod editor;
mod editor_syntax;
mod explain;
mod export;
mod files_panel;
mod format;
mod general;
mod hover;
mod query;
mod results;
mod snippets;
mod status_bar;
mod transactions;
