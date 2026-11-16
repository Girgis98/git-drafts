pub mod agent;
pub mod git;
pub mod json;
pub mod mcp;
pub mod ops;
pub mod store;

use agent::AgentTarget;
use git::RepoCtx;
use mcp::AgentRole;
use ops::{DropOpts, InsertPos, RmMode};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Component, Path};
use store::LayerStore;

fn resolve_user_paths(repo: &RepoCtx, paths: &[String]) -> Vec<String> {
    if paths.is_empty() {
        return Vec::new();
    }
    let Ok(cwd) = env::current_dir() else {
        return paths.to_vec();
    };
    let Ok(cwd_canon) = fs::canonicalize(&cwd) else {
        return paths.to_vec();
    };
    let Ok(wt_canon) = fs::canonicalize(&repo.work_tree) else {
        return paths.to_vec();
    };
    let Ok(prefix) = cwd_canon.strip_prefix(&wt_canon) else {
        return paths.to_vec();
    };
    if prefix.as_os_str().is_empty() {
        return paths.to_vec();
    }
    paths
        .iter()
        .map(|p| {
            if p.starts_with(":(") || Path::new(p).is_absolute() {
                return p.clone();
            }
            normalize_rel_posix(&prefix.join(p))
        })
        .collect()
}

fn normalize_rel_posix(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => parts.push(s.to_string_lossy().to_string()),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

fn print_help() {
    println!(
        "\
git-drafts (gd) - multi-phase layered staging areas for Git

USAGE:
  gd init [--force] [--sync-git-index] [layers...]
                                           Initialize draft layers (default: ready reviewed draft)
  gd status | st [--json]                  Show layered status (L1..Ln + WT)
  gd diff [<layer>|wt] [--from A --to B] [-- <paths...>]
                                           Show diff of a layer or between two states
  gd stage [-l <layer>] [-p|-A] [<paths...>]
                                           Stage WT changes into <layer> (default: top layer)
  gd promote [--from L] [--to L] [-p|-A] [<paths...>]
                                           Move changes down toward HEAD (commit-ready)
  gd demote [--from L] [--to L|wt] [-p|-A] [<paths...>]
                                           Move changes up away from commit
  gd drop <layer> [-p|-A] [--cascade] [--hard] [<paths...>]
                                           Drop changes from <layer>
  gd commit [-p] [<paths...>] [git-commit-opts...]
                                           Commit [L1] ready (all or selected paths/hunks)
  gd layer list                            List ordered layers
  gd layer add <name> [--at N|--above L|--below L]
                                           Add a new revision layer
  gd layer rm <name> [--merge-up|--merge-down|--drop]
                                           Remove a layer
  gd layer rename <old> <new>              Rename a layer
  gd blob <layer|head|wt> <path> [--allow-missing]
                                           Output file bytes at a layer (for IDE diff providers)
  gd patch-apply --from <src> --to <dst>   Apply unified patch from stdin between layers
  gd config <key> [<value>]                Get or set repo draft config (sync-git-index)
  gd agent init [--target T] [--role R]    Write AI agent instructions, skill & MCP config
  gd agent snippet                         Print concise agent instruction markdown block
  gd mcp [--role coder|reviewer|lead]      Run stdio MCP server with role guardrails
  gd undo                                  Restore previous layer state
"
    );
}

fn split_double_dash(args: &[String]) -> (&[String], &[String]) {
    if let Some(pos) = args.iter().position(|a| a == "--") {
        (&args[..pos], &args[pos + 1..])
    } else {
        (args, &[])
    }
}

pub fn run() -> Result<(), String> {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    if raw_args.is_empty() || matches!(raw_args[0].as_str(), "-h" | "--help" | "help") {
        print_help();
        return Ok(());
    }
    if matches!(raw_args[0].as_str(), "-V" | "--version") {
        println!("git-drafts {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let cmd = raw_args[0].as_str();
    let rest = &raw_args[1..];

    if cmd == "mcp" {
        let mut role = AgentRole::Lead;
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--role" => {
                    i += 1;
                    let v = rest.get(i).ok_or("--role requires a value")?;
                    role = AgentRole::parse(v)?;
                }
                "-h" | "--help" => {
                    println!("USAGE: gd mcp [--role coder|reviewer|lead]");
                    return Ok(());
                }
                other => return Err(format!("unknown flag '{other}'")),
            }
            i += 1;
        }
        return mcp::serve(role);
    }

    if cmd == "agent" && rest.first().map(String::as_str) == Some("snippet") {
        agent::cmd_agent_snippet();
        return Ok(());
    }

    let repo = RepoCtx::discover(None)?;

    match cmd {
        "init" => {
            let mut force = false;
            let mut sync_git_index = false;
            let mut names = Vec::new();
            for a in rest {
                match a.as_str() {
                    "-f" | "--force" => force = true,
                    "--sync-git-index" => sync_git_index = true,
                    "-h" | "--help" => {
                        println!("USAGE: gd init [--force] [--sync-git-index] [layer1 layer2 ...]");
                        return Ok(());
                    }
                    s if s.starts_with('-') => return Err(format!("unknown flag '{s}'")),
                    _ => names.push(a.clone()),
                }
            }
            let store = LayerStore::init_with_opts(&repo, &names, force, sync_git_index)?;
            println!("initialized layers: {}", store.manifest.layers.join(" < "));
            Ok(())
        }
        "status" | "st" => {
            let mut json = false;
            for a in rest {
                match a.as_str() {
                    "--json" => json = true,
                    s => return Err(format!("unknown argument '{s}'")),
                }
            }
            let (store, synced) = LayerStore::load_and_sync(&repo)?;
            if json {
                ops::cmd_status_json(&repo, &store, synced)
            } else {
                ops::cmd_status(&repo, &store, synced)
            }
        }
        "diff" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let (flags, tail_paths) = split_double_dash(rest);
            let mut from: Option<String> = None;
            let mut to: Option<String> = None;
            let mut pos = Vec::new();
            let mut i = 0;
            while i < flags.len() {
                match flags[i].as_str() {
                    "--from" => {
                        i += 1;
                        from = Some(flags.get(i).ok_or("--from requires a value")?.clone());
                    }
                    "--to" => {
                        i += 1;
                        to = Some(flags.get(i).ok_or("--to requires a value")?.clone());
                    }
                    s if s.starts_with('-') => return Err(format!("unknown flag '{s}'")),
                    s => pos.push(s.to_string()),
                }
                i += 1;
            }
            let mut layer: Option<String> = None;
            let mut paths = Vec::new();
            if from.is_none() && to.is_none() && !pos.is_empty() {
                let first = pos[0].clone();
                let looks_like_layer =
                    first.eq_ignore_ascii_case("wt") || store.resolve_layer(&first).is_ok();
                if looks_like_layer
                    && !Path::new(&first).exists()
                    && !repo.work_tree.join(&first).exists()
                {
                    layer = Some(first);
                    paths.extend(pos.into_iter().skip(1));
                } else {
                    paths.extend(pos);
                }
            } else {
                paths.extend(pos);
            }
            paths.extend(tail_paths.iter().cloned());
            let paths = resolve_user_paths(&repo, &paths);
            ops::cmd_diff(
                &repo,
                &store,
                layer.as_deref(),
                from.as_deref(),
                to.as_deref(),
                &paths,
            )
        }
        "stage" | "add" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let (flags, tail_paths) = split_double_dash(rest);
            let mut layer: Option<String> = None;
            let mut interactive = false;
            let mut all = false;
            let mut pos = Vec::new();
            let mut i = 0;
            while i < flags.len() {
                match flags[i].as_str() {
                    "-l" | "--layer" => {
                        i += 1;
                        layer = Some(flags.get(i).ok_or("--layer requires a value")?.clone());
                    }
                    "-p" | "--patch" => interactive = true,
                    "-A" | "--all" => all = true,
                    s if s.starts_with('-') => return Err(format!("unknown flag '{s}'")),
                    s => pos.push(s.to_string()),
                }
                i += 1;
            }
            let mut paths = Vec::new();
            if layer.is_none() && !pos.is_empty() {
                let first = pos[0].clone();
                let is_layer = store.resolve_layer(&first).is_ok();
                let explicit_path_follows =
                    pos.len() > 1 || !tail_paths.is_empty() || all || interactive;
                if is_layer
                    && explicit_path_follows
                    && !Path::new(&first).exists()
                    && !repo.work_tree.join(&first).exists()
                {
                    layer = Some(first);
                    paths.extend(pos.into_iter().skip(1));
                } else {
                    paths.extend(pos);
                }
            } else {
                paths.extend(pos);
            }
            paths.extend(tail_paths.iter().cloned());
            let paths = resolve_user_paths(&repo, &paths);
            ops::cmd_stage(&repo, &store, layer.as_deref(), &paths, interactive, all)
        }
        "promote" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let (flags, tail_paths) = split_double_dash(rest);
            let mut from: Option<String> = None;
            let mut to: Option<String> = None;
            let mut interactive = false;
            let mut all = false;
            let mut paths = Vec::new();
            let mut i = 0;
            while i < flags.len() {
                match flags[i].as_str() {
                    "--from" => {
                        i += 1;
                        from = Some(flags.get(i).ok_or("--from requires a value")?.clone());
                    }
                    "--to" => {
                        i += 1;
                        to = Some(flags.get(i).ok_or("--to requires a value")?.clone());
                    }
                    "-p" | "--patch" => interactive = true,
                    "-A" | "--all" => all = true,
                    s if s.starts_with('-') => return Err(format!("unknown flag '{s}'")),
                    s => paths.push(s.to_string()),
                }
                i += 1;
            }
            paths.extend(tail_paths.iter().cloned());
            let paths = resolve_user_paths(&repo, &paths);
            ops::cmd_promote(
                &repo,
                &store,
                from.as_deref(),
                to.as_deref(),
                &paths,
                interactive,
                all,
            )
        }
        "demote" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let (flags, tail_paths) = split_double_dash(rest);
            let mut from: Option<String> = None;
            let mut to: Option<String> = None;
            let mut interactive = false;
            let mut all = false;
            let mut paths = Vec::new();
            let mut i = 0;
            while i < flags.len() {
                match flags[i].as_str() {
                    "--from" => {
                        i += 1;
                        from = Some(flags.get(i).ok_or("--from requires a value")?.clone());
                    }
                    "--to" => {
                        i += 1;
                        to = Some(flags.get(i).ok_or("--to requires a value")?.clone());
                    }
                    "-p" | "--patch" => interactive = true,
                    "-A" | "--all" => all = true,
                    s if s.starts_with('-') => return Err(format!("unknown flag '{s}'")),
                    s => paths.push(s.to_string()),
                }
                i += 1;
            }
            paths.extend(tail_paths.iter().cloned());
            let paths = resolve_user_paths(&repo, &paths);
            ops::cmd_demote(
                &repo,
                &store,
                from.as_deref(),
                to.as_deref(),
                &paths,
                interactive,
                all,
            )
        }
        "drop" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let (flags, tail_paths) = split_double_dash(rest);
            let mut layer: Option<String> = None;
            let mut interactive = false;
            let mut all = false;
            let mut cascade = false;
            let mut hard = false;
            let mut paths = Vec::new();
            let mut i = 0;
            while i < flags.len() {
                match flags[i].as_str() {
                    "-l" | "--layer" => {
                        i += 1;
                        layer = Some(flags.get(i).ok_or("--layer requires a value")?.clone());
                    }
                    "-p" | "--patch" => interactive = true,
                    "-A" | "--all" => all = true,
                    "--cascade" => cascade = true,
                    "--hard" => hard = true,
                    s if s.starts_with('-') => return Err(format!("unknown flag '{s}'")),
                    s => {
                        if layer.is_none() {
                            layer = Some(s.to_string());
                        } else {
                            paths.push(s.to_string());
                        }
                    }
                }
                i += 1;
            }
            paths.extend(tail_paths.iter().cloned());
            let paths = resolve_user_paths(&repo, &paths);
            let layer_spec = layer.ok_or("drop requires <layer>")?;
            ops::cmd_drop(
                &repo,
                &store,
                DropOpts {
                    layer: &layer_spec,
                    paths: &paths,
                    interactive,
                    all,
                    cascade,
                    hard,
                },
            )
        }
        "commit" => {
            let (mut store, _) = LayerStore::load_and_sync(&repo)?;
            let (flags, tail_paths) = split_double_dash(rest);
            let mut interactive = false;
            let mut paths = Vec::new();
            let mut git_args = Vec::new();
            let mut i = 0;
            while i < flags.len() {
                let a = &flags[i];
                match a.as_str() {
                    "-p" | "--patch" => interactive = true,
                    "--only" => {
                        i += 1;
                        paths.push(flags.get(i).ok_or("--only requires a path")?.clone());
                    }
                    "-m" | "--message" | "-F" | "--file" | "-C" | "--reuse-message" | "-c"
                    | "--reedit-message" | "--fixup" | "--squash" | "--author" | "--date"
                    | "-t" | "--template" | "--cleanup" | "--trailer" => {
                        git_args.push(a.clone());
                        i += 1;
                        let val = flags
                            .get(i)
                            .ok_or_else(|| format!("{a} requires a value"))?;
                        git_args.push(val.clone());
                    }
                    s if s.starts_with('-') => git_args.push(a.clone()),
                    s => paths.push(s.to_string()),
                }
                i += 1;
            }
            paths.extend(tail_paths.iter().cloned());
            let paths = resolve_user_paths(&repo, &paths);
            ops::cmd_commit(&repo, &mut store, &paths, interactive, &git_args)
        }
        "layer" => {
            let (mut store, _) = LayerStore::load_and_sync(&repo)?;
            let sub = rest.first().map(String::as_str).unwrap_or("list");
            let sub_rest = if rest.is_empty() { &[] } else { &rest[1..] };
            match sub {
                "list" | "ls" => ops::cmd_layer_list(&repo, &store),
                "add" => {
                    let mut name = None;
                    let mut pos = InsertPos::Top;
                    let mut i = 0;
                    while i < sub_rest.len() {
                        match sub_rest[i].as_str() {
                            "--at" => {
                                i += 1;
                                let v = sub_rest.get(i).ok_or("--at requires a number")?;
                                let n: usize =
                                    v.parse().map_err(|_| format!("invalid number '{v}'"))?;
                                pos = InsertPos::At(n);
                            }
                            "--above" => {
                                i += 1;
                                let v = sub_rest.get(i).ok_or("--above requires a layer")?;
                                pos = InsertPos::Above(v.clone());
                            }
                            "--below" => {
                                i += 1;
                                let v = sub_rest.get(i).ok_or("--below requires a layer")?;
                                pos = InsertPos::Below(v.clone());
                            }
                            s if s.starts_with('-') => return Err(format!("unknown flag '{s}'")),
                            s => {
                                if name.is_none() {
                                    name = Some(s.to_string());
                                } else {
                                    return Err(format!("unexpected argument '{s}'"));
                                }
                            }
                        }
                        i += 1;
                    }
                    let name = name.ok_or("layer add requires <name>")?;
                    ops::cmd_layer_add(&repo, &mut store, &name, pos)
                }
                "rm" | "remove" => {
                    let mut target = None;
                    let mut mode = RmMode::RequireEmpty;
                    for a in sub_rest {
                        match a.as_str() {
                            "--merge-up" => mode = RmMode::MergeUp,
                            "--merge-down" => mode = RmMode::MergeDown,
                            "--drop" => mode = RmMode::Drop,
                            s if s.starts_with('-') => return Err(format!("unknown flag '{s}'")),
                            s => {
                                if target.is_none() {
                                    target = Some(s.to_string());
                                } else {
                                    return Err(format!("unexpected argument '{s}'"));
                                }
                            }
                        }
                    }
                    let target = target.ok_or("layer rm requires <name>")?;
                    ops::cmd_layer_rm(&repo, &mut store, &target, mode)
                }
                "rename" | "mv" => {
                    if sub_rest.len() != 2 {
                        return Err("USAGE: gd layer rename <old> <new>".to_string());
                    }
                    ops::cmd_layer_rename(&repo, &mut store, &sub_rest[0], &sub_rest[1])
                }
                other => Err(format!("unknown layer subcommand '{other}'")),
            }
        }
        "blob" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let mut allow_missing = false;
            let mut pos = Vec::new();
            for a in rest {
                match a.as_str() {
                    "--allow-missing" => allow_missing = true,
                    s if s.starts_with('-') => return Err(format!("unknown flag '{s}'")),
                    s => pos.push(s.to_string()),
                }
            }
            if pos.len() != 2 {
                return Err("USAGE: gd blob <layer|head|wt> <path> [--allow-missing]".to_string());
            }
            let resolved = resolve_user_paths(&repo, &pos[1..2]);
            ops::cmd_blob(&repo, &store, &pos[0], &resolved[0], allow_missing)
        }
        "patch-apply" => {
            let (store, _) = LayerStore::load_and_sync(&repo)?;
            let mut from: Option<String> = None;
            let mut to: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--from" => {
                        i += 1;
                        from = Some(rest.get(i).ok_or("--from requires a value")?.clone());
                    }
                    "--to" => {
                        i += 1;
                        to = Some(rest.get(i).ok_or("--to requires a value")?.clone());
                    }
                    s => return Err(format!("unknown argument '{s}'")),
                }
                i += 1;
            }
            let from = from.ok_or("patch-apply requires --from <src>")?;
            let to = to.ok_or("patch-apply requires --to <dst>")?;
            let mut patch = Vec::new();
            std::io::stdin()
                .read_to_end(&mut patch)
                .map_err(|e| format!("read patch stdin: {e}"))?;
            ops::cmd_patch_apply(&repo, &store, &from, &to, &patch)
        }
        "config" => {
            let (mut store, _) = LayerStore::load_and_sync(&repo)?;
            if rest.is_empty() || rest.len() > 2 {
                return Err("USAGE: gd config <key> [<value>]".to_string());
            }
            ops::cmd_config(&repo, &mut store, &rest[0], rest.get(1).map(String::as_str))
        }
        "agent" => {
            let sub = rest.first().map(String::as_str).unwrap_or("init");
            let sub_rest = if rest.is_empty() { &[] } else { &rest[1..] };
            match sub {
                "init" => {
                    let mut targets = Vec::new();
                    let mut role = AgentRole::Lead;
                    let mut i = 0;
                    while i < sub_rest.len() {
                        match sub_rest[i].as_str() {
                            "--target" | "-t" => {
                                i += 1;
                                let spec = sub_rest.get(i).ok_or("--target requires a value")?;
                                targets.extend(AgentTarget::parse_list(spec)?);
                            }
                            "--role" => {
                                i += 1;
                                let r = sub_rest.get(i).ok_or("--role requires a value")?;
                                role = AgentRole::parse(r)?;
                            }
                            "-h" | "--help" => {
                                println!("USAGE: gd agent init [--target auto|all|agents|claude|cursor|copilot|gemini|skill|mcp] [--role coder|reviewer|lead]");
                                return Ok(());
                            }
                            other => return Err(format!("unknown flag '{other}'")),
                        }
                        i += 1;
                    }
                    agent::cmd_agent_init(&repo, &targets, role)
                }
                "snippet" => {
                    agent::cmd_agent_snippet();
                    Ok(())
                }
                other => Err(format!("unknown agent subcommand '{other}'")),
            }
        }
        "undo" => {
            let mut store = LayerStore::load(&repo)?;
            store.pop_undo(&repo)?;
            println!("restored previous layer state");
            Ok(())
        }
        other => Err(format!("unknown command '{other}' (run `gd --help`)")),
    }
}
