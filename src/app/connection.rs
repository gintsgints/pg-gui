//! Connections: the title-bar picker, the new/edit dialog and its live test,
//! and switching — which parks one connection's untitled tabs and output and
//! brings back the next one's.

use std::cell::Cell;
use std::rc::Rc;

use gpui::Subscription;
use gpui::{
    App, AppContext as _, Context, Entity, Hsla, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, IndexPath, WindowExt as _,
    button::{Button, ButtonVariants as _},
    combobox::{ComboboxEvent, ComboboxState},
    h_flex,
    input::{Input, InputEvent, InputState},
    searchable_list::{SearchableListItem, SearchableVec},
    v_flex,
};

use crate::app::{
    ParkedResults, PgGuiApp, default_conn, mask_credentials, percent_decode, percent_encode,
    record_recent,
};
use crate::{Connect, EditConnection, NewConnection, config, db};

/// One entry of the title-bar connection combobox: a recent connection,
/// shown by its saved name (or masked url when unnamed) and identified by
/// its [`config::connection_key`], so two entries pointing at the same
/// server stay distinguishable.
#[derive(Clone)]
pub(super) struct ConnectionItem {
    key: SharedString,
    label: SharedString,
}

impl SearchableListItem for ConnectionItem {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &SharedString {
        &self.key
    }
}

/// The combobox items for the current recent-connections list, most recent
/// first — the same order as the Connection ▸ Recent menu.
fn connection_items(recents: &[config::RecentConnection]) -> SearchableVec<ConnectionItem> {
    SearchableVec::new(
        recents
            .iter()
            .map(|c| ConnectionItem {
                key: c.key().to_string().into(),
                label: if c.name.is_empty() {
                    mask_credentials(&c.url).into()
                } else {
                    c.name.clone().into()
                },
            })
            .collect::<Vec<_>>(),
    )
}

/// The Test Connection outcome, shown inline in the New Connection dialog
/// as green ("succeeded") or red ("failed: …") text.
enum ConnectionTest {
    /// No test run yet — nothing is shown.
    Idle,
    Testing,
    Ok,
    Failed(SharedString),
}

impl Render for ConnectionTest {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (text, color): (SharedString, Hsla) = match self {
            ConnectionTest::Idle => return div(),
            ConnectionTest::Testing => ("Testing connection…".into(), cx.theme().muted_foreground),
            ConnectionTest::Ok => ("Connection succeeded".into(), cx.theme().green),
            ConnectionTest::Failed(message) => (message.clone(), cx.theme().red),
        };
        // `w_full` keeps a long failure message wrapping within the dialog
        // instead of stretching it wider than the window.
        div().w_full().text_sm().text_color(color).child(text)
    }
}

/// The individual pieces of a `PostgreSQL` connection, edited as separate
/// fields in the New Connection dialog and recombined into a URL.
#[derive(Default)]
struct ConnectionParts {
    host: String,
    port: String,
    database: String,
    user: String,
    password: String,
}

/// The five text inputs of the New Connection dialog, grouped so their
/// current values can be read back into [`ConnectionParts`] in one place.
#[derive(Clone)]
struct ConnectionFields {
    host: Entity<InputState>,
    port: Entity<InputState>,
    database: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
}

/// Everything the New / Edit Connection dialog is built over: the name box,
/// the five connection fields, the editable connection-string preview, the
/// Test Connection result, and the key of the recent entry being edited
/// (`None` for a new connection).
#[derive(Clone)]
struct ConnectionForm {
    name: Entity<InputState>,
    fields: ConnectionFields,
    preview: Entity<InputState>,
    test_status: Entity<ConnectionTest>,
    replacing: Option<String>,
}

impl ConnectionFields {
    fn as_array(&self) -> [Entity<InputState>; 5] {
        [
            self.host.clone(),
            self.port.clone(),
            self.database.clone(),
            self.user.clone(),
            self.password.clone(),
        ]
    }

