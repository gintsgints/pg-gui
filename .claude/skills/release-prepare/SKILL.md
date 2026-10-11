## Steps

### 1. Read the current version

Read `Cargo.toml` and extract the `version` field. This is the release version, referred to as `VERSION` below.

### 2. Check README.md for completeness

Read `README.md`.

Check that every keybinding defined in the README keybindings table. Also check that every feature mentioned in the Features list actually exists in the code (not stale). Fix any gaps — missing keybindings or stale feature entries.

**If there are gaps:** update `README.md` to fix them. Stage and commit the fix as:
```
docs: update README for vVERSION
```

Do not proceed until README is accurate.

### 3. Check the in-app help panel for completeness

Check source code so help panel is up to date with changes.

```bash
cargo fmt && cargo clippy -- -D warnings -W clippy::pedantic && cargo test
```
Fix any failures. Stage and commit:
```
docs: sync help panel keybindings for vVERSION
```

Do not proceed until the help panel is accurate.

### 4. Determine commits since last release tag

Run:
```bash
git tag --sort=-version:refname | head -5
git log $(git describe --tags --abbrev=0 2>/dev/null || git rev-list --max-parents=0 HEAD)..HEAD --oneline
```

This gives all commits since the last tag (or all commits if no tags exist). These are the changes to describe in the changelog.

### 5. Update CHANGELOG.md

Read `CHANGELOG.md`. Prepend a new section for `VERSION` above the previous top entry using the format already established in the file:

```markdown
## vVERSION

### New features

- **Feature name** (`key`) — one-line description

### Fixes

- one-line description

### Other

- one-line description
```

Only include sections that have entries. Derive entries from the commit list gathered in step 3 — use `feat:` commits for "New features", `fix:` for "Fixes", `chore:`/`refactor:`/`docs:` for "Other". Skip dependency-only or formatting-only commits.

Stage and commit the changelog:
```
docs: add CHANGELOG entry for vVERSION
```

### 6. Verify the remote is up to date

Run:
```bash
git status
git log origin/master..HEAD --oneline
```

If there are unpushed commits, stop and tell the user:

> There are unpushed commits. Push to remote first (`git push`), then re-run `/release-prepare`.

Do **not** push automatically.

### 7. Create the version tag

Run:
```bash
git tag vVERSION
```

Then confirm success:
```bash
git tag --sort=-version:refname | head -3
```

Tell the user the tag was created and remind them to push it:
```
git push origin vVERSION
```

## Rules

- Never push (branches or tags) unless the user explicitly asks.
- Do not create a tag if there are unpushed commits — the tag must point to a commit already on the remote.
- If the tag `vVERSION` already exists, stop and tell the user rather than moving or overwriting it.
