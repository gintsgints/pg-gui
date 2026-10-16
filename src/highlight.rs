//! The keywords and parameters the SQL grammar does not colour.
//!
//! Syntax colouring in the editor comes from gpui-component's tree-sitter
//! layer, whose SQL grammar (`tree_sitter_sequel`) covers plain SQL and no
//! PL/pgSQL: it has no token for `CALL`, `PERFORM`, `RAISE`, `LOOP` and the
//! rest, so a `DO $$ … $$` body parses into one big `ERROR` node with no
//! identifiers inside it. That leaves nothing for a highlight query to
//! capture — the words draw in the plain text colour.
//!
//! So pg-gui supplies them itself, as the editor's semantic-tokens provider
//! ([`Tokens`]): a scanner finds the words and the editor paints them over
//! the tree-sitter result with the theme's `keyword` style. Only words the
//! grammar has no token of its own for are reported, so a word the
//! tree-sitter layer already colours (`BEGIN`, `IF`, `DECLARE`, …) is never
//! styled twice — the two sets overlap in no word, and an overlap would
//! resolve unpredictably at paint time.
//!
//! Positional parameters (`$1`, `$2`, …) go through the same provider. The
//! grammar does have a node for them, but captures it as `@parameter`, a name
//! the editor's theme has no style for (it matches capture names exactly), so
//! they drew as plain text — and inside a routine body, where they are most
//! common, there is no node at all. They are reported with the theme's
//! `variable.special` style, which no tree-sitter capture of the SQL grammar
//! uses, so nothing is styled twice there either.

use std::ops::Range;

use anyhow::Result;
use gpui::{App, Task, Window};
use gpui_component::input::{DocumentRangeSemanticTokensProvider, Rope};
use lsp_types::{SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensLegend};

use crate::statement;

/// Words that only mean anything inside a routine body, and are ordinary
/// identifiers outside one — a column named `close` or `exception` is legal
/// SQL, so these are reported only between dollar quotes.
const BODY_KEYWORDS: &[&str] = &[
    "alias",
    "assert",
    "constant",
    "continue",
    "cursor",
    "diagnostics",
    "elseif",
    "elsif",
    "exception",
    "exit",
    "loop",
    "perform",
    "raise",
    "reverse",
    "rowtype",
    "slice",
    "while",
];

/// Words reported anywhere in the buffer. `CALL` is a statement of its own in
/// plain SQL as much as it is inside a body, and the grammar misses it in
/// both.
const KEYWORDS: &[&str] = &["call"];

/// What a reported span is; the discriminant is its index in the legend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Keyword = 0,
    Parameter = 1,
}

/// A reported span of the buffer, in bytes.
#[derive(Debug, PartialEq, Eq)]
struct Span {
    range: Range<usize>,
    kind: Kind,
}

/// The editor's semantic-tokens provider: see the module comment.
pub struct Tokens;

impl DocumentRangeSemanticTokensProvider for Tokens {
    fn legend(&self) -> SemanticTokensLegend {
        // Indexed by `Kind`. The names are looked up in the highlight theme,
        // so `variable.special` is the theme's style name, not an LSP token
        // type.
        SemanticTokensLegend {
            token_types: vec![
                SemanticTokenType::KEYWORD,
                SemanticTokenType::new("variable.special"),
            ],
            token_modifiers: vec![],
        }
    }

