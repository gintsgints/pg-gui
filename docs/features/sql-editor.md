# SQL editor

Every open script lives in its own editor tab. The editor is gpui-component's
code editor driving a tree-sitter SQL grammar, so highlighting follows the
parse tree rather than a regex, and the theme is the monospace half of the
active Catppuccin flavour (see [Themes](themes.md)).

What the editor gives you:

- **Line numbers** in the gutter, with a clickable area for
  [debug breakpoints](debugging.md).
- **Comment / uncomment** the current line or selection with `cmd-/`.
- **Find** (`cmd-f`) and **find and replace** (`cmd-r`) inside the script.
- **Zoom** with `cmd-plus` / `cmd-minus`, back to the default with `cmd-0`.
- Language features — completions, hover, diagnostics, formatting — from the
  embedded [Postgres Language Server](language-server.md).
- [Snippets](snippets.md) with numbered tab stops.
- [Go to Definition](go-to-definition.md) on cmd-click.

## Routine-relative line numbers

When a tab holds exactly one `CREATE FUNCTION` or `CREATE PROCEDURE`, the
gutter numbers the lines the way Postgres does in its error messages and in
`pldbg` stack frames: body-relative, where body line 1 is the line carrying
the opening `$tag$`. The lines above the body therefore read `0`, `-1`, `-2`.

This only shifts the drawn numbers. Breakpoints, the cursor position and
diagnostics all still speak buffer rows, and a buffer with no routine — or
with several — falls back to plain file numbering.
