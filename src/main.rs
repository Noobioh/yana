// release builds on Windows: no console window behind the app
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod conflict;
mod doc;
mod editor;
mod git;
mod history;
mod notes;
mod pdf;
mod tags;
mod theme;
mod update;
mod widget;

use config::{Config, Repo};
use doc::Kind;
use editor::{Action, Editor, Mark, TableOp};
use iced::widget::{button, center, column, container, mouse_area, opaque, operation, pick_list, row, rule, scrollable, space, stack, svg, text, text_editor, text_input};
use iced::{Border, Color, Element, Fill, Subscription, Task, Theme, time, window};
use tags::{Front, TagsFile};
use notes::Node;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SAVE_AFTER: Duration = Duration::from_millis(1500);
/// How often to pull collaborators' changes in the background.
const PULL_EVERY: Duration = Duration::from_secs(60);
const MERGE_ABORTED: &str = "merge aborted: your changes are not pushed yet";
const DIALOG_INPUT: &str = "dialog-input";
const ADD_REPO_URL: &str = "add-repo-url";
const TITLE_INPUT: &str = "title-input";
const LOGO_ID: &str = "logo";

fn main() -> iced::Result {
    let app = iced::application(App::boot, App::update, App::view)
        .title("Yana")
        .subscription(App::subscription)
        .exit_on_close_request(false)
        .window(window::Settings { size: iced::Size::new(1280.0, 820.0), icon: theme::window_icon(), ..window::Settings::default() })
        .theme(App::theme)
        .default_font(theme::SANS);
    theme::FONTS.into_iter().fold(app, |app, f| app.font(f)).run()
}

struct OpenNote {
    dir: PathBuf,
    front: Front,
    editor: Editor,
    saved_rev: u64,
    /// Tags changed since the last save.
    meta_dirty: bool,
    last_edit: Instant,
    /// The file as last read from or written to disk. When a pull changes
    /// the file under an edit, saving merges against this instead of
    /// overwriting (which would silently revert the collaborator).
    base: String,
}

impl OpenNote {
    fn load(dir: PathBuf) -> std::io::Result<Self> {
        let md = std::fs::read_to_string(notes::md_path(&dir))?.replace("\r\n", "\n");
        let (front, body) = tags::split(&md);
        let editor = Editor::new(body);
        Ok(OpenNote { dir, front, editor, saved_rev: 0, meta_dirty: false, last_edit: Instant::now(), base: md.clone() })
    }

    /// Replace the content with `text` from disk, keeping the cursor.
    fn refresh(&mut self, text: String) {
        let (front, body) = tags::split(&text);
        let (cursor, focused) = (self.editor.cursor, self.editor.focused);
        self.editor = Editor::new(body);
        self.editor.cursor = self.editor.clamp(cursor);
        self.editor.focused = focused;
        self.front = front;
        self.saved_rev = self.editor.revision;
        self.meta_dirty = false;
        self.base = text;
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
    NewNote,
    NewFolder,
    Rename,
    Link,
    AddTag,
    /// Change the color of the tag named in the dialog value.
    TagColor,
    /// Push a local-only repo to a remote URL.
    Publish,
}

#[derive(Default)]
struct AddRepoForm {
    url: String,
    /// Optional; derived from the URL when left empty.
    name: String,
    busy: bool,
    error: Option<String>,
}

/// `git@github.com:me/notes.git` -> `notes`
fn repo_name_from_url(url: &str) -> String {
    let name = url.trim().trim_end_matches('/').trim_end_matches(".git").rsplit(['/', ':']).next().unwrap_or("");
    if name.is_empty() { "notes".into() } else { name.into() }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum Sort {
    /// The folder tree; notes in each folder most recent first.
    #[default]
    Folders,
    /// Every note in one list, most recent first.
    Notes,
}

impl Sort {
    const ALL: [Sort; 2] = [Sort::Folders, Sort::Notes];
}

impl std::fmt::Display for Sort {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(match self {
            Sort::Folders => "Sort folders on recency",
            Sort::Notes => "Sort notes on recency",
        })
    }
}

/// Outcome of asking GitHub for a newer release.
#[derive(Default)]
enum Update {
    #[default]
    Unknown,
    Checking,
    UpToDate,
    Available(String),
    Failed,
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
    sort: Sort,
    /// Created/updated per `.md` path, from git history.
    dates: HashMap<PathBuf, history::Dates>,
    /// (repo, HEAD) the dates were read at: history only changes with HEAD.
    dates_at: (PathBuf, String),
    /// The tree was rescanned since the last dates check.
    dates_stale: bool,
    dates_loading: bool,
    expanded: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    note: Option<OpenNote>,
    dialog: Option<(DialogKind, String)>,
    sync: Sync,
    git_busy: bool,
    /// Commits waiting for the running git job: (repo dir, message).
    git_queue: Vec<(PathBuf, String)>,
    /// Shown as a dismissable toast.
    error: Option<String>,
    /// Last sync error toasted, so a repeating one (auto-pull) pops up once.
    sync_error: Option<String>,
    tags: TagsFile,
    /// Color chosen in the tag dialog; None = keep existing (or auto for new tags).
    dialog_color: Option<String>,
    show_theme: bool,
    /// App menu under the logo.
    menu_open: bool,
    /// "Add repository" modal, when open.
    add_repo: Option<AddRepoForm>,
    /// Inline rename of the open note via its title.
    title_edit: Option<String>,
    /// Version history of the open note, shown instead of the editor.
    history: Option<history::History>,
    /// A merge stopped on conflicts; the resolver modal is open while set.
    conflict: Option<conflict::Conflict>,
    /// A pull is rewriting the working tree: hold off writing notes.
    pulling: bool,
    update: Update,
}

#[derive(Debug, Clone)]
enum Message {
    Editor(Action),
    /// Toolbar formatting: like Editor, but keeps the editor focused.
    Format(Action),
    Tick,
    AutoPull,
    AutoPulled(Result<(), String>),
    SelectRepo(Repo),
    SortBy(Sort),
    /// None: HEAD hadn't moved.
    DatesLoaded(Result<Option<DatesAt>, String>),
    Toggle(PathBuf),
    Open(PathBuf),
    Dialog(DialogKind),
    DialogInput(String),
    DialogSubmit,
    DialogCancel,
    Delete,
    RemoveRepo,
    Export,
    ExportPdf,
    InsertImage,
    SyncNow,
    /// Re-show the sync error after its toast was dismissed.
    ShowSyncError,
    Cloned(Result<Repo, String>),
    Published(PathBuf, Result<String, String>),
    Pulled(Result<(), String>),
    Pushed(Result<(), String>),
    CloseRequested(window::Id),
    DismissError,
    RemoveTag(String),
    EditTag(String),
    PickColor(String),
    ToggleTheme,
    ToggleMenu,
    AddRepoOpen,
    AddLocalRepo,
    AddRepoUrl(String),
    AddRepoName(String),
    AddRepoSubmit,
    AddRepoCancel,
    EditTitle,
    TitleInput(String),
    TitleSubmit,
    TitleCancel,
    ThemeColor(&'static str, String),
    ThemePreset(theme::Colors),
    HistoryOpen,
    HistoryLoaded(Result<Vec<history::Version>, String>),
    HistorySelect(usize),
    HistoryMode(history::Mode),
    DiffLoaded(usize, history::Mode, Result<String, String>),
    ShowLargeDiff,
    Restore,
    HistoryClose,
    ConflictSelect(usize),
    ConflictChoose(usize, conflict::Choice),
    ConflictKeep(conflict::Choice),
    ConflictManual,
    ConflictEdit(text_editor::Action),
    ConflictAbort,
    ConflictFinish,
    CheckUpdate,
    UpdateChecked(Result<Option<String>, String>),
    OpenReleases,
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let mut app = App { config: Config::load(), ..App::default() };
        let task = app.open_repo();
        let dates = app.load_dates();
        let update = app.check_update();
        (app, Task::batch([task, dates, update]))
    }

    fn check_update(&mut self) -> Task<Message> {
        self.update = Update::Checking;
        Task::perform(async { update::check() }, Message::UpdateChecked)
    }

    /// The active repo's folder, unless it was deleted (or isn't a repo
    /// anymore) since it was added.
    fn repo_dir(&self) -> Option<PathBuf> {
        self.config.active().map(|r| r.path.clone()).filter(|p| p.join(".git").exists())
    }

    fn rescan(&mut self) {
        self.dates_stale = true; // a commit may have moved dates; update() checks in the background
        self.tree = self.repo_dir().map(|d| notes::scan(&d, &self.dates)).unwrap_or_default();
        self.tags = self.repo_dir().map(|d| TagsFile::load(&d)).unwrap_or_default();
    }

    fn open_repo(&mut self) -> Task<Message> {
        self.rescan();
        match self.repo_dir() {
            Some(dir) => {
                self.sync = Sync::Busy;
                self.pulling = true;
                Task::perform(async move { git::pull(&dir) }, Message::Pulled)
            }
            None => {
                let missing = self.config.active().map(|r| format!("{} is missing at {}: restore it or remove the repo", r.name, r.path.display()));
                self.set_sync(missing.map_or(Ok(()), Err));
                Task::none()
            }
        }
    }

    fn fail(&mut self, e: impl ToString) {
        self.error = Some(e.to_string());
    }

    fn set_sync(&mut self, r: Result<(), String>) {
        match r {
            Ok(()) => {
                self.sync = Sync::Idle;
                self.sync_error = None;
            }
            Err(e) => {
                if self.sync_error.as_ref() != Some(&e) {
                    self.sync_error = Some(e.clone());
                    self.fail(&e);
                }
                self.sync = Sync::Error(e);
            }
        }
    }