    fn semantic_tokens(
        &self,
        text: &Rope,
        _range: Range<usize>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<Result<SemanticTokens>> {
        // The editor asks for the whole document and caches the answer, and
        // the scan is a single pass over the buffer, so there is nothing to
        // move off the main thread.
        let text = text.to_string();
        Task::ready(Ok(SemanticTokens {
            result_id: None,
            data: tokens(&text),
        }))
    }
}

/// The delta-encoded tokens for every span in `text`, in the order the LSP
/// encoding requires (by position).
fn tokens(text: &str) -> Vec<SemanticToken> {
    let mut data = Vec::new();
    let mut spans = spans(text).into_iter().peekable();
    // Walk the text once, tracking the line and the column *in characters*
    // (what the editor's `position_to_offset` expects), and emit each span as
    // it is passed.
    let mut line = 0_u32;
    let mut column = 0_u32;
    let mut previous: Option<(u32, u32)> = None;
    for (offset, ch) in text.char_indices() {
        if let Some(span) = spans.peek()
            && span.range.start == offset
        {
            let (delta_line, delta_start) = match previous {
                Some((previous_line, previous_column)) if previous_line == line => {
                    (0, column - previous_column)
                }
                _ => (line - previous.map_or(0, |(line, _)| line), column),
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                // Keywords and parameters are ASCII, so the byte length is
                // the length in characters, and none is anywhere near
                // `u32::MAX`.
                length: u32::try_from(span.range.len()).unwrap_or(u32::MAX),
                token_type: span.kind as u32,
                token_modifiers_bitset: 0,
            });
            previous = Some((line, column));
            spans.next();
        }
        if ch == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    data
}

/// The keywords and parameters in `text`, skipping strings, comments and
/// quoted identifiers the way [`statement::ranges`] does, but stepping *into*
/// dollar-quoted regions rather than over them — that is where the PL/pgSQL
/// is.
fn spans(text: &str) -> Vec<Span> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    // The tag of the dollar-quoted region we are inside, if any. A different
    // tag met while inside one is literal body text, not a nested region.
    let mut body: Option<&str> = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' => i = statement::skip_quoted(bytes, i, bytes[i]),
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = statement::skip_line_comment(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = statement::skip_block_comment(bytes, i),
            b'$' => {
                if let Some(tag) = statement::dollar_quote_tag(text, i) {
                    match body {
                        Some(open) if open == tag => body = None,
                        Some(_) => {}
                        None => body = Some(tag),
                    }
                    i += tag.len();
                } else if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    let start = i;
                    i += 1;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    spans.push(Span {
                        range: start..i,
                        kind: Kind::Parameter,
                    });
                } else {
                    i += 1;
                }
            }
            c if c.is_ascii_alphanumeric() || c == b'_' => {
                let start = i;
                // `$` continues an identifier (`price$1` is one name), so it
                // is taken with the word rather than read as a parameter.
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
                {
                    i += 1;
                }
                // A word is taken whole: `recall` must not match `call`, and
                // `pg_exit` must not match `exit`.
                if is_keyword(&text[start..i], body.is_some()) {
                    spans.push(Span {
                        range: start..i,
                        kind: Kind::Keyword,
                    });
                }
            }
            _ => i += 1,
        }
    }
    spans
}

fn is_keyword(word: &str, in_body: bool) -> bool {
    let matches = |list: &[&str]| {
        list.iter()
            .any(|keyword| keyword.eq_ignore_ascii_case(word))
    };
    matches(KEYWORDS) || (in_body && matches(BODY_KEYWORDS))
}

#[cfg(test)]
mod tests {
    use gpui_component::highlighter::HighlightTheme;
    use gpui_component::input::DocumentRangeSemanticTokensProvider as _;

    use super::{Kind, Tokens, tokens};

    /// The keywords `super::spans` finds in `text`, as byte ranges.
    fn keyword_spans(text: &str) -> Vec<std::ops::Range<usize>> {
        spans_of(text, Kind::Keyword)
    }

    fn spans_of(text: &str, kind: Kind) -> Vec<std::ops::Range<usize>> {
        super::spans(text)
            .into_iter()
            .filter(|span| span.kind == kind)
            .map(|span| span.range)
            .collect()
    }

