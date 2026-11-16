use crate::git::RepoCtx;
use crate::store::{validate_layer_name, LayerStore};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::thread;

fn is_tty() -> bool {
    io::stdout().is_terminal()
}

fn c_bold(s: &str, tty: bool) -> String {
    if tty {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn c_dim(s: &str, tty: bool) -> String {
    if tty {
        format!("\x1b[2m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn c_status(code: &str, tty: bool) -> String {
    if !tty {
        return code.to_string();
    }
    let color = match code.chars().next().unwrap_or('?') {
        'A' => "32",
        'M' => "33",
        'D' => "31",
        '?' => "36",
        _ => "35",
    };
    format!("\x1b[{color}m{code}\x1b[0m")
}

/// Returns [T0 (HEAD), T1, ..., Tn].
pub fn compute_trees(repo: &RepoCtx, store: &LayerStore) -> Result<Vec<String>, String> {
    let (head_res, layer_trees) = thread::scope(|s| {
        let head_h = s.spawn(|| store.cached_head_tree(repo));
        let handles: Vec<_> = store
            .manifest
            .layers
            .iter()
            .map(|l| s.spawn(move || store.cached_tree_for_index(repo, l)))
            .collect();
        let head_res = head_h
            .join()
            .unwrap_or_else(|_| Err("head tree thread panicked".to_string()));
        let trees: Vec<Result<String, String>> = handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("worker thread panicked".to_string()))
            })
            .collect();
        (head_res, trees)
    });

    let mut out = Vec::with_capacity(1 + store.manifest.layers.len());
    out.push(head_res?);
    for res in layer_trees {
        out.push(res?);
    }
    Ok(out)
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct StatusSnapshot {
    trees: Vec<String>,
    layer_diffs: Vec<Vec<(String, String)>>,
    wt_modified: Vec<(String, String)>,
    wt_untracked: Vec<String>,
}

fn collect_status(repo: &RepoCtx, store: &LayerStore) -> Result<StatusSnapshot, String> {
    let trees = compute_trees(repo, store)?;
    let n = store.manifest.layers.len();
    let top_idx = store.idx_path(&store.manifest.layers[n - 1]);

    let (layer_diffs_res, wt_res) = thread::scope(|s| {
        let mut diff_handles = Vec::with_capacity(n);
        for i in 0..n {
            let ta = &trees[i];
            let tb = &trees[i + 1];
            diff_handles.push(s.spawn(move || repo.diff_tree_names(ta, tb, &[])));
        }
        let wt_h = s.spawn(|| repo.wt_status(&top_idx));

        let mut diffs = Vec::with_capacity(n);
        for h in diff_handles {
            diffs.push(
                h.join()
                    .unwrap_or_else(|_| Err("diff thread panicked".to_string())),
            );
        }
        let wt = wt_h
            .join()
            .unwrap_or_else(|_| Err("wt thread panicked".to_string()));
        (diffs, wt)
    });

    let mut layer_diffs = Vec::with_capacity(n);
    for r in layer_diffs_res {
        layer_diffs.push(r?);
    }
    let wt = wt_res?;

    Ok(StatusSnapshot {
        trees,
        layer_diffs,
        wt_modified: wt.modified,
        wt_untracked: wt.untracked,
    })
}

pub fn cmd_status(repo: &RepoCtx, store: &LayerStore, synced: bool) -> Result<(), String> {
    let tty = is_tty();
    if synced {
        println!(
            "{}",
            c_dim("(HEAD moved externally; layers 3-way re-anchored)", tty)
        );
    }
    let snap = collect_status(repo, store)?;

    for (i, name) in store.manifest.layers.iter().enumerate() {
        let files = &snap.layer_diffs[i];
        let tag = if i == 0 { " (commit target)" } else { "" };
        println!(
            "{} {}{} {}",
            c_bold(&format!("[L{}]", i + 1), tty),
            c_bold(name, tty),
            c_dim(tag, tty),
            c_dim(&format!("({} files)", files.len()), tty)
        );
        if files.is_empty() {
            println!("  {}", c_dim("(clean)", tty));
        } else {
            for (st, path) in files {
                println!("  {}  {}", c_status(st, tty), path);
            }
        }
    }

    println!(
        "{} {} {}",
        c_bold("[WT]", tty),
        c_bold("working-tree", tty),
        c_dim(
            &format!(
                "({} modified, {} untracked)",
                snap.wt_modified.len(),
                snap.wt_untracked.len()
            ),
            tty
        )
    );
    if snap.wt_modified.is_empty() && snap.wt_untracked.is_empty() {
        println!("  {}", c_dim("(clean)", tty));
    } else {
        for (st, path) in &snap.wt_modified {
            println!("  {}  {}", c_status(st, tty), path);
        }
        for path in &snap.wt_untracked {
            println!("  {}  {}", c_status("?", tty), path);
        }
    }
    Ok(())
}

pub fn build_status_json(
    repo: &RepoCtx,
    store: &LayerStore,
    synced: bool,
) -> Result<String, String> {
    let snap = collect_status(repo, store)?;
    let mut out = String::new();
    out.push('{');
    out.push_str(&format!(
        "\"base_head\":{},\"head_tree\":{},\"synced\":{},\"sync_git_index\":{},\"layers\":[",
        json_escape(&store.manifest.base_head),
        json_escape(&snap.trees[0]),
        synced,
        store.manifest.sync_git_index
    ));
    for (i, name) in store.manifest.layers.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"index\":{},\"name\":{},\"tree\":{},\"files\":[",
            i + 1,
            json_escape(name),
            json_escape(&snap.trees[i + 1])
        ));
        for (j, (st, path)) in snap.layer_diffs[i].iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"status\":{},\"path\":{}}}",
                json_escape(st),
                json_escape(path)
            ));
        }
        out.push_str("]}");
    }
    out.push_str("],\"wt\":{\"modified\":[");
    for (i, (st, path)) in snap.wt_modified.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"status\":{},\"path\":{}}}",
            json_escape(st),
            json_escape(path)
        ));
    }
    out.push_str("],\"untracked\":[");
    for (i, path) in snap.wt_untracked.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_escape(path));
    }
    out.push_str("]}}");
    Ok(out)
}

