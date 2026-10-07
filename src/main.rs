mod config;
mod doc;
mod editor;
mod git;
mod notes;
mod tags;
mod widget;

use config::{Config, Repo};
use doc::Kind;
use editor::{Action, Editor, Mark};
use iced::widget::{button, center, column, container, operation, pick_list, row, rule, scrollable, space, text, text_input};
use iced::{Border, Color, Element, Fill, Subscription, Task, Theme, time, window};
use tags::{Front, TagsFile};
use notes::Node;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SAVE_AFTER: Duration = Duration::from_millis(1500);
const DIALOG_INPUT: &str = "dialog-input";

fn main() -> iced::Result {
    iced::application(App::boot, App::update, App::view)
        .title("ez-notes")
        .subscription(App::subscription)
        .exit_on_close_request(false)
        .window_size((1200.0, 800.0))
        .run()
}

struct OpenNote {
    dir: PathBuf,
    front: Front,
    editor: Editor,
    saved_rev: u64,
    /// Tags changed since the last save.
    meta_dirty: bool,
    last_edit: Instant,
}

impl OpenNote {
    fn load(dir: PathBuf) -> std::io::Result<Self> {
        let md = std::fs::read_to_string(notes::md_path(&dir))?.replace("\r\n", "\n");
        let (front, body) = tags::split(&md);
        let editor = Editor::new(body);
        Ok(OpenNote { dir, front, editor, saved_rev: 0, meta_dirty: false, last_edit: Instant::now() })
    }

    fn dirty(&self) -> bool {
        self.meta_dirty || self.editor.revision != self.saved_rev
    }