    /// Write the open note if it changed, then commit + push. Waits while a
    /// pull or push (which may pull and merge) rewrites the tree; the next
    /// tick retries.
    fn save(&mut self) -> Task<Message> {
        if self.pulling || self.git_busy {
            return Task::none();
        }
        self.flush()
    }

    /// `save` without waiting for git: for leaving the note (switch, rename,
    /// quit), where a deferred save would drop the edits.
    fn flush(&mut self) -> Task<Message> {
        // the file on disk holds conflict markers until the merge is resolved
        if self.conflict.is_some() {
            return Task::none();
        }
        let repo = self.repo_dir();
        let Some(n) = self.note.as_mut().filter(|n| n.dirty()) else { return Task::none() };
        let md = notes::md_path(&n.dir);
        let mine = n.content();
        let text = match std::fs::read_to_string(&md).map(|s| s.replace("\r\n", "\n")) {
            // changed on disk since loaded (a pull landed mid-edit): merge, don't overwrite
            Ok(disk) if disk != n.base && disk != mine => match git::merge_text(&mine, &n.base, &disk) {
                Ok((merged, false)) => merged,
                Ok((merged, true)) => {
                    let rel = repo.as_ref().map(|r| history::rel(r, &md)).unwrap_or_default();
                    self.conflict = repo.map(|r| conflict::local(&r, rel, &merged));
                    return Task::none();
                }
                Err(e) => {
                    self.fail(e);
                    return Task::none();
                }
            },
            _ => mine.clone(),
        };
        if let Err(e) = std::fs::write(&md, &text) {
            let msg = format!("could not save {}: {e}", md.display());
            self.fail(msg);
            return Task::none();
        }
        n.saved_rev = n.editor.revision;
        if text == mine {
            n.base = text;
        } else {
            n.refresh(text); // show the collaborator's merged-in changes
        }
        if std::mem::take(&mut n.meta_dirty) {
            self.rescan(); // refresh tag dots in the sidebar
        }
        let file = md.file_name().unwrap().to_string_lossy().into_owned();
        self.commit(format!("updated {file}"))
    }

