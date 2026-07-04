use gpui::{App, Context, IntoElement, ParentElement as _, Styled as _, Window, div, px};
use gpui_component::{
    ActiveTheme as _,
    table::{Column, TableDelegate, TableState},
};

/// Table delegate holding the current query result set, displayed one
/// page at a time.
pub struct ResultsDelegate {
    columns: Vec<Column>,
    rows: Vec<Vec<Option<String>>>,
    page: usize,
    page_size: usize,
}

impl ResultsDelegate {
    /// `page_size` is the number of rows shown per page (from the config;
    /// clamped to at least 1).
    pub fn new(page_size: usize) -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            page: 0,
            page_size: page_size.max(1),
        }
    }

    /// Change the rows-per-page (e.g. after a config reload), keeping the
    /// current page within the new page count.
    pub fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size.max(1);
        self.page = self.page.min(self.page_count() - 1);
    }

    pub fn set_data(&mut self, columns: Vec<String>, rows: Vec<Vec<Option<String>>>) {
        self.columns = columns
            .into_iter()
            .enumerate()
            .map(|(ix, name)| Column::new(format!("col-{ix}"), name).width(px(160.)))
            .collect();
        self.rows = rows;
        self.page = 0;
    }

    pub fn clear(&mut self) {
        self.columns.clear();
        self.rows.clear();
        self.page = 0;
    }

    pub fn total_rows(&self) -> usize {
        self.rows.len()
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn page_count(&self) -> usize {
        self.rows.len().div_ceil(self.page_size).max(1)
    }

    /// Go to the next page. Returns false when already on the last page.
    pub fn next_page(&mut self) -> bool {
        if self.page + 1 < self.page_count() {
            self.page += 1;
            true
        } else {
            false
        }
    }

    /// Go to the previous page. Returns false when already on the first page.
    pub fn prev_page(&mut self) -> bool {
        if self.page > 0 {
            self.page -= 1;
            true
        } else {
            false
        }
    }

    fn page_start(&self) -> usize {
        self.page * self.page_size
    }
}

impl TableDelegate for ResultsDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows
            .len()
            .saturating_sub(self.page_start())
            .min(self.page_size)
    }

    fn column(&self, col_ix: usize, _: &App) -> &Column {
        &self.columns[col_ix]
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let value = self
            .rows
            .get(self.page_start() + row_ix)
            .and_then(|r| r.get(col_ix))
            .cloned()
            .flatten();

        match value {
            Some(v) => div().child(v),
            None => div().text_color(cx.theme().muted_foreground).child("NULL"),
        }
    }
}
