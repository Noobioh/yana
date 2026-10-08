//! Version history of one note: list its commits, diff them, restore one.

use crate::theme::{self, Pal, i, icon, weight};
use crate::{Message, btn, git, notes};
use iced::font::Weight::{Medium, Semibold};
use iced::widget::{button, column, container, row, rule, scrollable, space, text};
use iced::{Element, Fill};
use std::path::Path;

/// Above this many diff lines, ask before rendering.
pub const LARGE_DIFF_LINES: usize = 2000;

#[derive(Debug, Clone, PartialEq)]
pub struct Version {
    pub sha: String,
    pub short: String,
    pub author: String,
    /// "3 hours ago"
    pub ago: String,
    /// "08 Oct 2026 14:05", local time
    pub date: String,
    pub subject: String,
    /// The note's `.md` path at this commit; changes when the note is renamed.
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// What this commit changed.
    Changes,
    /// This version against the note as it is now.
    VsCurrent,
}

pub struct History {
    pub versions: Vec<Version>,
    pub selected: usize,
    pub mode: Mode,
    /// None while loading.
    pub diff: Option<Result<String, String>>,
    pub show_large: bool,
    pub error: Option<String>,
}

impl History {
    pub fn new() -> Self {
        History { versions: Vec::new(), selected: 0, mode: Mode::Changes, diff: None, show_large: false, error: None }
    }
}

/// Repo-relative path with forward slashes, as git prints it.
pub fn rel(repo: &Path, path: &Path) -> String {
    let r = path.strip_prefix(repo).unwrap_or(path);
    r.iter().map(|c| c.to_string_lossy()).collect::<Vec<_>>().join("/")
}

const FORMAT: &str = "--format=%x1e%H%x1f%h%x1f%an%x1f%ar%x1f%ad%x1f%s";

pub fn load(repo: &Path, md_rel: &str) -> Result<Vec<Version>, String> {
    let out = git::run(repo, &["log", "--follow", FORMAT, "--date=format-local:%d %b %Y %H:%M", "--name-only", "--", md_rel])?;
    Ok(parse_log(&out, md_rel))
}

pub fn parse_log(out: &str, current: &str) -> Vec<Version> {
    let mut versions: Vec<Version> = Vec::new();
    for rec in out.split('\x1e').filter(|r| !r.trim().is_empty()) {
        let (head, files) = rec.split_once('\n').unwrap_or((rec, ""));
        let f: Vec<&str> = head.split('\x1f').collect();
        if f.len() < 6 {
            continue;
        }
        // merge commits list no files: they keep the newer neighbour's path
        let path = files.lines().find(|l| !l.trim().is_empty()).map(str::to_string);
        let path = path.or_else(|| versions.last().map(|v| v.path.clone())).unwrap_or_else(|| current.to_string());
        versions.push(Version {
            sha: f[0].into(),
            short: f[1].into(),
            author: f[2].into(),
            ago: f[3].into(),
            date: f[4].into(),
            subject: f[5..].join("\x1f"),
            path,
        });
    }
    versions
}

/// Diff for `versions[idx]` in the given mode.
pub fn diff(repo: &Path, versions: &[Version], idx: usize, mode: Mode, current: &str) -> Result<String, String> {
    let v = &versions[idx];
    match mode {
        Mode::Changes => {
            // the older version's path pairs up renames (-M)
            let older = versions.get(idx + 1).map_or(v.path.as_str(), |o| o.path.as_str());
            git::run(repo, &["show", "--format=", "--no-color", "-M", "--diff-merges=first-parent", &v.sha, "--", older, &v.path])
        }
        Mode::VsCurrent => git::run(repo, &["diff", "--no-color", "-M", &v.sha, "--", &v.path, current]),
    }
}

