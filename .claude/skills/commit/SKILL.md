
## Steps

1. Run `git status` and `git diff` in parallel to see what has changed.
2. Run `git log --oneline -10` to match the existing commit message style.
3. Analyse the changes and determine how many logical commits are needed.
   - Each functional change gets its own commit (never bundle unrelated changes).
   - Dependency-only changes (Cargo.toml / Cargo.lock) are separate from code changes.
4. For each commit:
   a. Stage only the files that belong to that commit.
   b. Run `cargo fmt && cargo clippy -- -D warnings -W clippy::pedantic && cargo test` — fix any failures before proceeding.
   c. Commit immediately — do NOT ask for confirmation. Invoking `/commit` is the confirmation.
   d. Commit with a message in this format:

```
<type>: <short imperative summary>

<optional body — explain the why, not the what>
```

Allowed types: `feat`, `fix`, `refactor`, `chore`, `docs`, `test`.

Pass the message via heredoc to preserve formatting:

```bash
git commit -m "$(cat <<'EOF'
type: summary

Optional body.
)"
```

5. Run `git status` after each commit to confirm success.

## Rules

- Never use `--no-verify` or `--amend` unless the user explicitly asks.
- Never `git add -A` or `git add .` — stage specific files by name.
- Never push unless the user explicitly asks.