    fn parameters(text: &str) -> Vec<&str> {
        spans_of(text, Kind::Parameter)
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    /// The sample from issue #23: `CALL` and `PERFORM` inside a `DO` block.
    #[test]
    fn do_block_keywords_are_reported() {
        let sql = "DO $$\n\
                   BEGIN\n\
                   CALL refresh_continuous_aggregate('daily_premiums', '2026-01-01');\n\
                   \n\
                   PERFORM pg_reload_conf();\n\
                   END;\n\
                   $$;\n";
        let words: Vec<&str> = keyword_spans(sql)
            .into_iter()
            .map(|span| &sql[span])
            .collect();
        assert_eq!(words, ["CALL", "PERFORM"]);
    }

    /// `CALL` is a plain-SQL statement too, but a body-only word outside a
    /// body is somebody's column.
    #[test]
    fn body_words_need_a_body() {
        assert_eq!(keyword_spans("CALL do_work();").len(), 1);
        assert!(keyword_spans("SELECT close, exception FROM trades;").is_empty());
        assert_eq!(
            keyword_spans("DO $$ BEGIN RAISE NOTICE 'x'; END $$;").len(),
            1
        );
    }

    /// A keyword is only a keyword as a whole word, and not inside a string,
    /// a comment, or a quoted identifier.
    #[test]
    fn words_are_taken_whole_and_only_as_code() {
        assert!(keyword_spans("SELECT recall, call_count FROM t;").is_empty());
        assert!(keyword_spans("SELECT 'call';").is_empty());
        assert!(keyword_spans("-- call\nSELECT 1;").is_empty());
        assert!(keyword_spans("/* call */ SELECT 1;").is_empty());
        assert!(keyword_spans("SELECT \"call\" FROM t;").is_empty());
        // `$1` does not open a body, so the word after it stays plain SQL.
        assert!(keyword_spans("SELECT $1, loop FROM t;").is_empty());
    }

    /// Positions are delta-encoded from the previous token, with the column
    /// in characters — the unit the editor converts back to a byte offset.
    #[test]
    fn tokens_are_delta_encoded_in_characters() {
        let sql = "DO $$\nBEGIN\n  CALL f();\n  PERFORM g();\nEND $$;";
        let data = tokens(sql);
        assert_eq!(data.len(), 2);
        assert_eq!(
            (data[0].delta_line, data[0].delta_start, data[0].length),
            (2, 2, 4)
        );
        assert_eq!(
            (data[1].delta_line, data[1].delta_start, data[1].length),
            (1, 2, 7)
        );
    }

    /// The `keyword` token type the legend names has to be a style the
    /// editor's theme knows, or the editor drops the tokens.
    #[test]
    fn the_theme_resolves_the_token_type() {
        let legend = Tokens.legend();
        let theme = HighlightTheme::default_dark();
        for token_type in legend.token_types {
            assert!(theme.style(token_type.as_str()).is_some());
        }
    }

    /// The sample from issue #26: positional parameters in plain SQL.
    #[test]
    fn parameters_are_reported() {
        assert_eq!(
            parameters("SELECT * FROM orders WHERE id = $1 AND status = $12;"),
            ["$1", "$12"]
        );
    }

    /// Inside a routine body, where the grammar has no node for them.
    #[test]
    fn parameters_are_reported_in_bodies() {
        let sql = "CREATE FUNCTION f(int) RETURNS int AS $$\n\
                   BEGIN RETURN $1 + 1; END\n\
                   $$ LANGUAGE plpgsql;";
        assert_eq!(parameters(sql), ["$1"]);
    }

    /// Not a parameter: a dollar-quote delimiter, a `$` inside an identifier, or a
    /// `$1` inside a string, comment or quoted identifier.
    #[test]
    fn parameters_are_only_taken_as_code() {
        assert!(parameters("SELECT $tag$x$tag$, $$y$$;").is_empty());
        assert!(parameters("SELECT price$1 FROM t;").is_empty());
        assert!(parameters("SELECT '$1';").is_empty());
        assert!(parameters("-- $1\nSELECT 1;").is_empty());
        assert!(parameters("/* $1 */ SELECT 1;").is_empty());
        assert!(parameters("SELECT \"$1\" FROM t;").is_empty());
        assert!(parameters("SELECT $ FROM t;").is_empty());
    }

    /// Keywords and parameters share one token stream, each with its own
    /// legend index.
    #[test]
    fn token_types_follow_the_kind() {
        let data = tokens("CALL f($1);");
        let types: Vec<u32> = data.iter().map(|token| token.token_type).collect();
        assert_eq!(types, [0, 1]);
        assert_eq!(
            (data[1].delta_line, data[1].delta_start, data[1].length),
            (0, 7, 2)
        );
    }

    /// A multi-byte character ahead of a keyword shifts its byte offset but
    /// not its column.
    #[test]
    fn columns_count_characters_not_bytes() {
        let sql = "DO $$\n-- ßßß\n  CALL f();\nEND $$;";
        let data = tokens(sql);
        assert_eq!(data.len(), 1);
        assert_eq!((data[0].delta_line, data[0].delta_start), (2, 2));
    }
}
