//! Resolving a merge conflict: per change pick mine / theirs / both, or edit
//! the file by hand, then commit the merge and push.

use crate::theme::{self, Pal, i, icon, weight};
use crate::{Message, btn, git};
use iced::font::Weight::{Medium, Semibold};
use iced::widget::{button, column, container, row, rule, scrollable, space, text, text_editor};
use iced::{Color, Element, Fill, Theme};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    Common(String),
    Conflict { mine: String, base: String, theirs: String },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Choice {
    Mine,
    Theirs,
    Both,
}

pub enum Kind {
    Text {
        segments: Vec<Segment>,
        choices: Vec<Option<Choice>>,
        /// Hand-edited result; replaces the choices while set.
        manual: Option<text_editor::Content>,
    },
    /// Binary, or deleted on one side: keep one whole side.
    Whole { mine: bool, theirs: bool, pick: Option<Choice> },
}

pub struct File {
    pub path: String,
    pub kind: Kind,
}

pub enum Resolution {
    Text(String),
    Mine,
    Theirs,
}

impl File {
    pub fn resolution(&self) -> Option<Resolution> {
        match &self.kind {
            Kind::Text { manual: Some(c), .. } => Some(c.text()).filter(|t| !has_markers(t)).map(Resolution::Text),
            Kind::Text { segments, choices, .. } => choices.iter().all(Option::is_some).then(|| Resolution::Text(render(segments, choices))),
            Kind::Whole { pick: Some(Choice::Theirs), .. } => Some(Resolution::Theirs),
            Kind::Whole { pick: Some(_), .. } => Some(Resolution::Mine),
            Kind::Whole { pick: None, .. } => None,
        }
    }
}

pub struct Conflict {
    pub files: Vec<File>,
    pub selected: usize,
    /// Who made the remote changes.
    pub author: String,
    pub busy: bool,
    /// A git merge (MERGE_HEAD) rather than edits that clashed with a pull
    /// while the note was open; those are only in memory until resolved.
    pub merge: bool,
}

impl Conflict {
    pub fn ready(&self) -> bool {
        !self.busy && self.files.iter().all(|f| f.resolution().is_some())
    }
}

// ponytail: marker detection is line-based; a note that itself contains a
// line starting with "<<<<<<<" or exactly "=======" can misparse. Use the
// index stages (`git show :2:path`) and `git merge-file` if that ever bites.
fn marker(line: &str, m: &str) -> bool {
    line.starts_with(m) && (m != "=======" || line.trim_end() == m)
}

pub fn has_markers(s: &str) -> bool {
    s.lines().any(|l| marker(l, "<<<<<<<") || marker(l, ">>>>>>>") || marker(l, "======="))
}

/// Split a file with (diff3) conflict markers into shared text and conflicts.
pub fn parse(s: &str) -> Vec<Segment> {
    #[derive(PartialEq)]
    enum At {
        Common,
        Mine,
        Base,
        Theirs,
    }
    let (mut out, mut at) = (Vec::new(), At::Common);
    let (mut common, mut mine, mut base, mut theirs) = (String::new(), String::new(), String::new(), String::new());
    for line in s.split_inclusive('\n') {
        match at {
            At::Common if marker(line, "<<<<<<<") => {
                if !common.is_empty() {
                    out.push(Segment::Common(std::mem::take(&mut common)));
                }
                at = At::Mine;
            }
            At::Common => common.push_str(line),
            At::Mine | At::Base if marker(line, "=======") => at = At::Theirs,
            At::Mine if marker(line, "|||||||") => at = At::Base,
            At::Mine => mine.push_str(line),
            At::Base => base.push_str(line),
            At::Theirs if marker(line, ">>>>>>>") => {
                let (m, b, t) = (std::mem::take(&mut mine), std::mem::take(&mut base), std::mem::take(&mut theirs));
                out.push(Segment::Conflict { mine: m, base: b, theirs: t });
                at = At::Common;
            }
            At::Theirs => theirs.push_str(line),
        }
    }
    if at != At::Common {
        // unterminated: keep what was read as plain text rather than lose it
        common.extend([mine, base, theirs]);
    }
    if !common.is_empty() {
        out.push(Segment::Common(common));
    }
    out
}