    /// Full file content: front matter + markdown body.
    fn content(&self) -> String {
        self.front.render(&self.editor.markdown())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DialogKind {
    AddRepo,
    NewNote,
    NewFolder,
    Rename,
    Link,
    AddTag,
    /// Change the color of the tag named in the dialog value.
    TagColor,
}

#[derive(Default)]
enum Sync {
    #[default]
    Idle,
    Busy,
    Error(String),
}

#[derive(Default)]
struct App {
    config: Config,
    tree: Vec<Node>,
    expanded: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    note: Option<OpenNote>,
    dialog: Option<(DialogKind, String)>,
    sync: Sync,
    git_busy: bool,
    /// Commits waiting for the running git job: (repo dir, message).
    git_queue: Vec<(PathBuf, String)>,
    error: Option<String>,
    tags: TagsFile,
    /// Color chosen in the tag dialog; None = keep existing (or auto for new tags).
    dialog_color: Option<String>,
}

#[derive(Debug, Clone)]
enum Message {
    Editor(Action),
    /// Toolbar formatting: like Editor, but keeps the editor focused.
    Format(Action),
    Tick,
    SelectRepo(Repo),
    Toggle(PathBuf),
    Open(PathBuf),
    Dialog(DialogKind),
    DialogInput(String),
    DialogSubmit,
    DialogCancel,
    Delete,
    RemoveRepo,
    Export,
    InsertImage,
    SyncNow,
    Cloned(Result<Repo, String>),
    Pulled(Result<(), String>),
    Pushed(Result<(), String>),
    CloseRequested(window::Id),
    DismissError,
    RemoveTag(String),
    EditTag(String),
    PickColor(String),
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let mut app = App { config: Config::load(), ..App::default() };
        let task = app.open_repo();
        (app, task)
    }

    fn repo_dir(&self) -> Option<PathBuf> {
        self.config.active().map(|r| r.path.clone())
    }

    fn rescan(&mut self) {
        self.tree = self.repo_dir().map(|d| notes::scan(&d)).unwrap_or_default();
        self.tags = self.repo_dir().map(|d| TagsFile::load(&d)).unwrap_or_default();
    }

    fn open_repo(&mut self) -> Task<Message> {
        self.rescan();
        match self.repo_dir() {
            Some(dir) => {
                self.sync = Sync::Busy;
                Task::perform(async move { git::pull(&dir) }, Message::Pulled)
            }
            None => Task::none(),
        }
    }

    fn fail(&mut self, e: impl ToString) {
        self.error = Some(e.to_string());
    }

    /// Write the open note if it changed, then commit + push.
    fn save(&mut self) -> Task<Message> {
        let Some(n) = self.note.as_mut().filter(|n| n.dirty()) else { return Task::none() };
        let md = notes::md_path(&n.dir);
        if let Err(e) = std::fs::write(&md, n.content()) {
            let msg = format!("could not save {}: {e}", md.display());
            self.fail(msg);
            return Task::none();
        }
        n.saved_rev = n.editor.revision;
        if std::mem::take(&mut n.meta_dirty) {
            self.rescan(); // refresh tag dots in the sidebar
        }
        let file = md.file_name().unwrap().to_string_lossy().into_owned();
        self.commit(format!("updated {file}"))
    }

    fn commit(&mut self, msg: String) -> Task<Message> {
        let Some(dir) = self.repo_dir() else { return Task::none() };
        if self.git_busy {
            // one job at a time; later saves to the same repo collapse into one commit
            self.git_queue.retain(|(d, _)| *d != dir);
            self.git_queue.push((dir, msg));
            return Task::none();
        }
        self.run_commit(dir, msg)
    }

    fn run_commit(&mut self, dir: PathBuf, msg: String) -> Task<Message> {
        self.git_busy = true;
        self.sync = Sync::Busy;
        Task::perform(async move { git::commit_push(&dir, &msg) }, Message::Pushed)
    }

    fn open_note(&mut self, dir: PathBuf) -> Task<Message> {
        let save = self.save();
        match OpenNote::load(dir.clone()) {
            Ok(note) => {
                self.note = Some(note);
                self.selected = Some(dir);
            }
            Err(e) => self.fail(e),
        }
        save
    }

    /// Folder new notes/folders go into: the selected folder, the selected
    /// note's parent, or the repo root.
    fn target_folder(&self) -> Option<PathBuf> {
        let root = self.repo_dir()?;
        Some(match &self.selected {
            Some(p) if p.is_dir() && !notes::is_note(p) => p.clone(),
            Some(p) => p.parent().map(Path::to_path_buf).unwrap_or(root),
            None => root,
        })
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let format = matches!(message, Message::Format(_));
        match message {
            Message::Editor(Action::RequestLink) | Message::Format(Action::RequestLink) => {
                return self.update(Message::Dialog(DialogKind::Link));
            }
            Message::Editor(action) | Message::Format(action) if self.note.is_some() => {
                let n = self.note.as_mut().unwrap();
                if n.editor.perform(action) {
                    n.last_edit = Instant::now();
                }
                if format {
                    n.editor.focused = true;
                }
            }
            Message::Editor(_) | Message::Format(_) => {}
            Message::Tick => {
                if self.note.as_ref().is_some_and(|n| n.dirty() && n.last_edit.elapsed() >= SAVE_AFTER) {
                    return self.save();
                }
            }
            Message::SelectRepo(repo) => {
                let save = self.save();
                self.config.active = self.config.repos.iter().position(|r| *r == repo).unwrap_or(0);
                if let Err(e) = self.config.save() {
                    self.fail(e);
                }
                self.note = None;
                self.selected = None;
                return Task::batch([save, self.open_repo()]);
            }
            Message::Toggle(path) => {
                if !self.expanded.remove(&path) {
                    self.expanded.insert(path.clone());
                }
                self.selected = Some(path);
            }
            Message::Open(dir) => return self.open_note(dir),
            Message::Dialog(kind) => {
                let initial = match kind {
                    DialogKind::Rename => match &self.selected {
                        Some(p) => p.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                        None => return Task::none(),
                    },
                    DialogKind::Link => self
                        .note
                        .as_ref()
                        .and_then(|n| n.editor.link_at(n.editor.selection().map_or(n.editor.cursor, |s| s.0)))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                self.dialog = Some((kind, initial));
                self.dialog_color = None;
                return operation::focus(DIALOG_INPUT);
            }
            Message::DialogInput(v) => {
                if let Some((_, value)) = &mut self.dialog {
                    *value = v;
                }
            }
            Message::DialogCancel => self.dialog = None,
            Message::DialogSubmit => {
                let Some((kind, value)) = self.dialog.take() else { return Task::none() };
                return self.submit(kind, value);
            }
            Message::Delete => {
                let Some(path) = self.selected.clone() else { return Task::none() };
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let confirmed = rfd::MessageDialog::new()
                    .set_title("Delete")
                    .set_description(format!("Delete \"{name}\" and everything in it?"))
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    == rfd::MessageDialogResult::Yes;
                if !confirmed {
                    return Task::none();
                }
                if self.note.as_ref().is_some_and(|n| n.dir.starts_with(&path)) {
                    self.note = None;
                }
                if let Err(e) = std::fs::remove_dir_all(&path) {
                    self.fail(e);
                }
                self.selected = None;
                self.rescan();
                return self.commit(format!("deleted {name}"));
            }
            Message::RemoveRepo => {
                let Some(repo) = self.config.active().cloned() else { return Task::none() };
                let confirmed = rfd::MessageDialog::new()
                    .set_title("Remove repository")
                    .set_description(format!(
                        "Remove \"{}\" from ez-notes? The local clone at {} is deleted; anything not pushed yet is lost. The remote repository is not touched.",
                        repo.name,
                        repo.path.display()
                    ))
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    == rfd::MessageDialogResult::Yes;
                if !confirmed {
                    return Task::none();
                }
                self.note = None;
                self.selected = None;
                let _ = std::fs::remove_dir_all(&repo.path);
                self.config.repos.retain(|r| *r != repo);
                self.config.active = 0;
                if let Err(e) = self.config.save() {
                    self.fail(e);
                }
                return self.open_repo();
            }
            Message::Export => {
                let (Some(dir), Some(repo)) = (self.repo_dir(), self.config.active()) else { return Task::none() };
                if let Some(dest) = rfd::FileDialog::new().set_title("Export notes to…").pick_folder() {
                    if let Err(e) = notes::export(&dir, &dest.join(&repo.name)) {
                        self.fail(e);
                    }
                }
            }
            Message::InsertImage => {
                let Some(n) = self.note.as_mut() else { return Task::none() };
                let picked = rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
                    .pick_file();
                if let Some(src) = picked {
                    match notes::import_image(&n.dir, &src) {
                        Ok(name) => {
                            n.editor.perform(Action::InsertImage { src: name, alt: String::new() });
                            n.editor.focused = true;
                            n.last_edit = Instant::now();
                        }
                        Err(e) => self.fail(e),
                    }
                }
            }
            Message::SyncNow => {
                let save = self.save();
                let pull = self.open_repo();
                return Task::batch([save, pull]);
            }
            Message::Cloned(Ok(repo)) => {
                self.config.repos.push(repo);
                self.config.active = self.config.repos.len() - 1;
                if let Err(e) = self.config.save() {
                    self.fail(e);
                }
                self.note = None;
                self.selected = None;
                return self.open_repo();
            }
            Message::Cloned(Err(e)) => {
                self.sync = Sync::Idle;
                self.fail(e);
            }
            Message::Pulled(r) => {
                if !self.git_busy {
                    self.sync = match r {
                        Ok(()) => Sync::Idle,
                        Err(e) => Sync::Error(e),
                    };
                }
                self.rescan();
                // pick up remote changes to the open note unless the user is mid-edit
                if let Some(n) = self.note.as_mut().filter(|n| !n.dirty()) {
                    let on_disk = std::fs::read_to_string(notes::md_path(&n.dir)).unwrap_or_default();
                    if on_disk != n.content() {
                        if let Ok(fresh) = OpenNote::load(n.dir.clone()) {
                            *n = fresh;
                        }
                    }
                }
            }
            Message::Pushed(r) => {
                self.git_busy = false;
                self.sync = match r {
                    Ok(()) => Sync::Idle,
                    Err(e) => Sync::Error(e),
                };
                if !self.git_queue.is_empty() {
                    let (dir, msg) = self.git_queue.remove(0);
                    return self.run_commit(dir, msg);
                }
            }
            Message::CloseRequested(id) => {
                // flush synchronously: the process is about to exit
                if let Some(n) = self.note.as_ref().filter(|n| n.dirty()) {
                    let md = notes::md_path(&n.dir);
                    if std::fs::write(&md, n.content()).is_ok() {
                        let file = md.file_name().unwrap().to_string_lossy().into_owned();
                        self.git_queue.push((self.repo_dir().unwrap(), format!("updated {file}")));
                    }
                }
                for (dir, msg) in self.git_queue.drain(..) {
                    let _ = git::commit_push(&dir, &msg);
                }
                return window::close(id);
            }
            Message::DismissError => self.error = None,
            Message::RemoveTag(tag) => {
                if let Some(n) = self.note.as_mut() {
                    n.front.tags.retain(|t| *t != tag);
                    n.meta_dirty = true;
                    n.last_edit = Instant::now();
                }
            }
            Message::EditTag(tag) => {
                self.dialog_color = Some(self.tags.setting(&tag).to_string());
                self.dialog = Some((DialogKind::TagColor, tag));
            }
            Message::PickColor(c) => self.dialog_color = Some(c),
        }
        Task::none()
    }

    fn submit(&mut self, kind: DialogKind, value: String) -> Task<Message> {
        if kind == DialogKind::Link {
            return self.update(Message::Format(Action::SetLink(value)));
        }
        if matches!(kind, DialogKind::AddTag | DialogKind::TagColor) {
            let Some(tag) = tags::valid_tag(&value) else {
                self.fail(format!("invalid tag {value:?}: use letters, digits, - _ /"));
                return Task::none();
            };
            if let Some(n) = self.note.as_mut().filter(|n| !n.front.tags.contains(&tag)) {
                n.front.tags.push(tag.clone());
                n.meta_dirty = true;
                n.last_edit = Instant::now();
            }
            // record the color in the shared tags file when chosen, or when the tag is new
            let color = self.dialog_color.take().or_else(|| (!self.tags.tags.contains_key(&tag)).then(|| tags::AUTO.into()));
            if let (Some(color), Some(dir)) = (color, self.repo_dir()) {
                if self.tags.tags.get(&tag) != Some(&color) {
                    self.tags.tags.insert(tag, color);
                    if let Err(e) = self.tags.save(&dir) {
                        self.fail(e);
                    }
                    return self.commit("updated tags".into());
                }
            }
            return Task::none();
        }
        if kind == DialogKind::AddRepo {
            let url = value.trim().to_string();
            let name = url.trim_end_matches('/').trim_end_matches(".git").rsplit(['/', ':']).next().unwrap_or("notes").to_string();
            let mut path = config::clones_dir().join(&name);
            let mut i = 1;
            while path.exists() {
                path = config::clones_dir().join(format!("{name}-{i}"));
                i += 1;
            }
            let repo = Repo { name: path.file_name().unwrap().to_string_lossy().into_owned(), url, path };
            self.sync = Sync::Busy;
            return Task::perform(async move { git::clone(&repo.url, &repo.path).map(|_| repo) }, Message::Cloned);
        }
        let name = match notes::valid_name(&value) {
            Ok(n) => n.to_string(),
            Err(e) => {
                self.fail(e);
                return Task::none();
            }
        };
        let Some(folder) = self.target_folder() else { return Task::none() };
        let result = match kind {
            DialogKind::NewNote => notes::create_note(&folder, &name).map(|dir| {
                self.expanded.insert(folder.clone());
                (Some(dir), format!("created {name}.md"))
            }),
            DialogKind::NewFolder => notes::create_folder(&folder, &name).map(|dir| {
                self.expanded.insert(dir.clone());
                self.selected = Some(dir);
                (None, format!("created folder {name}"))
            }),
            DialogKind::Rename => {
                let Some(old) = self.selected.clone() else { return Task::none() };
                let save = self.save();
                let open = self.note.as_ref().and_then(|n| n.dir.strip_prefix(&old).ok().map(Path::to_path_buf));
                let old_name = old.file_name().unwrap_or_default().to_string_lossy().into_owned();
                match notes::rename(&old, &name) {
                    Ok(new) => {
                        self.selected = Some(new.clone());
                        self.rescan();
                        // re-open the note if it lived at or under the renamed path
                        let reopen = open.map(|rel| self.open_note(new.join(rel))).unwrap_or(Task::none());
                        let commit = self.commit(format!("renamed {old_name} to {name}"));
                        return Task::batch([save, reopen, commit]);
                    }
                    Err(e) => {
                        self.fail(e);
                        return save;
                    }
                }
            }
            DialogKind::AddRepo | DialogKind::Link | DialogKind::AddTag | DialogKind::TagColor => unreachable!(),
        };
        match result {
            Ok((open, msg)) => {
                self.rescan();
                let open = open.map(|dir| self.open_note(dir)).unwrap_or(Task::none());
                let commit = self.commit(msg);
                Task::batch([open, commit])
            }
            Err(e) => {
                self.fail(e);
                Task::none()
            }
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let close = window::close_requests().map(Message::CloseRequested);
        if self.note.as_ref().is_some_and(OpenNote::dirty) {
            Subscription::batch([close, time::every(Duration::from_millis(500)).map(|_| Message::Tick)])
        } else {
            close
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let status = match &self.sync {
            Sync::Idle if self.note.as_ref().is_some_and(OpenNote::dirty) => text("● unsaved"),
            Sync::Idle => text("✓ synced"),
            Sync::Busy => text("↻ syncing…"),
            Sync::Error(e) => text(format!("⚠ {e}")),
        };
        let has_repo = self.config.active().is_some();
        let top = row![
            pick_list(&self.config.repos[..], self.config.active(), Message::SelectRepo).placeholder("No repository"),
            button("+ Repo").on_press(Message::Dialog(DialogKind::AddRepo)),
            button("Remove repo").on_press_maybe(has_repo.then_some(Message::RemoveRepo)),
            space::horizontal(),
            container(status).max_width(500),
            button("Sync").on_press_maybe(has_repo.then_some(Message::SyncNow)),
            button("Export…").on_press_maybe(has_repo.then_some(Message::Export)),
        ]
        .spacing(8)
        .align_y(iced::Center);

        let selected = self.selected.is_some();
        let sidebar = column![
            row![
                button("+ Note").on_press_maybe(has_repo.then_some(Message::Dialog(DialogKind::NewNote))),
                button("+ Folder").on_press_maybe(has_repo.then_some(Message::Dialog(DialogKind::NewFolder))),
            ]
            .spacing(4),
            row![
                button("Rename").on_press_maybe(selected.then_some(Message::Dialog(DialogKind::Rename))),
                button("Delete").on_press_maybe(selected.then_some(Message::Delete)),
            ]
            .spacing(4),
            rule::horizontal(1),
            scrollable(column(self.tree_view(&self.tree, 0)).spacing(2)).height(Fill),
        ]
        .spacing(8)
        .width(260);

        let mut main = column![].spacing(8);
        if let Some(e) = &self.error {
            main = main.push(row![text(format!("⚠ {e}")).width(Fill), button("×").on_press(Message::DismissError)].spacing(8));
        }
        if let Some((kind, value)) = &self.dialog {
            let (label, placeholder) = match kind {
                DialogKind::AddRepo => ("Clone repository", "git@github.com:you/notes.git"),
                DialogKind::NewNote => ("New note", "Note name"),
                DialogKind::NewFolder => ("New folder", "Folder name"),
                DialogKind::Rename => ("Rename", "New name"),
                DialogKind::Link => ("Link URL", "https://… (empty removes the link)"),
                DialogKind::AddTag => ("Add tag", "tag name"),
                DialogKind::TagColor => ("Color for", ""),
            };
            let input: Element<'_, Message> = if *kind == DialogKind::TagColor {
                text(value.clone()).into()
            } else {
                text_input(placeholder, value)
                    .id(DIALOG_INPUT)
                    .on_input(Message::DialogInput)
                    .on_submit(Message::DialogSubmit)
                    .into()
            };
            let mut dialog = row![text(label), input].spacing(8).align_y(iced::Center);
            if matches!(kind, DialogKind::AddTag | DialogKind::TagColor) {
                dialog = dialog.push(self.swatches(value));
            }
            main = main.push(dialog.extend([
                button("OK").on_press(Message::DialogSubmit).into(),
                button("Cancel").on_press(Message::DialogCancel).into(),
            ]));
        }
        let body: Element<'_, Message> = match &self.note {
            Some(n) => column![
                self.tag_bar(n),
                self.toolbar(),
                rule::horizontal(1),
                scrollable(container(widget::view(&n.editor, &n.dir, Message::Editor)).padding([16, 32])).height(Fill),
            ]
            .spacing(8)
            .into(),
            None if !has_repo => center(text("Add a git repository to start (+ Repo)")).into(),
            None => center(text("Select or create a note")).into(),
        };
        main = main.push(body);

        column![top, rule::horizontal(1), row![sidebar, rule::vertical(1), main.width(Fill)].spacing(12)]
            .spacing(8)
            .padding(12)
            .into()
    }

    fn tag_bar<'a>(&'a self, n: &'a OpenNote) -> Element<'a, Message> {
        let chips = n.front.tags.iter().map(|t| {
            let bg = self.tags.color(t);
            let fg = if bg.relative_luminance() > 0.5 { Color::BLACK } else { Color::WHITE };
            let plain = move |_: &Theme, _| button::Style { text_color: fg, ..button::Style::default() };
            container(
                row![
                    button(text(t.as_str()).size(13)).padding(0).style(plain).on_press(Message::EditTag(t.clone())),
                    button(text("×").size(13)).padding(0).style(plain).on_press(Message::RemoveTag(t.clone())),
                ]
                .spacing(6),
            )
            .padding([2, 10])
            .style(move |_| container::Style {
                background: Some(bg.into()),
                border: Border { radius: 10.0.into(), ..Border::default() },
                ..container::Style::default()
            })
            .into()
        });
        row(chips)
            .push(button(text("+ Tag").size(13)).padding([2, 10]).style(button::secondary).on_press(Message::Dialog(DialogKind::AddTag)))
            .spacing(6)
            .align_y(iced::Center)
            .into()
    }

    /// "Auto" plus the fixed palette; the current choice gets an outline.
    fn swatches(&self, tag: &str) -> Element<'_, Message> {
        let current = self.dialog_color.clone().unwrap_or_else(|| self.tags.setting(tag.trim()).to_string());
        let ring = |selected: bool| Border { radius: 11.0.into(), width: if selected { 3.0 } else { 0.0 }, color: Color::BLACK };
        let auto_color = tags::hex_color(tags::auto(tag.trim())).unwrap();
        let auto_selected = current == tags::AUTO;
        let auto = button(text("Auto").size(12))
            .padding([3, 8])
            .style(move |_, _| button::Style {
                background: Some(auto_color.into()),
                text_color: Color::WHITE,
                border: ring(auto_selected),
                ..button::Style::default()
            })
            .on_press(Message::PickColor(tags::AUTO.into()));
        let colors = tags::PALETTE.iter().map(|hex| {
            let c = tags::hex_color(hex).unwrap();
            let selected = current == *hex;
            button(space().width(22).height(22))
                .padding(0)
                .style(move |_, _| button::Style { background: Some(c.into()), border: ring(selected), ..button::Style::default() })
                .on_press(Message::PickColor(hex.to_string()))
                .into()
        });
        row![auto].extend(colors).spacing(4).align_y(iced::Center).into()
    }

    fn toolbar(&self) -> Element<'_, Message> {
        let b = |label: &'static str, action: Action| button(text(label)).on_press(Message::Format(action));
        row![
            b("B", Action::Toggle(Mark::Bold)),
            b("I", Action::Toggle(Mark::Italic)),
            b("H1", Action::SetKind(Kind::Heading(1))),
            b("H2", Action::SetKind(Kind::Heading(2))),
            b("H3", Action::SetKind(Kind::Heading(3))),
            b("¶", Action::SetKind(Kind::Paragraph)),
            b("• List", Action::SetKind(Kind::List { ordered: false, depth: 0 })),
            b("1. List", Action::SetKind(Kind::List { ordered: true, depth: 0 })),
            b("Code", Action::SetKind(Kind::Code { lang: String::new() })),
            b("Link", Action::RequestLink),
            button("Image").on_press(Message::InsertImage),
        ]
        .spacing(4)
        .into()
    }

