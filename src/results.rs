use std::collections::HashMap;

use gpui::{
    App, Context, FontId, IntoElement, ParentElement as _, Pixels, Styled as _, Window, div, font,
    px,
};
use gpui_component::{
    ActiveTheme as _,
    table::{Column, TableDelegate, TableState},
};

/// Rows measured to size a column, the header counting as one of them:
/// the name plus the first four values. A wider value further down is
/// clipped instead of re-flowing the whole table mid-scroll.
const SAMPLE_ROWS: usize = 5;
/// Both horizontal cell paddings (`Size::Medium`), which sit inside the
/// column's width.
const CELL_PADDING: Pixels = px(16.);
/// Breathing room beyond the padding, so a value never runs up against
/// the column's resize edge.
const CELL_GUTTER: Pixels = px(12.);
/// Floor for a column of one-character values, leaving its header
/// readable and its resize edge grabbable.
const MIN_COL_WIDTH: Pixels = px(48.);
/// Keeps one long text/JSON column from pushing the rest off-screen.
const MAX_COL_WIDTH: Pixels = px(450.);
/// A set with fewer columns than this leaves an empty strip beside it
/// that reads as a rendering fault, so its columns share the spare
/// width out. Wider sets keep their fitted widths and scroll.
const STRETCH_MAX_COLS: usize = 5;
/// Held back from the viewport so a stretched row never rounds its way
/// into a horizontal scrollbar.
const STRETCH_SLACK: Pixels = px(2.);
/// Anything longer is past `MAX_COL_WIDTH` regardless, so only this much
/// of a value is measured.
const MEASURE_CHARS: usize = 120;
/// What a SQL NULL renders (and therefore measures) as.
const NULL_TEXT: &str = "NULL";

/// Measures cell text in the font a table cell draws with.
///
/// Widths are summed per character rather than shaped as a line: full
/// shaping lives on `WindowTextSystem`, and the delegate is fed from
/// paths that carry no `Window` (a result arriving on a background tab,
/// say). The sum ignores kerning and ligatures, which for sizing is
/// within a pixel or two and always under the padding slack.
/// Per-character widths are cached — a result set's cells draw from a
/// handful of distinct characters.
struct CellMetrics {
    font_id: FontId,
    font_size: Pixels,
    /// Width used for a character the font has no glyph for.
    fallback: Pixels,
    widths: HashMap<char, Pixels>,
}

impl CellMetrics {
    fn new(cx: &App) -> Self {
        let text_system = cx.text_system();
        let font_id = text_system.resolve_font(&font(cx.theme().font_family.clone()));
        // A cell sets no text size of its own, so it draws at the root's
        // default of one rem, which `Root` sets from the theme.
        let font_size = cx.theme().font_size;
        let fallback = text_system
            .ch_advance(font_id, font_size)
            .unwrap_or(font_size);
        Self {
            font_id,
            font_size,
            fallback,
            widths: HashMap::new(),
        }
    }

    fn char_width(&mut self, ch: char, cx: &App) -> Pixels {
        if let Some(width) = self.widths.get(&ch) {
            return *width;
        }
        let width = cx
            .text_system()
            .advance(self.font_id, self.font_size, ch)
            .map_or(self.fallback, |advance| advance.width);
        self.widths.insert(ch, width);
        width
    }

    /// The drawn width of `text` in a cell.
    fn text_width(&mut self, text: &str, cx: &App) -> Pixels {
        text.chars()
            .take(MEASURE_CHARS)
            .map(|ch| self.char_width(ch, cx))
            .sum()
    }
}

/// Table delegate holding the current query result set, displayed one
/// page at a time.
pub struct ResultsDelegate {
    columns: Vec<Column>,
    /// Each column's measured content width, kept apart from the width
    /// it draws at so stretching stays idempotent and reversible.
    fitted: Vec<Pixels>,
    /// Width of the table's column viewport, as of the last frame.
    viewport: Pixels,
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
            fitted: Vec::new(),
            viewport: px(0.),
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

