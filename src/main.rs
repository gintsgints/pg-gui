mod ai;
mod app;
mod config;
mod db;
mod lsp;
mod results;
mod snippets;
mod statement;

use gpui::{
    Action, App, AppContext as _, Application, Bounds, KeyBinding, TitlebarOptions, WindowBounds,
    WindowOptions, actions, px, size,
};
use gpui_component::Root;

actions!(
    pg_gui,
    [
        RunQuery,
        AiComplete,
        NewFile,
        CloseTab,
        NextTab,
        PrevTab,
        OpenFile,
        SaveFile,
        OpenSnippets,
        OpenConfig,
        NewConnection,
        OpenGitHub,
        FormatScript,
        ToggleComment,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        ShowHelp,
        Quit
    ]
);

/// Connect to a specific connection string; carried by the "Recent"
/// application-menu items so each remembers which URL it reconnects to and
/// the name it was saved under (empty when unnamed).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = pg_gui, no_json)]
pub struct Connect {
    pub url: String,
    pub name: String,
}

fn main() {
    let app = Application::new();

    app.run(move |cx: &mut App| {
        gpui_component::init(cx);

        // `secondary` is cmd on macOS and ctrl elsewhere. cmd-h is taken by
        // "hide app" conventions outside macOS (and ctrl-h by backspace in
        // some Linux toolkits), so help lives on F1 there.
        let help_key = if cfg!(target_os = "macos") {
            "cmd-h"
        } else {
            "f1"
        };
        cx.bind_keys([
            KeyBinding::new("secondary-enter", RunQuery, None),
            KeyBinding::new("ctrl-enter", RunQuery, None),
            KeyBinding::new("secondary-i", AiComplete, None),
            KeyBinding::new("ctrl-space", AiComplete, None),
            KeyBinding::new("secondary-n", NewFile, None),
            KeyBinding::new("secondary-w", CloseTab, None),
            KeyBinding::new("ctrl-tab", NextTab, None),
            KeyBinding::new("ctrl-shift-tab", PrevTab, None),
            KeyBinding::new("secondary-o", OpenFile, None),
            KeyBinding::new("secondary-s", SaveFile, None),
            KeyBinding::new("secondary-p", OpenSnippets, None),
            KeyBinding::new("secondary-,", OpenConfig, None),
            KeyBinding::new("secondary-shift-f", FormatScript, None),
            KeyBinding::new("secondary-/", ToggleComment, None),
            KeyBinding::new("secondary-=", ZoomIn, None),
            KeyBinding::new("secondary-shift-=", ZoomIn, None),
            KeyBinding::new("secondary--", ZoomOut, None),
            KeyBinding::new("secondary-0", ZoomReset, None),
            KeyBinding::new(help_key, ShowHelp, None),
            KeyBinding::new("secondary-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);

        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("pg-gui".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        cx.spawn(async move |cx| {
            cx.open_window(options, |window, cx| {
                let view = app::PgGuiApp::view(window, cx);
                cx.new(|cx| Root::new(view, window, cx))
            })?;
            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });
}
