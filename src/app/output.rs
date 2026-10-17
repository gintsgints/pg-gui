//! Commands on the shown output: switching result sets, paging, selecting
//! and copying cells/rows, and exporting to CSV or INSERTs.

use gpui::{AppContext as _, ClipboardItem, Context, Window};
use gpui_component::table::{TableDelegate as _, TableSelection};

use crate::app::PgGuiApp;
use crate::{ClearRowSelection, CopyCell, ExportCsv, ExportInserts, SelectAllRows, db, export};

/// Which export the Connection ▸ Export menu items requested; maps onto
/// `db::export_csv` / `db::export_inserts`.
#[derive(Clone, Copy)]
enum ExportFormat {
    Csv,
    Inserts,
}

impl PgGuiApp {
    pub(in crate::app) fn export_csv(
        &mut self,
        _: &ExportCsv,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.export_results(ExportFormat::Csv, window, cx);
    }

    pub(in crate::app) fn export_inserts(
        &mut self,
        _: &ExportInserts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.export_results(ExportFormat::Inserts, window, cx);
    }

    /// Prompt for a destination file, then re-run the selection or the
    /// statement at the cursor in the background and stream its result
    /// there — CSV via server-side `COPY TO STDOUT`, or as generated
    /// INSERT statements.
    fn export_results(
        &mut self,
        format: ExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_running() {
            return;
        }
        let conn = self.config.connection_string.clone();
        let Some((sql, scope)) = self.sql_under_cursor(window, cx) else {
            self.set_status("Nothing to export", cx);
            return;
        };
        let tab_id = self.tabs[self.active_tab].id;

        let default_name = export::default_file_name(
            &sql,
            match format {
                ExportFormat::Csv => "csv",
                ExportFormat::Inserts => "sql",
            },
        );
        let rx = cx.prompt_for_new_path(&self.start_dir(), Some(&default_name));

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            this.update(cx, |this, cx| {
                this.start_progress_by_id(tab_id, format!("export of {scope}"), cx);
                this.set_status(format!("Exporting {scope}…"), cx);
            })
            .ok();

            let started = std::time::Instant::now();
            let result = cx
                .background_spawn({
                    let path = path.clone();
                    async move {
                        match format {
                            ExportFormat::Csv => db::export_csv(&conn, &sql, &path)
                                .map(|bytes| format!("{bytes} byte(s)")),
                            ExportFormat::Inserts => db::export_inserts(&conn, &sql, &path)
                                .map(|rows| format!("{rows} row(s)")),
                        }
                    }
                })
                .await;
            let elapsed = started.elapsed();

            this.update_in(cx, |this, window, cx| {
                this.set_tab_running(tab_id, false);
                match result {
                    Ok(detail) => {
                        this.remember_dir(&path);
                        this.save_config();
                        this.set_status(
                            format!("Exported {detail} to {} in {elapsed:.0?}", path.display()),
                            cx,
                        );
                    }
                    Err(err) => this.show_query_error(&err, window, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Show the `ix`th result set of the active tab's last run in the table.
    pub(in crate::app) fn select_result_set(&mut self, ix: usize, cx: &mut Context<Self>) {
        let active = self.active_tab;
        let Some(tab) = self.tabs.get_mut(active) else {
            return;
        };
        if ix >= tab.result.results.len() || tab.result.selected == ix {
            return;
        }
        tab.result.selected = ix;
        self.show_tab_result(active, cx);
        cx.notify();
    }

    /// Copy the results grid's selection to the clipboard (cmd-c, bound in
    /// the `DataTable` key context so it only fires while the grid has
    /// focus): a multi-row selection (shift/cmd-click, cmd-a) as one line
    /// per row with cells joined by tabs, across every page it spans;
    /// otherwise a cell as its own text, a row as its cells joined by tabs,
    /// a column as one value per line. A column copies the current page
    /// only, since that is what the grid holds row indices for.
    pub(in crate::app) fn copy_cell(
        &mut self,
        _: &CopyCell,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = {
            let table = self.results.read(cx);
            let delegate = table.delegate();
            let cols = delegate.columns_count(cx);
            let rows = delegate.rows_count(cx);
            if let Some(text) = delegate.selected_rows_text() {
                Some(text)
            } else if let Some((row_ix, col_ix)) = table.selected_cell() {
                Some(delegate.cell_text(row_ix, col_ix, cx))
            } else if let Some(row_ix) = table.selected_row() {
                Some(
                    (0..cols)
                        .map(|col_ix| delegate.cell_text(row_ix, col_ix, cx))
                        .collect::<Vec<_>>()
                        .join("\t"),
                )
            } else if let Some(col_ix) = table.selected_col() {
                Some(
                    (0..rows)
                        .map(|row_ix| delegate.cell_text(row_ix, col_ix, cx))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            } else {
                None
            }
        };

        // Nothing selected: let the action fall through, so a cmd-c that
        // reached the grid by accident still does whatever is behind it.
        let Some(text) = text else {
            cx.propagate();
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// Select every row of the result set, all pages and every fetched row
    /// (cmd-a while the grid has focus), for cmd-c to copy.
    pub(in crate::app) fn select_all_rows(
        &mut self,
        _: &SelectAllRows,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.results.update(cx, |table, cx| {
            table.delegate_mut().select_all_rows();
            cx.notify();
        });
        cx.notify();
    }

    /// Escape in the grid: drop the multi-row selection and the table's own
    /// row/cell/column selection alike.
    pub(in crate::app) fn clear_row_selection(
        &mut self,
        _: &ClearRowSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cleared = self.results.update(cx, |table, cx| {
            let had_rows = table.delegate_mut().clear_row_selection();
            let had_selection = table.selection() != TableSelection::None;
            if had_selection {
                table.clear_selection(cx);
            }
            cx.notify();
            had_rows || had_selection
        });
        if cleared {
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    pub(in crate::app) fn change_results_page(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.results.update(cx, |table, cx| {
            let moved = if forward {
                table.delegate_mut().next_page()
            } else {
                table.delegate_mut().prev_page()
            };
            if moved {
                table.scroll_to_row(0, cx);
                table.refresh(cx);
            }
        });
        cx.notify();
    }
}
