# Changelog

## 0.1.3

- Add built-in AI-agent instruction/skill generator (`gd agent init`) and role-scoped stdio MCP server (`gd mcp --role coder|reviewer|lead`)
- Add `Agents` setup button & command (`Git Drafts: Setup Agent Instructions & MCP`) in VS Code

## 0.1.2

- Auto-download prebuilt `gd` binary from GitHub Releases on first launch when not found on PATH (zero-step install across Linux, macOS, and Windows, including Remote-SSH)

## 0.1.1

- Fix Extension Details README images (`media/hero.png`, `media/preview.png`) using Markdown syntax
- Match Marketplace publisher ID (`GirgisMamdouh`)

## 0.1.0

- Initial release of **Git Drafts** (`gd`) for VS Code
- Activity Bar **Draft Pipeline** webview and **Changed Files** tree view
- Embedded **Draft Layers** tree view inside built-in Source Control
- Seamless bidirectional `.git/index` mirror with VS Code Git (`+`, `-`, Commit)
- Multiselect support across SCM and TreeViews (`Stage`, `Promote ↑`, `Demote ↓`, `Drop`, `Commit File`)
- Diff editor title-bar hunk actions (`Promote Hunk ↑` / `Demote Hunk ↓`) and Status Bar summary