/// The file with each conflict replaced by its choice; unchosen ones keep markers.
pub fn render(segments: &[Segment], choices: &[Option<Choice>]) -> String {
    let mut out = String::new();
    let mut k = 0;
    for s in segments {
        match s {
            Segment::Common(t) => out.push_str(t),
            Segment::Conflict { mine, theirs, .. } => {
                match choices.get(k).copied().flatten() {
                    Some(Choice::Mine) => out.push_str(mine),
                    Some(Choice::Theirs) => out.push_str(theirs),
                    Some(Choice::Both) => {
                        out.push_str(mine);
                        out.push_str(theirs);
                    }
                    None => out.push_str(&format!("<<<<<<< mine\n{mine}=======\n{theirs}>>>>>>> theirs\n")),
                }
                k += 1;
            }
        }
    }
    out
}

/// The merge waiting in `repo`, with every conflicted file.
pub fn load(repo: &Path) -> Conflict {
    let author = git::run(repo, &["log", "-1", "--format=%an", "MERGE_HEAD"]).map(|s| s.trim().to_string()).unwrap_or_default();
    let files = git::conflicts(repo)
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(repo.join(&path)).ok().filter(|t| has_markers(t));
            let kind = match text {
                Some(t) => {
                    let segments = parse(&t);
                    let n = segments.iter().filter(|s| matches!(s, Segment::Conflict { .. })).count();
                    Kind::Text { segments, choices: vec![None; n], manual: None }
                }
                None => {
                    // "<mode> <sha> <stage>\t<path>": stage 2 = ours, 3 = theirs
                    let stages = git::run(repo, &["ls-files", "-u", "--", &path]).unwrap_or_default();
                    let has = |n: &str| stages.lines().any(|l| l.split('\t').next().is_some_and(|m| m.ends_with(&format!(" {n}"))));
                    Kind::Whole { mine: has("2"), theirs: has("3"), pick: None }
                }
            };
            File { path, kind }
        })
        .collect();
    Conflict { files, selected: 0, author: if author.is_empty() { "Remote".into() } else { author }, busy: false, merge: true }
}

/// Unsaved edits to `path` that clash with what a pull wrote to disk;
/// `text` is the `git merge-file` output with markers.
pub fn local(repo: &Path, path: String, text: &str) -> Conflict {
    let author = git::run(repo, &["log", "-1", "--format=%an", "--", &path]).map(|s| s.trim().to_string()).unwrap_or_default();
    let segments = parse(text);
    let n = segments.iter().filter(|s| matches!(s, Segment::Conflict { .. })).count();
    let file = File { path, kind: Kind::Text { segments, choices: vec![None; n], manual: None } };
    Conflict { files: vec![file], selected: 0, author: if author.is_empty() { "Someone".into() } else { author }, busy: false, merge: false }
}

/// Stage every resolution, commit the merge and push it.
pub fn finish(repo: &Path, resolved: Vec<(String, Resolution, bool)>, merge: bool) -> Result<(), String> {
    if !merge {
        // edits clashing with a pull: plain save + commit
        let mut msg = String::from("updated");
        for (path, res, _) in resolved {
            if let Resolution::Text(t) = res {
                std::fs::write(repo.join(&path), t).map_err(|e| format!("could not write {path}: {e}"))?;
                msg = format!("updated {}", path.rsplit('/').next().unwrap_or(&path));
            }
        }
        return git::commit_push(repo, &msg);
    }
    for (path, res, exists) in resolved {
        match res {
            Resolution::Text(t) => {
                std::fs::write(repo.join(&path), t).map_err(|e| format!("could not write {path}: {e}"))?;
                git::run(repo, &["add", "--", &path])?;
            }
            Resolution::Mine | Resolution::Theirs if !exists => drop(git::run(repo, &["rm", "-q", "--", &path])?),
            Resolution::Mine | Resolution::Theirs => {
                let side = if matches!(res, Resolution::Mine) { "--ours" } else { "--theirs" };
                git::run(repo, &["checkout", side, "--", &path])?;
                git::run(repo, &["add", "--", &path])?;
            }
        }
    }
    git::finish_merge(repo)
}

