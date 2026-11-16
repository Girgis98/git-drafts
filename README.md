# `git-drafts` (`gd`)

**Multi-phase layered staging areas for Git — a safe sandbox for experimenting with AI agents.**

Standard Git gives you three phases: **Working Tree → Index (Staged) → Committed (`HEAD`)**.  
When coding with AI agents, one staging area isn't enough. Agents generate large, fast, half-baked diffs where some hunks are ready, some need another pass, and others are raw experiments.

`git-drafts` lets you stack **as many revision layers as you need** on top of `HEAD` using native Git indices (`GIT_INDEX_FILE`), so you can iterate, isolate hunks, and **never commit until you're sure** — and then commit all or part of your verified layer.

---

## Why Use `git-drafts`?

- **Built for AI-agent workflows:** Keep raw agent output in `draft`, human-checked edits in `reviewed`, and commit-ready changes in `ready` — without polluting your commit history or losing track of what you actually vetted.
- **As many layers as you want:** Start with `ready < reviewed < draft`, or add custom phases (`tested`, `security-checked`, `polish`, etc.) at any position in the stack.
- **Hunk-level precision (`-p`):** Promote, demote, drop, or partially commit individual hunks without touching your working tree.
- **Don't commit prematurely:** Experiment across multiple agent turns in layered draft buffers before anything touches `HEAD`.
- **100% Git-native & fast:** Written in zero-dependency Rust driving Git's own plumbing (`write-tree`, `diff-tree`, `apply --cached`, `merge-tree`). Zero repo format lock-in; your `.git/hooks`, GPG signing, and normal `git` commands keep working.
- **Reversible:** Up to 15 undo snapshots (`gd undo`) plus automatic 3-way re-anchoring if `HEAD` moves externally (`git pull`, `git rebase`, `git commit`).

---

## How It Works

Layers are stored in `.git/drafts/` as an ordered stack of standard Git index files from **most revised (closest to `HEAD`)** to **least revised (closest to Working Tree)**:

```text
HEAD (committed)
  └── [L1] ready      ← most revised; target for `gd commit`
        └── [L2] reviewed
              └── [L3] draft   ← raw agent output
                    └── [WT] working tree
```

Each layer index stores a cumulative tree snapshot (`HEAD ⊆ L1 ⊆ L2 ⊆ ... ⊆ Ln ⊆ WT`):
- What is shown *inside* layer `k` is the exact delta between `L(k-1)` and `Lk`.
- **Promoting** moves paths or hunks downward toward `ready`.
- **Demoting** pushes paths or hunks upward away from `commit`.
- **Committing** (`gd commit`) commits `L1` (`ready`) — or only selected paths/hunks from `L1` — while `L2..Ln` remain intact above the new `HEAD`.

---

## Installation

### Prerequisites
- `git` (2.38+ recommended for 3-way `merge-tree` re-anchoring)
- Rust toolchain (`cargo` / `rustc`)

### Build & Install
```bash
git clone https://github.com/Girgis98/git-drafts.git
cd git-drafts
cargo install --path .
```

This installs both `gd` and `git-drafts` (so you can run either `gd <cmd>` or `git drafts <cmd>`).

---

## Step-by-Step Guide

### Step 1: Initialize draft layers in any Git repository
```bash
gd init
# Initialized layers: ready < reviewed < draft
```
Want custom layer names right from the start?
```bash
gd init ready audited draft raw-agent
```

### Step 2: Let an AI agent edit files, then capture into the top draft layer
After your agent modifies files or creates new ones, inspect and stage them into `draft` (`L3`, the default top layer):
```bash
gd stage -A               # or: gd stage src/auth.rs
gd status                 # alias: gd st
```
Example output:
```text
[L1] ready (commit target) (0 files)
  (clean)
[L2] reviewed (0 files)
  (clean)
[L3] draft (2 files)
  M  src/auth.rs
  A  src/token.rs
[WT] working-tree (0 modified, 0 untracked)
  (clean)
```

### Step 3: Review diffs per layer
Inspect only what sits in `draft`, or compare any two layers:
```bash
gd diff draft             # diff of L3 (reviewed..draft)
gd diff wt                # unstaged diff in working tree
gd diff --from head --to draft
```

