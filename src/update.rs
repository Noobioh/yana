//! Is a newer release out? Asks GitHub for the release tags through `git ls-remote`,
//! so no HTTP client or API rate limit is involved.

const REPO: &str = "https://github.com/Noobioh/yana";
pub const RELEASES: &str = "https://github.com/Noobioh/yana/releases";

/// `v1.2.3` / `1.2.3-preview.4` -> (1, 2, 3).
// ponytail: pre-release suffix ignored, so a 0.4.1-preview build is not offered 0.4.1; compare suffixes if that matters
fn parse(v: &str) -> Option<(u32, u32, u32)> {
    let core = v.trim_start_matches('v').split('-').next()?;
    let mut it = core.split('.').map(|n| n.parse().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// Highest release tag (previews skipped) in `git ls-remote --tags --refs` output.
fn latest(ls_remote: &str) -> Option<&str> {
    ls_remote
        .lines()
        .filter_map(|l| l.split("refs/tags/").nth(1))
        .filter(|t| !t.contains('-'))
        .filter_map(|t| Some((parse(t)?, t.trim_start_matches('v'))))
        .max()
        .map(|(_, t)| t)
}

/// The newer release's version, if there is one.
pub fn check() -> Result<Option<String>, String> {
    // never ask for credentials (a renamed/private repo would pop a login dialog at startup),
    // and give up on a stalled connection instead of "Checking…" forever
    let args = ["-c", "credential.helper=", "-c", "http.lowSpeedLimit=1", "-c", "http.lowSpeedTime=10", "ls-remote", "--tags", "--refs", REPO];
    let out = crate::git::run(&std::env::temp_dir(), &args)?;
    Ok(newer(&out, env!("CARGO_PKG_VERSION")))
}

fn newer(ls_remote: &str, current: &str) -> Option<String> {
    let l = latest(ls_remote)?;
    (parse(l) > parse(current)).then(|| l.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = "a\trefs/tags/v0.9.0\nb\trefs/tags/v0.10.0\nc\trefs/tags/v0.11.0-preview.3\nd\trefs/tags/junk\n";

    #[test]
    fn finds_newer_release() {
        assert_eq!(latest(OUT), Some("0.10.0"));
        assert_eq!(newer(OUT, "0.9.0").as_deref(), Some("0.10.0"));
        assert_eq!(newer(OUT, "0.10.0"), None);
        assert_eq!(newer(OUT, "0.10.1-preview.2"), None);
        assert_eq!(newer("", "0.1.0"), None);
    }
}