pub fn cmd_status_json(repo: &RepoCtx, store: &LayerStore, synced: bool) -> Result<(), String> {
    let out = build_status_json(repo, store, synced)?;
    println!("{out}");
    Ok(())
}

pub fn build_diff_bytes(
    repo: &RepoCtx,
    store: &LayerStore,
    layer: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    paths: &[String],
    color: bool,
) -> Result<Vec<u8>, String> {
    let trees = compute_trees(repo, store)?;
    let n = store.manifest.layers.len();
    let top_idx = store.idx_path(&store.manifest.layers[n - 1]);

    if from.is_some() || to.is_some() {
        let f = from.unwrap_or("head");
        let t = to.unwrap_or("wt");
        if t.eq_ignore_ascii_case("wt") {
            if f.eq_ignore_ascii_case("wt") {
                return Ok(Vec::new());
            }
            return if f.eq_ignore_ascii_case("head") {
                let tmp = store.root.join("diff.tmp.idx");
                repo.read_tree(&tmp, &trees[0])?;
                let patch = repo.diff_files_patch(&tmp, paths, color);
                let _ = fs::remove_file(&tmp);
                patch
            } else {
                let k = store.resolve_layer(f)?;
                repo.diff_files_patch(&store.idx_path(&store.manifest.layers[k]), paths, color)
            };
        }
        let ta = resolve_tree_spec(store, &trees, f)?;
        let tb = resolve_tree_spec(store, &trees, t)?;
        return repo.diff_tree_patch(ta, tb, paths, color);
    }

    if let Some(spec) = layer {
        if spec.eq_ignore_ascii_case("wt") {
            return repo.diff_files_patch(&top_idx, paths, color);
        }
        let k = store.resolve_layer(spec)?;
        return repo.diff_tree_patch(&trees[k], &trees[k + 1], paths, color);
    }

    let mut buf = Vec::new();
    for (i, name) in store.manifest.layers.iter().enumerate() {
        let patch = repo.diff_tree_patch(&trees[i], &trees[i + 1], paths, color)?;
        if !patch.is_empty() {
            let hdr = c_bold(&format!("=== [L{}] {} ===", i + 1, name), color);
            buf.extend_from_slice(hdr.as_bytes());
            buf.push(b'\n');
            buf.extend_from_slice(&patch);
        }
    }
    let wt_patch = repo.diff_files_patch(&top_idx, paths, color)?;
    if !wt_patch.is_empty() {
        let hdr = c_bold("=== [WT] working-tree ===", color);
        buf.extend_from_slice(hdr.as_bytes());
        buf.push(b'\n');
        buf.extend_from_slice(&wt_patch);
    }
    Ok(buf)
}