    fn commit(&mut self, msg: String) -> Task<Message> {
        let Some(dir) = self.repo_dir() else { return Task::none() };
        if self.git_busy || self.conflict.is_some() {
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

    /// Open or close the conflict resolver to match the repo's merge state.
    fn check_conflict(&mut self) {
        let Some(dir) = self.repo_dir() else { return };
        let merging = git::merging(&dir);
        match &self.conflict {
            // new conflict, or the push after resolving hit another one: (re)load
            None if merging => {}
            Some(c) if merging && (c.busy || !c.merge) => {}
            // resolved and pushed, or aborted
            Some(c) if !merging && (c.busy || c.merge) => {
                // a local conflict's resolution on disk already holds the edits;
                // after a merge, unsaved edits stay and the next save merges them in
                let keep = c.merge && self.note.as_ref().is_some_and(OpenNote::dirty);
                self.conflict = None;
                if keep {
                    self.rescan();
                } else {
                    self.reload_note();
                }
                return;
            }
            _ => return,
        }
        self.conflict = Some(conflict::load(&dir));
        self.history = None;
        self.menu_open = false;
    }

    /// Start the next queued commit, if nothing blocks it.
    fn next_job(&mut self) -> Task<Message> {
        if self.git_busy || self.conflict.is_some() || self.git_queue.is_empty() {
            return Task::none();
        }
        let (dir, msg) = self.git_queue.remove(0);
        self.run_commit(dir, msg)
    }

    /// Re-read the open note from disk, dropping unsaved edits.
    fn reload_note(&mut self) {
        self.rescan();
        if let Some(n) = &mut self.note {
            match std::fs::read_to_string(notes::md_path(&n.dir)) {
                Ok(text) => n.refresh(text.replace("\r\n", "\n")),
                Err(_) => self.note = None,
            }
        }
    }

    fn load_diff(&mut self) -> Task<Message> {
        let (Some(dir), Some(n), Some(h)) = (self.repo_dir(), &self.note, self.history.as_mut()) else { return Task::none() };
        if h.versions.is_empty() {
            return Task::none();
        }
        h.diff = None;
        h.show_large = false;
        let (versions, idx, mode) = (h.versions.clone(), h.selected, h.mode);
        let current = history::rel(&dir, &notes::md_path(&n.dir));
        Task::perform(async move { history::diff(&dir, &versions, idx, mode, &current) }, move |r| Message::DiffLoaded(idx, mode, r))
    }

    fn open_note(&mut self, dir: PathBuf) -> Task<Message> {
        let save = self.flush();
        match OpenNote::load(dir.clone()) {
            Ok(note) => {
                self.note = Some(note);
                self.title_edit = None;
                self.history = None;
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
        let task = self.handle(message);
        Task::batch([task, self.load_dates()])
    }

    /// Re-read note dates off the UI thread after a rescan, one load at a time.
    fn load_dates(&mut self) -> Task<Message> {
        let Some(dir) = self.repo_dir().filter(|_| self.dates_stale && !self.dates_loading) else { return Task::none() };
        self.dates_stale = false;
        self.dates_loading = true;
        let known = self.dates_at.clone();
        Task::perform(async move { read_dates(dir, &known) }, Message::DatesLoaded)
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
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
            Message::AutoPull => {
                let Some(dir) = self.repo_dir() else { return Task::none() };
                // quietly, and only when idle: a pending edit reaches the remote
                // through its own push, which pulls and merges first
                // after "Abort merge", wait for the user's next save or sync
                let busy = self.git_busy || self.pulling || matches!(&self.sync, Sync::Busy) || matches!(&self.sync, Sync::Error(e) if e == MERGE_ABORTED);
                if busy || self.conflict.is_some() || self.note.as_ref().is_some_and(OpenNote::dirty) {
                    return Task::none();
                }
                self.git_busy = true;
                self.pulling = true;
                return Task::perform(async move { git::pull(&dir) }, Message::AutoPulled);
            }
            Message::AutoPulled(r) => {
                self.git_busy = false;
                let pulled = self.update(Message::Pulled(r));
                return Task::batch([pulled, self.next_job()]);
            }
            Message::SelectRepo(repo) => {
                let save = self.flush();
                self.config.active = self.config.repos.iter().position(|r| *r == repo).unwrap_or(0);
                if let Err(e) = self.config.save() {
                    self.fail(e);
                }
                self.note = None;
                self.selected = None;
                return Task::batch([save, self.open_repo()]);
            }
            Message::SortBy(sort) => self.sort = sort,
            Message::DatesLoaded(r) => {
                self.dates_loading = false;
                // on error keep the old dates; the next rescan retries
                if let Ok(Some((at, dates))) = r
                    && self.repo_dir().as_ref() == Some(&at.0)
                {
                    self.dates = dates;
                    self.dates_at = at;
                    self.rescan();
                }
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
                // only delete what we cloned; a local folder belongs to the user
                let clone = repo.path.starts_with(config::clones_dir());
                let description = if clone {
                    format!(
                        "Remove \"{}\" from ez-notes? The local clone at {} is deleted; anything not pushed yet is lost. The remote repository is not touched.",
                        repo.name,
                        repo.path.display()
                    )
                } else {
                    format!("Remove \"{}\" from ez-notes? The folder at {} is kept.", repo.name, repo.path.display())
                };
                let confirmed = rfd::MessageDialog::new()
                    .set_title("Remove repository")
                    .set_description(description)
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    == rfd::MessageDialogResult::Yes;
                if !confirmed {
                    return Task::none();
                }
                self.note = None;
                self.selected = None;
                if clone {
                    let _ = std::fs::remove_dir_all(&repo.path);
                }
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
            Message::ExportPdf => {
                let Some(n) = &self.note else { return Task::none() };
                let title = n.dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let picked = rfd::FileDialog::new()
                    .set_title("Export as PDF")
                    .add_filter("PDF", &["pdf"])
                    .set_file_name(format!("{title}.pdf"))
                    .save_file();
                if let Some(dest) = picked
                    && let Err(e) = pdf::export(&title, &n.editor.blocks, &n.dir, &dest)
                {
                    self.fail(e);
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
                self.add_repo = None;
                self.sync = Sync::Idle;
                self.config.repos.push(repo);
                self.config.active = self.config.repos.len() - 1;
                if let Err(e) = self.config.save() {
                    self.fail(e);
                }
                self.note = None;
                self.selected = None;
                return self.open_repo();
            }
            Message::Published(dir, r) => {
                self.git_busy = false;
                match r {
                    Ok(url) => {
                        self.set_sync(Ok(()));
                        if let Some(repo) = self.config.repos.iter_mut().find(|r| r.path == dir) {
                            repo.url = url;
                        }
                        if let Err(e) = self.config.save() {
                            self.fail(e);
                        }
                    }
                    Err(e) => self.set_sync(Err(e)),
                }
                return self.next_job();
            }
            Message::AddLocalRepo => {
                let Some(form) = self.add_repo.as_mut().filter(|f| !f.busy) else { return Task::none() };
                let Some(path) = rfd::FileDialog::new().set_title("Open local folder").pick_folder() else { return Task::none() };
                if self.config.repos.iter().any(|r| r.path == path) {
                    form.error = Some("This folder is already added.".into());
                    return Task::none();
                }
                form.busy = true;
                form.error = None;
                self.sync = Sync::Busy;
                let name = path.file_name().map_or_else(|| "notes".into(), |n| n.to_string_lossy().into_owned());
                return Task::perform(async move { git::open_local(&path).map(|url| Repo { name, url, path }) }, Message::Cloned);
            }
            Message::Cloned(Err(e)) => match self.add_repo.as_mut() {
                Some(form) => {
                    form.busy = false;
                    form.error = Some(e);
                }
                None => self.fail(e),
            },
            Message::AddRepoOpen => {
                self.menu_open = false;
                self.add_repo = Some(AddRepoForm::default());
                return operation::focus(ADD_REPO_URL);
            }
            Message::AddRepoUrl(v) | Message::AddRepoName(v) if self.add_repo.as_ref().is_some_and(|f| f.busy) => drop(v),
            Message::AddRepoUrl(v) => {
                if let Some(f) = self.add_repo.as_mut() {
                    f.url = v;
                    f.error = None;
                }
            }
            Message::AddRepoName(v) => {
                if let Some(f) = self.add_repo.as_mut() {
                    f.name = v;
                    f.error = None;
                }
            }
            Message::AddRepoCancel => {
                if !self.add_repo.as_ref().is_some_and(|f| f.busy) {
                    self.add_repo = None;
                }
            }
            Message::AddRepoSubmit => return self.clone_repo(),
            Message::EditTitle => {
                let Some(n) = &self.note else { return Task::none() };
                self.title_edit = Some(n.dir.file_name().unwrap_or_default().to_string_lossy().into_owned());
                return operation::focus(TITLE_INPUT);
            }
            Message::TitleInput(v) => {
                if let Some(t) = self.title_edit.as_mut() {
                    *t = v;
                }
            }
            Message::TitleCancel => self.title_edit = None,
            Message::TitleSubmit => {
                let (Some(value), Some(n)) = (self.title_edit.take(), &self.note) else { return Task::none() };
                if n.dir.file_name().is_some_and(|f| f.to_string_lossy() == value.trim()) {
                    return Task::none();
                }
                // same path as the sidebar Rename: renames folder + .md, reopens, commits
                self.selected = Some(n.dir.clone());
                return self.submit(DialogKind::Rename, value);
            }
            Message::Pulled(r) => {
                self.pulling = false;
                if !self.git_busy {
                    self.set_sync(r);
                }
                self.rescan();
                self.check_conflict();
                // pick up remote changes to the open note unless the user is mid-edit
                // (mid-edit, the next save merges them in instead)
                if let Some(n) = self.note.as_mut().filter(|n| !n.dirty() && self.conflict.is_none()) {
                    let on_disk = std::fs::read_to_string(notes::md_path(&n.dir)).unwrap_or_default().replace("\r\n", "\n");
                    if on_disk != n.base {
                        n.refresh(on_disk);
                    }
                }
            }
            Message::Pushed(r) => {
                self.git_busy = false;
                self.set_sync(r);
                self.rescan(); // the new commit moves the note's updated date
                self.check_conflict();
                return self.next_job();
            }
            Message::CloseRequested(id) => {
                // flush synchronously: the process is about to exit. save() merges
                // with a pull that landed mid-edit; git_busy routes its commit into
                // the queue instead of a task that would never run.
                let (pulling, git_busy) = (self.pulling, self.git_busy);
                self.pulling = false;
                self.git_busy = true;
                let _ = self.flush();
                if self.note.as_ref().is_some_and(OpenNote::dirty) {
                    // blocked by a conflict: writing would commit markers or revert a collaborator
                    let quit = rfd::MessageDialog::new()
                        .set_title("Unsaved edits")
                        .set_description("Your latest edits can't be saved until the conflict is resolved. Quit anyway and lose them?")
                        .set_buttons(rfd::MessageButtons::YesNo)
                        .show()
                        == rfd::MessageDialogResult::Yes;
                    if !quit {
                        (self.pulling, self.git_busy) = (pulling, git_busy);
                        return Task::none();
                    }
                }
                for (dir, msg) in self.git_queue.drain(..) {
                    let _ = git::commit_push(&dir, &msg);
                }
                return window::close(id);
            }
            Message::DismissError => self.error = None,
            Message::ShowSyncError => {
                if let Sync::Error(e) = &self.sync {
                    self.error = Some(e.clone());
                }
            }
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
            Message::ToggleTheme => {
                self.show_theme = !self.show_theme;
                self.menu_open = false;
            }
            Message::ToggleMenu => self.menu_open = !self.menu_open,
            Message::CheckUpdate => return self.check_update(),
            // offline at startup is no error worth a banner: the menu item says it
            Message::UpdateChecked(r) => {
                self.update = match r {
                    Ok(Some(v)) => Update::Available(v),
                    Ok(None) => Update::UpToDate,
                    Err(_) => Update::Failed,
                }
            }
            Message::OpenReleases => {
                widget::open_url(update::RELEASES);
                self.menu_open = false;
            }
            Message::ThemeColor(key, value) => {
                if let Some(field) = self.config.theme.get_mut(key) {
                    *field = value;
                }
                if let Err(e) = self.config.save() {
                    self.fail(e);
                }
            }
            Message::ThemePreset(colors) => {
                self.config.theme = colors;
                if let Err(e) = self.config.save() {
                    self.fail(e);
                }
            }
            Message::HistoryOpen => {
                let (Some(dir), Some(n)) = (self.repo_dir(), &self.note) else { return Task::none() };
                let md = history::rel(&dir, &notes::md_path(&n.dir));
                let save = self.save();
                self.history = Some(history::History::new());
                self.show_theme = false;
                let load = Task::perform(async move { history::load(&dir, &md) }, Message::HistoryLoaded);
                return Task::batch([save, load]);
            }
            Message::HistoryLoaded(r) => {
                let Some(h) = self.history.as_mut() else { return Task::none() };
                match r {
                    Ok(versions) => {
                        h.versions = versions;
                        h.selected = 0;
                        return self.load_diff();
                    }
                    Err(e) => h.error = Some(e),
                }
            }
            Message::HistorySelect(idx) => {
                if let Some(h) = self.history.as_mut() {
                    h.selected = idx;
                }
                return self.load_diff();
            }
            Message::HistoryMode(mode) => {
                if let Some(h) = self.history.as_mut() {
                    h.mode = mode;
                }
                return self.load_diff();
            }
            Message::DiffLoaded(idx, mode, r) => {
                // a slower answer for a version no longer selected: drop it
                if let Some(h) = self.history.as_mut().filter(|h| h.selected == idx && h.mode == mode) {
                    h.diff = Some(r);
                }
            }
            Message::ShowLargeDiff => {
                if let Some(h) = self.history.as_mut() {
                    h.show_large = true;
                }
            }
            Message::HistoryClose => self.history = None,
            Message::Restore => {
                let (Some(dir), Some(n), Some(h)) = (self.repo_dir(), &self.note, &self.history) else { return Task::none() };
                let Some(v) = h.versions.get(h.selected).cloned() else { return Task::none() };
                if self.git_busy || self.pulling {
                    return Task::none(); // the button says "Saving…"; restoring now could race that commit or pull
                }
                let (note_dir, file) = (n.dir.clone(), notes::md_path(&n.dir).file_name().unwrap().to_string_lossy().into_owned());
                let confirmed = rfd::MessageDialog::new()
                    .set_title("Restore version")
                    .set_description(format!(
                        "Restore \"{}\" to the version from {}? The current text stays in the history, so you can undo this.",
                        note_dir.file_name().unwrap_or_default().to_string_lossy(),
                        v.date
                    ))
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    == rfd::MessageDialogResult::Yes;
                if !confirmed {
                    return Task::none();
                }
                if let Err(e) = history::restore(&dir, &v, &note_dir) {
                    self.fail(e);
                    return Task::none();
                }
                self.history = None;
                self.reload_note();
                return self.commit(format!("restored {file} to {}", v.short));
            }
            Message::ConflictSelect(k) => {
                if let Some(c) = self.conflict.as_mut() {
                    c.selected = k;
                }
            }
            Message::ConflictChoose(idx, choice) => {
                if let Some(conflict::Kind::Text { choices, .. }) = self.conflict_file() {
                    choices[idx] = Some(choice);
                }
            }
            Message::ConflictKeep(choice) => {
                if let Some(conflict::Kind::Whole { pick, .. }) = self.conflict_file() {
                    *pick = Some(choice);
                }
            }
            Message::ConflictManual => {
                if let Some(conflict::Kind::Text { segments, choices, manual }) = self.conflict_file() {
                    *manual = match manual {
                        Some(_) => None,
                        None => Some(text_editor::Content::with_text(&conflict::render(segments, choices))),
                    };
                }
            }
            Message::ConflictEdit(action) => {
                if let Some(conflict::Kind::Text { manual: Some(content), .. }) = self.conflict_file() {
                    content.perform(action);
                }
            }
            Message::ConflictAbort if self.conflict.as_ref().is_some_and(|c| !c.merge) => {
                let author = self.conflict.as_ref().map(|c| c.author.clone()).unwrap_or_default();
                let confirmed = rfd::MessageDialog::new()
                    .set_title("Discard my edits")
                    .set_description(format!("Discard your unsaved edits to this note and keep {author}'s version?"))
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    == rfd::MessageDialogResult::Yes;
                if confirmed {
                    self.conflict = None;
                    self.reload_note();
                }
            }
            Message::ConflictAbort => {
                let Some(dir) = self.repo_dir() else { return Task::none() };
                let confirmed = rfd::MessageDialog::new()
                    .set_title("Abort merge")
                    .set_description(
                        "Stop merging? Your notes go back to how they were before this sync. Your changes stay saved on this computer but can't be pushed until the conflict is resolved, so the next sync asks again.",
                    )
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    == rfd::MessageDialogResult::Yes;
                if !confirmed {
                    return Task::none();
                }
                if let Err(e) = git::run(&dir, &["merge", "--abort"]) {
                    self.fail(e);
                }
                self.check_conflict();
                self.set_sync(Err(MERGE_ABORTED.into()));
            }
            Message::ConflictFinish => {
                let Some(dir) = self.repo_dir() else { return Task::none() };
                let Some(c) = self.conflict.as_mut().filter(|c| c.ready()) else { return Task::none() };
                let resolved: Vec<_> = c
                    .files
                    .iter()
                    .filter_map(|f| {
                        let res = f.resolution()?;
                        let exists = match (&f.kind, &res) {
                            (conflict::Kind::Whole { mine, .. }, conflict::Resolution::Mine) => *mine,
                            (conflict::Kind::Whole { theirs, .. }, conflict::Resolution::Theirs) => *theirs,
                            _ => true,
                        };
                        Some((f.path.clone(), res, exists))
                    })
                    .collect();
                c.busy = true;
                let merge = c.merge;
                self.git_busy = true;
                self.sync = Sync::Busy;
                // reports through Pushed, which closes the resolver once the merge is gone
                return Task::perform(async move { conflict::finish(&dir, resolved, merge) }, Message::Pushed);
            }
        }
        Task::none()
    }

    /// The file selected in the conflict resolver.
    fn conflict_file(&mut self) -> Option<&mut conflict::Kind> {
        let c = self.conflict.as_mut()?;
        c.files.get_mut(c.selected).map(|f| &mut f.kind)
    }

    fn publish(&mut self, url: String) -> Task<Message> {
        let url = url.trim().to_string();
        let Some(dir) = self.repo_dir().filter(|_| !url.is_empty()) else { return Task::none() };
        if self.git_busy {
            self.fail("Wait for the sync to finish, then publish again.");
            return Task::none();
        }
        self.git_busy = true;
        self.sync = Sync::Busy;
        let at = dir.clone();
        Task::perform(async move { git::publish(&dir, &url).map(|_| url) }, move |r| Message::Published(at.clone(), r))
    }

    fn clone_repo(&mut self) -> Task<Message> {
        let Some(form) = self.add_repo.as_mut().filter(|f| !f.busy) else { return Task::none() };
        let url = form.url.trim().to_string();
        if url.is_empty() {
            form.error = Some("Enter the repository URL.".into());
            return Task::none();
        }
        let wanted = if form.name.trim().is_empty() { repo_name_from_url(&url) } else { form.name.clone() };
        let name = match notes::valid_name(&wanted) {
            Ok(n) => n.to_string(),
            Err(e) => {
                form.error = Some(e);
                return Task::none();
            }
        };
        if self.config.repos.iter().any(|r| r.url == url) {
            form.error = Some("This repository is already added.".into());
            return Task::none();
        }
        let mut path = config::clones_dir().join(&name);
        let mut i = 1;
        while path.exists() {
            path = config::clones_dir().join(format!("{name}-{i}"));
            i += 1;
        }
        form.busy = true;
        form.error = None;
        self.sync = Sync::Busy;
        let repo = Repo { name: path.file_name().unwrap().to_string_lossy().into_owned(), url, path };
        Task::perform(async move { git::clone(&repo.url, &repo.path).map(|_| repo) }, Message::Cloned)
    }

    fn add_repo_modal<'a>(&'a self, form: &'a AddRepoForm, p: Pal) -> Element<'a, Message> {
        let derived = repo_name_from_url(&form.url);
        let name = if form.name.trim().is_empty() { derived.clone() } else { form.name.trim().to_string() };
        let field = |label: &'static str| text(label).size(13).font(weight(Medium)).color(p.muted);
        let mut card = column![
            row![
                container(icon(i::BOOK).color(theme::on(p.signal))).padding(8).style(p.fill(p.signal, 8.0)),
                text("Add repository").size(22).font(weight(Semibold)),
            ]
            .spacing(12)
            .align_y(iced::Center),
            text("Clone a git repository to keep your notes in, or open a local folder (it becomes a git repository you can publish later). Private repositories work with your existing SSH keys or git credential helper.")
                .size(14)
                .color(p.muted),
            column![
                field("Repository URL"),
                text_input("git@github.com:you/notes.git", &form.url)
                    .id(ADD_REPO_URL)
                    .on_input(Message::AddRepoUrl)
                    .on_submit(Message::AddRepoSubmit)
                    .font(theme::MONO)
                    .size(13)
                    .padding([10, 12])
                    .style(p.input()),
            ]
            .spacing(6),
            column![
                field("Name (optional)"),
                text_input(&derived, &form.name)
                    .on_input(Message::AddRepoName)
                    .on_submit(Message::AddRepoSubmit)
                    .size(14)
                    .padding([10, 12])
                    .style(p.input()),
                text(format!("Cloned to {}", config::clones_dir().join(name).display())).font(theme::MONO).size(11).color(p.faint),
            ]
            .spacing(6),
        ]
        .spacing(18);
        if let Some(e) = &form.error {
            card = card.push(
                container(row![icon(i::ALERT).color(p.danger), text(e.as_str()).size(13).color(p.danger).width(Fill)].spacing(8))
                    .padding([8, 10])
                    .style(p.fill(theme::alpha(p.danger, 0.1), 8.0)),
            );
        }
        let ready = !form.busy && !form.url.trim().is_empty();
        card = card.push(
            row![
                space::horizontal(),
                btn(Some(i::BOOK), "Open local folder…", p.secondary()).on_press_maybe((!form.busy).then_some(Message::AddLocalRepo)),
                btn(None, "Cancel", p.ghost()).on_press_maybe((!form.busy).then_some(Message::AddRepoCancel)),
                btn(Some(i::REFRESH), if form.busy { "Cloning…" } else { "Clone" }, p.primary())
                    .on_press_maybe(ready.then_some(Message::AddRepoSubmit)),
            ]
            .spacing(8),
        );
        container(card)
            .width(500)
            .padding(24)
            .style(move |t: &Theme| container::Style {
                shadow: iced::Shadow { color: theme::alpha(Color::BLACK, 0.5), offset: iced::Vector::new(0.0, 12.0), blur_radius: 40.0 },
                ..p.panel()(t)
            })
            .into()
    }

    fn submit(&mut self, kind: DialogKind, value: String) -> Task<Message> {
        if kind == DialogKind::Link {
            return self.update(Message::Format(Action::SetLink(value)));
        }
        if kind == DialogKind::Publish {
            return self.publish(value);
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
                let save = self.flush();
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
            DialogKind::Link | DialogKind::AddTag | DialogKind::TagColor | DialogKind::Publish => unreachable!(),
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
        let mut subs = vec![window::close_requests().map(Message::CloseRequested)];
        if self.note.as_ref().is_some_and(OpenNote::dirty) {
            subs.push(time::every(Duration::from_millis(500)).map(|_| Message::Tick));
        }
        if self.config.active().is_some() {
            subs.push(time::every(PULL_EVERY).map(|_| Message::AutoPull));
        }
        Subscription::batch(subs)
    }


    fn theme(&self) -> Theme {
        self.config.theme.pal().theme()
    }

    fn view(&self) -> Element<'_, Message> {
        let p = self.config.theme.pal();
        let has_repo = self.repo_dir().is_some();

        // ---- top bar
        let logo = container(svg(theme::logo(&p, false)).width(36).height(36)).id(LOGO_ID);
        let logo = button(logo).padding(0).style(p.bare(p.text)).on_press(Message::ToggleMenu);
        let repo = container(
            row![
                icon(i::BOOK).color(p.muted),
                pick_list(&self.config.repos[..], self.config.active(), Message::SelectRepo)
                    .placeholder("No repository")
                    .font(theme::MONO)
                    .text_size(13)
                    .padding([4, 4])
                    .style(p.pick())
                    .menu_style(p.menu()),
            ]
            .spacing(6)
            .align_y(iced::Center),
        )
        .padding([4, 10])
        .style(p.group());
        let (status_icon, status_color, status) = match &self.sync {
            Sync::Error(_) => (i::ALERT, p.danger, "sync failed"),
            Sync::Busy => (i::REFRESH, p.muted, "syncing…"),
            Sync::Idle if self.note.as_ref().is_some_and(OpenNote::dirty) => (i::PENCIL, p.faint, "unsaved"),
            Sync::Idle => (i::CHECK, p.signal, "synced"),
        };
        let status = row![
            icon(status_icon).size(14).color(status_color),
            text(status).font(theme::MONO).size(12).color(if matches!(self.sync, Sync::Error(_)) { p.danger } else { p.muted }),
        ]
        .spacing(6)
        .align_y(iced::Center);
        let top = container(
            row![
                logo,
                repo,
                btn(Some(i::PLUS), "Add repo", p.ghost()).on_press(Message::AddRepoOpen),
                btn(Some(i::MINUS), "Remove repo", p.ghost_with(p.muted, false)).on_press_maybe(self.config.active().is_some().then_some(Message::RemoveRepo)),
                space::horizontal(),
                matches!(self.update, Update::Available(_))
                    .then(|| btn(Some(i::REFRESH), "Update available", p.ghost_with(p.signal, false)).on_press(Message::OpenReleases)),
                button(status)
                    .padding(0)
                    .style(p.bare(p.text))
                    .on_press_maybe(matches!(self.sync, Sync::Error(_)).then_some(Message::ShowSyncError)),
                btn(Some(i::REFRESH), "Sync", p.primary()).on_press_maybe(has_repo.then_some(Message::SyncNow)),
                self.config
                    .active()
                    .is_some_and(|r| r.url.is_empty())
                    .then(|| btn(Some(i::UPLOAD), "Publish…", p.secondary()).on_press_maybe(has_repo.then_some(Message::Dialog(DialogKind::Publish)))),
                btn(Some(i::UPLOAD), "Export…", p.secondary()).on_press_maybe(has_repo.then_some(Message::Export)),
            ]
            .spacing(8)
            .align_y(iced::Center),
        )
        .padding(12)
        .style(p.panel());

        // ---- sidebar
        let full = |b: button::Button<'static, Message>| b.width(Fill);
        let selected_name = self
            .selected
            .as_ref()
            .and_then(|s| s.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let selected = self.selected.is_some();
        let sidebar = container(
            column![
                row![
                    full(btn(Some(i::FILE_PLUS), "Note", p.secondary()))
                        .on_press_maybe(has_repo.then_some(Message::Dialog(DialogKind::NewNote))),
                    full(btn(Some(i::FOLDER_PLUS), "Folder", p.secondary()))
                        .on_press_maybe(has_repo.then_some(Message::Dialog(DialogKind::NewFolder))),
                ]
                .spacing(8)
                .padding(12),
                container(
                    pick_list(&Sort::ALL[..], Some(self.sort), Message::SortBy)
                        .width(Fill)
                        .text_size(13)
                        .padding([6, 10])
                        .style(p.pick())
                        .menu_style(p.menu()),
                )
                .padding(iced::Padding::ZERO.left(12.0).right(12.0).bottom(12.0)),
                rule::horizontal(1).style(p.rule()),
                scrollable(column(match self.sort {
                    Sort::Folders => self.tree_view(&self.tree, p),
                    Sort::Notes => {
                        let mut flat = vec![];
                        flatten(&self.tree, &mut flat);
                        flat.sort_by_key(|n| std::cmp::Reverse(notes::updated(n)));
                        self.tree_view(flat, p)
                    }
                })
                .spacing(2)
                .padding(12))
                .height(Fill),
                rule::horizontal(1).style(p.rule()),
                row![
                    text(selected_name).size(13).color(p.muted).width(Fill).wrapping(text::Wrapping::None),
                    btn(Some(i::PENCIL), "Rename", p.ghost()).on_press_maybe(selected.then_some(Message::Dialog(DialogKind::Rename))),
                    btn(Some(i::TRASH), "Delete", p.danger()).on_press_maybe(selected.then_some(Message::Delete)),
                ]
                .spacing(4)
                .padding([8, 12])
                .align_y(iced::Center),
            ],
        )
        .width(300)
        .height(Fill)
        .style(p.panel());

        // ---- main panel
        let mut main = column![];
        if let Some(dialog) = self.dialog_bar(p) {
            main = main.push(dialog).push(rule::horizontal(1).style(p.rule()));
        }
        let body: Element<'_, Message> = match &self.note {
            _ if self.show_theme => scrollable(container(self.theme_editor(p)).padding([24, 32]).max_width(760)).height(Fill).into(),
            Some(n) if let Some(h) = &self.history => {
                history::view(h, n.dir.file_name().unwrap_or_default().to_string_lossy().into_owned(), self.git_busy || self.pulling, p)
            }
            Some(n) => {
                let rel = self.repo_dir().and_then(|r| n.dir.parent()?.strip_prefix(r).ok().map(Path::to_path_buf));
                let crumbs = rel
                    .map(|r| r.iter().map(|c| c.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" / "))
                    .filter(|s| !s.is_empty())
                    .or_else(|| self.config.active().map(|r| r.name.clone()))
                    .unwrap_or_default();
                let title = n.dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let dates = self.dates.get(&notes::md_path(&n.dir)).map(|d| {
                    text(format!("Created {}  ·  Updated {}", d.created.label, d.updated.label)).font(theme::MONO).size(12).color(p.faint)
                });
                let page = column![
                    text(crumbs).font(theme::MONO).size(12).color(p.faint),
                    self.title(title, p),
                    dates,
                    self.tag_bar(n, p),
                    rule::horizontal(1).style(p.rule()),
                    widget::view(&n.editor, &n.dir, p, Message::Editor),
                ]
                .spacing(14)
                .max_width(760);
                column![
                    container(self.toolbar(n, p)).padding(12),
                    rule::horizontal(1).style(p.rule()),
                    scrollable(container(page).center_x(Fill).padding([24, 32])).height(Fill),
                ]
                .into()
            }
            None => center(
                column![
                    svg(theme::logo(&p, false)).width(64).height(64),
                    text("Yana").size(36).font(weight(Semibold)),
                    text(if has_repo {
                        "Select or create a note"
                    } else if self.config.active().is_some() {
                        "This repository's folder is missing"
                    } else {
                        "Add a git repository to start"
                    }).color(p.muted),
                ]
                .spacing(12)
                .align_x(iced::Center),
            )
            .into(),
        };
        let main = container(main.push(body)).width(Fill).height(Fill).style(p.panel());

        let base = container(column![top, row![sidebar, main].spacing(12)].spacing(12).padding(12))
            .width(Fill)
            .height(Fill)
            .style(p.ground());
        let mut layers: Vec<Element<'_, Message>> = vec![base.into()];
        if self.menu_open {
            // dropdown under the logo; clicking anywhere else closes it
        let item = row![icon(i::PALETTE), text("Theme").size(14).font(weight(Medium))].spacing(8).align_y(iced::Center);
        let theme_item = button(item).width(Fill).padding([9, 12]).style(p.ghost_with(p.text, self.show_theme)).on_press(Message::ToggleTheme);
        let (label, color, press) = match &self.update {
            Update::Available(v) => (format!("Update to v{v}"), p.signal, Some(Message::OpenReleases)),
            Update::Checking => ("Checking…".into(), p.text, None),
            Update::UpToDate => ("Up to date".into(), p.text, Some(Message::CheckUpdate)),
            Update::Failed => ("Couldn't check for updates".into(), p.text, Some(Message::CheckUpdate)),
            Update::Unknown => ("Check for updates".into(), p.text, Some(Message::CheckUpdate)),
        };
        let item = row![icon(i::REFRESH), text(label).size(14).font(weight(Medium))].spacing(8).align_y(iced::Center);
        let update_item = button(item).width(Fill).padding([9, 12]).style(p.ghost_with(color, false)).on_press_maybe(press);
        let menu = container(column![theme_item, update_item])
            .width(200)
            .padding(4)
            .style(move |t: &Theme| container::Style {
                shadow: iced::Shadow { color: theme::alpha(Color::BLACK, 0.4), offset: iced::Vector::new(0.0, 6.0), blur_radius: 18.0 },
                ..p.panel()(t)
            });
            layers.push(mouse_area(space().width(Fill).height(Fill)).on_press(Message::ToggleMenu).into());
            layers.push(container(menu).padding(iced::Padding::ZERO.top(68.0).left(24.0)).into());
        }
        if let Some(form) = &self.add_repo {
            // modal: dimmed backdrop (click = cancel), card swallows its own clicks
            let backdrop = container(space().width(Fill).height(Fill)).style(p.fill(theme::alpha(Color::BLACK, 0.55), 0.0));
            layers.push(mouse_area(backdrop).on_press(Message::AddRepoCancel).into());
            layers.push(center(opaque(self.add_repo_modal(form, p))).into());
        }
        if let Some(c) = &self.conflict {
            // no click-to-dismiss: the merge has to be finished or aborted
            let backdrop = container(space().width(Fill).height(Fill)).style(p.fill(theme::alpha(Color::BLACK, 0.55), 0.0));
            layers.push(opaque(backdrop));
            layers.push(container(opaque(conflict::view(c, p))).center_x(Fill).height(Fill).padding([40, 48]).into());
        }
        if let Some(e) = &self.error {
            let toast = container(
                row![
                    icon(i::ALERT).color(p.danger),
                    text(e.as_str()).size(13).color(p.text).width(Fill),
                    button(icon(i::X).color(p.muted)).style(p.ghost()).on_press(Message::DismissError),
                ]
                .spacing(8)
                .align_y(iced::Center),
            )
            .width(420)
            .padding([10, 12])
            .style(move |t: &Theme| container::Style {
                border: iced::Border { color: p.danger, width: 1.0, radius: 8.0.into() },
                shadow: iced::Shadow { color: theme::alpha(Color::BLACK, 0.4), offset: iced::Vector::new(0.0, 6.0), blur_radius: 18.0 },
                ..p.panel()(t)
            });
            layers.push(container(opaque(toast)).align_right(Fill).align_bottom(Fill).padding(24).into());
        }
        stack(layers).into()
    }

    /// Note title; click to rename in place.
    fn title(&self, title: String, p: Pal) -> Element<'_, Message> {
        match &self.title_edit {
            Some(value) => row![
                text_input("Note name", value)
                    .id(TITLE_INPUT)
                    .on_input(Message::TitleInput)
                    .on_submit(Message::TitleSubmit)
                    .size(36)
                    .font(weight(Semibold))
                    .padding([0, 6])
                    .style(p.input()),
                btn(Some(i::CHECK), "Rename", p.primary()).on_press(Message::TitleSubmit),
                button(icon(i::X).color(p.muted)).padding(10).style(p.ghost()).on_press(Message::TitleCancel),
            ]
            .spacing(8)
            .align_y(iced::Center)
            .into(),
            None => button(text(title).size(36).font(weight(Semibold)))
                .padding([0, 6])
                .style(p.row(false))
                .on_press(Message::EditTitle)
                .into(),
        }
    }

    fn dialog_bar(&self, p: Pal) -> Option<Element<'_, Message>> {
        let (kind, value) = self.dialog.as_ref()?;
        let (label, placeholder) = match kind {
            DialogKind::NewNote => ("New note", "Note name"),
            DialogKind::NewFolder => ("New folder", "Folder name"),
            DialogKind::Rename => ("Rename", "New name"),
            DialogKind::Link => ("Link URL", "https://… (empty removes the link)"),
            DialogKind::AddTag => ("Add tag", "tag name"),
            DialogKind::TagColor => ("Color for", ""),
            DialogKind::Publish => ("Remote URL", "git@github.com:you/notes.git (an empty repository)"),
        };
        let input: Element<'_, Message> = if *kind == DialogKind::TagColor {
            text(value.clone()).font(theme::MONO).size(13).into()
        } else {
            text_input(placeholder, value)
                .id(DIALOG_INPUT)
                .on_input(Message::DialogInput)
                .on_submit(Message::DialogSubmit)
                .padding([8, 10])
                .size(14)
                .style(p.input())
                .into()
        };
        let mut bar = row![text(label).size(14).color(p.muted), input].spacing(10).align_y(iced::Center);
        if matches!(kind, DialogKind::AddTag | DialogKind::TagColor) {
            bar = bar.push(self.swatches(value, p));
        }
        let bar = bar
            .push(btn(None, "OK", p.primary()).on_press(Message::DialogSubmit))
            .push(btn(None, "Cancel", p.ghost()).on_press(Message::DialogCancel));
        Some(container(bar).padding(12).into())
    }

    fn tag_bar<'a>(&'a self, n: &'a OpenNote, p: Pal) -> Element<'a, Message> {
        let chips = n.front.tags.iter().map(|t| {
            container(
                row![
                    dot(self.tags.color(t), 6.0),
                    button(text(t.as_str()).font(theme::MONO).size(12)).padding(0).style(p.bare(p.text)).on_press(Message::EditTag(t.clone())),
                    button(icon(i::X).size(12)).padding(0).style(p.bare(p.faint)).on_press(Message::RemoveTag(t.clone())),
                ]
                .spacing(8)
                .align_y(iced::Center),
            )
            .padding([4, 8])
            .style(p.group())
            .into()
        });
        let add = button(row![icon(i::PLUS).size(12), text("Tag").font(theme::MONO).size(12)].spacing(4).align_y(iced::Center))
            .padding([4, 8])
            .style(move |t: &Theme, s| button::Style { border: Border { color: theme::alpha(p.tag, 0.5), width: 1.0, radius: 6.0.into() }, ..p.bare(p.tag)(t, s) })
            .on_press(Message::Dialog(DialogKind::AddTag));
        row(chips).push(add).spacing(8).align_y(iced::Center).into()
    }

    /// "Auto" plus the fixed palette; the current choice gets an outline.
    fn swatches(&self, tag: &str, p: Pal) -> Element<'_, Message> {
        let current = self.dialog_color.clone().unwrap_or_else(|| self.tags.setting(tag.trim()).to_string());
        let auto = tags::hex_color(tags::auto(tag.trim())).unwrap();
        let auto = button(text("Auto").size(12))
            .padding([4, 8])
            .style(p.swatch(auto, current == tags::AUTO))
            .on_press(Message::PickColor(tags::AUTO.into()));
        let colors = tags::PALETTE.iter().map(|hex| {
            button(space().width(20).height(20))
                .padding(0)
                .style(p.swatch(tags::hex_color(hex).unwrap(), current == *hex))
                .on_press(Message::PickColor(hex.to_string()))
                .into()
        });
        row![auto].extend(colors).spacing(4).align_y(iced::Center).into()
    }

    fn toolbar(&self, n: &OpenNote, p: Pal) -> Element<'_, Message> {
        let kind = &n.editor.blocks[n.editor.cursor.block].kind;
        let tool = |content: Element<'static, Message>, action: Action, active: bool| -> Element<'static, Message> {
            button(container(content).center_x(18)).padding([7, 8]).style(p.ghost_with(p.text, active)).on_press(Message::Format(action)).into()
        };
        let label = |s: &'static str| -> Element<'static, Message> { text(s).size(13).font(weight(Medium)).line_height(1.0).into() };
        let group = |items: Vec<Element<'static, Message>>| container(row(items).spacing(2)).padding(2).style(p.group());
        let heading = |n: u8| tool(label(["H1", "H2", "H3"][n as usize - 1]), Action::SetKind(Kind::Heading(n)), *kind == Kind::Heading(n));
        let list = |ordered: bool| matches!(kind, Kind::List { ordered: o, .. } if *o == ordered);
        let in_table = matches!(kind, Kind::Table { .. });
        let mut bar = row![
            group(vec![
                tool(text("B").size(13).font(weight(Semibold)).line_height(1.0).into(), Action::Toggle(Mark::Bold), false),
                tool(text("I").size(13).font(iced::Font { style: iced::font::Style::Italic, ..theme::SANS }).line_height(1.0).into(), Action::Toggle(Mark::Italic), false),
            ]),
            group(vec![
                heading(1),
                heading(2),
                heading(3),
                tool(label("¶"), Action::SetKind(Kind::Paragraph), *kind == Kind::Paragraph),
            ]),
            group(vec![
                tool(icon(i::LIST).into(), Action::SetKind(Kind::List { ordered: false, depth: 0 }), list(false)),
                tool(icon(i::LIST_ORDERED).into(), Action::SetKind(Kind::List { ordered: true, depth: 0 }), list(true)),
                tool(icon(i::CODE).into(), Action::SetKind(Kind::Code { lang: String::new() }), matches!(kind, Kind::Code { .. })),
            ]),
            group(vec![
                tool(icon(i::LINK).into(), Action::RequestLink, false),
                button(container(icon(i::IMAGE)).center_x(18)).padding([7, 8]).style(p.ghost()).on_press(Message::InsertImage).into(),
                tool(icon(i::TABLE).into(), Action::InsertTable, in_table),
            ]),
        ]
        .spacing(8);
        if in_table {
            let op = |s: &'static str, op: TableOp| -> Element<'static, Message> {
                button(label(s)).padding([7, 8]).style(p.ghost()).on_press(Message::Format(Action::Table(op))).into()
            };
            bar = bar.push(group(vec![
                op("+ Row", TableOp::AddRow),
                op("− Row", TableOp::RemoveRow),
                op("+ Col", TableOp::AddColumn),
                op("− Col", TableOp::RemoveColumn),
            ]));
        }
        bar.push(space().width(Fill))
            .push(btn(Some(i::HISTORY), "History", p.ghost()).on_press(Message::HistoryOpen))
            .push(btn(Some(i::UPLOAD), "PDF", p.ghost()).on_press(Message::ExportPdf))
            .into()
    }