/// Collapse long unchanged stretches to their first and last 3 lines.
fn collapse(t: &str) -> String {
    let lines: Vec<&str> = t.lines().collect();
    if lines.len() <= 7 {
        return lines.join("\n");
    }
    format!("{}\n⋯ {} unchanged lines\n{}", lines[..3].join("\n"), lines.len() - 6, lines[lines.len() - 3..].join("\n"))
}

pub fn view<'a>(c: &'a Conflict, p: Pal) -> Element<'a, Message> {
    let head = column![
        row![
            container(icon(i::GIT_MERGE).color(theme::on(p.tag))).padding(8).style(p.fill(p.tag, 8.0)),
            text("Resolve conflicts").size(22).font(weight(Semibold)),
        ]
        .spacing(12)
        .align_y(iced::Center),
        text(if c.merge {
            format!(
                "You and {} changed the same part of {}. Choose what to keep for each change, then commit and push.",
                c.author,
                if c.files.len() == 1 { "a file".to_string() } else { format!("{} files", c.files.len()) }
            )
        } else {
            format!("{} changed this note while you were editing it. Choose what to keep for each change, then commit and push.", c.author)
        })
        .size(14)
        .color(p.muted),
    ]
    .spacing(12);

    let files = c.files.iter().enumerate().map(|(k, f)| {
        let done = f.resolution().is_some();
        let label = row![
            icon(if done { i::CHECK } else { i::ALERT }).size(14).color(if done { p.signal } else { p.tag }),
            text(f.path.rsplit('/').next().unwrap_or(&f.path)).size(13).wrapping(text::Wrapping::None),
        ]
        .spacing(8)
        .align_y(iced::Center);
        button(label).width(Fill).padding([8, 10]).style(p.row(k == c.selected)).on_press(Message::ConflictSelect(k)).into()
    });
    let files = container(scrollable(column(files).spacing(2).padding(6)).height(Fill)).width(220).height(Fill).style(p.group());

    let detail: Element<'a, Message> = match c.files.get(c.selected) {
        None => space().into(),
        Some(f) => file_view(f, &c.author, p),
    };

    let foot = row![
        btn(None, if c.merge { "Abort merge" } else { "Discard my edits" }, p.danger()).on_press_maybe((!c.busy).then_some(Message::ConflictAbort)),
        space::horizontal(),
        btn(Some(i::UPLOAD), if c.busy { "Pushing…" } else { "Commit & push" }, p.primary()).on_press_maybe(c.ready().then_some(Message::ConflictFinish)),
    ]
    .spacing(8);

    container(column![head, row![files, detail].spacing(16).height(Fill), foot].spacing(18))
        .max_width(1100)
        .height(Fill)
        .padding(24)
        .style(move |t: &Theme| container::Style {
            shadow: iced::Shadow { color: theme::alpha(Color::BLACK, 0.5), offset: iced::Vector::new(0.0, 12.0), blur_radius: 40.0 },
            ..p.panel()(t)
        })
        .into()
}