pub fn cmd_diff(
    repo: &RepoCtx,
    store: &LayerStore,
    layer: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    paths: &[String],
) -> Result<(), String> {
    let color = is_tty();
    let bytes = build_diff_bytes(repo, store, layer, from, to, paths, color)?;
    if bytes.is_empty() && layer.is_none() && from.is_none() && to.is_none() {
        println!("(no diffs)");
        return Ok(());
    }
    io::stdout().write_all(&bytes).map_err(|e| e.to_string())
}

fn resolve_tree_spec<'a>(
    store: &LayerStore,
    trees: &'a [String],
    spec: &str,
) -> Result<&'a str, String> {
    if spec.eq_ignore_ascii_case("head") {
        return Ok(&trees[0]);
    }
    let k = store.resolve_layer(spec)?;
    Ok(&trees[k + 1])
}

pub fn op_stage(
    repo: &RepoCtx,
    store: &LayerStore,
    layer: Option<&str>,
    paths: &[String],
    interactive: bool,
    all: bool,
) -> Result<String, String> {
    if !interactive && !all && paths.is_empty() {
        return Err("stage requires <paths...>, --all, or -p".to_string());
    }
    let n = store.manifest.layers.len();
    let target = match layer {
        Some(s) => store.resolve_layer(s)?,
        None => n - 1,
    };

    store.snapshot_for_undo()?;

    if interactive {
        let target_idx = store.idx_path(&store.manifest.layers[target]);
        let t_before = repo.write_tree(&target_idx)?;
        repo.add_interactive(&target_idx, paths)?;
        let t_after = repo.write_tree(&target_idx)?;
        if target + 1 < n && t_before != t_after {
            let delta = repo.diff_tree_patch(&t_before, &t_after, &[], false)?;
            for k in (target + 1)..n {
                repo.apply_cached(
                    &store.idx_path(&store.manifest.layers[k]),
                    &delta,
                    true,
                    false,
                )?;
            }
        }
    } else {
        for k in target..n {
            repo.add_paths(&store.idx_path(&store.manifest.layers[k]), paths, all)?;
        }
    }
    store.maybe_mirror_git_index(repo)?;
    Ok(format!(
        "staged into [L{}] {}",
        target + 1,
        store.manifest.layers[target]
    ))
}

pub fn cmd_stage(
    repo: &RepoCtx,
    store: &LayerStore,
    layer: Option<&str>,
    paths: &[String],
    interactive: bool,
    all: bool,
) -> Result<(), String> {
    let msg = op_stage(repo, store, layer, paths, interactive, all)?;
    println!("{msg}");
    Ok(())
}