    fn theme_editor(&self, p: Pal) -> Element<'_, Message> {
        let colors = &self.config.theme;
        let rows = theme::FIELDS.iter().map(|&(key, label)| {
            let value = colors.get(key);
            let valid = tags::hex_color(value.trim()).is_some();
            let swatch = tags::hex_color(value.trim()).unwrap_or(Color::TRANSPARENT);
            row![
                container(space().width(28).height(28)).style(move |_: &Theme| container::Style {
                    background: Some(swatch.into()),
                    border: Border { color: p.line, width: 1.0, radius: 6.0.into() },
                    ..container::Style::default()
                }),
                text(label).size(14).width(170),
                text_input("#RRGGBB", value)
                    .on_input(move |v| Message::ThemeColor(key, v))
                    .font(theme::MONO)
                    .size(13)
                    .padding([6, 10])
                    .width(130)
                    .style(p.input()),
                text(if valid { "" } else { "not a #RRGGBB color" }).size(12).color(p.danger),
            ]
            .spacing(12)
            .align_y(iced::Center)
            .into()
        });
        column![
            text("Theme").size(36).font(weight(Semibold)),
            text("Changes apply right away and are saved in your ez-notes config.").size(14).color(p.muted),
            row![
                btn(None, "Dark preset", p.secondary()).on_press(Message::ThemePreset(theme::Colors::dark())),
                btn(None, "Light preset", p.secondary()).on_press(Message::ThemePreset(theme::Colors::light())),
                space::horizontal(),
                btn(None, "Done", p.primary()).on_press(Message::ToggleTheme),
            ]
            .spacing(8),
            rule::horizontal(1).style(p.rule()),
            column(rows).spacing(10),
        ]
        .spacing(16)
        .into()
    }

