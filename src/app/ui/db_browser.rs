//! The object browser: the database panel's schema tree.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::Input,
    list::ListItem,
    tree::tree,
    v_flex,
};

use crate::RefreshDbTree;
use crate::app::{DbSchemaLoad, PgGuiApp};

impl PgGuiApp {
    /// The database object browser (left panel): a header with a system-schema
    /// toggle and refresh button, a filter box, and the lazily-loaded tree.
    pub(in crate::app) fn render_db_panel(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let header = h_flex()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child("Database"),
            )
            .child(
                Checkbox::new("db-system")
                    .label("System")
                    .checked(self.show_system_schemas)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_system_schemas(cx))),
            )
            .child(
                Button::new("db-refresh")
                    .ghost()
                    .xsmall()
                    .tooltip("Refresh")
                    .label("↻")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.refresh_db_tree(&RefreshDbTree, window, cx);
                    })),
            );

        let filter = div()
            .px_1()
            .pb_1()
            .child(Input::new(&self.db_filter_input).xsmall());

        v_flex()
            .size_full()
            .child(header)
            .child(filter)
            .child(div().flex_1().min_h(px(0.)).child(self.render_db_body(cx)))
    }

    /// The browser panel's body: one of the empty/error/loading placeholders,
    /// or the object tree itself.
    pub(in crate::app) fn render_db_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let message = |text: &str, danger: bool| {
            let color = if danger {
                cx.theme().danger
            } else {
                cx.theme().muted_foreground
            };
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .p_4()
                .child(div().text_sm().text_color(color).child(text.to_string()))
                .into_any_element()
        };

        if self.config.connection_string.is_empty() {
            message("Not connected", false)
        } else if let DbSchemaLoad::Failed(err) = &self.db_schema_load {
            message(err, true)
        } else if matches!(self.db_schema_load, DbSchemaLoad::Loading) && self.db_nodes.is_empty() {
            message("Loading…", false)
        } else if self.db_nodes.is_empty() {
            message("No schemas", false)
        } else {
            let view = cx.entity();
            div()
                .size_full()
                .px_1()
                .child(tree(
                    &self.db_tree_state,
                    move |ix, entry, _selected, _window, cx| {
                        view.update(cx, |_, cx| {
                            let item = entry.item();
                            // A text glyph rather than an `IconName` SVG:
                            // an arrow for expandable nodes, blank for leaves.
                            // See `tree_row_action` for why a tree row keeps
                            // its element count down.
                            let glyph = if !entry.is_folder() {
                                ""
                            } else if entry.is_expanded() {
                                "▾"
                            } else {
                                "▸"
                            };
                            let label_row = h_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .w_4()
                                        .flex_none()
                                        .text_center()
                                        // Sidebar foreground, not muted: the
                                        // arrow is a single thin glyph and the
                                        // muted tone all but disappears on a
                                        // dark theme.
                                        .text_color(cx.theme().sidebar_foreground)
                                        .child(glyph),
                                )
                                .child(item.label.clone());
                            let list_item = ListItem::new(ix)
                                .w_full()
                                .h(Self::tree_row_height(cx))
                                .rounded(cx.theme().radius)
                                .px_2()
                                .pl(px(14.) * entry.depth() + px(8.));
                            if entry.is_folder() {
                                // Folders expand through the tree's own click
                                // handling; only object leaves open. A refresh
                                // action re-fetches this folder's contents so
                                // objects created since the last load appear.
                                let node_id = item.id.clone();
                                list_item.child(
                                    h_flex()
                                        .group("db-row")
                                        .w_full()
                                        .items_center()
                                        .child(Self::clip_label(label_row.flex_1()))
                                        .child(Self::tree_row_action(
                                            SharedString::from(format!("db-refresh-{}", item.id)),
                                            "db-row",
                                            "↻",
                                            "Refresh",
                                            cx,
                                            cx.listener(move |this, _, _, cx| {
                                                this.refresh_db_node(node_id.clone(), cx);
                                            }),
                                        )),
                                )
                            } else {
                                let list_item =
                                    list_item.child(Self::clip_label(label_row.w_full()));
                                let id = item.id.clone();
                                list_item.on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_db_object(&id, window, cx);
                                }))
                            }
                        })
                    },
                ))
                .into_any_element()
        }
    }
}