pub fn op_promote(
    repo: &RepoCtx,
    store: &LayerStore,
    from: Option<&str>,
    to: Option<&str>,
    paths: &[String],
    interactive: bool,
    all: bool,
) -> Result<String, String> {
    if !interactive && !all && paths.is_empty() {
        return Err("promote requires <paths...>, --all, or -p".to_string());
    }
    if let Some("wt") | Some("WT") = from {
        return op_stage(repo, store, to, paths, interactive, all);
    }

    let trees = compute_trees(repo, store)?;
    let n = store.manifest.layers.len();

    let src = match from {
        Some(s) => store.resolve_layer(s)?,
        None => {
            let mut found = None;
            for i in 1..n {
                if !repo
                    .diff_tree_names(&trees[i], &trees[i + 1], paths)?
                    .is_empty()
                {
                    found = Some(i);
                    break;
                }
            }
            match found {
                Some(i) => i,
                None => {
                    if !repo
                        .diff_tree_names(&trees[0], &trees[1], paths)?
                        .is_empty()
                    {
                        return Err(format!(
                            "changes are already in [L1] {} (run `gd commit`)",
                            store.manifest.layers[0]
                        ));
                    }
                    return Err("no changes in upper layers to promote".to_string());
                }
            }
        }
    };

    if src == 0 {
        return Err(format!(
            "[L1] {} is already the lowest layer (run `gd commit`)",
            store.manifest.layers[0]
        ));
    }

    let dst = match to {
        Some(t) => {
            let d = store.resolve_layer(t)?;
            if d >= src {
                return Err(format!(
                    "destination '{}' must be closer to HEAD than '{}' (use `gd demote` to move up)",
                    store.manifest.layers[d], store.manifest.layers[src]
                ));
            }
            d
        }
        None => src - 1,
    };

    store.snapshot_for_undo()?;

    let t_src = &trees[src + 1];
    if interactive {
        let dst_idx = store.idx_path(&store.manifest.layers[dst]);
        let t_dst_before = &trees[dst + 1];
        repo.reset_patch_interactive(&dst_idx, t_src, paths)?;
        let t_dst_after = repo.write_tree(&dst_idx)?;
        if dst + 1 < src && &t_dst_after != t_dst_before {
            let delta = repo.diff_tree_patch(t_dst_before, &t_dst_after, &[], false)?;
            for k in (dst + 1)..src {
                repo.apply_cached(
                    &store.idx_path(&store.manifest.layers[k]),
                    &delta,
                    true,
                    false,
                )?;
            }
        }
    } else {
        for k in dst..src {
            let patch = repo.diff_tree_patch(&trees[k + 1], t_src, paths, false)?;
            repo.apply_cached(
                &store.idx_path(&store.manifest.layers[k]),
                &patch,
                false,
                false,
            )?;
        }
    }

    store.maybe_mirror_git_index(repo)?;
    Ok(format!(
        "promoted [L{}] {} -> [L{}] {}",
        src + 1,
        store.manifest.layers[src],
        dst + 1,
        store.manifest.layers[dst]
    ))
}

pub fn cmd_promote(
    repo: &RepoCtx,
    store: &LayerStore,
    from: Option<&str>,
    to: Option<&str>,
    paths: &[String],
    interactive: bool,
    all: bool,
) -> Result<(), String> {
    let msg = op_promote(repo, store, from, to, paths, interactive, all)?;
    println!("{msg}");
    Ok(())
}

pub fn op_demote(
    repo: &RepoCtx,
    store: &LayerStore,
    from: Option<&str>,
    to: Option<&str>,
    paths: &[String],
    interactive: bool,
    all: bool,
) -> Result<String, String> {
    if !interactive && !all && paths.is_empty() {
        return Err("demote requires <paths...>, --all, or -p".to_string());
    }
    let trees = compute_trees(repo, store)?;
    let n = store.manifest.layers.len();

    let src = match from {
        Some(s) => store.resolve_layer(s)?,
        None => {
            let mut found = None;
            for i in 0..n {
                if !repo
                    .diff_tree_names(&trees[i], &trees[i + 1], paths)?
                    .is_empty()
                {
                    found = Some(i);
                    break;
                }
            }
            found.ok_or_else(|| "no changes in layers to demote".to_string())?
        }
    };

    let dst = match to {
        Some(t) if t.eq_ignore_ascii_case("wt") => n,
        Some(t) => {
            let d = store.resolve_layer(t)?;
            if d <= src {
                return Err(format!(
                    "destination '{}' must be above '{}' (use `gd promote` to move down)",
                    store.manifest.layers[d], store.manifest.layers[src]
                ));
            }
            d
        }
        None => src + 1,
    };

    store.snapshot_for_undo()?;

    let t_below = &trees[src];
    if interactive {
        let src_idx = store.idx_path(&store.manifest.layers[src]);
        let t_src_before = &trees[src + 1];
        repo.reset_patch_interactive(&src_idx, t_below, paths)?;
        let t_src_after = repo.write_tree(&src_idx)?;
        if dst > src + 1 && &t_src_after != t_src_before {
            let delta = repo.diff_tree_patch(t_src_before, &t_src_after, &[], false)?;
            for k in (src + 1)..dst {
                repo.apply_cached(
                    &store.idx_path(&store.manifest.layers[k]),
                    &delta,
                    true,
                    false,
                )?;
            }
        }
    } else {
        for k in src..dst {
            let patch = repo.diff_tree_patch(&trees[k + 1], t_below, paths, false)?;
            repo.apply_cached(
                &store.idx_path(&store.manifest.layers[k]),
                &patch,
                false,
                false,
            )?;
        }
    }

    store.maybe_mirror_git_index(repo)?;
    let dst_label = if dst == n {
        "[WT] working-tree".to_string()
    } else {
        format!("[L{}] {}", dst + 1, store.manifest.layers[dst])
    };
    Ok(format!(
        "demoted [L{}] {} -> {}",
        src + 1,
        store.manifest.layers[src],
        dst_label
    ))
}

