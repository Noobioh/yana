//! Thin wrapper around the system `git` binary, so the user's SSH keys,
//! credential helpers and config just work.

use std::path::Path;
use std::process::{Command, Stdio};

pub fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir).stdin(Stdio::null()).env("GIT_TERMINAL_PROMPT", "0");
    // never block on an ssh password/host-key prompt nobody can see
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    // a GUI app on Windows would otherwise flash a console per git call
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000); // CREATE_NO_WINDOW
    let out = cmd.output().map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("git {}: {}", args[0], String::from_utf8_lossy(&out.stderr).trim()))
    }
}

pub fn clone(url: &str, dest: &Path) -> Result<(), String> {
    let parent = dest.parent().unwrap();
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    run(parent, &["clone", url, &dest.to_string_lossy()]).map(drop)
}

fn has_upstream(dir: &Path) -> bool {
    run(dir, &["rev-parse", "--verify", "-q", "@{u}"]).is_ok()
}

pub fn pull(dir: &Path) -> Result<(), String> {
    // a freshly cloned empty repo has nothing to pull yet
    if !has_upstream(dir) {
        return Ok(());
    }
    if let Err(e) = run(dir, &["pull", "--rebase", "--autostash"]) {
        let _ = run(dir, &["rebase", "--abort"]);
        return Err(e);
    }
    Ok(())
}

/// Stage everything, commit if anything changed, push if ahead.
pub fn commit_push(dir: &Path, msg: &str) -> Result<(), String> {
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
}
