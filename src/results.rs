use gpui::{App, Context, IntoElement, ParentElement as _, Styled as _, Window, div, px};
use gpui_component::{
    ActiveTheme as _,
    table::{Column, TableDelegate, TableState},
};

/// Table delegate holding the current query result set.
pub struct ResultsDelegate {
    columns: Vec<Column>,
    rows: Vec<Vec<Option<String>>>,
}

impl ResultsDelegate {
    pub fn new() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
        }
    }

    pub fn set_data(&mut self, columns: Vec<String>, rows: Vec<Vec<Option<String>>>) {
        self.columns = columns
            .into_iter()
            .enumerate()
            .map(|(ix, name)| Column::new(format!("col-{ix}"), name).width(px(160.)))
            .collect();
        self.rows = rows;
    }

    pub fn clear(&mut self) {
        self.columns.clear();
        self.rows.clear();
    }
}

impl TableDelegate for ResultsDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
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
            .get(row_ix)
            .and_then(|r| r.get(col_ix))
            .cloned()
            .flatten();

        match value {
            Some(v) => div().child(v),
            None => div().text_color(cx.theme().muted_foreground).child("NULL"),
        }
    }
}