    pub fn set_data(&mut self, columns: Vec<String>, rows: Vec<Vec<Option<String>>>, cx: &App) {
        self.columns = columns
            .into_iter()
            .enumerate()
            .map(|(ix, name)| Column::new(format!("col-{ix}"), name))
            .collect();
        self.rows = rows;
        self.page = 0;
        self.measure_columns(false, cx);
    }

    /// Append a fetched batch to the current result set, keeping the
    /// current page so the user continues from where they were.
    pub fn append_rows(&mut self, rows: Vec<Vec<Option<String>>>, cx: &App) {
        // A set shorter than the sample was sized on an incomplete one, so
        // the fetched rows can still widen a column.
        let undersampled = self.rows.len() < SAMPLE_ROWS - 1;
        self.rows.extend(rows);
        if undersampled {
            self.measure_columns(true, cx);
        }
    }

    /// Fit every column to its header and its sampled values.
    /// `grow_only` keeps a column from narrowing under the user's eyes
    /// while a run is still feeding it rows; a zoom re-fits both ways.
    pub fn measure_columns(&mut self, grow_only: bool, cx: &App) {
        let mut metrics = CellMetrics::new(cx);
        self.fitted = self
            .columns
            .iter()
            .enumerate()
            .map(|(ix, col)| {
                let fitted = self.fitted_width(ix, &col.name, &mut metrics, cx);
                match self.fitted.get(ix) {
                    Some(previous) if grow_only => fitted.max(*previous),
                    _ => fitted,
                }
            })
            .collect();
        self.apply_widths();
    }

    /// Record the width the table's columns are drawn into. Returns
    /// whether that changed the column widths, i.e. whether the caller
    /// owes the table a refresh.
    pub fn set_viewport(&mut self, viewport: Pixels) -> bool {
        if (viewport - self.viewport).abs() < px(1.) {
            return false;
        }
        self.viewport = viewport;
        self.apply_widths();
        true
    }

    /// Draw the columns at their fitted widths, or — for a set narrow
    /// enough to leave a gap — at those widths scaled up in proportion
    /// until they fill the viewport.
    fn apply_widths(&mut self) {
        let total: Pixels = self.fitted.iter().copied().sum();
        let target = self.viewport - STRETCH_SLACK;
        let scale = if self.columns.len() < STRETCH_MAX_COLS && total > px(0.) && total < target {
            target / total
        } else {
            1.
        };
        for (col, fitted) in self.columns.iter_mut().zip(&self.fitted) {
            col.width = *fitted * scale;
        }
    }

    /// The width that fits `name` and the sampled values of column
    /// `col_ix` — [`SAMPLE_ROWS`] rows counting the header — clamped to
    /// the drawable range.
    fn fitted_width(
        &self,
        col_ix: usize,
        name: &str,
        metrics: &mut CellMetrics,
        cx: &App,
    ) -> Pixels {
        let mut width = metrics.text_width(name, cx);
        for row in self.rows.iter().take(SAMPLE_ROWS - 1) {
            let text = row
                .get(col_ix)
                .and_then(Option::as_deref)
                .unwrap_or(NULL_TEXT);
            width = width.max(metrics.text_width(text, cx));
        }
        (width + CELL_PADDING + CELL_GUTTER).clamp(MIN_COL_WIDTH, MAX_COL_WIDTH)
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

    /// The value of a cell on the current page, `None` for a SQL NULL (or a
    /// row/column outside the result set).
    fn cell_value(&self, row_ix: usize, col_ix: usize) -> Option<String> {
        self.rows
            .get(self.page_start() + row_ix)
            .and_then(|r| r.get(col_ix))
            .cloned()
            .flatten()
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

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        match self.cell_value(row_ix, col_ix) {
            Some(v) => div().child(v),
            None => div().text_color(cx.theme().muted_foreground).child("NULL"),
        }
    }

    /// Copy/export text of a cell. A NULL copies as the empty string, so a
    /// pasted value is the value itself and never the word "NULL".
    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.cell_value(row_ix, col_ix).unwrap_or_default()
    }
}
