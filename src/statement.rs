//! Locating individual SQL statements inside the editor buffer.

use std::ops::Range;

/// Byte ranges of the top-level statements in `text`: split on semicolons
/// that sit outside strings, comments, and dollar-quoted blocks, then
/// trimmed of surrounding whitespace. Empty segments are dropped.
fn ranges(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' => i = skip_quoted(bytes, i, bytes[i]),
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = skip_line_comment(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i),
            b'$' => i = skip_dollar_quoted(text, i),
            b';' => {
                push_trimmed(text, start..i + 1, &mut ranges);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    push_trimmed(text, start..bytes.len(), &mut ranges);
    ranges
}

/// The statement at `offset`: the one whose range contains the offset,
/// otherwise the closest statement to the left (the cursor sits after a
/// statement, e.g. at the end of a line), otherwise the first statement.
pub fn at(text: &str, offset: usize) -> Option<Range<usize>> {
    let offset = offset.min(text.len());
    let ranges = ranges(text);
    if let Some(range) = ranges
        .iter()
        .find(|range| (range.start..=range.end).contains(&offset))
    {
        return Some(range.clone());
    }
    if let Some(range) = ranges.iter().rev().find(|range| range.end < offset) {
        return Some(range.clone());
    }
    ranges.into_iter().next()
}

fn push_trimmed(text: &str, range: Range<usize>, out: &mut Vec<Range<usize>>) {
    let segment = &text[range.clone()];
    let trimmed = segment.trim_start();
    let start = range.start + (segment.len() - trimmed.len());
    let end = start + trimmed.trim_end().len();
    if start < end {
        out.push(start..end);
    }
}

/// Skip a `'…'` or `"…"` region (the quote is doubled to escape it).
fn skip_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == quote {
            if bytes.get(i + 1) == Some(&quote) {
                i += 2;
            } else {
                return i + 1;
            }
        } else {
            i += 1;
        }
    }
    bytes.len()
}

fn skip_line_comment(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

/// Skip a `/* … */` comment; Postgres allows them to nest.
fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut depth = 1_usize;
    let mut i = start + 2;
    while i < bytes.len() {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    bytes.len()
}

/// Skip a `$tag$ … $tag$` dollar-quoted region. A `$` that doesn't open one
/// (e.g. a `$1` placeholder) is stepped over.
fn skip_dollar_quoted(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let mut tag_end = start + 1;
    while tag_end < bytes.len()
        && (bytes[tag_end].is_ascii_alphanumeric() || bytes[tag_end] == b'_')
    {
        tag_end += 1;
    }
    let opens_quote =
        tag_end < bytes.len() && bytes[tag_end] == b'$' && !bytes[start + 1].is_ascii_digit();
    if !opens_quote {
        return start + 1;
    }
    let delimiter = &text[start..=tag_end];
    match text[tag_end + 1..].find(delimiter) {
        Some(pos) => tag_end + 1 + pos + delimiter.len(),
        None => text.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::at;

    #[test]
    fn cursor_inside_statement() {
        let sql = "SELECT 1;\nSELECT 2;\n";
        assert_eq!(at(sql, 2), Some(0..9));
        assert_eq!(at(sql, 12), Some(10..19));
    }

    #[test]
    fn cursor_right_after_statement_picks_the_left_one() {
        let sql = "SELECT 1;\nSELECT 2;\n";
        assert_eq!(at(sql, 9), Some(0..9));
        assert_eq!(at(sql, sql.len()), Some(10..19));
    }

    #[test]
    fn cursor_on_blank_line_picks_the_statement_above() {
        let sql = "SELECT 1;\n\nSELECT 2;";
        assert_eq!(at(sql, 10), Some(0..9));
    }

    #[test]
    fn cursor_before_the_first_statement_picks_it() {
        let sql = "\n\nSELECT 1;";
        assert_eq!(at(sql, 0), Some(2..11));
    }

    #[test]
    fn statement_without_trailing_semicolon() {
        let sql = "SELECT 1;\nSELECT 2";
        assert_eq!(at(sql, sql.len()), Some(10..18));
    }

    #[test]
    fn semicolons_in_strings_and_comments_do_not_split() {
        let sql = "SELECT 'a;b', $$c;d$$ -- e;f\n/* g;/*h;*/ */;SELECT 2;";
        let second = sql.find("SELECT 2").unwrap();
        assert_eq!(at(sql, 0), Some(0..second));
        // Exactly on the boundary the tie goes to the left statement…
        assert_eq!(at(sql, second), Some(0..second));
        // …one step in, the right one wins.
        assert_eq!(at(sql, second + 1), Some(second..sql.len()));
    }

    #[test]
    fn tagged_dollar_quotes_hide_semicolons() {
        let sql = "CREATE FUNCTION f() RETURNS int AS $fn$ SELECT 1; $fn$ LANGUAGE sql;\nSELECT 2;";
        let newline = sql.find('\n').unwrap();
        assert_eq!(at(sql, 0), Some(0..newline));
    }

    #[test]
    fn dollar_placeholders_are_not_quotes() {
        let sql = "SELECT $1;\nSELECT 2;";
        assert_eq!(at(sql, 0), Some(0..10));
    }

    #[test]
    fn empty_text_has_no_statement() {
        assert_eq!(at("", 0), None);
        assert_eq!(at("  \n ", 2), None);
    }
}
