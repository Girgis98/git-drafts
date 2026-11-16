use crate::git::RepoCtx;
use crate::json::{self, Json};
use crate::ops::{self, DropOpts, InsertPos, RmMode};
use crate::store::LayerStore;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentRole {
    Coder,
    Reviewer,
    Lead,
}

impl AgentRole {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "coder" => Ok(Self::Coder),
            "reviewer" => Ok(Self::Reviewer),
            "lead" | "committer" => Ok(Self::Lead),
            other => Err(format!(
                "unknown role '{other}' (expected coder, reviewer, or lead)"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coder => "coder",
            Self::Reviewer => "reviewer",
            Self::Lead => "lead",
        }
    }

    fn allows_tool(self, name: &str) -> bool {
        match self {
            Self::Lead => true,
            Self::Reviewer => matches!(
                name,
                "draft_status"
                    | "draft_diff"
                    | "draft_blob"
                    | "draft_stage"
                    | "draft_promote"
                    | "draft_demote"
                    | "draft_drop"
                    | "draft_patch_apply"
                    | "draft_undo"
            ),
            Self::Coder => matches!(
                name,
                "draft_status"
                    | "draft_diff"
                    | "draft_blob"
                    | "draft_stage"
                    | "draft_demote"
                    | "draft_drop"
                    | "draft_undo"
            ),
        }
    }

    fn instructions(self) -> &'static str {
        match self {
            Self::Coder => {
                "Role: CODER. 1) Inspect via draft_status/draft_diff. 2) Stage edits into the top 'draft' layer only via draft_stage (or unstage to WT via draft_demote). Promote and commit are blocked."
            }
            Self::Reviewer => {
                "Role: REVIEWER. 1) Inspect drafts via draft_status/draft_diff/draft_blob. 2) Stage review fixes into top 'draft' layer via draft_stage. 3) Promote vetted files/hunks via draft_promote/draft_patch_apply, or push back via draft_demote/draft_drop. Commit is blocked."
            }
            Self::Lead => {
                "Role: LEAD. Full layered staging access: init, stage, promote toward [L1] ready, demote, drop, patch_apply, manage layers, and commit verified work via draft_commit."
            }
        }
    }
}

pub fn serve(role: AgentRole) -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    while let Some(raw) = read_message(&mut reader)? {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req = match json::parse(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let err_resp = jsonrpc_error(Json::Null, -32700, &format!("parse error: {e}"));
                writeln!(writer, "{}", err_resp.encode()).map_err(|w| w.to_string())?;
                writer.flush().map_err(|w| w.to_string())?;
                continue;
            }
        };

        if let Some(resp) = handle_request(&req, role) {
            writeln!(writer, "{}", resp.encode()).map_err(|w| w.to_string())?;
            writer.flush().map_err(|w| w.to_string())?;
        }
    }
    Ok(())
}

fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<String>, String> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("stdin read: {e}"))?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(len_str) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            let content_len: usize = len_str
                .trim()
                .parse()
                .map_err(|_| format!("invalid Content-Length: '{trimmed}'"))?;
            loop {
                line.clear();
                let h = reader
                    .read_line(&mut line)
                    .map_err(|e| format!("header read: {e}"))?;
                if h == 0 || line.trim().is_empty() {
                    break;
                }
            }
            let mut buf = vec![0u8; content_len];
            reader
                .read_exact(&mut buf)
                .map_err(|e| format!("body read: {e}"))?;
            let s = String::from_utf8(buf).map_err(|_| "non-UTF8 MCP body".to_string())?;
            return Ok(Some(s));
        }
        return Ok(Some(line));
    }
}