    fn tree_view<'a>(&'a self, nodes: impl IntoIterator<Item = &'a Node>, p: Pal) -> Vec<Element<'a, Message>> {
        let mut out = vec![];
        for node in nodes {
            let selected = self.selected.as_ref().is_some_and(|s| s == node_path(node));
            let content: Element<'a, Message> = match node {
                Node::Folder { path, name, children } => {
                    let open = self.expanded.contains(path);
                    row![
                        icon(if open { i::CHEVRON_DOWN } else { i::CHEVRON_RIGHT }).size(14).color(p.muted),
                        text(name.as_str()).size(14).font(weight(Medium)).width(Fill).wrapping(text::Wrapping::None),
                        text(count_notes(children).to_string()).font(theme::MONO).size(12).color(p.faint),
                    ]
                    .spacing(8)
                    .align_y(iced::Center)
                    .into()
                }
                Node::Note { name, tags, dates, .. } => column![
                    row![
                        dot(if selected { p.signal } else { p.faint }, 6.0),
                        text(name.as_str())
                            .size(14)
                            .font(weight(if selected { Semibold } else { iced::font::Weight::Normal }))
                            .width(Fill)
                            .wrapping(text::Wrapping::None),
                    ]
                    .extend(tags.iter().map(|t| dot(self.tags.color(t), 6.0)))
                    .spacing(8)
                    .align_y(iced::Center),
                    // under the title: past the 6 px dot and 8 px gap
                    dates.as_ref().map(|d| container(text(d.updated.label.as_str()).font(theme::MONO).size(11).color(p.faint)).padding(iced::Padding::ZERO.left(14.0))),
                ]
                .spacing(2)
                .into(),
            };
            let msg = match node {
                Node::Folder { path, .. } => Message::Toggle(path.clone()),
                Node::Note { path, .. } => Message::Open(path.clone()),
            };
            out.push(button(content).padding([7, 8]).width(Fill).style(p.row(selected)).on_press(msg).into());
            if let Node::Folder { path, children, .. } = node {
                if self.expanded.contains(path) && !children.is_empty() {
                    // nested level: 16 px indent with a 1 px rail in Line color. The rail is
                    // a 1 px strip of line-colored background, not a vertical rule: a rule is
                    // Fill-height, which makes nested rows collapse inside the scrollable.
                    let children = container(column(self.tree_view(children, p)).spacing(2))
                        .padding(iced::Padding::ZERO.left(6.0))
                        .style(p.fill(p.panel, 0.0));
                    let rail = container(children).padding(iced::Padding::ZERO.left(1.0)).style(p.fill(p.line, 0.0));
                    out.push(container(rail).padding(iced::Padding::ZERO.left(14.0)).into());
                }
            }
        }
        out
    }
}