### Step 4: Promote verified changes down toward `ready`
Promote whole files one layer down (`draft -> reviewed`):
```bash
gd promote src/token.rs
```
Promote again when you're confident (`reviewed -> ready`):
```bash
gd promote src/token.rs
```
Or jump straight across layers, or pick **individual hunks** interactively with `-p`:
```bash
gd promote --from draft --to ready -p src/auth.rs
```
*(Uses Git's interactive hunk selector `[y,n,q,a,d,s,e,?]` without modifying your working tree.)*

### Step 5: Experiment further without risking what's already verified
Your agent can keep editing `src/auth.rs` in the working tree. Stage new experiments into `draft`:
```bash
gd stage src/auth.rs
```
Your earlier vetted hunks remain safely isolated in `ready` or `reviewed`, while the new agent edits sit cleanly above them in `draft`.

### Step 6: Commit everything in `ready`, or only part of it
Commit the entire `ready` (`L1`) layer:
```bash
gd commit -m "feat(auth): add token validation"
```
Or commit **only specific paths or hunks** from `ready` — uncommitted remainder stays in `ready`, and `reviewed`/`draft` stay intact:
```bash
gd commit src/token.rs -m "feat(auth): add token helper"
gd commit -p -m "feat(auth): partial auth fix"
```

### Step 7: Demote, drop, or undo mistakes
Changed your mind before committing?
```bash
gd demote src/auth.rs                     # move one layer up (away from commit)
gd demote --from draft --to wt -A         # unstage draft back to working tree
gd drop draft --cascade src/exp.rs        # remove from draft & upper layers (keeps WT)
gd drop draft --hard src/exp.rs           # remove from layers AND revert WT
gd undo                                   # restore previous layer state (last 15 ops)
```

### Step 8: Add, rename, or remove layers anytime
```bash
gd layer list
gd layer add qa --above reviewed          # insert new phase between reviewed & draft
gd layer rename qa security-check
gd layer rm security-check --merge-down   # or --merge-up / --drop
```

---

## Command Reference

| Command | Description |
| :--- | :--- |
| `gd init [--force] [layers...]` | Initialize layers (default: `ready reviewed draft`) |
| `gd status` / `gd st` | Show parallel per-layer file status (`L1..Ln` + `WT`) |
| `gd diff [<layer>\|wt] [--from A --to B] [-- <paths...>]` | Show diff for a layer or between any two states |
| `gd stage [-l <layer>] [-p\|-A] [<paths...>]` | Stage `WT` changes into `<layer>` (default: top layer) |
| `gd promote [--from L] [--to L] [-p\|-A] [<paths...>]` | Move changes down toward `HEAD` |
| `gd demote [--from L] [--to L\|wt] [-p\|-A] [<paths...>]` | Move changes up away from commit |
| `gd drop <layer> [-p\|-A] [--cascade] [--hard] [<paths...>]` | Discard changes from a layer |
| `gd commit [-p] [<paths...>] [git-commit-opts...]` | Commit `[L1] ready` (all or selected paths/hunks) |
| `gd layer list \| add \| rm \| rename` | Manage revision layers dynamically |
| `gd blob <spec> <path> [--allow-missing]` | Print raw file bytes at a layer/`head`/`wt` |
| `gd patch-apply --from A --to B` | Apply unified diff from stdin between layers (IDE/script hook) |
| `gd config sync-git-index [true\|false]` | Mirror `[L1] ready` into `.git/index` for built-in IDE Git views |
| `gd undo` | Restore previous layer snapshot |

---

## VS Code & IDE Integration

`git-drafts` supports three integration levels:

### 1. Mirror `[L1] ready` into `.git/index` (any IDE, zero extensions)
```bash
gd init --sync-git-index
# or enable later:
gd config sync-git-index true
```
Every mutation mirrors `[L1] ready` into `.git/index`, so VS Code, JetBrains, Zed, and GitLens immediately display `ready` as **Staged Changes** while `reviewed`/`draft` stay isolated in `.git/drafts/`.

### 2. Machine-readable CLI primitives (custom editors / agents)
- `gd status --json` — structured JSON for layers, tree OIDs, file statuses, and `wt`
- `gd blob <layer|head|wt> <path> --allow-missing` — raw file bytes for virtual diff URIs
- `gd patch-apply --from <src> --to <dst>` — non-interactive hunk promote/demote via stdin patch

### 3. VS Code / Cursor Extension ([editors/vscode](editors/vscode))
Provides a native **Git Drafts** panel in Source Control with one group per layer, side-by-side layer diffs, inline Stage/Promote/Demote/Drop/Partial-Commit buttons, Undo, and `.git/index` mirror toggle:
```bash
cd editors/vscode
npm install
npm run compile
```
Then open `editors/vscode` in VS Code and press `F5` (Extension Development Host) or package with `npx @vscode/vsce package`.