pub fn handle_request(req: &Json, role: AgentRole) -> Option<Json> {
    let id = req.get("id").cloned();
    let method = req.get_str("method").unwrap_or("");
    let params = req.get("params");

    // Notifications have no id
    if id.is_none() || matches!(id, Some(Json::Null)) && method.starts_with("notifications/") {
        return None;
    }
    let id = id.unwrap_or(Json::Null);

    let resp = match method {
        "initialize" => {
            let proto = params
                .and_then(|p| p.get_str("protocolVersion"))
                .unwrap_or("2024-11-05");
            jsonrpc_ok(
                id,
                Json::obj(vec![
                    ("protocolVersion", Json::Str(proto.to_string())),
                    (
                        "capabilities",
                        Json::obj(vec![("tools", Json::obj(vec![]))]),
                    ),
                    (
                        "serverInfo",
                        Json::obj(vec![
                            ("name", Json::Str("git-drafts".to_string())),
                            ("version", Json::Str(env!("CARGO_PKG_VERSION").to_string())),
                        ]),
                    ),
                    ("instructions", Json::Str(role.instructions().to_string())),
                ]),
            )
        }
        "ping" => jsonrpc_ok(id, Json::obj(vec![])),
        "tools/list" => jsonrpc_ok(id, Json::obj(vec![("tools", tool_list_json(role))])),
        "tools/call" => {
            let Some(p) = params else {
                return Some(jsonrpc_error(id, -32602, "missing params"));
            };
            let Some(name) = p.get_str("name") else {
                return Some(jsonrpc_error(id, -32602, "missing tool name"));
            };
            let args = p
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| Json::obj(vec![]));
            let result = match call_tool(role, name, &args) {
                Ok(text) => Json::obj(vec![
                    (
                        "content",
                        Json::Arr(vec![Json::obj(vec![
                            ("type", Json::Str("text".to_string())),
                            ("text", Json::Str(text)),
                        ])]),
                    ),
                    ("isError", Json::Bool(false)),
                ]),
                Err(err) => Json::obj(vec![
                    (
                        "content",
                        Json::Arr(vec![Json::obj(vec![
                            ("type", Json::Str("text".to_string())),
                            ("text", Json::Str(err)),
                        ])]),
                    ),
                    ("isError", Json::Bool(true)),
                ]),
            };
            jsonrpc_ok(id, result)
        }
        other => jsonrpc_error(id, -32601, &format!("method not found: {other}")),
    };
    Some(resp)
}

fn jsonrpc_ok(id: Json, result: Json) -> Json {
    Json::obj(vec![
        ("jsonrpc", Json::Str("2.0".to_string())),
        ("id", id),
        ("result", result),
    ])
}

fn jsonrpc_error(id: Json, code: i32, message: &str) -> Json {
    Json::obj(vec![
        ("jsonrpc", Json::Str("2.0".to_string())),
        ("id", id),
        (
            "error",
            Json::obj(vec![
                ("code", Json::Num(f64::from(code))),
                ("message", Json::Str(message.to_string())),
            ]),
        ),
    ])
}

