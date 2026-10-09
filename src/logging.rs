//! The app's diagnostics log.
//!
//! pg-gui had no logging at all: the UI crates' own diagnostics went
//! nowhere, and the app's few failures were `eprintln!`s a windowed launch
//! never shows. [`init`] installs one subscriber for everything.
//!
//! The libraries report through two different facades — gpui-component and
//! gpui-base through `tracing`, gpui itself and the postgres driver through
//! `log` — so the subscriber is a `tracing` one with
//! `tracing-subscriber`'s `tracing-log` bridge (a default feature) turning
//! `log` records into tracing events on the way in. Installing it also sets
//! `log`'s max level, so a filtered-out record costs no formatting.
//!
//! Output goes to both a file in the config directory (what a windowed
//! launch has) and stderr (what `cargo run` has). `PG_GUI_LOG` sets the
//! filter, in the usual `target=level` syntax.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::Mutex;

use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{Layer as _, fmt};

/// The environment variable holding the filter directives.
const FILTER_VAR: &str = "PG_GUI_LOG";

/// What is logged when `PG_GUI_LOG` is unset: errors only, from the app and
/// the libraries alike. Anything more is opt-in — the libraries are chatty
/// at `debug`, and gpui logs a line per frame drop.
const DEFAULT_FILTER: &str = "error";

/// The log file's name in the config directory, beside `config.json`.
const FILE_NAME: &str = "pg-gui.log";

/// Install the subscriber, returning the file it writes to (`None` when the
/// platform has no config directory, or the file could not be opened — the
/// stderr half still works).
///
/// Call once, before anything worth logging happens; a second call is a
/// no-op, since the global subscriber is already set.
pub fn init() -> Option<PathBuf> {
    let filter = || {
        let directives = std::env::var(FILTER_VAR);
        let directives = directives.as_deref().unwrap_or(DEFAULT_FILTER);
        // A typo in the variable must not silence the log: fall back to the
        // default, and say so once the subscriber is up.
        Targets::from_str(directives)
            .or_else(|_| Targets::from_str(DEFAULT_FILTER))
            .unwrap_or_else(|_| Targets::new().with_default(tracing::Level::ERROR))
    };

    let path = path();
    let file = path.as_deref().and_then(open);
    // Only the file half is lost when the file cannot be opened, so the two
    // layers are built separately and the file one is optional.
    let to_file = file.map(|file| {
        fmt::layer()
            .with_ansi(false)
            // `Mutex<File>` is a writer in its own right: the layer takes
            // the lock per event, which is what serializes the lines.
            .with_writer(Mutex::new(file))
            .with_filter(filter())
    });
    let to_stderr = fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter());

    // `try_init` rather than `init`: a second call (a test, an embedded
    // run) must not take the app down.
    if tracing_subscriber::registry()
        .with(to_file)
        .with(to_stderr)
        .try_init()
        .is_err()
    {
        return None;
    }

    if let Ok(directives) = std::env::var(FILTER_VAR)
        && Targets::from_str(&directives).is_err()
    {
        // `error!`, not `warn!`: the fallback filter drops warnings, and
        // this is the one line that explains why the log went quiet.
        tracing::error!(
            "{FILTER_VAR}={directives:?} is not a valid filter, using {DEFAULT_FILTER}"
        );
    }
    path
}

/// Where the log file lives; `None` when the platform has no config
/// directory.
pub fn path() -> Option<PathBuf> {
    crate::config::dir().map(|dir| dir.join(FILE_NAME))
}

/// Open the log file, keeping the previous run's as `pg-gui.log.1`.
///
/// Each run starts a fresh file — a log that only grows is a log nobody
/// prunes — but the run before it is what a report about "it broke, so I
/// restarted it" needs, so one generation is kept.
fn open(path: &Path) -> Option<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    if path.exists() {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .ok()
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::str::FromStr as _;

    use tracing_subscriber::filter::Targets;

    use super::open;

    /// The directives the README documents have to parse, including the
    /// default used when `PG_GUI_LOG` is unset or malformed.
    #[test]
    fn the_documented_filters_parse() {
        for directives in [
            super::DEFAULT_FILTER,
            "debug",
            "warn,pg_gui=trace",
            "pgls_workspace=debug",
            "off",
        ] {
            assert!(
                Targets::from_str(directives).is_ok(),
                "{directives:?} should parse"
            );
        }
        assert!(Targets::from_str("nonsense=louder").is_err());
    }

    /// Each run starts a fresh file and pushes the previous one aside, so a
    /// restart does not cost the log of the run that went wrong.
    #[test]
    fn opening_keeps_the_previous_run() {
        let dir = std::env::temp_dir().join(format!("pg-gui-log-test-{}", std::process::id()));
        let path = dir.join("pg-gui.log");

        let mut first = open(&path).expect("first open");
        writeln!(first, "first run").unwrap();
        let mut second = open(&path).expect("second open");
        writeln!(second, "second run").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "second run");
        assert_eq!(
            std::fs::read_to_string(dir.join("pg-gui.log.1"))
                .unwrap()
                .trim(),
            "first run"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