pub fn cmd_demote(
    repo: &RepoCtx,
    store: &LayerStore,
    from: Option<&str>,
    to: Option<&str>,
    paths: &[String],
    interactive: bool,
    all: bool,
) -> Result<(), String> {
    let msg = op_demote(repo, store, from, to, paths, interactive, all)?;
    println!("{msg}");
    Ok(())
}

pub struct DropOpts<'a> {
    pub layer: &'a str,
    pub paths: &'a [String],
    pub interactive: bool,
    pub all: bool,
    pub cascade: bool,
    pub hard: bool,
}

pub fn op_drop(repo: &RepoCtx, store: &LayerStore, opts: DropOpts<'_>) -> Result<String, String> {
    if !opts.interactive && !opts.all && opts.paths.is_empty() {
        return Err("drop requires <paths...>, --all, or -p".to_string());
    }
    if opts.hard && opts.interactive {
        return Err("--hard cannot be combined with -p".to_string());
    }
    let k = store.resolve_layer(opts.layer)?;
    let trees = compute_trees(repo, store)?;
    let n = store.manifest.layers.len();
    let end = if opts.cascade || opts.hard { n } else { k + 1 };

    store.snapshot_for_undo()?;

    let t_below = &trees[k];
    if opts.interactive {
        let idx = store.idx_path(&store.manifest.layers[k]);
        let t_before = &trees[k + 1];
        repo.reset_patch_interactive(&idx, t_below, opts.paths)?;
        let t_after = repo.write_tree(&idx)?;
        if end > k + 1 && &t_after != t_before {
            let delta = repo.diff_tree_patch(t_before, &t_after, &[], false)?;
            for i in (k + 1)..end {
                repo.apply_cached(
                    &store.idx_path(&store.manifest.layers[i]),
                    &delta,
                    true,
                    false,
                )?;
            }
        }
    } else {
        for i in k..end {
            let patch = repo.diff_tree_patch(&trees[i + 1], t_below, opts.paths, false)?;
            repo.apply_cached(
                &store.idx_path(&store.manifest.layers[i]),
                &patch,
                false,
                false,
            )?;
        }
        if opts.hard {
            let top_idx = store.idx_path(&store.manifest.layers[n - 1]);
            let wt_patch = repo.diff_files_patch(&top_idx, opts.paths, false)?;
            repo.apply_worktree_reverse(&wt_patch)?;
            let untracked = repo.untracked_files(&top_idx, opts.paths)?;
            repo.remove_untracked_files(&untracked)?;
        }
    }

    store.maybe_mirror_git_index(repo)?;
    Ok(format!(
        "dropped changes from [L{}] {}",
        k + 1,
        store.manifest.layers[k]
    ))
}

pub fn cmd_drop(repo: &RepoCtx, store: &LayerStore, opts: DropOpts<'_>) -> Result<(), String> {
    let msg = op_drop(repo, store, opts)?;
    println!("{msg}");
    Ok(())
}