use iced::font::Weight::{Medium, Semibold};
use theme::{Pal, i, icon, weight};

/// Button per the style sheet: 36 px high, 14 px Medium label, optional icon.
fn btn<'a>(
    glyph: Option<char>,
    label: &'a str,
    style: impl Fn(&Theme, button::Status) -> button::Style + 'a,
) -> button::Button<'a, Message> {
    let mut content = row![].spacing(8).align_y(iced::Center);
    if let Some(g) = glyph {
        content = content.push(icon(g));
    }
    content = content.push(text(label).size(14).font(weight(Medium)));
    button(container(content).center_x(iced::Shrink)).padding([9, 14]).style(style)
}

fn node_path(n: &Node) -> &PathBuf {
    match n {
        Node::Folder { path, .. } | Node::Note { path, .. } => path,
    }
}

/// (repo, HEAD) and the dates of every file at that HEAD, keyed by absolute path.
type DatesAt = ((PathBuf, String), HashMap<PathBuf, history::Dates>);

/// Dates at the current HEAD, or None if it is still `known`.
fn read_dates(dir: PathBuf, known: &(PathBuf, String)) -> Result<Option<DatesAt>, String> {
    // empty repo: no HEAD, no history
    let head = git::run(&dir, &["rev-parse", "HEAD"]).unwrap_or_default();
    if dir == known.0 && head == known.1 {
        return Ok(None);
    }
    // ponytail: walks the whole history per new commit; walk known..HEAD if repos get huge
    let dates = if head.is_empty() { HashMap::new() } else { history::dates(&dir)? };
    let dates = dates.into_iter().map(|(rel, d)| (dir.join(rel), d)).collect();
    Ok(Some(((dir, head), dates)))
}

