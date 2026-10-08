//! Thin wrapper around the system `git` binary, so the user's SSH keys,
//! credential helpers and config just work.

use std::path::Path;
use std::process::{Command, Stdio};

pub fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    run_bytes(dir, args).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Like `run`, but keeps stdout as raw bytes (for images from history).
pub fn run_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = command(dir, args).output().map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let sub = args.iter().find(|a| !a.starts_with('-') && !a.contains('=')).unwrap_or(&"");
        Err(format!("git {sub}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    // print non-ASCII paths as-is instead of "\303\251"-quoted
    cmd.args(["-c", "core.quotepath=false"]).args(args).current_dir(dir).stdin(Stdio::null()).env("GIT_TERMINAL_PROMPT", "0");
    // never block on an ssh password/host-key prompt nobody can see
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    // a GUI app on Windows would otherwise flash a console per git call
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

/// Three-way merge of whole texts with `git merge-file`: the result, and
/// whether it still holds (diff3) conflict markers.
pub fn merge_text(mine: &str, base: &str, theirs: &str) -> Result<(String, bool), String> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let tmp = std::env::temp_dir().join(format!("ez-notes-merge-{}-{nanos}", std::process::id()));
    // private: it holds the note's full text
    let mut dir = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
    dir.create(&tmp).map_err(|e| e.to_string())?;
    let result = (|| {
        for (name, text) in [("mine", mine), ("base", base), ("theirs", theirs)] {
            std::fs::write(tmp.join(name), text).map_err(|e| e.to_string())?;
        }
        let args = ["merge-file", "-p", "--diff3", "-L", "mine", "-L", "base", "-L", "theirs", "mine", "base", "theirs"];
        let out = command(&tmp, &args).output().map_err(|e| format!("could not run git: {e}"))?;
        // exit code = number of conflicts; negative (None on signal) = failure
        match out.status.code() {
            Some(n) if n >= 0 => Ok((String::from_utf8_lossy(&out.stdout).into_owned(), n > 0)),
            _ => Err(format!("git merge-file: {}", String::from_utf8_lossy(&out.stderr).trim())),
        }
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

pub fn clone(url: &str, dest: &Path) -> Result<(), String> {
    let parent = dest.parent().unwrap();
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    run(parent, &["clone", url, &dest.to_string_lossy()]).map(drop)
}

fn has_upstream(dir: &Path) -> bool {
    run(dir, &["rev-parse", "--verify", "-q", "@{u}"]).is_ok()
}

/// Shown when a sync stopped on a merge conflict the user has to resolve.
pub const CONFLICT: &str = "merge conflict: choose what to keep to finish syncing";

/// A merge is waiting for conflicts to be resolved.
pub fn merging(dir: &Path) -> bool {
    run(dir, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).is_ok()
}

/// Paths (relative to the repo) that still have conflicts.
pub fn conflicts(dir: &Path) -> Vec<String> {
    run(dir, &["diff", "--name-only", "--diff-filter=U"]).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
}

pub fn pull(dir: &Path) -> Result<(), String> {
    if merging(dir) {
        return Err(CONFLICT.into());
    }
    // a freshly cloned empty repo has nothing to pull yet
    if !has_upstream(dir) {
        return Ok(());
    }
    let Err(e) = run(dir, &["pull", "--rebase", "--autostash"]) else { return Ok(()) };
    let conflicted = !conflicts(dir).is_empty();
    let _ = run(dir, &["rebase", "--abort"]);
    if !conflicted {
        return Err(e);
    }
    // A rebase replays every autosave commit and could stop on each one; a
    // merge asks once. diff3 markers also carry the common base.
    match run(dir, &["-c", "merge.conflictStyle=diff3", "pull", "--no-rebase", "--ff", "--no-edit", "--autostash"]) {
        Ok(_) => Ok(()),
        Err(_) if merging(dir) => Err(CONFLICT.into()),
        Err(e) => Err(e),
    }
}

/// Commit a merge whose conflicts are all staged, then push it.
pub fn finish_merge(dir: &Path) -> Result<(), String> {
    run(dir, &["commit", "--no-edit"])?;
    commit_push(dir, "merged remote changes")
}

/// Stage everything, commit if anything changed, push if ahead.
pub fn commit_push(dir: &Path, msg: &str) -> Result<(), String> {
    // `add -A` would commit the conflict markers
    if merging(dir) {
        return Err(CONFLICT.into());
    }
    run(dir, &["add", "-A"])?;
    if !run(dir, &["status", "--porcelain"])?.trim().is_empty() {
        run(dir, &["commit", "-m", msg])?;
    }
    if has_upstream(dir) && run(dir, &["rev-list", "--count", "@{u}..HEAD"])?.trim() == "0" {
        return Ok(());
    }
    if run(dir, &["push", "-u", "origin", "HEAD"]).is_ok() {
        return Ok(());
    }
    // probably behind: rebase onto the remote and try once more
    pull(dir)?;
    run(dir, &["push", "-u", "origin", "HEAD"]).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_commit_push_and_rebase() {
        let root = std::env::temp_dir().join(format!("ez-notes-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        run(&root, &["init", "--bare", "-q", "remote.git"]).unwrap();
        let url = root.join("remote.git").to_string_lossy().into_owned();
        let (a, b) = (root.join("a"), root.join("b"));
        clone(&url, &a).unwrap(); // empty remote
        for d in [&a] {
            run(d, &["config", "user.email", "t@t"]).unwrap();
            run(d, &["config", "user.name", "t"]).unwrap();
        }
        pull(&a).unwrap(); // no upstream yet: no-op
        std::fs::write(a.join("one.md"), "1").unwrap();
        commit_push(&a, "updated one.md").unwrap();

        clone(&url, &b).unwrap();
        run(&b, &["config", "user.email", "t@t"]).unwrap();
        run(&b, &["config", "user.name", "t"]).unwrap();
        std::fs::write(b.join("two.md"), "2").unwrap();
        commit_push(&b, "updated two.md").unwrap();

        // `a` is now behind; its push must rebase and succeed
        std::fs::write(a.join("one.md"), "1b").unwrap();
        commit_push(&a, "updated one.md").unwrap();
        let log = run(&root.join("remote.git"), &["log", "--format=%s"]).unwrap();
        assert_eq!(log.lines().collect::<Vec<_>>(), ["updated one.md", "updated two.md", "updated one.md"]);
        // nothing changed: no new commit
        commit_push(&a, "updated one.md").unwrap();
        assert_eq!(run(&root.join("remote.git"), &["rev-list", "--count", "HEAD"]).unwrap().trim(), "3");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn merges_texts() {
        let base = "a\nb\nc\n";
        assert_eq!(merge_text("A\nb\nc\n", base, "a\nb\nC\n").unwrap(), ("A\nb\nC\n".into(), false));
        let (text, conflicted) = merge_text("a\nmine\nc\n", base, "a\ntheirs\nc\n").unwrap();
        assert!(conflicted && text.contains("<<<<<<< mine") && text.contains("||||||| base"));
    }

    #[test]
    fn conflict_merges_once_and_pushes() {
        let root = std::env::temp_dir().join(format!("ez-notes-conflict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        run(&root, &["init", "--bare", "-q", "remote.git"]).unwrap();
        let url = root.join("remote.git").to_string_lossy().into_owned();
        let (a, b) = (root.join("a"), root.join("b"));
        clone(&url, &a).unwrap();
        let ident = |d: &Path| {
            run(d, &["config", "user.email", "t@t"]).unwrap();
            run(d, &["config", "user.name", "t"]).unwrap();
        };
        ident(&a);
        std::fs::write(a.join("n.md"), "one\ntwo\nthree\n").unwrap();
        commit_push(&a, "created n.md").unwrap();
        clone(&url, &b).unwrap();
        ident(&b);
        std::fs::write(b.join("n.md"), "one\nTWO from b\nthree\n").unwrap();
        commit_push(&b, "updated n.md").unwrap();

        // two local autosaves touching the same line: one merge, not two rebase stops
        std::fs::write(a.join("n.md"), "one\ntwo from a\nthree\n").unwrap();
        run(&a, &["commit", "-qam", "updated n.md"]).unwrap();
        std::fs::write(a.join("n.md"), "one\nTwo from a\nthree\n").unwrap();
        assert_eq!(commit_push(&a, "updated n.md").unwrap_err(), CONFLICT);
        assert!(merging(&a));
        assert_eq!(conflicts(&a), ["n.md"]);
        let marked = std::fs::read_to_string(a.join("n.md")).unwrap();
        assert!(marked.contains("<<<<<<<") && marked.contains("|||||||"), "diff3 markers: {marked}");
        assert_eq!(commit_push(&a, "updated n.md").unwrap_err(), CONFLICT, "never commits markers");
        assert_eq!(pull(&a).unwrap_err(), CONFLICT);

        std::fs::write(a.join("n.md"), "one\nTwo from a\nTWO from b\nthree\n").unwrap();
        run(&a, &["add", "n.md"]).unwrap();
        finish_merge(&a).unwrap();
        assert!(!merging(&a));
        let remote = root.join("remote.git");
        assert_eq!(run(&remote, &["show", "HEAD:n.md"]).unwrap(), "one\nTwo from a\nTWO from b\nthree\n");
        assert_eq!(run(&remote, &["rev-list", "--count", "--merges", "HEAD"]).unwrap().trim(), "1");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