fn call_tool(role: AgentRole, name: &str, args: &Json) -> Result<String, String> {
    if !role.allows_tool(name) {
        return Err(format!(
            "tool '{name}' is not permitted for role '{}'",
            role.as_str()
        ));
    }

    let repo_dir = args.get_str("repo_path").map(Path::new);
    let repo = RepoCtx::discover(repo_dir)?;

    match name {
        "draft_init" => {
            let layers = args.get_string_vec("layers")?;
            let force = args.get_bool("force").unwrap_or(false);
            let sync = args.get_bool("sync_git_index").unwrap_or(false);
            let store = LayerStore::init_with_opts(&repo, &layers, force, sync)?;
            Ok(format!(
                "initialized layers: {}",
                store.manifest.layers.join(" < ")
            ))
        }
        "draft_status" => {
            let (store, synced) = LayerStore::load_and_sync(&repo)?;
            ops::build_status_json(&repo, &store, synced)
        }
        "draft_diff" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let layer = args.get_str("layer");
            let from = args.get_str("from");
            let to = args.get_str("to");
            let paths = args.get_string_vec("paths")?;
            let bytes = ops::build_diff_bytes(&repo, &store, layer, from, to, &paths, false)?;
            let text = String::from_utf8_lossy(&bytes).to_string();
            if text.trim().is_empty() {
                Ok("(no differences)".to_string())
            } else {
                Ok(text)
            }
        }
        "draft_blob" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let spec = args.get_str("spec").ok_or("draft_blob requires 'spec'")?;
            let path = args.get_str("path").ok_or("draft_blob requires 'path'")?;
            let allow_missing = args.get_bool("allow_missing").unwrap_or(false);
            let bytes = ops::read_blob_bytes(&repo, &store, spec, path, allow_missing)?;
            Ok(String::from_utf8_lossy(&bytes).to_string())
        }
        "draft_stage" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let requested_layer = args.get_str("layer");
            if role != AgentRole::Lead {
                if let Some(l) = requested_layer {
                    let top = store.manifest.layers.last().map(String::as_str);
                    if Some(l) != top && l != format!("L{}", store.manifest.layers.len()) {
                        return Err(format!(
                            "{} role may only stage directly into top layer '{}' (use draft_promote to move changes toward ready)",
                            role.as_str(),
                            top.unwrap_or("draft")
                        ));
                    }
                }
            }
            let paths = args.get_string_vec("paths")?;
            let all = args.get_bool("all").unwrap_or(false);
            ops::op_stage(&repo, &store, requested_layer, &paths, false, all)
        }
        "draft_promote" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let from = args.get_str("from");
            let to = args.get_str("to");
            let paths = args.get_string_vec("paths")?;
            let all = args.get_bool("all").unwrap_or(false);
            ops::op_promote(&repo, &store, from, to, &paths, false, all)
        }
        "draft_demote" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let mut from = args.get_str("from").map(str::to_string);
            let mut to = args.get_str("to").map(str::to_string);
            if role == AgentRole::Coder {
                let top = store.manifest.layers.last().cloned().ok_or("no layers")?;
                if let Some(ref f) = from {
                    if f != &top && f != &format!("L{}", store.manifest.layers.len()) {
                        return Err(format!("coder role may only demote from top layer '{top}'"));
                    }
                } else {
                    from = Some(top);
                }
                to = Some("wt".to_string());
            }
            let paths = args.get_string_vec("paths")?;
            let all = args.get_bool("all").unwrap_or(false);
            ops::op_demote(
                &repo,
                &store,
                from.as_deref(),
                to.as_deref(),
                &paths,
                false,
                all,
            )
        }
        "draft_drop" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let layer = args.get_str("layer").ok_or("draft_drop requires 'layer'")?;
            if role == AgentRole::Coder {
                let top = store.manifest.layers.last().map(String::as_str);
                if Some(layer) != top && layer != format!("L{}", store.manifest.layers.len()) {
                    return Err(format!(
                        "coder role may only drop from top layer '{}'",
                        top.unwrap_or("draft")
                    ));
                }
            }
            let paths = args.get_string_vec("paths")?;
            let all = args.get_bool("all").unwrap_or(false);
            let cascade = args.get_bool("cascade").unwrap_or(false);
            let hard = args.get_bool("hard").unwrap_or(false);
            ops::op_drop(
                &repo,
                &store,
                DropOpts {
                    layer,
                    paths: &paths,
                    interactive: false,
                    all,
                    cascade,
                    hard,
                },
            )
        }
        "draft_patch_apply" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let from = args
                .get_str("from")
                .ok_or("draft_patch_apply requires 'from'")?;
            let to = args
                .get_str("to")
                .ok_or("draft_patch_apply requires 'to'")?;
            let patch = args
                .get_str("patch")
                .ok_or("draft_patch_apply requires 'patch'")?;
            ops::cmd_patch_apply(&repo, &store, from, to, patch.as_bytes())?;
            Ok(format!("applied patch {from} -> {to}"))
        }
        "draft_commit" => {
            let (mut store, _) = LayerStore::load_and_sync(&repo)?;
            let msg = args
                .get_str("message")
                .filter(|s| !s.trim().is_empty())
                .ok_or("draft_commit requires non-empty 'message'")?;
            let paths = args.get_string_vec("paths")?;
            let mut git_args = vec!["-m".to_string(), msg.to_string()];
            if args.get_bool("amend").unwrap_or(false) {
                git_args.push("--amend".to_string());
            }
            if args.get_bool("allow_empty").unwrap_or(false) {
                git_args.push("--allow-empty".to_string());
            }
            let out = ops::op_commit(&repo, &mut store, &paths, false, &git_args, true)?;
            Ok(if out.is_empty() {
                "committed ready layer".to_string()
            } else {
                out
            })
        }
        "draft_layer_manage" => {
            let (mut store, _) = LayerStore::load_and_sync(&repo)?;
            let action = args
                .get_str("action")
                .ok_or("draft_layer_manage requires 'action'")?;
            match action {
                "list" => ops::format_layer_list(&repo, &store),
                "add" => {
                    let layer_name = args.get_str("name").ok_or("action 'add' requires 'name'")?;
                    let pos = if let Some(above) = args.get_str("above") {
                        InsertPos::Above(above.to_string())
                    } else if let Some(below) = args.get_str("below") {
                        InsertPos::Below(below.to_string())
                    } else if let Some(Json::Num(n)) = args.get("at") {
                        InsertPos::At(*n as usize)
                    } else {
                        InsertPos::Top
                    };
                    ops::op_layer_add(&repo, &mut store, layer_name, pos)
                }
                "rm" => {
                    let layer_name = args.get_str("name").ok_or("action 'rm' requires 'name'")?;
                    let mode = match args.get_str("mode").unwrap_or("require-empty") {
                        "merge-up" => RmMode::MergeUp,
                        "merge-down" => RmMode::MergeDown,
                        "drop" => RmMode::Drop,
                        _ => RmMode::RequireEmpty,
                    };
                    ops::op_layer_rm(&repo, &mut store, layer_name, mode)
                }
                "rename" => {
                    let old = args
                        .get_str("name")
                        .ok_or("action 'rename' requires 'name'")?;
                    let new_name = args
                        .get_str("new_name")
                        .ok_or("action 'rename' requires 'new_name'")?;
                    ops::op_layer_rename(&repo, &mut store, old, new_name)
                }
                other => Err(format!("unknown layer action '{other}'")),
            }
        }
        "draft_undo" => {
            let mut store = LayerStore::load(&repo)?;
            store.pop_undo(&repo)?;
            Ok("restored previous layer state".to_string())
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

fn schema_prop(ty: &str, desc: &str) -> Json {
    Json::obj(vec![
        ("type", Json::Str(ty.to_string())),
        ("description", Json::Str(desc.to_string())),
    ])
}

fn schema_str_array(desc: &str) -> Json {
    Json::obj(vec![
        ("type", Json::Str("array".to_string())),
        (
            "items",
            Json::obj(vec![("type", Json::Str("string".to_string()))]),
        ),
        ("description", Json::Str(desc.to_string())),
    ])
}

fn tool_def(
    name: &str,
    desc: &str,
    read_only: bool,
    destructive: bool,
    props: Vec<(&str, Json)>,
    required: Vec<&str>,
) -> Json {
    let mut all_props = vec![(
        "repo_path",
        schema_prop("string", "Optional repository path (defaults to cwd)"),
    )];
    all_props.extend(props);
    let req_json: Vec<Json> = required
        .into_iter()
        .map(|s| Json::Str(s.to_string()))
        .collect();
    Json::obj(vec![
        ("name", Json::Str(name.to_string())),
        ("description", Json::Str(desc.to_string())),
        (
            "annotations",
            Json::obj(vec![
                ("readOnlyHint", Json::Bool(read_only)),
                ("destructiveHint", Json::Bool(destructive)),
                ("idempotentHint", Json::Bool(read_only)),
            ]),
        ),
        (
            "inputSchema",
            Json::obj(vec![
                ("type", Json::Str("object".to_string())),
                ("properties", Json::obj(all_props)),
                ("required", Json::Arr(req_json)),
            ]),
        ),
    ])
}

fn tool_list_json(role: AgentRole) -> Json {
    let defs = vec![
        tool_def(
            "draft_init",
            "Initialize layered staging phases in a Git repository.",
            false,
            false,
            vec![
                ("layers", schema_str_array("Ordered layers (default: ready, reviewed, draft)")),
                ("force", schema_prop("boolean", "Reinitialize if already initialized")),
                ("sync_git_index", schema_prop("boolean", "Mirror L1 ready into .git/index")),
            ],
            vec![],
        ),
        tool_def(
            "draft_status",
            "Get JSON status of all revision layers, tree OIDs, and working tree changes.",
            true,
            false,
            vec![],
            vec![],
        ),
        tool_def(
            "draft_diff",
            "Inspect unified diff for a specific layer, working tree ('wt'), or between --from and --to.",
            true,
            false,
            vec![
                ("layer", schema_prop("string", "Layer name/index or 'wt'")),
                ("from", schema_prop("string", "Source spec ('head' or layer)")),
                ("to", schema_prop("string", "Target spec (layer or 'wt')")),
                ("paths", schema_str_array("Optional file paths filter")),
            ],
            vec![],
        ),
        tool_def(
            "draft_blob",
            "Read raw file content from 'head', 'wt', or any draft layer.",
            true,
            false,
            vec![
                ("spec", schema_prop("string", "Layer name, 'head', or 'wt'")),
                ("path", schema_prop("string", "Repository-relative file path")),
                ("allow_missing", schema_prop("boolean", "Return empty string if missing")),
            ],
            vec!["spec", "path"],
        ),
        tool_def(
            "draft_stage",
            "Stage working-tree edits into a draft layer (defaults to top layer; non-lead roles restricted to top layer).",
            false,
            false,
            vec![
                ("layer", schema_prop("string", "Target layer (defaults to top)")),
                ("paths", schema_str_array("File paths to stage")),
                ("all", schema_prop("boolean", "Stage all working-tree changes")),
            ],
            vec![],
        ),
        tool_def(
            "draft_promote",
            "Promote verified files toward commit-ready [L1] ready.",
            false,
            false,
            vec![
                ("from", schema_prop("string", "Source layer")),
                ("to", schema_prop("string", "Target layer (defaults to one closer to HEAD)")),
                ("paths", schema_str_array("File paths to promote")),
                ("all", schema_prop("boolean", "Promote all files in source layer")),
            ],
            vec![],
        ),
        tool_def(
            "draft_demote",
            "Push changes back away from commit (toward draft or working tree).",
            false,
            false,
            vec![
                ("from", schema_prop("string", "Source layer")),
                ("to", schema_prop("string", "Target layer or 'wt'")),
                ("paths", schema_str_array("File paths to demote")),
                ("all", schema_prop("boolean", "Demote all files in source layer")),
            ],
            vec![],
        ),
        tool_def(
            "draft_drop",
            "Discard changes from a layer.",
            false,
            true,
            vec![
                ("layer", schema_prop("string", "Target layer")),
                ("paths", schema_str_array("File paths to drop")),
                ("all", schema_prop("boolean", "Drop all changes in layer")),
                ("cascade", schema_prop("boolean", "Also remove from upper layers")),
                ("hard", schema_prop("boolean", "Also revert working tree (destructive)")),
            ],
            vec!["layer"],
        ),
        tool_def(
            "draft_patch_apply",
            "Apply a unified diff patch between two layers (for non-interactive hunk promotion/demotion).",
            false,
            false,
            vec![
                ("from", schema_prop("string", "Source layer or 'wt'")),
                ("to", schema_prop("string", "Target layer or 'wt'")),
                ("patch", schema_prop("string", "Unified diff text")),
            ],
            vec!["from", "to", "patch"],
        ),
        tool_def(
            "draft_commit",
            "Commit [L1] ready changes, either all or selected paths.",
            false,
            true,
            vec![
                ("message", schema_prop("string", "Git commit message")),
                ("paths", schema_str_array("Optional subset of paths from L1 to commit")),
                ("amend", schema_prop("boolean", "Amend previous commit")),
                ("allow_empty", schema_prop("boolean", "Allow empty commit")),
            ],
            vec!["message"],
        ),
        tool_def(
            "draft_layer_manage",
            "List, add, remove, or rename revision layers.",
            false,
            true,
            vec![
                ("action", schema_prop("string", "'list', 'add', 'rm', or 'rename'")),
                ("name", schema_prop("string", "Layer name")),
                ("new_name", schema_prop("string", "New name (for 'rename')")),
                ("above", schema_prop("string", "Insert above this layer (for 'add')")),
                ("below", schema_prop("string", "Insert below this layer (for 'add')")),
                ("at", schema_prop("number", "1-based position (for 'add')")),
                ("mode", schema_prop("string", "For 'rm': 'merge-up', 'merge-down', or 'drop'")),
            ],
            vec!["action"],
        ),
        tool_def(
            "draft_undo",
            "Restore the previous layer state snapshot.",
            false,
            false,
            vec![],
            vec![],
        ),
    ];

    let filtered: Vec<Json> = defs
        .into_iter()
        .filter(|d| d.get_str("name").is_some_and(|n| role.allows_tool(n)))
        .collect();
    Json::Arr(filtered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_tool_filtering_and_guardrails() {
        let coder_tools = tool_list_json(AgentRole::Coder);
        let coder_arr = coder_tools.as_arr().unwrap();
        let names: Vec<_> = coder_arr.iter().filter_map(|t| t.get_str("name")).collect();
        assert!(names.contains(&"draft_stage"));
        assert!(!names.contains(&"draft_promote"));
        assert!(!names.contains(&"draft_commit"));

        // Read-only annotations present on status tool
        let status_def = coder_arr
            .iter()
            .find(|t| t.get_str("name") == Some("draft_status"))
            .unwrap();
        assert_eq!(
            status_def
                .get("annotations")
                .and_then(|a| a.get_bool("readOnlyHint")),
            Some(true)
        );

        let rev_tools = tool_list_json(AgentRole::Reviewer);
        let rev_names: Vec<_> = rev_tools
            .as_arr()
            .unwrap()
            .iter()
            .filter_map(|t| t.get_str("name"))
            .collect();
        assert!(rev_names.contains(&"draft_stage"));
        assert!(rev_names.contains(&"draft_promote"));
        assert!(!rev_names.contains(&"draft_commit"));

        let call = json::parse(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"draft_commit","arguments":{"message":"nope"}}}"#,
        )
        .unwrap();
        let resp = handle_request(&call, AgentRole::Coder).unwrap();
        let is_err = resp
            .get("result")
            .and_then(|r| r.get_bool("isError"))
            .unwrap();
        assert!(is_err);
    }
}
