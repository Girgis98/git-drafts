use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[derive(Clone, Debug)]
pub struct RepoCtx {
    pub git_dir: PathBuf,
    pub work_tree: PathBuf,
}

impl RepoCtx {
    pub fn discover(cwd: Option<&Path>) -> Result<Self, String> {
        let mut cmd = Command::new("git");
        cmd.args(["rev-parse", "--absolute-git-dir", "--show-toplevel"]);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let out = cmd.output().map_err(|e| format!("failed to run git: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if err.is_empty() {
                "not a git repository".to_string()
            } else {
                err
            });
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines();
        let git_dir = lines
            .next()
            .ok_or("missing --absolute-git-dir output")?
            .trim();
        let work_tree = lines
            .next()
            .ok_or("repository has no working tree")?
            .trim();
        Ok(Self {
            git_dir: PathBuf::from(git_dir),
            work_tree: PathBuf::from(work_tree),
        })
    }

    fn git_cmd(&self, index_path: Option<&Path>) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.work_tree);
        if let Some(idx) = index_path {
            cmd.env("GIT_INDEX_FILE", idx);
        }
        cmd
    }

    fn run_checked(&self, mut cmd: Command, ctx: &str) -> Result<Output, String> {
        let out = cmd.output().map_err(|e| format!("{ctx}: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let msg = if err.is_empty() {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            } else {
                err
            };
            return Err(format!("{ctx}: {msg}"));
        }
        Ok(out)
    }

    pub fn head_oid(&self) -> Result<Option<String>, String> {
        let out = self
            .git_cmd(None)
            .args(["rev-parse", "--verify", "-q", "HEAD"])
            .output()
            .map_err(|e| format!("git rev-parse HEAD: {e}"))?;
        if !out.status.success() {
            return Ok(None);
        }
        let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if oid.is_empty() {
            Ok(None)
        } else {
            Ok(Some(oid))
        }
    }

    pub fn empty_tree(&self) -> Result<String, String> {
        let mut child = self
            .git_cmd(None)
            .arg("mktree")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("git mktree: {e}"))?;
        drop(child.stdin.take());
        let out = child
            .wait_with_output()
            .map_err(|e| format!("git mktree wait: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git mktree: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn tree_for_ref_or_unborn(&self, head_ref: Option<&str>) -> Result<String, String> {
        match head_ref {
            None | Some("UNBORN") => self.empty_tree(),
            Some(oid) => {
                let spec = format!("{oid}^{{tree}}");
                let out = self.run_checked(
                    {
                        let mut c = self.git_cmd(None);
                        c.args(["rev-parse", "--verify", &spec]);
                        c
                    },
                    "resolve tree",
                )?;
                Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
            }
        }
    }

    pub fn write_tree(&self, index_path: &Path) -> Result<String, String> {
        let mut c = self.git_cmd(Some(index_path));
        c.arg("write-tree");
        let out = self.run_checked(c, "git write-tree")?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn read_tree(&self, index_path: &Path, tree_oid: &str) -> Result<(), String> {
        let mut c = self.git_cmd(Some(index_path));
        c.args(["read-tree", tree_oid]);
        self.run_checked(c, "git read-tree")?;
        Ok(())
    }

    pub fn read_tree_merge(
        &self,
        index_path: &Path,
        base_tree: &str,
        ours_tree: &str,
        theirs_tree: &str,
    ) -> Result<(), String> {
        let out = self
            .git_cmd(None)
            .args([
                "merge-tree",
                "--write-tree",
                &format!("--merge-base={base_tree}"),
                ours_tree,
                theirs_tree,
            ])
            .output()
            .map_err(|e| format!("git merge-tree: {e}"))?;

        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let merged_tree = stdout
                .lines()
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or("git merge-tree returned empty tree")?;
            return self.read_tree(index_path, merged_tree);
        }

        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("unknown option") || stderr.contains("usage: git merge-tree") {
            let mut c = self.git_cmd(Some(index_path));
            c.args(["read-tree", "-i", "-m", base_tree, theirs_tree, ours_tree]);
            self.run_checked(c, "git read-tree -i -m")?;
            return Ok(());
        }

        Err(format!(
            "conflict re-anchoring layer onto new HEAD: {}",
            String::from_utf8_lossy(&out.stdout).trim()
        ))
    }

    pub fn diff_tree_names(
        &self,
        tree_a: &str,
        tree_b: &str,
        paths: &[String],
    ) -> Result<Vec<(String, String)>, String> {
        if tree_a == tree_b {
            return Ok(Vec::new());
        }
        let mut c = self.git_cmd(None);
        c.args(["diff-tree", "--no-renames", "--name-status", "-r", "-z", tree_a, tree_b]);
        if !paths.is_empty() {
            c.arg("--");
            c.args(paths);
        }
        let out = self.run_checked(c, "git diff-tree --name-status")?;
        Ok(parse_z_name_status(&out.stdout))
    }

    pub fn diff_tree_patch(
        &self,
        tree_a: &str,
        tree_b: &str,
        paths: &[String],
        color: bool,
    ) -> Result<Vec<u8>, String> {
        if tree_a == tree_b {
            return Ok(Vec::new());
        }
        let mut c = self.git_cmd(None);
        c.args([
            "diff-tree",
            "--no-renames",
            "-p",
            "--binary",
            "--full-index",
        ]);
        if color {
            c.arg("--color=always");
        }
        c.args([tree_a, tree_b]);
        if !paths.is_empty() {
            c.arg("--");
            c.args(paths);
        }
        let out = self.run_checked(c, "git diff-tree -p")?;
        Ok(out.stdout)
    }

    pub fn refresh_index(&self, index_path: &Path) {
        let _ = self
            .git_cmd(Some(index_path))
            .args(["update-index", "-q", "--refresh"])
            .output();
    }

    pub fn diff_files_names(
        &self,
        index_path: &Path,
        paths: &[String],
    ) -> Result<Vec<(String, String)>, String> {
        self.refresh_index(index_path);
        let mut c = self.git_cmd(Some(index_path));
        c.args(["diff-files", "--no-renames", "--name-status", "-z"]);
        if !paths.is_empty() {
            c.arg("--");
            c.args(paths);
        }
        let out = self.run_checked(c, "git diff-files")?;
        Ok(parse_z_name_status(&out.stdout))
    }

    pub fn diff_files_patch(
        &self,
        index_path: &Path,
        paths: &[String],
        color: bool,
    ) -> Result<Vec<u8>, String> {
        self.refresh_index(index_path);
        let mut c = self.git_cmd(Some(index_path));
        c.args(["diff-files", "--no-renames", "-p", "--binary", "--full-index"]);
        if color {
            c.arg("--color=always");
        }
        if !paths.is_empty() {
            c.arg("--");
            c.args(paths);
        }
        let out = self.run_checked(c, "git diff-files -p")?;
        Ok(out.stdout)
    }

    pub fn untracked_files(
        &self,
        index_path: &Path,
        paths: &[String],
    ) -> Result<Vec<String>, String> {
        let mut c = self.git_cmd(Some(index_path));
        c.args(["ls-files", "--others", "--exclude-standard", "-z"]);
        if !paths.is_empty() {
            c.arg("--");
            c.args(paths);
        }
        let out = self.run_checked(c, "git ls-files --others")?;
        Ok(out
            .stdout
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect())
    }

    pub fn apply_cached(
        &self,
        index_path: &Path,
        patch: &[u8],
        three_way: bool,
        reverse: bool,
    ) -> Result<(), String> {
        if patch.is_empty() {
            return Ok(());
        }
        let mut c = self.git_cmd(Some(index_path));
        c.args(["apply", "--cached", "--whitespace=nowarn"]);
        if three_way {
            c.arg("--3way");
        }
        if reverse {
            c.arg("-R");
        }
        c.arg("-");
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| format!("git apply --cached: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(patch)
                .map_err(|e| format!("write patch stdin: {e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("git apply wait: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git apply --cached: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    pub fn apply_worktree_reverse(&self, patch: &[u8]) -> Result<(), String> {
        if patch.is_empty() {
            return Ok(());
        }
        let mut c = self.git_cmd(None);
        c.args(["apply", "-R", "--whitespace=nowarn", "-"]);
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| format!("git apply -R: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(patch)
                .map_err(|e| format!("write patch stdin: {e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("git apply -R wait: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git apply -R: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    pub fn add_paths(
        &self,
        index_path: &Path,
        paths: &[String],
        all: bool,
    ) -> Result<(), String> {
        let mut c = self.git_cmd(Some(index_path));
        c.arg("add");
        if all {
            c.arg("-A");
        } else {
            c.arg("--");
            c.args(paths);
        }
        self.run_checked(c, "git add")?;
        Ok(())
    }

    pub fn add_interactive(&self, index_path: &Path, paths: &[String]) -> Result<(), String> {
        let mut c = self.git_cmd(Some(index_path));
        c.args(["add", "-p"]);
        if !paths.is_empty() {
            c.arg("--");
            c.args(paths);
        }
        let status = c
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| format!("git add -p: {e}"))?;
        if !status.success() {
            return Err(format!("git add -p exited with {status}"));
        }
        Ok(())
    }

    pub fn reset_patch_interactive(
        &self,
        index_path: &Path,
        tree_oid: &str,
        paths: &[String],
    ) -> Result<(), String> {
        let mut c = self.git_cmd(Some(index_path));
        c.args(["reset", "-p", tree_oid]);
        if !paths.is_empty() {
            c.arg("--");
            c.args(paths);
        }
        let status = c
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| format!("git reset -p: {e}"))?;
        if !status.success() {
            return Err(format!("git reset -p exited with {status}"));
        }
        Ok(())
    }

    pub fn commit_interactive(&self, index_path: &Path, extra_args: &[String]) -> Result<(), String> {
        let mut c = self.git_cmd(Some(index_path));
        c.arg("commit");
        c.args(extra_args);
        let status = c
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| format!("git commit: {e}"))?;
        if !status.success() {
            return Err(format!("git commit exited with {status}"));
        }
        Ok(())
    }

    pub fn remove_untracked_files(&self, paths: &[String]) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut c = self.git_cmd(None);
        c.args(["clean", "-fd", "--"]);
        c.args(paths);
        self.run_checked(c, "git clean")?;
        Ok(())
    }
}

fn parse_z_name_status(raw: &[u8]) -> Vec<(String, String)> {
    let mut parts = raw.split(|&b| b == 0).filter(|s| !s.is_empty());
    let mut out = Vec::new();
    while let Some(status) = parts.next() {
        let Some(path) = parts.next() else {
            break;
        };
        out.push((
            String::from_utf8_lossy(status).to_string(),
            String::from_utf8_lossy(path).to_string(),
        ));
    }
    out
}
