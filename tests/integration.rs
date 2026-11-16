use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempRepo {
    dir: PathBuf,
}

impl TempRepo {
    fn new() -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("gd-test-{}-{id}", std::process::id()));
        if dir.exists() {
            let _ = fs::remove_dir_all(&dir);
        }
        fs::create_dir_all(&dir).unwrap();
        let repo = Self { dir };
        repo.git(&["init"]);
        repo.git(&["config", "user.name", "Test User"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo
    }

    fn git(&self, args: &[&str]) -> Output {
        let out = Command::new("git")
            .current_dir(&self.dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn gl(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_gd"))
            .current_dir(&self.dir)
            .args(args)
            .output()
            .unwrap()
    }

    fn gl_ok(&self, args: &[&str]) -> String {
        let out = self.gl(args);
        assert!(
            out.status.success(),
            "gd {:?} failed:\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn gl_stdin_ok(&self, args: &[&str], input: &str) -> String {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gd"))
            .current_dir(&self.dir)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "gd {:?} failed:\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.dir.join(rel)).unwrap()
    }

    fn exists(&self, rel: &str) -> bool {
        Path::new(&self.dir.join(rel)).exists()
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn test_unborn_init_stage_promote_partial_commit() {
    let r = TempRepo::new();
    r.gl_ok(&["init"]);
    r.write("a.txt", "alpha\n");
    r.write("b.txt", "beta\n");
    r.write("c.txt", "gamma\n");

    // Stage all into top layer (draft)
    r.gl_ok(&["stage", "-A"]);
    let st = r.gl_ok(&["status"]);
    assert!(st.contains("[L3] draft (3 files)"), "{st}");

    // Promote a.txt and b.txt all the way to ready
    r.gl_ok(&[
        "promote", "--from", "draft", "--to", "ready", "a.txt", "b.txt",
    ]);
    let st = r.gl_ok(&["status"]);
    assert!(st.contains("[L1] ready (commit target) (2 files)"), "{st}");
    assert!(st.contains("[L2] reviewed (0 files)"), "{st}");
    assert!(st.contains("[L3] draft (1 files)"), "{st}");

    // Partial commit: only a.txt
    r.gl_ok(&["commit", "a.txt", "-m", "initial a"]);
    let st = r.gl_ok(&["status"]);
    assert!(st.contains("[L1] ready (commit target) (1 files)"), "{st}");
    assert!(st.contains("b.txt"), "{st}");
    assert!(st.contains("[L3] draft (1 files)"), "{st}");

    // Commit remaining L1 (b.txt)
    r.gl_ok(&["commit", "-m", "commit b"]);
    let st = r.gl_ok(&["status"]);
    assert!(st.contains("[L1] ready (commit target) (0 files)"), "{st}");
    assert!(st.contains("[L3] draft (1 files)"), "{st}");
    assert!(st.contains("c.txt"), "{st}");
}

#[test]
fn test_demote_drop_and_undo() {
    let r = TempRepo::new();
    r.write("f.txt", "v1\n");
    r.git(&["add", "f.txt"]);
    r.git(&["commit", "-m", "init"]);

    r.gl_ok(&["init"]);
    r.write("f.txt", "v2\n");
    r.gl_ok(&["stage", "f.txt"]);
    r.gl_ok(&["promote", "f.txt"]); // draft -> reviewed
    r.gl_ok(&["promote", "f.txt"]); // reviewed -> ready

    // Demote ready -> reviewed
    r.gl_ok(&["demote", "f.txt"]);
    let st = r.gl_ok(&["status"]);
    assert!(st.contains("[L1] ready (commit target) (0 files)"), "{st}");
    assert!(st.contains("[L2] reviewed (1 files)"), "{st}");

    // Undo restores ready
    r.gl_ok(&["undo"]);
    let st = r.gl_ok(&["status"]);
    assert!(st.contains("[L1] ready (commit target) (1 files)"), "{st}");

    // Drop --cascade returns to WT
    r.gl_ok(&["drop", "ready", "--cascade", "f.txt"]);
    let st = r.gl_ok(&["status"]);
    assert!(st.contains("[L1] ready (commit target) (0 files)"), "{st}");
    assert!(
        st.contains("[WT] working-tree (1 modified, 0 untracked)"),
        "{st}"
    );
    assert_eq!(r.read("f.txt"), "v2\n");

    // Stage + drop --hard reverts WT + cleans untracked
    r.write("new.txt", "temp\n");
    r.gl_ok(&["stage", "-A"]);
    r.gl_ok(&["drop", "ready", "--hard", "-A"]);
    assert_eq!(r.read("f.txt"), "v1\n");
    assert!(!r.exists("new.txt"));
}

#[test]
fn test_dynamic_layers_and_head_drift() {
    let r = TempRepo::new();
    r.write("base.txt", "line1\n");
    r.write("other.txt", "one\n");
    r.git(&["add", "."]);
    r.git(&["commit", "-m", "init"]);

    r.gl_ok(&["init", "ready", "draft"]);
    r.gl_ok(&["layer", "add", "polish", "--above", "ready"]);
    let list = r.gl_ok(&["layer", "list"]);
    assert!(list.contains("L1: ready"), "{list}");
    assert!(list.contains("L2: polish"), "{list}");
    assert!(list.contains("L3: draft"), "{list}");

    r.write("base.txt", "line1\nline2\n");
    r.gl_ok(&["stage", "base.txt"]);
    r.gl_ok(&["promote", "base.txt"]); // draft -> polish

    // External git commit on unrelated file (HEAD drift)
    r.write("other.txt", "two\n");
    r.git(&["add", "other.txt"]);
    r.git(&["commit", "-m", "external"]);

    let st = r.gl_ok(&["status"]);
    assert!(st.contains("HEAD moved externally"), "{st}");
    assert!(st.contains("[L2] polish (1 files)"), "{st}");

    // Rename and merge-down remove
    r.gl_ok(&["layer", "rename", "polish", "qa"]);
    r.gl_ok(&["layer", "rm", "qa", "--merge-down"]);
    let st = r.gl_ok(&["status"]);
    assert!(st.contains("[L1] ready (commit target) (1 files)"), "{st}");
    assert!(st.contains("base.txt"), "{st}");
}

#[test]
fn test_interactive_patch_promote_and_commit() {
    let r = TempRepo::new();
    let initial = (1..=20).map(|i| format!("line {i}\n")).collect::<String>();
    r.write("multi.txt", &initial);
    r.git(&["add", "multi.txt"]);
    r.git(&["commit", "-m", "init"]);
    r.gl_ok(&["init"]);

    // Two well-separated hunks (line 2 and line 19)
    let modified = (1..=20)
        .map(|i| {
            if i == 2 {
                "line 2 CHANGED A\n".to_string()
            } else if i == 19 {
                "line 19 CHANGED B\n".to_string()
            } else {
                format!("line {i}\n")
            }
        })
        .collect::<String>();
    r.write("multi.txt", &modified);
    r.gl_ok(&["stage", "multi.txt"]);

    // Promote only first hunk from draft -> ready (`y\nn\n`)
    r.gl_stdin_ok(
        &[
            "promote",
            "--from",
            "draft",
            "--to",
            "ready",
            "-p",
            "multi.txt",
        ],
        "y\nn\n",
    );

    let diff_ready = r.gl_ok(&["diff", "ready"]);
    let diff_reviewed = r.gl_ok(&["diff", "reviewed"]);
    let diff_draft = r.gl_ok(&["diff", "draft"]);
    assert!(diff_ready.contains("CHANGED A"), "{diff_ready}");
    assert!(!diff_ready.contains("CHANGED B"), "{diff_ready}");
    // Monotonic invariant: ready & reviewed both have A; draft has A+B => delta in reviewed is empty, delta in draft is B!
    assert!(
        diff_reviewed.is_empty(),
        "expected empty reviewed delta, got: {diff_reviewed}"
    );
    assert!(diff_draft.contains("CHANGED B"), "{diff_draft}");
    assert!(!diff_draft.contains("CHANGED A"), "{diff_draft}");

    // Partial interactive commit from ready (`y\n`)
    r.gl_stdin_ok(&["commit", "-p", "-m", "commit hunk A"], "y\n");
    let diff_ready_after = r.gl_ok(&["diff", "ready"]);
    let diff_draft_after = r.gl_ok(&["diff", "draft"]);
    assert!(diff_ready_after.is_empty(), "{diff_ready_after}");
    assert!(diff_draft_after.contains("CHANGED B"), "{diff_draft_after}");
}

#[test]
fn test_ide_json_blob_patch_apply_and_sync_git_index() {
    let r = TempRepo::new();
    r.write("app.txt", "one\ntwo\n");
    r.git(&["add", "app.txt"]);
    r.git(&["commit", "-m", "init"]);

    r.gl_ok(&["init", "--sync-git-index"]);
    r.write("app.txt", "one\ntwo modified\n");
    r.gl_ok(&["stage", "app.txt"]);

    // JSON status check
    let json = r.gl_ok(&["status", "--json"]);
    assert!(json.contains("\"sync_git_index\":true"), "{json}");
    assert!(json.contains("\"name\":\"draft\""), "{json}");
    assert!(json.contains("\"path\":\"app.txt\""), "{json}");

    // Blob checks
    assert_eq!(r.gl_ok(&["blob", "head", "app.txt"]), "one\ntwo\n");
    assert_eq!(
        r.gl_ok(&["blob", "draft", "app.txt"]),
        "one\ntwo modified\n"
    );
    assert_eq!(
        r.gl_ok(&["blob", "head", "missing.txt", "--allow-missing"]),
        ""
    );

    // Non-interactive patch-apply from draft -> ready
    let patch = r.gl_ok(&["diff", "draft"]);
    r.gl_stdin_ok(&["patch-apply", "--from", "draft", "--to", "ready"], &patch);

    // Native git index should mirror L1 (ready)!
    let native_staged =
        String::from_utf8_lossy(&r.git(&["diff", "--cached", "--name-only"]).stdout).to_string();
    assert_eq!(native_staged.trim(), "app.txt");

    // Non-interactive patch-apply demote ready -> reviewed
    r.gl_stdin_ok(
        &["patch-apply", "--from", "ready", "--to", "reviewed"],
        &patch,
    );
    let native_staged_after =
        String::from_utf8_lossy(&r.git(&["diff", "--cached", "--name-only"]).stdout).to_string();
    assert!(
        native_staged_after.trim().is_empty(),
        "{native_staged_after}"
    );

    // Toggle config off
    r.gl_ok(&["config", "sync-git-index", "false"]);
    let json2 = r.gl_ok(&["status", "--json"]);
    assert!(json2.contains("\"sync_git_index\":false"), "{json2}");

    // Turn sync back on and test reverse sync from external `git add` / `git reset`
    r.gl_ok(&["config", "sync-git-index", "true"]);
    r.write("ext.txt", "from default git\n");
    r.git(&["add", "ext.txt"]);
    let st_after_git_add = r.gl_ok(&["status"]);
    assert!(
        st_after_git_add.contains("[L1] ready (commit target) (1 files)"),
        "{st_after_git_add}"
    );
    assert!(st_after_git_add.contains("ext.txt"), "{st_after_git_add}");

    // External `git reset HEAD ext.txt` unstages from ready; app.txt + ext.txt stay in reviewed
    r.git(&["reset", "HEAD", "--", "ext.txt"]);
    let st_after_reset = r.gl_ok(&["status"]);
    assert!(
        st_after_reset.contains("[L1] ready (commit target) (0 files)"),
        "{st_after_reset}"
    );
    assert!(
        st_after_reset.contains("[L2] reviewed (2 files)"),
        "{st_after_reset}"
    );
    assert!(st_after_reset.contains("ext.txt"), "{st_after_reset}");
}