fn file_view<'a>(f: &'a File, author: &'a str, p: Pal) -> Element<'a, Message> {
    let mono = |t: String, c: Color| text(t).font(theme::MONO).size(12).color(c);
    let choice = |label: &'static str, msg: Message, active: bool| {
        button(text(label).size(13).font(weight(Medium))).padding([6, 10]).style(p.ghost_with(p.text, active)).on_press(msg)
    };
    match &f.kind {
        Kind::Whole { mine, theirs, pick } => {
            let say = |exists: bool| if exists { "kept" } else { "deleted" };
            column![
                mono(f.path.clone(), p.muted),
                text(format!(
                    "This file can't be merged line by line. Your version {}, {author}'s version {}.",
                    if *mine { "changed it" } else { "deleted it" },
                    if *theirs { "changed it" } else { "deleted it" }
                ))
                .size(14),
                container(
                    row![
                        choice(if *mine { "Keep mine" } else { "Delete (mine)" }, Message::ConflictKeep(Choice::Mine), *pick == Some(Choice::Mine)),
                        choice(if *theirs { "Keep theirs" } else { "Delete (theirs)" }, Message::ConflictKeep(Choice::Theirs), *pick == Some(Choice::Theirs)),
                    ]
                    .spacing(2)
                )
                .padding(2)
                .style(p.group()),
                text(match pick {
                    Some(Choice::Theirs) => format!("The file will be {}.", say(*theirs)),
                    Some(_) => format!("The file will be {}.", say(*mine)),
                    None => String::new(),
                })
                .size(13)
                .color(p.muted),
            ]
            .spacing(14)
            .width(Fill)
            .into()
        }
        Kind::Text { segments, choices, manual } => {
            let top = row![
                mono(f.path.clone(), p.muted).width(Fill),
                btn(Some(i::PENCIL), if manual.is_some() { "Back to choices" } else { "Edit manually" }, p.ghost()).on_press(Message::ConflictManual),
            ]
            .align_y(iced::Center);
            if let Some(content) = manual {
                let hint = if has_markers(&content.text()) {
                    "Remove every <<<<<<<, ======= and >>>>>>> line to mark this file resolved."
                } else {
                    "Resolved."
                };
                return column![
                    top,
                    text(hint).size(13).color(p.muted),
                    text_editor(content).on_action(Message::ConflictEdit).font(theme::MONO).size(13).height(Fill).padding(10),
                ]
                .spacing(10)
                .width(Fill)
                .into();
            }
            let side = |label: String, body: &str, tint: Color, picked: bool| {
                let body = if body.is_empty() { mono("(nothing)".into(), p.faint) } else { mono(body.trim_end_matches('\n').to_string(), p.text) };
                container(column![text(label).size(12).font(weight(Medium)).color(p.muted), body].spacing(6))
                    .padding(10)
                    .width(Fill)
                    .style(move |_: &Theme| container::Style {
                        background: Some(theme::alpha(tint, if picked { 0.18 } else { 0.07 }).into()),
                        border: iced::Border { color: if picked { tint } else { Color::TRANSPARENT }, width: 1.0, radius: 8.0.into() },
                        ..container::Style::default()
                    })
            };
            let mut k = 0;
            let blocks = segments.iter().map(|s| -> Element<'a, Message> {
                match s {
                    Segment::Common(t) => mono(collapse(t), p.faint).into(),
                    Segment::Conflict { mine, theirs, .. } => {
                        let (idx, cur) = (k, choices.get(k).copied().flatten());
                        k += 1;
                        let is = |c: Choice| cur == Some(c);
                        column![
                            row![
                                text(format!("Change {}", idx + 1)).size(13).font(weight(Semibold)).width(Fill),
                                container(
                                    row![
                                        choice("Use mine", Message::ConflictChoose(idx, Choice::Mine), is(Choice::Mine)),
                                        choice("Use theirs", Message::ConflictChoose(idx, Choice::Theirs), is(Choice::Theirs)),
                                        choice("Use both", Message::ConflictChoose(idx, Choice::Both), is(Choice::Both)),
                                    ]
                                    .spacing(2)
                                )
                                .padding(2)
                                .style(p.group()),
                            ]
                            .align_y(iced::Center),
                            row![
                                side("Your version".into(), mine, p.signal, is(Choice::Mine) || is(Choice::Both)),
                                side(format!("{author}'s version"), theirs, p.tag, is(Choice::Theirs) || is(Choice::Both)),
                            ]
                            .spacing(8),
                        ]
                        .spacing(8)
                        .into()
                    }
                }
            });
            column![top, rule::horizontal(1).style(p.rule()), scrollable(column(blocks).spacing(14).padding(iced::Padding::ZERO.right(12))).height(Fill)]
                .spacing(10)
                .width(Fill)
                .into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "top\n<<<<<<< HEAD\nmine 1\n||||||| base\nbase 1\n=======\ntheirs 1\n>>>>>>> origin/main\nmiddle\n<<<<<<< HEAD\n=======\ntheirs 2\n>>>>>>> origin/main\nend\n";

    #[test]
    fn parse_and_render_each_choice() {
        let s = parse(FILE);
        assert_eq!(s.len(), 5);
        assert_eq!(s[1], Segment::Conflict { mine: "mine 1\n".into(), base: "base 1\n".into(), theirs: "theirs 1\n".into() });
        assert_eq!(s[3], Segment::Conflict { mine: String::new(), base: String::new(), theirs: "theirs 2\n".into() });
        use Choice::*;
        assert_eq!(render(&s, &[Some(Mine), Some(Mine)]), "top\nmine 1\nmiddle\nend\n");
        assert_eq!(render(&s, &[Some(Theirs), Some(Theirs)]), "top\ntheirs 1\nmiddle\ntheirs 2\nend\n");
        assert_eq!(render(&s, &[Some(Both), Some(Both)]), "top\nmine 1\ntheirs 1\nmiddle\ntheirs 2\nend\n");
        let partial = render(&s, &[Some(Mine), None]);
        assert!(has_markers(&partial) && partial.starts_with("top\nmine 1\nmiddle\n<<<<<<<"));
        assert_eq!(parse(&partial).len(), 3, "resolved hunk joins the common text; the open one re-parses");
        assert!(!has_markers("a ======= b\n=========\n"), "only exact marker lines count");
    }

    #[test]
    fn file_resolution_needs_every_choice() {
        let segments = parse(FILE);
        let mut f = File { path: "n.md".into(), kind: Kind::Text { segments, choices: vec![Some(Choice::Mine), None], manual: None } };
        assert!(f.resolution().is_none());
        if let Kind::Text { choices, .. } = &mut f.kind {
            choices[1] = Some(Choice::Theirs);
        }
        assert!(matches!(f.resolution(), Some(Resolution::Text(t)) if t == "top\nmine 1\nmiddle\ntheirs 2\nend\n"));
        if let Kind::Text { manual, .. } = &mut f.kind {
            *manual = Some(text_editor::Content::with_text("<<<<<<< x\n"));
        }
        assert!(f.resolution().is_none(), "manual text with markers is unresolved");
    }

    #[test]
    fn load_and_finish_a_real_merge() {
        let root = std::env::temp_dir().join(format!("ez-notes-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git::run(&root, &["init", "--bare", "-q", "remote.git"]).unwrap();
        let url = root.join("remote.git").to_string_lossy().into_owned();
        let (a, b) = (root.join("a"), root.join("b"));
        let setup = |d: &Path| {
            git::clone(&url, d).unwrap();
            git::run(d, &["config", "user.email", "t@t"]).unwrap();
            git::run(d, &["config", "user.name", "Kim"]).unwrap();
        };
        setup(&a);
        std::fs::write(a.join("n.md"), "one\ntwo\n").unwrap();
        std::fs::write(a.join("p.png"), [0u8, 200, 1]).unwrap();
        git::commit_push(&a, "base").unwrap();
        setup(&b);
        std::fs::write(b.join("n.md"), "one\nTWO b\n").unwrap();
        std::fs::write(b.join("p.png"), [0u8, 200, 2]).unwrap();
        git::commit_push(&b, "b").unwrap();
        std::fs::write(a.join("n.md"), "one\ntwo a\n").unwrap();
        std::fs::write(a.join("p.png"), [0u8, 200, 3]).unwrap();
        assert_eq!(git::commit_push(&a, "a").unwrap_err(), git::CONFLICT);

        let mut c = load(&a);
        assert_eq!(c.author, "Kim");
        let paths: Vec<_> = c.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["n.md", "p.png"]);
        assert!(matches!(c.files[1].kind, Kind::Whole { mine: true, theirs: true, pick: None }));
        if let Kind::Text { choices, .. } = &mut c.files[0].kind {
            choices[0] = Some(Choice::Both);
        }
        if let Kind::Whole { pick, .. } = &mut c.files[1].kind {
            *pick = Some(Choice::Theirs);
        }
        assert!(c.ready());
        let resolved = c.files.iter().map(|f| (f.path.clone(), f.resolution().unwrap(), true)).collect();
        finish(&a, resolved, true).unwrap();
        assert!(!git::merging(&a));
        let remote = root.join("remote.git");
        assert_eq!(git::run(&remote, &["show", "HEAD:n.md"]).unwrap(), "one\ntwo a\nTWO b\n");
        assert_eq!(git::run_bytes(&remote, &["show", "HEAD:p.png"]).unwrap(), [0u8, 200, 2]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}

