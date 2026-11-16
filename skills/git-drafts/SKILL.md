---
name: git-drafts
description: Multi-phase layered Git staging workflow for AI coding and review agents. Use when working in a repository with `.git/drafts/` (or initialized via `gd init`) so coding agents stage drafts safely without committing prematurely and review/lead agents promote vetted changes layer-by-layer before committing.
---

# `git-drafts` (`gd`) Multi-Agent Workflow

This repository replaces Git's single staging area with an ordered stack of **layered staging buffers** on top of `HEAD`:

```text
  HEAD            (committed history — untouched until final gate)
   ▲  gd commit
  [L1] ready      (commit target · optional .git/index mirror)
   ▲  gd promote / ↓ gd demote
  [L2] reviewed   (verified changes awaiting final approval)
   ▲  gd promote / ↓ gd demote
  [L3] draft      (top layer · sandbox for raw AI-agent edits)
   ▲  gd stage
  [WT] working    (unstaged & untracked workspace edits)
```

*(Layer names may be customized — always inspect actual names via `gd status --json` or `draft_status`.)*

---

## Core Invariants

1. **Never bypass layers:** Do NOT run raw `git add` or `git commit` (unless `sync_git_index` is enabled for `[L1] ready`).
2. **Inspect before acting:** Always call `gd status --json` (or MCP `draft_status`) first to read current layer names and file placement.
3. **Bottom (`[L1]`) = commit target; Top (`[Ln]`) = draft sandbox:** Promote vetted work toward `HEAD` (`ready`); demote rejected work back toward `draft` or `wt`.
4. **Never pass `-p` without a TTY:** Automated agents must use file-level commands (`gd stage -- <files>`) or non-interactive hunk patching (`gd patch-apply --from A --to B`).
5. **Recover safely:** Use `gd undo` (last 15 layer operations) instead of destructive `git reset`.

---

## Choosing Your Workflow Mode

- **Solo Dev + 1 AI Agent:** Run MCP as `--role reviewer` (default) or `--role coder`. You edit, verify, stage into `draft`, and optionally promote clean changes to `reviewed` — **you cannot commit**; the human promotes to `ready` and commits when sure.
- **Multi-Agent Team (`Coder -> Reviewer -> Lead`):** Run separate roles so each stage acts as a hard permission gate.

---

## Role Protocols

### 1. Coder / Implementer (`gd mcp --role coder`)
*Permissions: inspect (`status`, `diff`, `blob`), stage/demote/drop **only in the top `draft` layer**, undo. **Cannot promote or commit.***

1. Implement edits in the working tree.
2. Stage finished task files into the top draft layer:
   ```bash
   gd stage -- <file1> <file2>
   ```
3. Verify your isolated draft delta (`gd diff draft`, `gd status --json`).

### 2. Reviewer / QA (`gd mcp --role reviewer`)
*Permissions: Coder permissions + `promote`, `demote`, and `patch_apply` across layers. **Cannot commit.***

1. Inspect unreviewed changes in the top layer:
   ```bash
   gd status --json
   gd diff draft
   ```
2. Run build/tests and verify behavior. Apply small review fixes and stage them into `draft` if needed (`gd stage -- <files>`).
3. **Approve whole files** toward commit readiness:
   ```bash
   gd promote --from draft --to reviewed -- <approved_files...>
   ```
4. **Approve individual hunks (non-TTY):** inspect the diff (`gd diff --from reviewed --to draft -- <file>`) and pipe selected unified diff hunks into:
   ```bash
   gd patch-apply --from draft --to reviewed
   ```
5. **Reject / push back:** move changes back (`gd demote --from <layer> --to wt -- <files>`) or drop from the layer (`gd drop <layer> -- <files>`).

### 3. Lead / Committer (`gd mcp --role lead`)
*Permissions: full pipeline including `draft_commit`, `draft_init`, and `draft_layer_manage`.*

1. Review `[L2] reviewed` (`gd diff reviewed`) and promote final approved files into `[L1] ready`:
   ```bash
   gd promote --from reviewed --to ready -- <files...>
   ```
2. Run final checks, then commit all or part of `[L1] ready`:
   ```bash
   gd commit -m "feat: concise summary"               # commit all of ready
   gd commit <file1> -m "feat: partial ready commit"   # commit subset only
   ```
