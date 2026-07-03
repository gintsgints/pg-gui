mod ai;
mod app;
mod config;
mod db;
mod results;

use gpui::{
    App, AppContext as _, Application, Bounds, KeyBinding, TitlebarOptions, WindowBounds,
    WindowOptions, actions, px, size,
};
use gpui_component::Root;

actions!(pg_gui, [RunQuery, AiComplete, OpenFile, SaveFile, Quit]);

fn main() {
    let app = Application::new();

    app.run(move |cx: &mut App| {
        gpui_component::init(cx);

        cx.bind_keys([
            KeyBinding::new("cmd-enter", RunQuery, None),
            KeyBinding::new("ctrl-enter", RunQuery, None),
            KeyBinding::new("cmd-i", AiComplete, None),
            KeyBinding::new("ctrl-space", AiComplete, None),
            KeyBinding::new("cmd-o", OpenFile, None),
            KeyBinding::new("cmd-s", SaveFile, None),
            KeyBinding::new("cmd-q", Quit, None),
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