/// Write the note folder as it was in `v` into `note_dir` (its current
/// location). Files added since are left alone; nothing is deleted.
pub fn restore(repo: &Path, v: &Version, note_dir: &Path) -> Result<(), String> {
    let old_dir = v.path.rsplit_once('/').map_or("", |(d, _)| d);
    let listing = git::run(repo, &["ls-tree", &v.sha, "--", &format!("{old_dir}/")])?;
    for line in listing.lines() {
        // "<mode> <type> <sha>\t<path>"
        let Some((meta, path)) = line.split_once('\t') else { continue };
        if !meta.contains(" blob ") {
            continue;
        }
        let dest = if path == v.path {
            notes::md_path(note_dir)
        } else {
            note_dir.join(path.rsplit('/').next().unwrap_or(path))
        };
        let bytes = git::run_bytes(repo, &["show", &format!("{}:{path}", v.sha)])?;
        std::fs::write(&dest, bytes).map_err(|e| format!("could not write {}: {e}", dest.display()))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineKind {
    Added,
    Removed,
    Context,
    Hunk,
    /// diff/index/---/+++ headers; not shown
    Meta,
}

/// Each line of a diff with its kind. `---`/`+++` are headers only before a
/// file's first hunk; inside one they are a removed `--…` or added `++…` line.
pub fn classify(diff: &str) -> Vec<(LineKind, &str)> {
    let mut in_hunk = false;
    diff.lines()
        .map(|line| {
            in_hunk &= !line.starts_with("diff ");
            let kind = match line.as_bytes().first() {
                Some(b'+') if in_hunk => LineKind::Added,
                Some(b'-') if in_hunk => LineKind::Removed,
                Some(b' ') if in_hunk => LineKind::Context,
                _ if line.starts_with("@@") => LineKind::Hunk,
                _ => LineKind::Meta,
            };
            in_hunk |= kind == LineKind::Hunk;
            (kind, line)
        })
        .collect()
}

pub fn is_large(diff: &str) -> bool {
    diff.lines().count() > LARGE_DIFF_LINES
}

/// `saving`: a commit is still running; restoring now could race it.
pub fn view<'a>(h: &'a History, title: String, saving: bool, p: Pal) -> Element<'a, Message> {
    let header = row![
        btn(Some(i::ARROW_LEFT), "Back to note", p.ghost()).on_press(Message::HistoryClose),
        text(format!("History of {title}")).size(18).font(weight(Semibold)).width(Fill),
        text(format!("{} versions", h.versions.len())).font(theme::MONO).size(12).color(p.faint),
    ]
    .spacing(12)
    .align_y(iced::Center);

    if let Some(e) = &h.error {
        return column![header, notice(i::ALERT, e.clone(), String::new(), None, p)].spacing(16).padding(16).into();
    }

    let list = h.versions.iter().enumerate().map(|(idx, v)| {
        let item = column![
            text(&v.subject).size(13).font(weight(Medium)).wrapping(text::Wrapping::None),
            text(format!("{} · {}", v.author, v.ago)).size(12).color(p.muted).wrapping(text::Wrapping::None),
        ]
        .spacing(2);
        button(item).width(Fill).padding([8, 10]).style(p.row(idx == h.selected)).on_press(Message::HistorySelect(idx)).into()
    });
    let list = container(scrollable(column(list).spacing(2).padding(8)).height(Fill)).width(280).height(Fill).style(p.group());

    let Some(v) = h.versions.get(h.selected) else {
        return column![header, text("No saved versions yet.").color(p.muted)].spacing(16).padding(16).into();
    };
    let mode = |label: &'static str, m: Mode| {
        button(text(label).size(13).font(weight(Medium))).padding([7, 10]).style(p.ghost_with(p.text, h.mode == m)).on_press(Message::HistoryMode(m))
    };
    let detail_head = column![
        row![
            column![
                text(&v.subject).size(16).font(weight(Semibold)),
                text(format!("{} · {} · {}", v.author, v.date, v.short)).font(theme::MONO).size(12).color(p.muted),
            ]
            .spacing(4)
            .width(Fill),
            btn(Some(i::HISTORY), if saving { "Saving…" } else { "Restore this version" }, p.primary())
                .on_press_maybe((h.selected > 0 && !saving).then_some(Message::Restore)),
        ]
        .spacing(12)
        .align_y(iced::Center),
        container(row![mode("Changes in this version", Mode::Changes), mode("Compare with current", Mode::VsCurrent)].spacing(2))
            .padding(2)
            .style(p.group()),
    ]
    .spacing(12);

    let body: Element<'a, Message> = match &h.diff {
        None => text("Loading…").color(p.muted).into(),
        Some(Err(e)) => notice(i::ALERT, e.clone(), String::new(), None, p),
        Some(Ok(d)) if d.trim().is_empty() => text(match h.mode {
            Mode::Changes => "This version made no changes to the note text.",
            Mode::VsCurrent => "This version is the same as the current note.",
        })
        .color(p.muted)
        .into(),
        Some(Ok(d)) if d.lines().any(|l| l.starts_with("Binary files")) => text("Binary file; no diff to show.").color(p.muted).into(),
        Some(Ok(d)) if is_large(d) && !h.show_large => notice(
            i::ALERT,
            format!("This diff is very large ({} lines).", d.lines().count()),
            "Showing it may make Yana slow for a moment.".into(),
            Some(Message::ShowLargeDiff),
            p,
        ),
        Some(Ok(d)) => diff_view(d, p),
    };
    let detail = column![detail_head, rule::horizontal(1).style(p.rule()), body].spacing(12).width(Fill).height(Fill);
    column![header, row![list, detail].spacing(16).height(Fill)].spacing(16).padding(16).height(Fill).into()
}

fn notice<'a>(glyph: char, title: String, sub: String, action: Option<Message>, p: Pal) -> Element<'a, Message> {
    let mut col = column![row![icon(glyph).color(p.tag), text(title).size(14).font(weight(Medium))].spacing(8).align_y(iced::Center)].spacing(8);
    if !sub.is_empty() {
        col = col.push(text(sub).size(13).color(p.muted));
    }
    if let Some(m) = action {
        col = col.push(btn(None, "Show diff", p.secondary()).on_press(m));
    }
    container(col).padding(16).width(Fill).style(p.group()).into()
}