    fn tree_view<'a>(&'a self, nodes: &'a [Node], depth: usize) -> Vec<Element<'a, Message>> {
        let mut out = vec![];
        for node in nodes {
            let (path, label, msg) = match node {
                Node::Folder { path, name, .. } => {
                    let arrow = if self.expanded.contains(path) { "▾" } else { "▸" };
                    (path, format!("{arrow} {name}"), Message::Toggle(path.clone()))
                }
                Node::Note { path, name, .. } => (path, format!("  {name}"), Message::Open(path.clone())),
            };
            let style = if self.selected.as_ref() == Some(path) { button::primary } else { button::text };
            let mut content = row![text(label)].spacing(4).align_y(iced::Center);
            if let Node::Note { tags, .. } = node {
                content = content.extend(tags.iter().map(|t| dot(self.tags.color(t), 8.0)));
            }
            out.push(
                container(button(content).style(style).width(Fill).on_press(msg))
                    .padding(iced::Padding::ZERO.left(depth as f32 * 14.0))
                    .into(),
            );
            if let Node::Folder { path, children, .. } = node {
                if self.expanded.contains(path) {
                    out.extend(self.tree_view(children, depth + 1));
                }
            }
        }
        out
    }
}

fn dot<'a>(color: Color, size: f32) -> Element<'a, Message> {
    container(space().width(size).height(size))
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: Border { radius: (size / 2.0).into(), ..Border::default() },
            ..container::Style::default()
        })
        .into()
}