    /// Snapshot the fields; everything but the password is trimmed (a
    /// password may legitimately contain leading/trailing spaces).
    fn read(&self, cx: &App) -> ConnectionParts {
        ConnectionParts {
            host: self.host.read(cx).value().trim().to_string(),
            port: self.port.read(cx).value().trim().to_string(),
            database: self.database.read(cx).value().trim().to_string(),
            user: self.user.read(cx).value().trim().to_string(),
            password: self.password.read(cx).value().to_string(),
        }
    }

    /// Overwrite every field from `parts`. Used when the connection string is
    /// edited directly, to drive the individual fields from the parsed URL.
    fn set(&self, parts: &ConnectionParts, window: &mut Window, cx: &mut App) {
        self.host
            .update(cx, |s, cx| s.set_value(parts.host.clone(), window, cx));
        self.port
            .update(cx, |s, cx| s.set_value(parts.port.clone(), window, cx));
        self.database
            .update(cx, |s, cx| s.set_value(parts.database.clone(), window, cx));
        self.user
            .update(cx, |s, cx| s.set_value(parts.user.clone(), window, cx));
        self.password
            .update(cx, |s, cx| s.set_value(parts.password.clone(), window, cx));
    }
}

impl ConnectionParts {
    /// Split a `postgres://user:password@host:port/database` URL into its
    /// fields. A string that is not in URL form (e.g. key-value) yields
    /// empty fields, leaving the dialog for the user to fill in.
    fn parse(conn: &str) -> Self {
        let Some((_, rest)) = conn.split_once("://") else {
            return Self::default();
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };

        let mut parts = Self {
            database: percent_decode(path.split(['?', '#']).next().unwrap_or("")),
            ..Self::default()
        };

        let (userinfo, hostport) = match authority.rfind('@') {
            Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
            None => (None, authority),
        };
        if let Some(userinfo) = userinfo {
            match userinfo.split_once(':') {
                Some((user, password)) => {
                    parts.user = percent_decode(user);
                    parts.password = percent_decode(password);
                }
                None => parts.user = percent_decode(userinfo),
            }
        }
        // A bracketed IPv6 host keeps its own colons; only a trailing
        // `:port` after the closing bracket (or on a bare host) is the port.
        match hostport.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(']') => {
                parts.host = host.to_string();
                parts.port = port.to_string();
            }
            _ => parts.host = hostport.to_string(),
        }
        parts
    }

    /// Recombine the fields into a `postgres://` URL, percent-encoding the
    /// credential and database segments.
    fn to_url(&self) -> String {
        let mut url = String::from("postgres://");
        if !self.user.is_empty() {
            url.push_str(&percent_encode(&self.user));
            if !self.password.is_empty() {
                url.push(':');
                url.push_str(&percent_encode(&self.password));
            }
            url.push('@');
        }
        url.push_str(&self.host);
        if !self.port.is_empty() {
            url.push(':');
            url.push_str(&self.port);
        }
        url.push('/');
        url.push_str(&percent_encode(&self.database));
        url
    }
}

impl PgGuiApp {
    /// The [`config::connection_key`] the active connection is filed under:
    /// the name of its recent entry, or the connection string when it has no
    /// entry (or an unnamed one). The head of the list wins, which is the
    /// entry last connected to, so two connections sharing a url resolve to
    /// the one the user actually picked.
    pub(in crate::app) fn active_key(config: &config::Config) -> String {
        config
            .recent_connections
            .iter()
            .find(|c| c.url == config.connection_string)
            .map_or_else(|| config.connection_string.clone(), |c| c.key().to_string())
    }

