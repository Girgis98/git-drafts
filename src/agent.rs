use crate::git::RepoCtx;
use crate::json::{self, Json};
use crate::mcp::AgentRole;
use std::fs;
use std::path::Path;

pub const SKILL_MD: &str = include_str!("../skills/git-drafts/SKILL.md");

const MARKER_START: &str = "<!-- git-drafts:start -->";
const MARKER_END: &str = "<!-- git-drafts:end -->";

pub fn agent_snippet() -> &'static str {
    "\
<!-- git-drafts:start -->
## Layered Staging (`git-drafts` / `gd`)

This repository uses `git-drafts` (`gd`) for multi-phase layered staging instead of a single Git index:

```text
HEAD <- [L1] ready (commit target) <- [L2] reviewed <- [L3] draft (sandbox) <- [WT] working-tree
```

- **Inspect first:** Always run `gd status --json` and `gd layer list` (or MCP `draft_status`) before acting.
- **Coder / single agent:** Stage finished edits into the top draft layer (`gd stage -- <paths...>` or `draft_stage`). Do **not** run `git add`, `git commit`, or `gd commit` unless acting as Lead/Committer.
- **Reviewer / QA:** Inspect `gd diff draft` (`draft_diff`), run checks, and promote verified files (`gd promote --from draft --to reviewed -- <paths...>`) or hunks (`gd patch-apply --from draft --to reviewed`), or push back (`gd demote` / `gd drop`).
- **Lead / Commit gate:** Promote `reviewed` -> `ready`, run final checks, then commit (`gd commit [<paths...>] -m \"...\"`).
- **Non-TTY safety:** Never pass `-p` without an interactive TTY; use `gd status --json`, `gd diff`, `gd blob`, and `gd patch-apply`. Use `gd undo` to revert layer mistakes. See `.agents/skills/git-drafts/SKILL.md` for full workflows.
<!-- git-drafts:end -->"
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentTarget {
    Auto,
    All,
    Agents,
    Claude,
    Cursor,
    Copilot,
    Gemini,
    Skill,
    Mcp,
}

impl AgentTarget {
    pub fn parse_list(spec: &str) -> Result<Vec<Self>, String> {
        let mut out = Vec::new();
        for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let t = match part.to_ascii_lowercase().as_str() {
                "auto" => Self::Auto,
                "all" => Self::All,
                "agents" => Self::Agents,
                "claude" => Self::Claude,
                "cursor" => Self::Cursor,
                "copilot" => Self::Copilot,
                "gemini" => Self::Gemini,
                "skill" => Self::Skill,
                "mcp" => Self::Mcp,
                other => {
                    return Err(format!(
                        "unknown agent target '{other}' (expected auto, all, agents, claude, cursor, copilot, gemini, skill, mcp)"
                    ))
                }
            };
            if !out.contains(&t) {
                out.push(t);
            }
        }
        if out.is_empty() {
            out.push(Self::Auto);
        }
        Ok(out)
    }
}

pub fn cmd_agent_snippet() {
    println!("{}", agent_snippet());
}

pub fn cmd_agent_init(
    repo: &RepoCtx,
    targets: &[AgentTarget],
    role: AgentRole,
    mcp_command: Option<&str>,
) -> Result<(), String> {
    let root = &repo.work_tree;
    let effective = if targets.is_empty() {
        vec![AgentTarget::Auto]
    } else {
        targets.to_vec()
    };

    let has = |t: AgentTarget| effective.contains(&t) || effective.contains(&AgentTarget::All);
    let is_auto = effective.contains(&AgentTarget::Auto);

    let mut updated: Vec<String> = Vec::new();

    if has(AgentTarget::Agents) || is_auto {
        let p = root.join("AGENTS.md");
        upsert_marked_file(&p, agent_snippet())?;
        updated.push("AGENTS.md".to_string());
    }

    if has(AgentTarget::Skill) || is_auto {
        let p = root
            .join(".agents")
            .join("skills")
            .join("git-drafts")
            .join("SKILL.md");
        write_file_ensuring_parent(&p, SKILL_MD)?;
        updated.push(".agents/skills/git-drafts/SKILL.md".to_string());
    }

    let claude_path = root.join("CLAUDE.md");
    if has(AgentTarget::Claude) || (is_auto && claude_path.exists()) {
        upsert_marked_file(&claude_path, agent_snippet())?;
        updated.push("CLAUDE.md".to_string());
    }

    let gemini_path = root.join("GEMINI.md");
    if has(AgentTarget::Gemini) || (is_auto && gemini_path.exists()) {
        upsert_marked_file(&gemini_path, agent_snippet())?;
        updated.push("GEMINI.md".to_string());
    }

    let copilot_path = root.join(".github").join("copilot-instructions.md");
    if has(AgentTarget::Copilot) || (is_auto && copilot_path.exists()) {
        upsert_marked_file(&copilot_path, agent_snippet())?;
        updated.push(".github/copilot-instructions.md".to_string());
    }

    let cursor_dir = root.join(".cursor");
    if has(AgentTarget::Cursor) || (is_auto && cursor_dir.is_dir()) {
        let p = cursor_dir.join("rules").join("git-drafts.mdc");
        let content = format!(
            "---\ndescription: Multi-phase layered Git staging workflow (git-drafts / gd)\nalwaysApply: true\n---\n\n{}\n",
            agent_snippet()
        );
        write_file_ensuring_parent(&p, &content)?;
        updated.push(".cursor/rules/git-drafts.mdc".to_string());
    }

    let vscode_dir = root.join(".vscode");
    if has(AgentTarget::Mcp) || (is_auto && vscode_dir.is_dir()) {
        let p = vscode_dir.join("mcp.json");
        upsert_vscode_mcp(&p, role, mcp_command.unwrap_or("gd"))?;
        updated.push(".vscode/mcp.json".to_string());
    }

    println!("configured agent instructions: {}", updated.join(", "));
    Ok(())
}

