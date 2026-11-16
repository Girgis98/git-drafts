use crate::git::RepoCtx;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_LAYERS: &[&str] = &["ready", "reviewed", "draft"];
const MAX_UNDO: usize = 15;

#[derive(Clone, Debug)]
pub struct Manifest {
    pub base_head: String,
    pub layers: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct LayerStore {
    pub root: PathBuf,
    pub manifest: Manifest,
}

pub fn validate_layer_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("layer name cannot be empty".to_string());
    }
    let lower = name.to_ascii_lowercase();
    if matches!(lower.as_str(), "wt" | "head" | "unborn" | "all") {
        return Err(format!("'{name}' is a reserved name"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(format!(
            "invalid layer name '{name}': use only ASCII letters, digits, '-' or '_'"
        ));
    }
    Ok(())
}

impl LayerStore {
    pub fn layers_root(repo: &RepoCtx) -> PathBuf {
        repo.git_dir.join("drafts")
    }

    pub fn idx_dir(&self) -> PathBuf {
        self.root.join("idx")
    }

    pub fn idx_path(&self, name: &str) -> PathBuf {
        self.idx_dir().join(name)
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest")
    }

    fn undo_dir(&self) -> PathBuf {
        self.root.join("undo")
    }

    pub fn exists(repo: &RepoCtx) -> bool {
        Self::layers_root(repo).join("manifest").exists()
    }

    pub fn init(repo: &RepoCtx, names: &[String], force: bool) -> Result<Self, String> {
        let root = Self::layers_root(repo);
        if root.join("manifest").exists() && !force {
            return Err("layers already initialized (use --force to reinitialize)".to_string());
        }
        let layers: Vec<String> = if names.is_empty() {
            DEFAULT_LAYERS.iter().map(|s| (*s).to_string()).collect()
        } else {
            names.to_vec()
        };
        for l in &layers {
            validate_layer_name(l)?;
        }
        for i in 0..layers.len() {
            if layers[i + 1..].contains(&layers[i]) {
                return Err(format!("duplicate layer name '{}'", layers[i]));
            }
        }

        if root.exists() && force {
            fs::remove_dir_all(&root).map_err(|e| format!("clean layers dir: {e}"))?;
        }
        fs::create_dir_all(root.join("idx")).map_err(|e| format!("create idx dir: {e}"))?;
        fs::create_dir_all(root.join("undo")).map_err(|e| format!("create undo dir: {e}"))?;

        let head = repo.head_oid()?.unwrap_or_else(|| "UNBORN".to_string());
        let base_tree = repo.tree_for_ref_or_unborn(Some(&head))?;

        let store = Self {
            root,
            manifest: Manifest {
                base_head: head,
                layers: layers.clone(),
            },
        };
        for l in &layers {
            repo.read_tree(&store.idx_path(l), &base_tree)?;
        }
        store.save_manifest()?;
        Ok(store)
    }

    pub fn load(repo: &RepoCtx) -> Result<Self, String> {
        let root = Self::layers_root(repo);
        let mpath = root.join("manifest");
        if !mpath.exists() {
            return Err("draft layers not initialized; run `gd init` first".to_string());
        }
        let raw = fs::read_to_string(&mpath).map_err(|e| format!("read manifest: {e}"))?;
        let mut base_head = None;
        let mut layers = Vec::new();
        for line in raw.lines() {
            if let Some(v) = line.strip_prefix("base_head=") {
                base_head = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("layers=") {
                layers = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
        let base_head = base_head.ok_or("corrupt manifest: missing base_head")?;
        if layers.is_empty() {
            return Err("corrupt manifest: no layers defined".to_string());
        }
        Ok(Self {
            root,
            manifest: Manifest { base_head, layers },
        })
    }

    pub fn load_and_sync(repo: &RepoCtx) -> Result<(Self, bool), String> {
        let mut store = Self::load(repo)?;
        let synced = store.sync_head(repo)?;
        Ok((store, synced))
    }

    pub fn sync_head(&mut self, repo: &RepoCtx) -> Result<bool, String> {
        let current = repo.head_oid()?.unwrap_or_else(|| "UNBORN".to_string());
        if current == self.manifest.base_head {
            return Ok(false);
        }
        let base_tree = repo.tree_for_ref_or_unborn(Some(&self.manifest.base_head))?;
        let new_tree = repo.tree_for_ref_or_unborn(Some(&current))?;

        for l in &self.manifest.layers {
            let idx = self.idx_path(l);
            let tk = repo.write_tree(&idx)?;
            if tk == base_tree {
                repo.read_tree(&idx, &new_tree)?;
            } else {
                repo.read_tree_merge(&idx, &base_tree, &new_tree, &tk)?;
            }
        }
        self.manifest.base_head = current;
        self.save_manifest()?;
        Ok(true)
    }

    pub fn save_manifest(&self) -> Result<(), String> {
        let content = format!(
            "version=1\nbase_head={}\nlayers={}\n",
            self.manifest.base_head,
            self.manifest.layers.join(",")
        );
        let tmp = self.root.join("manifest.tmp");
        fs::write(&tmp, content).map_err(|e| format!("write manifest: {e}"))?;
        fs::rename(&tmp, self.manifest_path()).map_err(|e| format!("rename manifest: {e}"))?;
        Ok(())
    }

    pub fn resolve_layer(&self, spec: &str) -> Result<usize, String> {
        if let Some(pos) = self.manifest.layers.iter().position(|l| l == spec) {
            return Ok(pos);
        }
        let stripped = spec
            .strip_prefix('L')
            .or_else(|| spec.strip_prefix('l'))
            .unwrap_or(spec);
        if let Ok(n) = stripped.parse::<usize>() {
            if n >= 1 && n <= self.manifest.layers.len() {
                return Ok(n - 1);
            }
        }
        Err(format!(
            "unknown layer '{spec}' (available: {})",
            self.manifest.layers.join(", ")
        ))
    }

    pub fn snapshot_for_undo(&self) -> Result<(), String> {
        let undo = self.undo_dir();
        fs::create_dir_all(&undo).map_err(|e| format!("mkdir undo: {e}"))?;

        let mut seqs = list_undo_seqs(&undo)?;
        let next = seqs.last().copied().unwrap_or(0) + 1;
        let snap = undo.join(format!("{next:06}"));
        let snap_idx = snap.join("idx");
        fs::create_dir_all(&snap_idx).map_err(|e| format!("mkdir snap: {e}"))?;
        fs::copy(self.manifest_path(), snap.join("manifest"))
            .map_err(|e| format!("copy manifest snap: {e}"))?;
        for l in &self.manifest.layers {
            let src = self.idx_path(l);
            if src.exists() {
                fs::copy(&src, snap_idx.join(l))
                    .map_err(|e| format!("copy idx snap {l}: {e}"))?;
            }
        }

        seqs.push(next);
        while seqs.len() > MAX_UNDO {
            let old = seqs.remove(0);
            let _ = fs::remove_dir_all(undo.join(format!("{old:06}")));
        }
        Ok(())
    }

    pub fn pop_undo(&mut self, repo: &RepoCtx) -> Result<(), String> {
        let undo = self.undo_dir();
        let mut seqs = list_undo_seqs(&undo)?;
        let Some(latest) = seqs.pop() else {
            return Err("nothing to undo".to_string());
        };
        let snap = undo.join(format!("{latest:06}"));
        let snap_manifest = fs::read_to_string(snap.join("manifest"))
            .map_err(|e| format!("read undo manifest: {e}"))?;
        let mut snap_head = None;
        let mut snap_layers = Vec::new();
        for line in snap_manifest.lines() {
            if let Some(v) = line.strip_prefix("base_head=") {
                snap_head = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("layers=") {
                snap_layers = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
        let snap_head = snap_head.ok_or("corrupt undo snapshot")?;
        let current_head = repo.head_oid()?.unwrap_or_else(|| "UNBORN".to_string());
        if snap_head != current_head {
            return Err(
                "cannot undo across a git commit/checkout (HEAD changed since snapshot)"
                    .to_string(),
            );
        }

        // Replace idx directory atomically-ish: remove current idx files, copy snapshot idx files
        let cur_idx = self.idx_dir();
        for entry in fs::read_dir(&cur_idx).map_err(|e| format!("read idx dir: {e}"))? {
            let entry = entry.map_err(|e| format!("read idx entry: {e}"))?;
            if entry.path().is_file() {
                let _ = fs::remove_file(entry.path());
            }
        }
        for l in &snap_layers {
            let src = snap.join("idx").join(l);
            if src.exists() {
                fs::copy(&src, cur_idx.join(l))
                    .map_err(|e| format!("restore idx {l}: {e}"))?;
            }
        }
        self.manifest.base_head = snap_head;
        self.manifest.layers = snap_layers;
        self.save_manifest()?;
        fs::remove_dir_all(&snap).map_err(|e| format!("cleanup undo snap: {e}"))?;
        Ok(())
    }
}

fn list_undo_seqs(undo_dir: &Path) -> Result<Vec<u64>, String> {
    if !undo_dir.exists() {
        return Ok(Vec::new());
    }
    let mut seqs = Vec::new();
    for entry in fs::read_dir(undo_dir).map_err(|e| format!("read undo dir: {e}"))? {
        let entry = entry.map_err(|e| format!("undo entry: {e}"))?;
        if entry.path().is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                if let Ok(n) = name.parse::<u64>() {
                    seqs.push(n);
                }
            }
        }
    }
    seqs.sort_unstable();
    Ok(seqs)
}