fn diff_view<'a>(d: &str, p: Pal) -> Element<'a, Message> {
    let rows = classify(d).into_iter().filter_map(|(kind, line)| {
        let (bg, fg) = match kind {
            LineKind::Meta => return None,
            LineKind::Added => (Some(theme::alpha(p.signal, 0.14)), p.text),
            LineKind::Removed => (Some(theme::alpha(p.danger, 0.14)), p.text),
            LineKind::Context => (None, p.muted),
            LineKind::Hunk => (None, p.faint),
        };
        let line = container(text(line.to_string()).font(theme::MONO).size(12).color(fg)).width(Fill).padding([1, 8]);
        Some(match bg {
            Some(bg) => line.style(p.fill(bg, 0.0)).into(),
            None => line.into(),
        })
    });
    scrollable(column(rows).push(space().height(8))).height(Fill).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_log_across_renames_and_merges() {
        let out = "\x1eaaa\x1fa\x1fJo\x1f1 hour ago\x1f08 Oct 2026 10:00\x1frenamed Plan to Road\n\nWork/Road/Road.md\n\
                   \x1emmm\x1fm\x1fKim\x1f2 hours ago\x1f08 Oct 2026 09:00\x1fMerge branch 'main'\n\
                   \x1ebbb\x1fb\x1fJo\x1f3 hours ago\x1f08 Oct 2026 08:00\x1fupdated Plan.md\n\nWork/Plan/Plan.md\n";
        let v = parse_log(out, "Work/Road/Road.md");
        assert_eq!(v.len(), 3);
        assert_eq!((v[0].short.as_str(), v[0].path.as_str()), ("a", "Work/Road/Road.md"));
        assert_eq!(v[1].path, "Work/Road/Road.md", "merge keeps newer path");
        assert_eq!((v[2].author.as_str(), v[2].path.as_str()), ("Jo", "Work/Plan/Plan.md"));
    }

    #[test]
    fn classifies_diff_lines_and_size() {
        let d = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n same\n----\n+++x\ndiff --git a/y b/y\n--- a/y\n";
        let kinds: Vec<_> = classify(d).into_iter().map(|(k, _)| k).collect();
        use LineKind::*;
        assert_eq!(kinds, [Meta, Meta, Meta, Hunk, Removed, Added, Context, Removed, Added, Meta, Meta], "a removed `---` rule stays visible");
        assert!(!is_large(&"+x\n".repeat(LARGE_DIFF_LINES)));
        assert!(is_large(&"+x\n".repeat(LARGE_DIFF_LINES + 500)));
    }

    #[test]
    fn history_diff_and_restore_after_rename() {
        let root = std::env::temp_dir().join(format!("ez-notes-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let note = notes::create_note(&notes::create_folder(&root, "Work").unwrap(), "Plan").unwrap();
        git::run(&root, &["init", "-q"]).unwrap();
        git::run(&root, &["config", "user.email", "t@t"]).unwrap();
        git::run(&root, &["config", "user.name", "t"]).unwrap();
        let save = |dir: &Path, body: &str, msg: &str| {
            std::fs::write(notes::md_path(dir), body).unwrap();
            git::run(&root, &["add", "-A"]).unwrap();
            git::run(&root, &["commit", "-qm", msg]).unwrap();
        };
        std::fs::write(note.join("pic.png"), [0u8, 159, 146, 150]).unwrap(); // not UTF-8
        save(&note, "first\n", "v1");
        std::fs::remove_file(note.join("pic.png")).unwrap();
        save(&note, "second\n", "v2");
        let note = notes::rename(&note, "Road").unwrap(); // the app commits a rename on its own
        save(&note, "second\n", "renamed");
        save(&note, "third\n", "v3");

        let cur = rel(&root, &notes::md_path(&note));
        let v = load(&root, &cur).unwrap();
        assert_eq!(v.iter().map(|v| v.subject.as_str()).collect::<Vec<_>>(), ["v3", "renamed", "v2", "v1"]);
        assert_eq!(v[3].path, "Work/Plan/Plan.md");
        assert!(diff(&root, &v, 0, Mode::Changes, &cur).unwrap().contains("+third"));
        let vs = diff(&root, &v, 3, Mode::VsCurrent, &cur).unwrap();
        assert!(vs.contains("-first") && vs.contains("+third"), "{vs}");

        restore(&root, &v[3], &note).unwrap();
        assert_eq!(std::fs::read_to_string(notes::md_path(&note)).unwrap(), "first\n");
        assert_eq!(std::fs::read(note.join("pic.png")).unwrap(), [0u8, 159, 146, 150], "assets come back byte-exact");
        assert!(!root.join("Work/Plan").exists(), "old location not recreated");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