/// Every note under `nodes`, depth first.
fn flatten<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
    for n in nodes {
        match n {
            Node::Folder { children, .. } => flatten(children, out),
            Node::Note { .. } => out.push(n),
        }
    }
}

fn count_notes(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|n| match n {
            Node::Folder { children, .. } => count_notes(children),
            Node::Note { .. } => 1,
        })
        .sum()
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

#[cfg(test)]
mod ui_tests {
    use super::*;
    use iced_test::simulator::Simulator;

    fn temp_repo(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ez-notes-ui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut work = root.clone();
        for f in ["Work", "Tribe", "Facilo"] {
            work = notes::create_folder(&work, f).unwrap();
        }
        notes::create_note(&work, "Other").unwrap();
        let plan = notes::create_note(&work, "Plan").unwrap();
        std::fs::write(notes::md_path(&plan), "---\ntags: [facilo]\n---\n\nHello **world**\n\n- one\n- two\n").unwrap();
        git::run(&root, &["init", "-q"]).unwrap();
        root
    }

    #[test]
    fn missing_repo_folder_disables_notes() {
        let root = temp_repo("missing");
        let mut app = app(&root);
        assert!(app.repo_dir().is_some());
        std::fs::remove_dir_all(&root).unwrap();
        let _ = app.open_repo();
        assert!(app.repo_dir().is_none() && app.tree.is_empty());
        assert!(matches!(&app.sync, Sync::Error(e) if e.contains("missing")));
        assert!(app.error.as_ref().is_some_and(|e| e.contains("missing")), "toasted");
        let _ = app.update(Message::DismissError);
        let _ = app.open_repo();
        assert!(app.error.is_none(), "same error doesn't toast twice");
        let _ = app.update(Message::ShowSyncError);
        assert!(app.error.is_some(), "status click re-shows it");
        app.config.repos.clear();
        let _ = app.open_repo();
        assert!(matches!(app.sync, Sync::Idle), "no repo, no stale error");
        let _ = app.update(Message::Dialog(DialogKind::NewNote));
        let _ = app.update(Message::DialogInput("x".into()));
        let _ = app.update(Message::DialogSubmit);
        assert!(!root.exists(), "never recreates the folder");
    }

    fn app(root: &Path) -> App {
        let repo = Repo { name: "demo".into(), url: String::new(), path: root.to_path_buf() };
        let mut app = App { config: Config { repos: vec![repo], active: 0, theme: theme::Colors::dark() }, ..App::default() };
        app.rescan();
        app
    }

    fn settings() -> iced::Settings {
        iced::Settings { fonts: theme::FONTS.iter().map(|f| (*f).into()).collect(), default_font: theme::SANS, ..Default::default() }
    }

    /// Click the given text, then feed the produced messages back into the app.
    fn click(app: &mut App, label: &str) -> Vec<Message> {
        click_target(app, label)
    }

    fn click_target<S>(app: &mut App, target: S) -> Vec<Message>
    where
        S: iced_test::Selector + Send,
        S::Output: iced_test::selector::Bounded + Clone + Send + std::marker::Sync + 'static,
    {
        let mut ui = Simulator::with_size(settings(), (1280.0, 820.0), app.view());
        ui.click(target).unwrap_or_else(|e| panic!("click: {e:?}"));
        let messages: Vec<Message> = ui.into_messages().collect();
        for m in messages.clone() {
            let _ = app.update(m);
        }
        messages
    }

    fn snap(app: &App, name: &str) {
        let mut ui = Simulator::with_size(settings(), (1280.0, 820.0), app.view());
        let shot = ui.snapshot(&app.theme()).unwrap();
        let path = std::env::temp_dir().join(format!("ez-notes-{name}.png"));
        let _ = std::fs::remove_file(path.with_file_name(format!("ez-notes-{name}-wgpu.png")));
        shot.matches_image(&path).unwrap();
    }

