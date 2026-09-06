//! Locating individual SQL statements inside the editor buffer.

use std::ops::Range;

/// Byte ranges of the top-level statements in `text`: split on semicolons
/// that sit outside strings, comments, and dollar-quoted blocks, then
/// trimmed of surrounding whitespace. Empty segments are dropped. Also
/// used by `export` to reject multi-statement selections.
pub fn ranges(text: &str) -> Vec<Range<usize>> {
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

/// The gutter line-number offset that makes the editor agree with the line
/// numbers Postgres reports for a PL/pgSQL routine.
///
/// Postgres numbers a routine's lines relative to its body — the text between
/// the dollar quotes — so body line 1 is whatever follows the opening `$tag$`,
/// on the same physical line as that delimiter. Returning the negated row of
/// that delimiter turns the gutter into body numbering: the delimiter's line
/// becomes 1, the lines above it 0 and below.
///
/// Only a buffer holding exactly one routine definition is numbered this way;
/// with none, or with several (whose bodies would each want their own origin),
/// the offset is 0 and the gutter counts the file's own lines.
pub fn body_line_offset(text: &str) -> i32 {
    let mut opens = ranges(text)
        .into_iter()
        .filter(|range| is_routine_definition(&text[range.clone()]))
        .filter_map(|range| body_open(text, &range));
    let Some(open) = opens.next() else {
        return 0;
    };
    if opens.next().is_some() {
        return 0;
    }
    let row = text[..open].matches('\n').count();
    -i32::try_from(row).unwrap_or(i32::MAX)
}

/// Whether `statement` creates a function or procedure, judged by its leading
/// keywords (`CREATE [OR REPLACE] [...] FUNCTION|PROCEDURE`) — enough to tell
/// it from the `CREATE TABLE`s and `SELECT`s it shares a buffer with.
fn is_routine_definition(statement: &str) -> bool {
    let mut words = strip_leading_comments(statement)
        .split_whitespace()
        .take(6)
        .map(str::to_ascii_uppercase);
    words.next().is_some_and(|word| word == "CREATE")
        && words.any(|word| word == "FUNCTION" || word == "PROCEDURE")
}

/// `statement` without the comments it opens with — a statement range starts
/// at the previous semicolon, so a routine's own doc comment is part of it.
fn strip_leading_comments(statement: &str) -> &str {
    let bytes = statement.as_bytes();
    let mut i = 0;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes[i..].starts_with(b"--") {
            i = skip_line_comment(bytes, i);
        } else if bytes[i..].starts_with(b"/*") {
            i = skip_block_comment(bytes, i);
        } else {
            return &statement[i..];
        }
    }
}

/// The byte offset of the `$tag$` that opens the routine body in `range` —
/// the first dollar quote of the statement, since anything before it is the
/// signature. `None` when the body is quoted some other way (a plain string
/// literal) or the statement is unterminated.
fn body_open(text: &str, range: &Range<usize>) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = range.start;
    while i < range.end {
        match bytes[i] {
            b'\'' | b'"' => i = skip_quoted(bytes, i, bytes[i]),
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = skip_line_comment(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i),
            b'$' => {
                if dollar_quote_tag(text, i).is_some() {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
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
    let Some(delimiter) = dollar_quote_tag(text, start) else {
        return start + 1;
    };
    let after_open = start + delimiter.len();
    match text[after_open..].find(delimiter) {
        Some(pos) => after_open + pos + delimiter.len(),
        None => text.len(),
    }
}

/// The `$tag$` delimiter opening at `start`, or `None` when the `$` there does
/// not open a dollar-quoted region (a `$1` placeholder, a bare `$`).
fn dollar_quote_tag(text: &str, start: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut tag_end = start + 1;
    while tag_end < bytes.len()
        && (bytes[tag_end].is_ascii_alphanumeric() || bytes[tag_end] == b'_')
    {
        tag_end += 1;
    }
    if tag_end >= bytes.len() || bytes[tag_end] != b'$' || bytes[start + 1].is_ascii_digit() {
        return None;
    }
    Some(&text[start..=tag_end])
}

#[cfg(test)]
mod tests {
    use super::{at, body_line_offset};

    /// The sample from `sql/examples/length_function.sql`: Postgres reports the `RETURN`
    /// on file line 6 as line 3, because body line 1 is the `AS $function$`
    /// line — so the gutter has to start counting three lines in.
    #[test]
    fn routine_body_numbering_matches_postgres() {
        let sql = "CREATE OR REPLACE FUNCTION length(value text)\n\
                   RETURNS int\n\
                   LANGUAGE plpgsql\n\
                   AS $function$\n\
                   BEGIN\n\
                   RETURN char_length(value);\n\
                   END;\n\
                   $function$;\n";
        assert_eq!(body_line_offset(sql), -3);
    }

    #[test]
    fn leading_comments_shift_the_body_further_down() {
        let sql = "-- A greeting.\nCREATE FUNCTION f() RETURNS text AS $$\nBEGIN\nEND;\n$$;";
        assert_eq!(body_line_offset(sql), -1);
    }

    #[test]
    fn plain_sql_keeps_file_numbering() {
        assert_eq!(body_line_offset("SELECT 1;\nSELECT 2;\n"), 0);
        assert_eq!(body_line_offset(""), 0);
    }

    #[test]
    fn two_routines_have_no_single_origin() {
        let sql = "CREATE FUNCTION f() RETURNS int AS $$SELECT 1$$ LANGUAGE sql;\n\
                   CREATE FUNCTION g() RETURNS int AS $$SELECT 2$$ LANGUAGE sql;";
        assert_eq!(body_line_offset(sql), 0);
    }

    /// Statements around the routine don't move its body origin, and a
    /// `CREATE TABLE` is not mistaken for one.
    #[test]
    fn other_statements_do_not_count_as_routines() {
        let sql =
            "CREATE TABLE t (id int);\n\nCREATE FUNCTION f() RETURNS int\nAS $$\nBEGIN\nEND;\n$$;";
        assert_eq!(body_line_offset(sql), -3);
    }

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