    /// Create the title-bar connection combobox, selecting the active
    /// connection (at the head of the recent list), and the subscription
    /// that connects to whatever gets picked from it.
    pub(in crate::app) fn build_connection_combo(
        config: &config::Config,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (
        Entity<ComboboxState<SearchableVec<ConnectionItem>>>,
        Subscription,
    ) {
        let connections = cx.new(|cx| {
            ComboboxState::new(
                connection_items(&config.recent_connections),
                vec![IndexPath::default()],
                window,
                cx,
            )
            .searchable(true)
        });
        let subscription = cx.subscribe_in(
            &connections,
            window,
            |this, _, event: &ComboboxEvent<SearchableVec<ConnectionItem>>, window, cx| {
                if let ComboboxEvent::Confirm(values) = event
                    && let Some(key) = values.first()
                    && let Some((url, name)) = this
                        .config
                        .recent_connections
                        .iter()
                        .find(|c| c.key() == key.as_ref())
                        .map(|c| (c.url.clone(), c.name.clone()))
                {
                    this.apply_connection(&url, &name, None, window, cx);
                }
            },
        );
        (connections, subscription)
    }

    /// Mirror `config.recent_connections` into the title-bar combobox and
    /// mark the active connection selected.
    pub(in crate::app) fn sync_connection_combo(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = connection_items(&self.config.recent_connections);
        let active = Self::active_key(&self.config);
        let selected = self
            .config
            .recent_connections
            .iter()
            .position(|c| c.key() == active)
            .map(|row| IndexPath::default().row(row));
        self.connections.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            state.set_selected_indices(selected, window, cx);
        });
    }

    /// Connection ▸ New Connection…: open the connection form on a fresh
    /// default connection to fill in.
    pub(in crate::app) fn new_connection(
        &mut self,
        _: &NewConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_connection_form("New connection", &default_conn(), "", None, window, cx);
    }

    /// Connection ▸ Edit Connection…: open the connection form seeded with
    /// the current connection's details, to update it in place.
    pub(in crate::app) fn edit_connection(
        &mut self,
        _: &EditConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let url = self.config.connection_string.clone();
        // Seed the name from the matching recent entry, so an already-named
        // connection shows its name. Its key goes along as the entry being
        // edited, so a new name replaces that entry rather than adding one.
        let key = Self::active_key(&self.config);
        let name = self
            .config
            .recent_connections
            .iter()
            .find(|c| c.key() == key)
            .map_or(String::new(), |c| c.name.clone());
        self.open_connection_form("Edit connection", &url, &name, Some(key), window, cx);
    }

    /// Build the field inputs seeded from `seed_url`/`seed_name`, wire up the
    /// live connection-string preview, and show the connection dialog titled
    /// `title`. Shared by New Connection and Edit Connection; `replacing` is
    /// the key of the entry being edited, `None` for a new connection.
    fn open_connection_form(
        &mut self,
        title: &'static str,
        seed_url: &str,
        seed_name: &str,
        replacing: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let parts = ConnectionParts::parse(seed_url);
        let mut field = |value: String, placeholder: &str, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let name = field(seed_name.to_string(), "My database (optional)", cx);
        let fields = ConnectionFields {
            host: field(parts.host, "localhost", cx),
            port: field(parts.port, "5432", cx),
            database: field(parts.database, "postgres", cx),
            user: field(parts.user, "postgres", cx),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .default_value(parts.password)
            }),
        };
        let preview = cx.new(|cx| InputState::new(window, cx).default_value(seed_url.to_string()));
        let test_status = cx.new(|_| ConnectionTest::Idle);

        self.connection_dialog_subs =
            Self::wire_connection_sync(&fields, &preview, &test_status, window, cx);

        Self::open_connection_dialog(
            title,
            ConnectionForm {
                name,
                fields,
                preview,
                test_status,
                replacing,
            },
            window,
            cx,
        );
    }

    /// Wire the two-way sync between the individual fields and the editable
    /// connection-string preview: a field edit recomputes the string, and a
    /// string edit re-parses it back into the fields. A shared re-entrancy
    /// flag stops one side's programmatic write from echoing back through the
    /// other. Any edit also drops a stale Test Connection result. Returns the
    /// subscriptions, which the caller keeps alive for the dialog's lifetime.
    fn wire_connection_sync(
        fields: &ConnectionFields,
        preview: &Entity<InputState>,
        test_status: &Entity<ConnectionTest>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        // While one side programmatically writes the other, the other's Change
        // event is ignored instead of writing straight back.
        let syncing = Rc::new(Cell::new(false));

        let clear_test = {
            let test_status = test_status.clone();
            move |cx: &mut App| {
                test_status.update(cx, |status, cx| {
                    if !matches!(status, ConnectionTest::Idle) {
                        *status = ConnectionTest::Idle;
                        cx.notify();
                    }
                });
            }
        };

        // Field edit: recompute the previewed connection string.
        let recompute = {
            let (fields, preview, syncing, clear_test) = (
                fields.clone(),
                preview.clone(),
                syncing.clone(),
                clear_test.clone(),
            );
            move |window: &mut Window, cx: &mut App| {
                if syncing.get() {
                    return;
                }
                let url = fields.read(cx).to_url();
                syncing.set(true);
                preview.update(cx, |state, cx| state.set_value(url, window, cx));
                syncing.set(false);
                clear_test(cx);
            }
        };
        // String edit: parse it and drive the fields. Skipped for a partial
        // entry that is not yet a URL, so the fields are not wiped mid-type.
        let apply_preview = {
            let (fields, preview, syncing, clear_test) =
                (fields.clone(), preview.clone(), syncing, clear_test);
            move |window: &mut Window, cx: &mut App| {
                if syncing.get() {
                    return;
                }
                let url = preview.read(cx).value().to_string();
                if !url.contains("://") {
                    return;
                }
                let parts = ConnectionParts::parse(&url);
                syncing.set(true);
                fields.set(&parts, window, cx);
                syncing.set(false);
                clear_test(cx);
            }
        };

        let mut subs: Vec<Subscription> = fields
            .as_array()
            .iter()
            .map(|input| {
                let recompute = recompute.clone();
                cx.subscribe_in(
                    input,
                    window,
                    move |_, _, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::Change) {
                            recompute(window, cx);
                        }
                    },
                )
            })
            .collect();
        subs.push(cx.subscribe_in(
            preview,
            window,
            move |_, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    apply_preview(window, cx);
                }
            },
        ));
        subs
    }

    /// Build and show the connection dialog for the given title over the
    /// state in `form`.
    fn open_connection_dialog(
        title: &'static str,
        form: ConnectionForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let ConnectionForm {
                name,
                fields,
                preview,
                test_status,
                replacing,
            } = form.clone();
            let connect = {
                let (app, name, fields) = (app.clone(), name.clone(), fields.clone());
                move |window: &mut Window, cx: &mut App| {
                    let url = fields.read(cx).to_url();
                    let name = name.read(cx).value().trim().to_string();
                    window.close_dialog(cx);
                    app.update(cx, |this, cx| {
                        this.apply_connection(&url, &name, replacing.as_deref(), window, cx);
                    })
                    .ok();
                }
            };
            let test = {
                let (app, fields, test_status) = (app.clone(), fields.clone(), test_status.clone());
                move |cx: &mut App| {
                    let url = fields.read(cx).to_url();
                    let status = test_status.clone();
                    app.update(cx, |_, cx| Self::run_connection_test(url, status, cx))
                        .ok();
                }
            };
            let labeled = |label: &str, input: &Entity<InputState>, cx: &mut App| {
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(label.to_string()),
                    )
                    .child(Input::new(input))
            };

            dialog.title(title).w(px(520.)).child(
                v_flex()
                    .gap_4()
                    .pb_2()
                    .child(labeled("Name", &name, cx))
                    .child(
                        h_flex()
                            .gap_3()
                            .child(div().flex_1().child(labeled("Host", &fields.host, cx)))
                            .child(div().w(px(120.)).child(labeled("Port", &fields.port, cx))),
                    )
                    .child(labeled("Database", &fields.database, cx))
                    .child(labeled("Username", &fields.user, cx))
                    .child(labeled("Password", &fields.password, cx))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Connection string"),
                            )
                            .child(Input::new(&preview)),
                    )
                    .child(test_status)
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(Button::new("cancel").label("Cancel").on_click(
                                |_, window, cx| {
                                    window.close_dialog(cx);
                                },
                            ))
                            .child(
                                Button::new("test")
                                    .outline()
                                    .label("Test Connection")
                                    .on_click(move |_, _, cx| test(cx)),
                            )
                            .child(
                                Button::new("connect")
                                    .primary()
                                    .label("Connect")
                                    .on_click(move |_, window, cx| connect(window, cx)),
                            ),
                    ),
            )
        });
    }

    /// Try to open a connection with `url` in the background and report the
    /// outcome through `status`, which the New Connection dialog renders as
    /// green ("succeeded") or red ("failed: …") text — feedback without
    /// leaving the dialog.
    fn run_connection_test(url: String, status: Entity<ConnectionTest>, cx: &mut Context<Self>) {
        status.update(cx, |status, cx| {
            *status = ConnectionTest::Testing;
            cx.notify();
        });
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_spawn(async move { db::test_connection(&url) })
                .await;
            status.update(cx, |status, cx| {
                *status = match result {
                    Ok(()) => ConnectionTest::Ok,
                    Err(err) => ConnectionTest::Failed(
                        format!(
                            "Connection failed: {}",
                            err.lines().next().unwrap_or_default()
                        )
                        .into(),
                    ),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Connection ▸ Recent ▸ …: reconnect to a previously used connection.
    pub(in crate::app) fn connect_recent(
        &mut self,
        action: &Connect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_connection(&action.url, &action.name, None, window, cx);
    }

    /// Move the untitled tabs and the query output from `previous` into the
    /// park, then bring back whatever `key` last had. Both are
    /// [`config::connection_key`]s, not urls. The tabs backed by a file stay
    /// open throughout — only their results, log and plan are swapped, since
    /// those describe the server that produced them.
    pub(in crate::app) fn swap_connection_tabs(
        &mut self,
        previous: &str,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Follow the selection by id: the index shifts as untitled tabs leave
        // and arrive. An untitled tab that is itself parked leaves nothing to
        // return to, so the first tab takes over.
        let active_id = self.tabs.get(self.active_tab).map(|tab| tab.id);
        // An empty previous connection has no park to be filed under (and
        // could never be selected again to get them back), so its tabs simply
        // carry over.
        if !previous.is_empty() {
            self.park_connection_tabs(previous);
        }
        self.restore_connection_tabs(key, window, cx);
        if self.tabs.is_empty() {
            self.add_tab(String::new(), None, window, cx);
        }
        let ix = active_id
            .and_then(|id| self.tab_index_by_id(id))
            .unwrap_or(0);
        self.activate_tab(ix, window, cx);
    }

    /// Take the untitled tabs out of the tab bar and every tab's output off
    /// the tabs, filing both under the connection key `key`.
    fn park_connection_tabs(&mut self, key: &str) {
        let mut parked = Vec::new();
        let mut results = ParkedResults::default();
        // Walking from the back keeps a removal from shifting the tabs still
        // to be visited, so the recorded slot is the tab's original one —
        // which is what re-inserting them in ascending order needs.
        for ix in (0..self.tabs.len()).rev() {
            if let Some(path) = self.tabs[ix].path.clone() {
                results
                    .files
                    .insert(path, std::mem::take(&mut self.tabs[ix].result));
            } else {
                let tab = self.tabs.remove(ix);
                let script = self.config.tabs.remove(ix).script;
                parked.push(config::ParkedTab { script, index: ix });
                results.untitled.push(tab.result);
            }
        }
        parked.reverse();
        results.untitled.reverse();
        self.config.parked_tabs.insert(key.to_string(), parked);
        self.parked_results.insert(key.to_string(), results);
    }

    /// Put the untitled tabs parked under `key` back where they were and hand
    /// every tab the output it last had on that connection.
    fn restore_connection_tabs(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let parked = self.config.parked_tabs.remove(key).unwrap_or_default();
        let mut results = self.parked_results.remove(key).unwrap_or_default();
        // The two lists only line up when they were parked together; an
        // externally edited config can leave the scripts without their output.
        let aligned = results.untitled.len() == parked.len();
        for (n, tab) in parked.into_iter().enumerate() {
            let ix = self.insert_tab_at(
                tab.index,
                config::ScriptTab {
                    script: tab.script,
                    file: None,
                    disk_time: None,
                },
                window,
                cx,
            );
            if aligned {
                self.tabs[ix].result = std::mem::take(&mut results.untitled[n]);
            }
        }
        for tab in &mut self.tabs {
            if let Some(path) = &tab.path
                && let Some(result) = results.files.remove(path)
            {
                tab.result = result;
            }
        }
    }

    /// Bring the scripts parked under `key` into the tab bar without touching
    /// the tabs already there. Renaming the connection we are on onto a name
    /// some other entry held takes that name's park over, and the active
    /// connection never holds one — so those scripts have to come home or
    /// they become unreachable. Only the scripts do: the rows and logs in
    /// that park were produced on a different connection.
    fn adopt_parked_scripts(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.parked_results.remove(key);
        let parked = self.config.parked_tabs.remove(key).unwrap_or_default();
        if parked.is_empty() {
            return;
        }
        // Inserting ahead of the selected tab shifts its index, so the
        // selection is followed by id, as in `swap_connection_tabs`.
        let active_id = self.tabs.get(self.active_tab).map(|tab| tab.id);
        for tab in parked {
            self.insert_tab_at(
                tab.index,
                config::ScriptTab {
                    script: tab.script,
                    file: None,
                    disk_time: None,
                },
                window,
                cx,
            );
        }
        let ix = active_id
            .and_then(|id| self.tab_index_by_id(id))
            .unwrap_or(0);
        self.activate_tab(ix, window, cx);
    }

    /// Switch to `url` (saved under `name`, which may be empty): remember
    /// it, persist, and restart the language server so completions follow
    /// the new database's schema. `replacing` is the recent entry this is an
    /// edit of, if any — editing the connection we are on is not a switch,
    /// so its tabs and output stay put however the url or name changes.
    fn apply_connection(
        &mut self,
        url: &str,
        name: &str,
        replacing: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if url.is_empty() {
            return;
        }
        // Every tab's session belongs to the previous server; tear them all
        // down (rolling back any open transaction) so the next Run reconnects.
        // Bumping the epoch discards any query still in flight against the old
        // server when it returns.
        self.db_epoch += 1;
        for tab in &mut self.tabs {
            tab.session = None;
            tab.running = false;
            tab.progress = None;
            tab.cancel = None;
            tab.result.has_more = false;
        }
        let previous = Self::active_key(&self.config);
        self.config.connection_string = url.to_string();
        let key = record_recent(&mut self.config.recent_connections, url, name, replacing);
        // Untitled tabs and query output belong to the connection they were
        // opened and run against, so they travel with it rather than with the
        // window. The connection is identified by its key: re-pointing or
        // renaming the one we are on leaves the tabs alone.
        if key != previous && replacing != Some(previous.as_str()) {
            self.swap_connection_tabs(&previous, &key, window, cx);
        } else if key != previous {
            self.adopt_parked_scripts(&key, window, cx);
        }
        self.save_config();
        self.refresh_menus(cx);
        self.sync_connection_combo(window, cx);
        self.restart_lsp(cx);
        if self.config.db_panel_visible {
            self.load_db_schemas(cx);
        } else {
            self.db_nodes.clear();
            self.db_expanded.clear();
        }
        let label = if name.is_empty() {
            mask_credentials(url)
        } else {
            name.to_string()
        };
        self.set_status(format!("Connecting to {label}"), cx);
    }
}
