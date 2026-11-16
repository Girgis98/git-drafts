# Git Drafts for VS Code

**Multi-phase layered staging areas for Git — a safe space to experiment with AI agents before committing.**

Stop forcing messy AI-agent edits into a single Git staging area. `Git Drafts` adds a dedicated Activity Bar view, Source Control integration, and diff-editor hunk controls backed by [`gd`](https://github.com/Girgis98/git-drafts).

## Features

- **Activity Bar Pipeline View:** Visual stack of your revision layers (`HEAD ← ready ← reviewed ← draft ← WT`) with file counts, one-click Promote/Demote, Layer Add/Rename/Remove, Undo, and `.git/index` mirror toggle.
- **Draft Layers Tree & Source Control Integration:** Browse changed files per layer, open native side-by-side diffs (`L(k-1) ↔ Lk`), and Stage, Promote (`↓`), Demote (`↑`), Drop (`✕`), or Commit individual files or the entire `ready` layer.
- **Diff Editor Hunk Promotion/Demotion:** When inspecting any `git-drafts` diff in the editor, promote or demote the hunk at your cursor/selection directly from the editor title bar (`Promote Hunk` / `Demote Hunk`).
- **Status Bar Counter:** Live layer summary in the bottom status bar (`Drafts: ready(1) · reviewed(0) · draft(2)`).
- **Optional `.git/index` Mirror:** Sync `[L1] ready` into `.git/index` so built-in Git tools see your commit-ready layer as staged.

## Requirements

Install the `gd` CLI (`git-drafts`):

```bash
cargo install --git https://github.com/Girgis98/git-drafts.git
```

## Quick Start

1. Open any Git repository in VS Code.
2. Click the **Git Drafts** icon in the Activity Bar (or run `Git Drafts: Initialize Draft Layers` from the Command Palette).
3. Stage agent edits into `draft`, inspect diffs, promote verified files or hunks down to `reviewed` → `ready`, and commit all or selected files when you're sure.

## Extension Settings

- `gitDrafts.executablePath`: Path to the `gd` binary (default: `"gd"`, with automatic fallback to `~/.cargo/bin/gd` and `<workspace>/target/release/gd`).
