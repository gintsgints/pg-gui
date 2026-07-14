//! Single-instance enforcement over a unix domain socket.
//!
//! The first instance binds `instance.sock` next to `config.json` and
//! listens; a later launch finds the address taken, connects to the socket
//! (which the running instance treats as an "activate your window" request)
//! and exits. A socket left behind by a crash refuses the connection, so it
//! is removed and re-bound. This keeps two instances from clobbering each
//! other's `config.json` — the watcher would otherwise swap one instance's
//! tabs in for the other's on every save.

use gpui::App;
use std::path::PathBuf;

use crate::config;

/// The instance socket, held by the sole running instance for its
/// lifetime. The inner listener is `None` when the socket couldn't be
/// created (no config directory, permissions), in which case the app
/// runs unguarded rather than not at all.
pub struct InstanceSocket {
    #[cfg(unix)]
    listener: Option<std::os::unix::net::UnixListener>,
}

fn socket_path() -> Option<PathBuf> {
    config::dir().map(|dir| dir.join("instance.sock"))
}

/// Try to become the sole running instance. `None` means another instance
/// is already running — it has been asked to raise its window and the
/// caller should exit.
#[cfg(unix)]
pub fn acquire() -> Option<InstanceSocket> {
    use std::os::unix::net::{UnixListener, UnixStream};

    let unguarded = |err: Option<std::io::Error>| {
        if let Some(err) = err {
            eprintln!("pg-gui: single-instance socket unavailable: {err}");
        }
        Some(InstanceSocket { listener: None })
    };
    let Some(path) = socket_path() else {
        return unguarded(None);
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match UnixListener::bind(&path) {
        Ok(listener) => Some(InstanceSocket {
            listener: Some(listener),
        }),
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            // A successful connect proves a live instance is listening; the
            // connection itself is the "activate" request, no payload needed.
            if UnixStream::connect(&path).is_ok() {
                return None;
            }
            // Nobody home — the socket is left over from a crash.
            let _ = std::fs::remove_file(&path);
            match UnixListener::bind(&path) {
                Ok(listener) => Some(InstanceSocket {
                    listener: Some(listener),
                }),
                Err(err) => unguarded(Some(err)),
            }
        }
        Err(err) => unguarded(Some(err)),
    }
}

#[cfg(not(unix))]
pub fn acquire() -> Option<InstanceSocket> {
    Some(InstanceSocket {})
}

impl InstanceSocket {
    /// Raise the window whenever a later launch connects, and remove the
    /// socket file on graceful quit (a crash skips this; the stale socket
    /// is then detected and replaced by the next launch's `acquire`).
    #[cfg(unix)]
    pub fn install(self, cx: &mut App) {
        use futures::StreamExt as _;

        let Some(listener) = self.listener else {
            return;
        };
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        // The accept loop blocks forever, so it gets a plain OS thread
        // rather than a task on gpui's background pool; it dies with the
        // process.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stream.is_err() || tx.unbounded_send(()).is_err() {
                    break;
                }
            }
        });
        cx.spawn(async move |cx| {
            while rx.next().await.is_some() {
                cx.update(|cx| cx.activate(true));
            }
        })
        .detach();
        if let Some(path) = socket_path() {
            cx.on_app_quit(move |_| {
                let _ = std::fs::remove_file(&path);
                async {}
            })
            .detach();
        }
    }

    #[cfg(not(unix))]
    pub fn install(self, _cx: &mut App) {}
}