pub fn op_commit(
    repo: &RepoCtx,
    store: &mut LayerStore,
    paths: &[String],
    interactive: bool,
    git_args: &[String],
    captured: bool,
) -> Result<String, String> {
    let trees = compute_trees(repo, store)?;
    let t0 = &trees[0];
    let t1 = &trees[1];
    let allow_empty = git_args
        .iter()
        .any(|a| a == "--allow-empty" || a == "--amend");

    let run_commit = |idx: &std::path::Path| -> Result<String, String> {
        if captured {
            repo.commit_captured(idx, git_args)
        } else {
            repo.commit_interactive(idx, git_args)?;
            Ok(String::new())
        }
    };

    let output = if paths.is_empty() && !interactive {
        if t0 == t1 && !allow_empty {
            return Err(format!(
                "nothing in [L1] {} to commit",
                store.manifest.layers[0]
            ));
        }
        let l1_idx = store.idx_path(&store.manifest.layers[0]);
        run_commit(&l1_idx)?
    } else {
        let tmp_idx = store.root.join("commit.tmp.idx");
        repo.read_tree(&tmp_idx, t0)?;
        let res = (|| -> Result<String, String> {
            if interactive {
                repo.reset_patch_interactive(&tmp_idx, t1, paths)?;
            } else {
                let patch = repo.diff_tree_patch(t0, t1, paths, false)?;
                repo.apply_cached(&tmp_idx, &patch, false, false)?;
            }
            let t_sub = repo.write_tree(&tmp_idx)?;
            if &t_sub == t0 && !allow_empty {
                return Err(format!(
                    "no changes selected from [L1] {} to commit",
                    store.manifest.layers[0]
                ));
            }
            run_commit(&tmp_idx)
        })();
        let _ = fs::remove_file(&tmp_idx);
        res?
    };

    store.manifest.base_head = repo.head_oid()?.unwrap_or_else(|| "UNBORN".to_string());
    store.save_manifest()?;
    store.maybe_mirror_git_index(repo)?;
    Ok(output)
}

pub fn cmd_commit(
    repo: &RepoCtx,
    store: &mut LayerStore,
    paths: &[String],
    interactive: bool,
    git_args: &[String],
) -> Result<(), String> {
    let _ = op_commit(repo, store, paths, interactive, git_args, false)?;
    Ok(())
}

pub fn format_layer_list(repo: &RepoCtx, store: &LayerStore) -> Result<String, String> {
    let trees = compute_trees(repo, store)?;
    let mut lines = Vec::with_capacity(store.manifest.layers.len());
    for (i, name) in store.manifest.layers.iter().enumerate() {
        let files = repo.diff_tree_names(&trees[i], &trees[i + 1], &[])?;
        let tag = if i == 0 { " (commit target)" } else { "" };
        lines.push(format!(
            "L{}: {}{} [{} files]",
            i + 1,
            name,
            tag,
            files.len()
        ));
    }
    Ok(lines.join("\n"))
}

pub fn cmd_layer_list(repo: &RepoCtx, store: &LayerStore) -> Result<(), String> {
    println!("{}", format_layer_list(repo, store)?);
    Ok(())
}

pub enum InsertPos {
    Top,
    At(usize),
    Above(String),
    Below(String),
}

pub fn op_layer_add(
    repo: &RepoCtx,
    store: &mut LayerStore,
    name: &str,
    pos: InsertPos,
) -> Result<String, String> {
    validate_layer_name(name)?;
    if store.manifest.layers.iter().any(|l| l == name) {
        return Err(format!("layer '{name}' already exists"));
    }
    let n = store.manifest.layers.len();
    let idx = match pos {
        InsertPos::Top => n,
        InsertPos::At(0) => return Err("position is 1-based (1..=n+1)".to_string()),
        InsertPos::At(one_based) => {
            if one_based > n + 1 {
                return Err(format!("position {one_based} out of range (1..={})", n + 1));
            }
            one_based - 1
        }
        InsertPos::Above(ref target) => store.resolve_layer(target)? + 1,
        InsertPos::Below(ref target) => store.resolve_layer(target)?,
    };

    store.snapshot_for_undo()?;
    let src_tree = if idx == 0 {
        repo.tree_for_ref_or_unborn(Some(&store.manifest.base_head))?
    } else {
        repo.write_tree(&store.idx_path(&store.manifest.layers[idx - 1]))?
    };
    repo.read_tree(&store.idx_path(name), &src_tree)?;
    store.manifest.layers.insert(idx, name.to_string());
    store.save_manifest()?;
    store.maybe_mirror_git_index(repo)?;
    Ok(format!("added layer [L{}] {name}", idx + 1))
}

