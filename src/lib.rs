pub mod git;
pub mod ops;
pub mod store;

use git::RepoCtx;
use ops::{DropOpts, InsertPos, RmMode};
use std::env;
use std::io::Read;
use store::LayerStore;

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

    let repo = RepoCtx::discover(None)?;
    let cmd = raw_args[0].as_str();
    let rest = &raw_args[1..];

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
                let looks_like_layer = first.eq_ignore_ascii_case("wt")
                    || store.resolve_layer(&first).is_ok();
                if looks_like_layer && !repo.work_tree.join(&first).exists() {
                    layer = Some(first);
                    paths.extend(pos.into_iter().skip(1));
                } else {
                    paths.extend(pos);
                }
            } else {
                paths.extend(pos);
            }
            paths.extend(tail_paths.iter().cloned());
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
                if is_layer && explicit_path_follows && !repo.work_tree.join(&first).exists() {
                    layer = Some(first);
                    paths.extend(pos.into_iter().skip(1));
                } else {
                    paths.extend(pos);
                }
            } else {
                paths.extend(pos);
            }
            paths.extend(tail_paths.iter().cloned());
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
                        let val = flags.get(i).ok_or_else(|| format!("{a} requires a value"))?;
                        git_args.push(val.clone());
                    }
                    s if s.starts_with('-') => git_args.push(a.clone()),
                    s => paths.push(s.to_string()),
                }
                i += 1;
            }
            paths.extend(tail_paths.iter().cloned());
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
            ops::cmd_blob(&repo, &store, &pos[0], &pos[1], allow_missing)
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
        "undo" => {
            let mut store = LayerStore::load(&repo)?;
            store.pop_undo(&repo)?;
            println!("restored previous layer state");
            Ok(())
        }
        other => Err(format!("unknown command '{other}' (run `gd --help`)")),
    }
}