fn write_file_ensuring_parent(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))
}

fn upsert_marked_file(path: &Path, block: &str) -> Result<(), String> {
    if !path.exists() {
        return write_file_ensuring_parent(path, &format!("{block}\n"));
    }
    let existing = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let next = if let (Some(start), Some(end)) =
        (existing.find(MARKER_START), existing.find(MARKER_END))
    {
        if end >= start {
            let after_end = end + MARKER_END.len();
            format!("{}{}{}", &existing[..start], block, &existing[after_end..])
        } else {
            append_block(&existing, block)
        }
    } else {
        append_block(&existing, block)
    };
    fs::write(path, next).map_err(|e| format!("write {}: {e}", path.display()))
}

fn append_block(existing: &str, block: &str) -> String {
    if existing.trim().is_empty() {
        format!("{block}\n")
    } else if existing.ends_with('\n') {
        format!("{existing}\n{block}\n")
    } else {
        format!("{existing}\n\n{block}\n")
    }
}

fn upsert_vscode_mcp(path: &Path, role: AgentRole, command: &str) -> Result<(), String> {
    let mut root_entries = if path.exists() {
        let raw = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        match json::parse(&raw)? {
            Json::Obj(entries) => entries,
            _ => return Err("existing .vscode/mcp.json root must be a JSON object".to_string()),
        }
    } else {
        Vec::new()
    };

    let server_cfg = Json::obj(vec![
        ("type", Json::Str("stdio".to_string())),
        ("command", Json::Str(command.to_string())),
        (
            "args",
            Json::Arr(vec![
                Json::Str("mcp".to_string()),
                Json::Str("--role".to_string()),
                Json::Str(role.as_str().to_string()),
            ]),
        ),
    ]);

    if let Some((_, servers_val)) = root_entries.iter_mut().find(|(k, _)| k == "servers") {
        match servers_val {
            Json::Obj(ref mut map) => {
                if let Some((_, entry)) = map.iter_mut().find(|(k, _)| k == "git-drafts") {
                    *entry = server_cfg;
                } else {
                    map.push(("git-drafts".to_string(), server_cfg));
                }
            }
            _ => return Err(".vscode/mcp.json 'servers' must be a JSON object".to_string()),
        }
    } else {
        root_entries.push((
            "servers".to_string(),
            Json::Obj(vec![("git-drafts".to_string(), server_cfg)]),
        ));
    }

    let formatted = format_json_pretty(&Json::Obj(root_entries), 0);
    write_file_ensuring_parent(path, &format!("{formatted}\n"))
}

fn format_json_pretty(val: &Json, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    let inner_pad = "  ".repeat(indent + 1);
    match val {
        Json::Arr(items) if !items.is_empty() => {
            let parts: Vec<String> = items
                .iter()
                .map(|item| format!("{inner_pad}{}", format_json_pretty(item, indent + 1)))
                .collect();
            format!("[\n{}\n{pad}]", parts.join(",\n"))
        }
        Json::Obj(entries) if !entries.is_empty() => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| {
                    let mut key_esc = String::new();
                    json::write_escaped_str(k, &mut key_esc);
                    format!(
                        "{inner_pad}{key_esc}: {}",
                        format_json_pretty(v, indent + 1)
                    )
                })
                .collect();
            format!("{{\n{}\n{pad}}}", parts.join(",\n"))
        }
        _ => val.encode(),
    }
}
