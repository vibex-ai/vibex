# Release Notes

Write the notes a Vibex release publishes, from a version such as `0.1.0`.
When no version is supplied, use the `version` in the workspace `Cargo.toml`.

## Gather

1. Write the brief for the range:

   ```bash
   node scripts/release-notes.mjs --version <version> --context target/release-notes/context-v<version>.md
   ```

   It lists the pending commits grouped by type and scope, the changed areas,
   the structure of the previous release's notes and the pull requests merged in
   the range. Read it before writing: it is the map of what actually landed.

2. Read the previous document with
   `git show v<previous>:docs/operations/release-notes-v<previous>.md` to match
   its section names, its level of detail and its tone.

3. Open the body of every change that adds a component, a public API, a
   platform, a setting or a behaviour a reader can notice. Take API names,
   method names and migration snippets from the code or the pull request —
   never invent a name from a commit subject. Fix-only changes may be
   summarised from their subject.

4. Scaffold the document when it does not exist yet:

   ```bash
   node scripts/release-notes.mjs --version <version> --scaffold
   ```

   It writes the header and the two language headings and refuses to overwrite
   an existing file.

## Write

Produce one document at `docs/operations/release-notes-v<version>.md`:

```markdown
# Vibex v<version> Release Notes

- Released: <YYYY-MM-DD>

---

## English

### 🚀 Highlights

- **<Feature>** — what a reader can now do.

### 🛠 New features

**<Product area>**

- <What changed for the person using it.>

### ⚡️ Performance

### 🔧 Under the hood

---

## 中文

### 🚀 核心亮点

### 🛠 新功能

### ⚡️ 性能

### 🔧 底层
```

Rules:

- Group by product area, ordered by significance: the headline features first,
  then the surface they belong to (desktop, mobile, TUI, gateway, remote,
  packaging). Drop a section that has nothing to say; merge small ones.
- Every bullet says what a reader can now do, not which file changed. Name the
  identifier in backticks.
- Leave out chores, CI, lockfile refreshes, test-only changes and version bumps.
- The Chinese section mirrors the English one heading for heading; it is written
  prose, not a translation dump.
- Cite a pull request when the change has one; do not invent links for direct
  commits.
- Both language sections are required. `--check` rejects a document that lost
  one, or that still carries only the scaffold comments.

## Verify

```bash
node scripts/release-notes.mjs --version <version> --check
node scripts/release-body.mjs --version <version> \
  --notes docs/operations/release-notes-v<version>.md \
  --output target/release-notes/body-v<version>.md
```

Read the composed body: it must open with English, carry the Chinese section,
continue with the Downloads table, and close with the contributor credit and the
comparison link. There is no per-change list — these notes are the changelog, so
write them as the only thing a reader will see above the download links. The
publish job builds the same body and fails the release when the notes are
missing or unpublishable.

Do not create or edit a GitHub Release by hand; the tagged workflow owns it.