    #[test]
    fn add_repo_modal() {
        let root = temp_repo("modal");
        let mut app = app(&root);
        click(&mut app, "Add repo");
        assert!(app.add_repo.is_some());
        click(&mut app, "Clone"); // disabled while the URL is empty: no-op
        assert!(!app.add_repo.as_ref().unwrap().busy);
        let _ = app.update(Message::AddRepoUrl("git@github.com:me/my-notes.git".into()));
        let _ = app.update(Message::AddRepoName("../escape".into()));
        let _ = app.update(Message::AddRepoSubmit);
        assert!(app.add_repo.as_ref().unwrap().error.is_some(), "invalid name rejected");
        let _ = app.update(Message::AddRepoName(String::new()));
        snap(&app, "modal");
        let _ = app.update(Message::AddRepoSubmit);
        assert!(app.add_repo.as_ref().unwrap().busy);
        let _ = app.update(Message::Cloned(Err("git clone: Repository not found.".into())));
        let form = app.add_repo.as_ref().unwrap();
        assert!(!form.busy && form.error.as_deref() == Some("git clone: Repository not found."));
        click(&mut app, "Cancel");
        assert!(app.add_repo.is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rename_from_title() {
        let root = temp_repo("title");
        let mut app = app(&root);
        let facilo = root.join("Work/Tribe/Facilo");
        let _ = app.update(Message::Open(facilo.join("Plan")));
        let _ = app.update(Message::EditTitle);
        assert_eq!(app.title_edit.as_deref(), Some("Plan"));
        let _ = app.update(Message::TitleInput("Roadmap".into()));
        snap(&app, "title");
        let _ = app.update(Message::TitleSubmit);
        assert!(notes::md_path(&facilo.join("Roadmap")).is_file() && !facilo.join("Plan").exists());
        assert_eq!(app.note.as_ref().unwrap().dir, facilo.join("Roadmap"));
        assert!(app.title_edit.is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn logo_menu_opens_theme() {
        let root = temp_repo("menu");
        let mut app = app(&root);
        app.update = Update::Available("9.9.9".into());
        assert!(Simulator::with_size(settings(), (1280.0, 820.0), app.view()).find("Update available").is_ok(), "update notice in the top bar");
        click_target(&mut app, iced_test::selector::id(LOGO_ID));
        assert!(app.menu_open);
        {
            let mut ui = Simulator::with_size(settings(), (1280.0, 820.0), app.view());
            let shot = ui.snapshot(&app.theme()).unwrap();
            let path = std::env::temp_dir().join("ez-notes-menu.png");
            let _ = std::fs::remove_file(path.with_file_name("ez-notes-menu-wgpu.png"));
            shot.matches_image(&path).unwrap();
        }
        click(&mut app, "Theme");
        assert!(app.show_theme && !app.menu_open);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn table_note() {
        let root = temp_repo("table");
        let note = root.join("Work/Tribe/Facilo/Plan");
        std::fs::write(
            notes::md_path(&note),
            "Intro\n\n| Name | Qty | Note |\n|:---|---:|:---:|\n| **apple** | 3 | `fresh` [link](http://x.y) |\n| a much longer cell that has to wrap onto several lines in the editor because it is long | 12 | - |\n\nAfter\n",
        )
        .unwrap();
        let mut app = app(&root);
        let _ = app.update(Message::Open(note));
        let n = app.note.as_mut().unwrap();
        n.editor.focused = true;
        n.editor.perform(Action::Select { pos: editor::Pos { block: 1, offset: 13 }, extend: false });
        n.editor.perform(Action::Select { pos: editor::Pos { block: 1, offset: 9 }, extend: true });
        snap(&app, "table");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn open_folder_then_note() {
        let root = temp_repo("open");
        let mut app = app(&root);
        // three levels deep: nested rails used to collapse rows and misroute clicks
        for folder in ["Work", "Tribe", "Facilo"] {
            let msgs = click(&mut app, folder);
            assert!(app.expanded.iter().any(|p| p.ends_with(folder)), "{folder} should stay open, messages: {msgs:?}");
        }
        let msgs = click(&mut app, "Plan");
        assert!(app.note.is_some(), "note should open, messages: {msgs:?}");
        let mut ui = Simulator::with_size(settings(), (1280.0, 820.0), app.view());
        let shot = ui.snapshot(&app.theme()).unwrap();
        let path = std::env::temp_dir().join("ez-notes-ui.png");
        let _ = std::fs::remove_file(&path);
        shot.matches_image(&path).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn version(n: usize) -> history::Version {
        history::Version {
            sha: format!("{n:040}"),
            short: format!("{n:07}"),
            author: "Kim".into(),
            ago: format!("{n} hours ago"),
            date: "08 Oct 2026 10:00".into(),
            subject: format!("updated Plan.md #{n}"),
            path: "Work/Tribe/Facilo/Plan/Plan.md".into(),
        }
    }

    #[test]
    fn history_panel() {
        let root = temp_repo("history");
        let mut app = app(&root);
        let _ = app.update(Message::Open(root.join("Work/Tribe/Facilo/Plan")));
        click(&mut app, "History");
        assert!(app.history.is_some());
        let _ = app.update(Message::HistoryLoaded(Ok(vec![version(1), version(2)])));
        let _ = app.update(Message::DiffLoaded(0, history::Mode::Changes, Ok("@@ -1 +1 @@\n-old line\n+new line\n same\n".into())));
        click(&mut app, "updated Plan.md #2");
        assert_eq!(app.history.as_ref().unwrap().selected, 1);
        assert!(app.history.as_ref().unwrap().diff.is_none(), "loading the newly selected diff");
        let _ = app.update(Message::DiffLoaded(0, history::Mode::Changes, Ok("+stale".into())));
        assert!(app.history.as_ref().unwrap().diff.is_none(), "stale answer dropped");
        let _ = app.update(Message::DiffLoaded(1, history::Mode::Changes, Ok("+x\n".repeat(history::LARGE_DIFF_LINES + 1))));
        let msgs = click(&mut app, "Show diff");
        assert!(matches!(msgs[..], [Message::ShowLargeDiff]) && app.history.as_ref().unwrap().show_large);
        let _ = app.update(Message::DiffLoaded(1, history::Mode::Changes, Ok("@@ -1 +1 @@\n-old line\n+new line\n".into())));
        snap(&app, "history");
        click(&mut app, "Compare with current");
        assert_eq!(app.history.as_ref().unwrap().mode, history::Mode::VsCurrent);
        click(&mut app, "Back to note");
        assert!(app.history.is_none() && app.note.is_some());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn conflict_resolver() {
        let root = temp_repo("conflict");
        let mut app = app(&root);
        let text = "Intro\n<<<<<<< HEAD\nmy line\n||||||| base\nold\n=======\ntheir line\n>>>>>>> origin/main\nOutro\n";
        let segments = conflict::parse(text);
        app.conflict = Some(conflict::Conflict {
            files: vec![
                conflict::File {
                    path: "Work/Plan/Plan.md".into(),
                    kind: conflict::Kind::Text { segments, choices: vec![None], manual: None },
                },
                conflict::File { path: "Work/Plan/pic.png".into(), kind: conflict::Kind::Whole { mine: true, theirs: true, pick: None } },
            ],
            selected: 0,
            author: "Kim".into(),
            busy: false,
            merge: true,
        });
        assert!(app.save().units() == 0, "no writes while merging");
        click(&mut app, "Use theirs");
        snap(&app, "conflict");
        assert!(!app.conflict.as_ref().unwrap().ready(), "image still open");
        click(&mut app, "pic.png");
        click(&mut app, "Keep mine");
        assert!(app.conflict.as_ref().unwrap().ready());
        let c = app.conflict.as_ref().unwrap();
        assert!(matches!(c.files[0].resolution(), Some(conflict::Resolution::Text(t)) if t == "Intro\ntheir line\nOutro\n"));
        let msgs = click(&mut app, "Commit & push");
        assert!(matches!(msgs[..], [Message::ConflictFinish]));
        assert!(app.conflict.as_ref().unwrap().busy && app.git_busy);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Open Plan, type `typed` at the start of the first line, then let a
    /// "pull" rewrite the file on disk with `remote(original)`.
    fn edit_while_pulled(name: &str, typed: &str, remote: impl Fn(&str) -> String) -> (PathBuf, App) {
        let root = temp_repo(name);
        let mut app = app(&root);
        let plan = root.join("Work/Tribe/Facilo/Plan");
        let _ = app.update(Message::Open(plan.clone()));
        let n = app.note.as_mut().unwrap();
        n.editor.perform(Action::Select { pos: editor::Pos { block: 0, offset: 0 }, extend: false });
        n.editor.perform(Action::Insert(typed.into()));
        let md = notes::md_path(&plan);
        let original = std::fs::read_to_string(&md).unwrap();
        std::fs::write(&md, remote(&original)).unwrap();
        (root, app)
    }

    #[test]
    fn save_merges_a_pull_that_landed_mid_edit() {
        let (root, mut app) = edit_while_pulled("midedit", "Local ", |o| o.replace("- two", "- two (remote)"));
        let _ = app.update(Message::Pulled(Ok(()))); // dirty: left alone, no reload
        assert!(app.note.as_ref().unwrap().dirty());
        let _ = app.save();
        let disk = std::fs::read_to_string(notes::md_path(&root.join("Work/Tribe/Facilo/Plan"))).unwrap();
        assert!(disk.contains("Local Hello") && disk.contains("two (remote)"), "both kept, nothing reverted:\n{disk}");
        let n = app.note.as_ref().unwrap();
        assert!(!n.dirty() && n.content() == disk, "editor shows the merged note");
        assert_eq!(n.editor.cursor.block, 0, "cursor kept");
        assert!(app.conflict.is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn clashing_edit_opens_the_resolver() {
        let (root, mut app) = edit_while_pulled("clash", "Local ", |o| o.replace("Hello", "Remote hello"));
        let _ = app.save();
        let c = app.conflict.as_ref().expect("resolver opens");
        assert!(!c.merge && c.files.len() == 1);
        let disk = std::fs::read_to_string(notes::md_path(&root.join("Work/Tribe/Facilo/Plan"))).unwrap();
        assert!(disk.contains("Remote hello") && !disk.contains("<<<<<<<"), "disk untouched until resolved");
        click(&mut app, "Use both");
        assert!(app.conflict.as_ref().unwrap().ready());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn edits_typed_during_a_merge_survive_its_resolution() {
        let root = temp_repo("keepedits");
        let mut app = app(&root);
        let _ = app.update(Message::Open(root.join("Work/Tribe/Facilo/Plan")));
        app.note.as_mut().unwrap().editor.perform(Action::Insert("typed during push ".into()));
        app.conflict = Some(conflict::Conflict { files: vec![], selected: 0, author: "Kim".into(), busy: true, merge: true });
        app.check_conflict(); // resolved and pushed: no MERGE_HEAD
        assert!(app.conflict.is_none());
        let n = app.note.as_ref().unwrap();
        assert!(n.dirty() && n.content().contains("typed during push"), "unsaved edits kept");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn background_pull_waits_for_idle() {
        let root = temp_repo("autopull");
        let mut app = app(&root);
        let _ = app.update(Message::Open(root.join("Work/Tribe/Facilo/Plan")));
        app.note.as_mut().unwrap().editor.perform(Action::Insert("x".into()));
        let _ = app.update(Message::AutoPull);
        assert!(!app.pulling, "skipped while an edit is pending");
        app.note.as_mut().unwrap().saved_rev = app.note.as_ref().unwrap().editor.revision;
        let _ = app.update(Message::AutoPull);
        assert!(app.pulling && app.git_busy);
        let md = notes::md_path(&root.join("Work/Tribe/Facilo/Plan"));
        let before = std::fs::read_to_string(&md).unwrap();
        app.note.as_mut().unwrap().editor.perform(Action::Insert("y".into())); // typed during the pull
        let _ = app.save();
        assert_eq!(std::fs::read_to_string(&md).unwrap(), before, "no writes during a pull");
        let _ = app.update(Message::AutoPulled(Ok(())));
        assert!(!app.pulling && !app.git_busy && matches!(app.sync, Sync::Idle));
        let _ = app.save();
        assert_ne!(std::fs::read_to_string(&md).unwrap(), before, "saves resume after the pull");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn notes_sorted_by_recency() {
        let root = temp_repo("recency");
        let commit = |date: &str, msg: &str| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-qm", msg])
                .env("GIT_AUTHOR_DATE", date)
                .current_dir(&root)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        };
        git::run(&root, &["init", "-q"]).unwrap();
        git::run(&root, &["add", "-A"]).unwrap();
        commit("2026-01-02T10:00:00", "created");
        let facilo = root.join("Work/Tribe/Facilo");
        std::fs::write(notes::md_path(&facilo.join("Other")), "newer\n").unwrap();
        git::run(&root, &["add", "-A"]).unwrap();
        commit("2026-03-04T10:00:00", "updated Other.md");

        let mut app = app(&root);
        let _ = app.update(Message::DatesLoaded(read_dates(root.clone(), &app.dates_at)));
        let Node::Folder { children, .. } = &app.tree[0] else { panic!() };
        let Node::Folder { children, .. } = &children[0] else { panic!() };
        let Node::Folder { children, .. } = &children[0] else { panic!() };
        let names: Vec<_> = children.iter().map(Node::name).collect();
        assert_eq!(names, ["Other", "Plan"], "most recently updated first");
        let other = &app.dates[&notes::md_path(&facilo.join("Other"))];
        assert_eq!((other.created.label.as_str(), other.updated.label.as_str()), ("02 Jan 2026 10:00", "04 Mar 2026 10:00"));

        let _ = app.update(Message::SortBy(Sort::Notes));
        let _ = app.update(Message::Open(facilo.join("Other")));
        snap(&app, "recency");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