pub fn cmd_layer_add(
    repo: &RepoCtx,
    store: &mut LayerStore,
    name: &str,
    pos: InsertPos,
) -> Result<(), String> {
    let msg = op_layer_add(repo, store, name, pos)?;
    println!("{msg}");
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RmMode {
    RequireEmpty,
    MergeUp,
    MergeDown,
    Drop,
}

pub fn op_layer_rm(
    repo: &RepoCtx,
    store: &mut LayerStore,
    spec: &str,
    mode: RmMode,
) -> Result<String, String> {
    if store.manifest.layers.len() <= 1 {
        return Err("cannot remove the last remaining layer".to_string());
    }
    let k = store.resolve_layer(spec)?;
    let name = store.manifest.layers[k].clone();
    let trees = compute_trees(repo, store)?;
    let delta_files = repo.diff_tree_names(&trees[k], &trees[k + 1], &[])?;

    if !delta_files.is_empty() && mode == RmMode::RequireEmpty {
        return Err(format!(
            "layer '{name}' has {} changed file(s); pass --merge-up, --merge-down, or --drop",
            delta_files.len()
        ));
    }

    store.snapshot_for_undo()?;

    match mode {
        RmMode::RequireEmpty | RmMode::MergeUp => {}
        RmMode::MergeDown => {
            if k == 0 {
                return Err(
                    "cannot --merge-down bottom layer; commit or use --merge-up".to_string()
                );
            }
            let below = &store.manifest.layers[k - 1];
            repo.read_tree(&store.idx_path(below), &trees[k + 1])?;
        }
        RmMode::Drop => {
            let patch = repo.diff_tree_patch(&trees[k + 1], &trees[k], &[], false)?;
            for i in (k + 1)..store.manifest.layers.len() {
                repo.apply_cached(
                    &store.idx_path(&store.manifest.layers[i]),
                    &patch,
                    true,
                    false,
                )?;
            }
        }
    }

    let _ = fs::remove_file(store.idx_path(&name));
    store.manifest.layers.remove(k);
    store.save_manifest()?;
    store.maybe_mirror_git_index(repo)?;
    Ok(format!("removed layer '{name}'"))
}

pub fn cmd_layer_rm(
    repo: &RepoCtx,
    store: &mut LayerStore,
    spec: &str,
    mode: RmMode,
) -> Result<(), String> {
    let msg = op_layer_rm(repo, store, spec, mode)?;
    println!("{msg}");
    Ok(())
}

pub fn op_layer_rename(
    repo: &RepoCtx,
    store: &mut LayerStore,
    old_spec: &str,
    new_name: &str,
) -> Result<String, String> {
    validate_layer_name(new_name)?;
    if store.manifest.layers.iter().any(|l| l == new_name) {
        return Err(format!("layer '{new_name}' already exists"));
    }
    let k = store.resolve_layer(old_spec)?;
    let old_name = store.manifest.layers[k].clone();
    store.snapshot_for_undo()?;
    fs::rename(store.idx_path(&old_name), store.idx_path(new_name))
        .map_err(|e| format!("rename index: {e}"))?;
    store.manifest.layers[k] = new_name.to_string();
    store.save_manifest()?;
    store.maybe_mirror_git_index(repo)?;
    Ok(format!("renamed layer '{old_name}' -> '{new_name}'"))
}

pub fn cmd_layer_rename(
    repo: &RepoCtx,
    store: &mut LayerStore,
    old_spec: &str,
    new_name: &str,
) -> Result<(), String> {
    let msg = op_layer_rename(repo, store, old_spec, new_name)?;
    println!("{msg}");
    Ok(())
}

pub fn read_blob_bytes(
    repo: &RepoCtx,
    store: &LayerStore,
    spec: &str,
    rel_path: &str,
    allow_missing: bool,
) -> Result<Vec<u8>, String> {
    if spec.eq_ignore_ascii_case("wt") {
        let target = repo.work_tree.join(rel_path);
        return match fs::read(&target) {
            Ok(bytes) => Ok(bytes),
            Err(_) if allow_missing => Ok(Vec::new()),
            Err(e) => Err(format!("read WT '{rel_path}': {e}")),
        };
    }

    let trees = compute_trees(repo, store)?;
    let tree = resolve_tree_spec(store, &trees, spec)?;
    match repo.read_blob_from_tree(tree, rel_path)? {
        Some(bytes) => Ok(bytes),
        None if allow_missing => Ok(Vec::new()),
        None => Err(format!("path '{rel_path}' not found in '{spec}'")),
    }
}

pub fn cmd_blob(
    repo: &RepoCtx,
    store: &LayerStore,
    spec: &str,
    rel_path: &str,
    allow_missing: bool,
) -> Result<(), String> {
    let bytes = read_blob_bytes(repo, store, spec, rel_path, allow_missing)?;
    io::stdout().write_all(&bytes).map_err(|e| e.to_string())
}

pub fn cmd_patch_apply(
    repo: &RepoCtx,
    store: &LayerStore,
    from: &str,
    to: &str,
    patch: &[u8],
) -> Result<(), String> {
    if patch.is_empty() {
        return Ok(());
    }
    let n = store.manifest.layers.len();
    let from_pos: isize = if from.eq_ignore_ascii_case("wt") {
        n as isize
    } else if from.eq_ignore_ascii_case("head") {
        -1
    } else {
        store.resolve_layer(from)? as isize
    };
    let to_pos: isize = if to.eq_ignore_ascii_case("wt") {
        n as isize
    } else if to.eq_ignore_ascii_case("head") {
        -1
    } else {
        store.resolve_layer(to)? as isize
    };

    if from_pos == to_pos {
        return Err("patch-apply requires distinct --from and --to".to_string());
    }
    if to_pos < 0 {
        return Err(
            "cannot apply patches directly to HEAD; target a layer then commit".to_string(),
        );
    }

    store.snapshot_for_undo()?;
    if from_pos > to_pos {
        // Promoting / staging patch downward from higher state into to_pos..(from_pos - 1)
        let start = to_pos as usize;
        let end = (from_pos as usize).min(n);
        for k in start..end {
            repo.apply_cached(
                &store.idx_path(&store.manifest.layers[k]),
                patch,
                true,
                false,
            )?;
        }
    } else {
        // Demoting / dropping patch upward from lower layer: reverse-apply on from_pos..(to_pos - 1)
        if from_pos < 0 {
            return Err("cannot demote from HEAD".to_string());
        }
        let start = from_pos as usize;
        let end = (to_pos as usize).min(n);
        for k in start..end {
            repo.apply_cached(
                &store.idx_path(&store.manifest.layers[k]),
                patch,
                true,
                true,
            )?;
        }
    }
    store.maybe_mirror_git_index(repo)?;
    Ok(())
}

pub fn cmd_config(
    repo: &RepoCtx,
    store: &mut LayerStore,
    key: &str,
    val: Option<&str>,
) -> Result<(), String> {
    match key {
        "sync-git-index" => {
            if let Some(v) = val {
                let b = match v {
                    "true" | "1" | "yes" | "on" => true,
                    "false" | "0" | "no" | "off" => false,
                    _ => return Err(format!("invalid boolean '{v}' (expected true|false)")),
                };
                store.manifest.sync_git_index = b;
                store.save_manifest()?;
                store.maybe_mirror_git_index(repo)?;
                println!("sync-git-index={b}");
            } else {
                println!("{}", store.manifest.sync_git_index);
            }
            Ok(())
        }
        _ => Err(format!(
            "unknown config key '{key}' (available: sync-git-index)"
        )),
    }
}
