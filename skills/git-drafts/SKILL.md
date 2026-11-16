---
name: git-drafts
description: Multi-phase layered Git staging workflow for AI coding and review agents. Use when working in a repository with `.git/drafts/` (or initialized via `gd init`) so coding agents stage drafts safely without committing prematurely and review/lead agents promote vetted changes layer-by-layer before committing.
---

# `git-drafts` (`gd`) Multi-Agent Workflow

This repository uses **layered staging phases** on top of `HEAD` (`ready < reviewed < draft < working-tree` by default) instead of Git's single staging area.

## Core Invariants

- **Never bypass layers with raw `git add` or `git commit`** unless `sync_git_index` is intentionally enabled for `[L1] ready`.
- **Inspect state first:** Always run `gd status --json` (or `draft_status` MCP tool) and `gd layer list` before staging or promoting.
- **Keep layers ordered:** `[L1]` (bottommost, e.g. `ready`) is the ONLY commit target; `[Ln]` (topmost, e.g. `draft`) holds raw unreviewed work.
- **Undo mistakes safely:** Run `gd undo` (up to 15 snapshots) rather than destructive `git reset`.

## Role-Based Multi-Agent Protocol

### 1. Coder / Implementer Agent
1. Make edits in the working tree.
2. Stage finished task files into the top draft layer only:
   ```bash
   gd stage -- <file1> <file2>
   ```
3. Verify your layer delta with `gd diff draft` and `gd status --json`.
4. **Do NOT promote into `ready` or run `gd commit` / `git commit`** unless explicitly acting as Lead/Committer.

### 2. Reviewer / QA Agent
1. Inspect pending drafts:
   ```bash
   gd status --json
   gd diff draft
   ```
2. Run build/test checks against the working tree or layer changes.
3. For approved files, promote one layer closer to commit:
   ```bash
   gd promote --from draft --to reviewed -- <approved_files...>
   ```
4. For partial file approval (hunk-level without TTY), extract the unified diff hunk via `gd diff --from reviewed --to draft -- <file>` and pipe selected hunks into:
   ```bash
   gd patch-apply --from draft --to reviewed
   ```
5. For rejected changes, push them back (`gd demote --from <layer> --to wt -- <files>`) or drop from layers (`gd drop <layer> -- <files>`).

### 3. Lead / Finalizer Agent (or Human)
1. Review `[L2] reviewed` (`gd diff reviewed`) and promote verified work to `[L1] ready`:
   ```bash
   gd promote --from reviewed --to ready -- <files...>
   ```
2. Run final verification, then commit all or part of `[L1] ready`:
   ```bash
   gd commit -m "feat: concise summary"               # full ready layer
   gd commit <file1> -m "feat: partial ready commit"   # subset only
   ```

## Non-Interactive Tooling & MCP

Prefer `gd status --json`, `gd blob <layer|head|wt> <path> --allow-missing`, and `gd patch-apply --from A --to B` in automated/non-TTY environments. Never pass `-p` without an interactive TTY.
If running via MCP (`gd mcp --role coder|reviewer|lead`), use the `draft_*` tools directly; role guardrails enforce layer boundaries automatically.
