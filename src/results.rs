use std::collections::{BTreeSet, HashMap};

use gpui::{
    App, Context, Div, FontId, InteractiveElement as _, IntoElement, Modifiers, MouseButton,
    MouseDownEvent, ParentElement as _, Pixels, Stateful, Styled as _, Window, div, font,
    prelude::FluentBuilder as _, px,
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
    /// Rows picked with shift/cmd-click or select-all, as indices into the
    /// whole result set rather than the page, so a selection can span
    /// pages. The table itself selects one row or cell at a time; this
    /// set lives beside that selection and is painted by `render_tr`.
    selected_rows: BTreeSet<usize>,
    /// Where a shift-click range starts: the row last clicked without
    /// shift, as a result-set index.
    anchor: Option<usize>,
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
            selected_rows: BTreeSet::new(),
            anchor: None,
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
        self.selected_rows.clear();
        self.anchor = None;
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

    /// Update the row selection for a left mouse-down on row `row_ix` of
    /// the current page: shift takes the range from the anchor, cmd/ctrl
    /// toggles the row, and a plain click drops the selection back to the
    /// table's own single row/cell and re-anchors.
    fn click_row(&mut self, row_ix: usize, modifiers: Modifiers) {
        let row = self.page_start() + row_ix;
        if row >= self.rows.len() {
            return;
        }
        if modifiers.shift {
            let anchor = self.anchor.unwrap_or(row);
            self.selected_rows = (anchor.min(row)..=anchor.max(row)).collect();
            self.anchor = Some(anchor);
        } else if modifiers.secondary() {
            // The plainly clicked row the toggle starts from counts as
            // picked, as it does in a file list.
            if self.selected_rows.is_empty()
                && let Some(anchor) = self.anchor
            {
                self.selected_rows.insert(anchor);
            }
            if !self.selected_rows.remove(&row) {
                self.selected_rows.insert(row);
            }
            self.anchor = Some(row);
        } else {
            self.selected_rows.clear();
            self.anchor = Some(row);
        }
    }

    /// Select every row of the result set, across all pages.
    pub fn select_all_rows(&mut self) {
        self.selected_rows = (0..self.rows.len()).collect();
    }

    /// Drop the multi-row selection. Returns whether there was one.
    pub fn clear_row_selection(&mut self) -> bool {
        let had = !self.selected_rows.is_empty();
        self.selected_rows.clear();
        had
    }

    pub fn selected_row_count(&self) -> usize {
        self.selected_rows.len()
    }

    /// The selected rows as copy text, in result-set order: one line per
    /// row, cells joined by tabs (NULL as the empty string, as
    /// `cell_text` has it). `None` when no rows are selected.
    pub fn selected_rows_text(&self) -> Option<String> {
        if self.selected_rows.is_empty() {
            return None;
        }
        Some(
            self.selected_rows
                .iter()
                .filter_map(|&row| self.rows.get(row))
                .map(|row| {
                    row.iter()
                        .map(|value| value.as_deref().unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join("\t")
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
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

    /// A click on the header's top-left corner — the row-header column's
    /// head, left of the columns' viewport — selects every row, as in a
    /// spreadsheet. The table draws that corner without a click handler of
    /// its own, so it is found by position on the header row instead.
    fn render_header(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        div().id("header").on_mouse_down(
            MouseButton::Left,
            cx.listener(|table, event: &MouseDownEvent, _, cx| {
                let columns_left = table.horizontal_scroll_handle.bounds().origin.x;
                if event.position.x < columns_left {
                    table.delegate_mut().select_all_rows();
                    cx.notify();
                }
            }),
        )
    }

    /// Paints rows of the multi-row selection and feeds left mouse-downs
    /// (which bubble up from the cells and the row header alike) to
    /// `click_row`, since the table's own click handling ignores modifiers.
    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let selected = self.selected_rows.contains(&(self.page_start() + row_ix));
        div()
            .id(("row", row_ix))
            .when(selected, |tr| tr.bg(cx.theme().tokens.table_active))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |table, event: &MouseDownEvent, _, cx| {
                    table.delegate_mut().click_row(row_ix, event.modifiers);
                    cx.notify();
                }),
            )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn delegate(rows: usize, page_size: usize) -> ResultsDelegate {
        let mut delegate = ResultsDelegate::new(page_size);
        delegate.rows = (0..rows)
            .map(|ix| vec![Some(ix.to_string()), None])
            .collect();
        delegate
    }

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Modifiers::default()
        }
    }

    fn selected(delegate: &ResultsDelegate) -> Vec<usize> {
        delegate.selected_rows.iter().copied().collect()
    }

    #[test]
    fn shift_click_takes_range_from_anchor() {
        let mut delegate = delegate(10, 100);
        delegate.click_row(5, Modifiers::default());
        assert!(selected(&delegate).is_empty());
        delegate.click_row(2, shift());
        assert_eq!(selected(&delegate), [2, 3, 4, 5]);
        delegate.click_row(7, shift());
        assert_eq!(selected(&delegate), [5, 6, 7]);
    }

    #[test]
    fn secondary_click_toggles_and_keeps_anchor_row() {
        let mut delegate = delegate(10, 100);
        delegate.click_row(1, Modifiers::default());
        delegate.click_row(4, Modifiers::secondary_key());
        assert_eq!(selected(&delegate), [1, 4]);
        delegate.click_row(1, Modifiers::secondary_key());
        assert_eq!(selected(&delegate), [4]);
        delegate.click_row(0, Modifiers::default());
        assert!(selected(&delegate).is_empty());
    }

    #[test]
    fn selection_spans_pages_and_copies_every_row() {
        let mut delegate = delegate(5, 2);
        delegate.click_row(1, Modifiers::default());
        assert!(delegate.next_page());
        delegate.click_row(1, shift());
        assert_eq!(selected(&delegate), [1, 2, 3]);
        // A click past the last row of a short page is ignored.
        assert!(delegate.next_page());
        delegate.click_row(1, Modifiers::default());
        assert_eq!(selected(&delegate), [1, 2, 3]);

        delegate.select_all_rows();
        assert_eq!(delegate.selected_row_count(), 5);
        assert_eq!(
            delegate.selected_rows_text().as_deref(),
            Some("0\t\n1\t\n2\t\n3\t\n4\t")
        );
        assert!(delegate.clear_row_selection());
        assert_eq!(delegate.selected_rows_text(), None);
    }
}
