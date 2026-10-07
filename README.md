# Yana

**Yet Another Note Application.** A desktop note-taking app written in Rust with [iced](https://iced.rs). Notes are plain Markdown files in a git repository you own. Every save is committed and pushed automatically, so your notes are versioned, synced, and readable anywhere, even without Yana.

## Features

- **WYSIWYG editor.** You see formatted text, not Markdown syntax. It supports bold, italic, headings, links, images, bulleted and numbered lists (nested), and code blocks.
- **Git-backed storage.** Connect one or more (private) git repositories and switch between them. Saving commits `updated <file>.md` and pushes it.
- **Plain files.** Every note is a folder with a Markdown file and its images, organised in folders nested as deep as you like. The repository *is* the export.
- **Tags** with fixed or automatic colors, stored in the repository so they sync too.
- **Theming.** Edit the app's color palette, or switch between the dark and light presets.

## Requirements

- [Rust](https://rustup.rs) (stable)
- `git` on your `PATH`, with access to your repository. Yana runs the system `git`, so your existing SSH keys, `ssh-agent` / macOS keychain, and git credential helpers just work.

Yana never shows a password prompt: git runs non-interactively (SSH in batch mode). If your SSH key has a passphrase, load it into your agent first (`ssh-add`).

## Building and running

### Prerequisites

1. **Rust**: install with [rustup](https://rustup.rs):

   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

   Open a new terminal afterwards (or run `source ~/.cargo/env`) so `cargo` is on your `PATH`.

2. **macOS: Xcode Command Line Tools**: needed for the linker and for `git`:

   ```sh
   xcode-select --install
   ```

   If the build fails with *"You have not agreed to the Xcode license agreements"*, run `sudo xcodebuild -license accept` in a terminal.

### Run from source

```sh
cargo run --release
```

### Build the macOS app

```sh
./scripts/bundle-macos.sh
```

This produces **`dist/Yana.app`**:

- a release build of the binary,
- an `Info.plist` (name, bundle id `app.yana.Yana`, version from `Cargo.toml`),
- the app icon, rendered from `assets/icons/` (the small logo at 16/32 px, the full logo above that),
- an ad-hoc code signature, which is enough to run it on the Mac that built it.

Start it with `open dist/Yana.app`, or drag it into `/Applications` and launch it like any other app.

Notes:

- The app is built for the architecture of the Mac you build on (Apple Silicon or Intel).
- An ad-hoc signed app is **not notarized**. If you copy it to another Mac, Gatekeeper will block it the first time. Right-click → **Open** once, or run `xattr -dr com.apple.quarantine /Applications/Yana.app`. To distribute it properly, sign it with a Developer ID certificate and notarize it.
- When launched from Finder or the Dock, Yana uses `/usr/bin/git` and the SSH agent macOS provides. Keys added with `ssh-add --apple-use-keychain` work as expected.

### Releases

Every push to `main` can publish a release (`.github/workflows/release.yml`). The version is derived from the [Conventional Commits](https://www.conventionalcommits.org) since the last `v*` tag:

| Commits since the last release | Next version |
|---|---|
| `feat!:` / `fix!:` or a `BREAKING CHANGE:` footer | major (0.4.2 → 1.0.0) |
| `feat:` | minor (0.4.2 → 0.5.0) |
| `fix:` / `perf:` | patch (0.4.2 → 0.4.3) |
| only `docs:`, `chore:`, `ci:`, … | no release |

The workflow creates the tag and a release (listed under **Releases**, marked *Latest*) with notes grouped into Features, Fixes and Other. The git tag is the source of truth for the version: CI writes it into `Cargo.toml` before building, the version in the repository isn't bumped. Run `./scripts/release-plan.sh` to preview the next version and notes locally.

The release gets:

| File | Contents |
|---|---|
| `Yana-<version>-macos-universal.zip` | `Yana.app` for Apple Silicon and Intel (ad-hoc signed: right-click → **Open** the first time) |
| `Yana-<version>-windows-x86_64.zip` | `Yana.exe` |
| `Yana-<version>-linux-x86_64.tar.gz` | `yana` binary (needs a Vulkan or OpenGL driver) |
| Source code (zip, tar.gz) | Added by GitHub automatically |

All builds need `git` installed on the user's machine.

### Tests

```sh
cargo test
```

The UI tests drive the real views headlessly with [`iced_test`](https://crates.io/crates/iced_test) and write rendered snapshots (`ez-notes-*.png`) to your temp directory.

## Getting started

1. Click **Add repo** and paste the repository URL (e.g. `git@github.com:you/notes.git`). An empty repository is fine.
2. Create folders and notes with **Folder** and **Note** in the sidebar.
3. Write. About 1.5 seconds after you stop typing, the note is saved, committed and pushed. The status in the top bar shows *unsaved*, *syncing…*, *synced*, or the git error.

**Sync** saves the open note, pulls the latest changes and pushes. Remote changes to the open note are loaded unless you're in the middle of editing it. Unsaved changes are flushed when you close the window.

## How notes are stored

```
your-repo/
├── .ez-notes/
│   └── tags.toml              # tag colors for the whole repo
├── Work/                      # a folder (any directory without a same-named .md)
│   └── Planning/
│       └── Roadmap/           # a note = a folder…
│           ├── Roadmap.md     # …with a Markdown file of the same name
│           └── diagram.png    # …and its images next to it
```

- **Notes.** A folder `Name/` containing `Name.md` is a note. The note's title is the folder name. Click the title (or use **Rename** in the sidebar) to rename it; the folder and the file are renamed together.
- **Images.** Inserting an image copies it into the note's folder and links it relatively (`![](diagram.png)`). It renders on GitHub and in any Markdown viewer.
- **Tags.** Tags live in the note's YAML front matter, so they travel with the note:

  ```markdown
  ---
  tags: [work, idea]
  ---

  Note text…
  ```

  Tag colors are shared in `.ez-notes/tags.toml`. A tag either has a fixed color from the palette or `auto`, which derives a stable color from the tag's name:

  ```toml
  [tags]
  work = "#3b82f6"
  idea = "auto"
  ```

- **Markdown Yana can't edit** (tables, block quotes, footnotes, strikethrough, task lists, HTML, …) is preserved exactly and shown as greyed-out source. Opening and saving a note never drops content written in another editor.
- **Export.** **Export…** copies the whole repository, minus `.git`, to a folder of your choice.

## Editor

### Toolbar

Bold, italic · H1, H2, H3, paragraph · bulleted list, numbered list, code block · link, image. The button for the current block type is highlighted.

### Typing shortcuts

At the start of an empty line:

| Type | Becomes |
|---|---|
| `# ` / `## ` / `### ` | Heading 1 / 2 / 3 |
| `- ` or `* ` | Bulleted list |
| `1. ` | Numbered list |
| ```` ```lang ```` then Enter | Code block (language optional) |

In a list, Enter on an empty item leaves the list. In a code block, Enter on an empty last line leaves the block, and Tab inserts four spaces.

### Keyboard

| Keys | Action |
|---|---|
| ⌘B / ⌘I | Bold / italic (the selection, or what you type next) |
| ⌘K | Add or edit a link |
| ⌘Z / ⇧⌘Z (⌘Y) | Undo / redo |
| ⌘A, ⌘C, ⌘X, ⌘V | Select all, copy, cut, paste (copied text is Markdown; pasted Markdown is formatted) |
| Tab / ⇧Tab | Indent / outdent a list item |
| ⌥← / ⌥→, ⌥⌫ | Move by word, delete word |
| ⌘← / ⌘→, ⌘↑ / ⌘↓ | Start/end of block, start/end of note |
| Double-click | Select word |
| ⌘-click a link | Open it in the browser (http, https and mailto only) |
| Esc | Leave the editor |

## Theming

Click the logo and choose **Theme**. Each of the ten palette colors (ground, panel, raised, line, signal/primary, tag, destructive, text, muted, faint) can be edited as a hex value and applies immediately. **Dark preset** and **Light preset** reset the whole palette. The palette is stored in your local config, not in the notes repository.

## Where things live on your machine

| What | macOS location |
|---|---|
| Settings (repositories, theme) | `~/Library/Application Support/ez-notes/config.toml` |
| Cloned repositories | `~/Library/Application Support/ez-notes/repos/` |

On Linux and Windows these follow the platform's config and data directories (via the [`dirs`](https://crates.io/crates/dirs) crate).

**Remove repo** deletes the local clone (anything not yet pushed is lost) but never touches the remote repository.

## Project layout

| File | Responsibility |
|---|---|
| `src/main.rs` | The iced application: state, messages, views |
| `src/editor.rs` | Editing logic (cursor, selection, undo, block and inline operations), UI-free |
| `src/widget.rs` | The custom WYSIWYG widget: layout, drawing, mouse and keyboard input |
| `src/doc.rs` | Document model and Markdown parsing/serialisation |
| `src/notes.rs` | Notes on disk: tree scanning, create/rename/delete, images, export |
| `src/git.rs` | Wrapper around the `git` command line (clone, pull, commit, push) |
| `src/tags.rs` | Front matter and tag colors |
| `src/theme.rs` | Palette, presets, widget styles, fonts and icons |
| `src/config.rs` | Local settings file |
| `scripts/bundle-macos.sh` | Builds `dist/Yana.app` |
| `examples/iconset.rs` | Renders the app icon sizes from the SVG logos |
| `scripts/release-plan.sh` | Next version and release notes from the commit log |
| `.github/workflows/release.yml` | Versions, builds and publishes releases on push to `main` |

## Known limitations

- Input methods for composed characters (IME, some dead keys) aren't supported yet.
- The editor doesn't yet scroll to keep the cursor in view while you type.
- No syntax highlighting in code blocks.
- A hard line break inside a paragraph is preserved but shown as source.

## Credits

Logo: `assets/icons/yana-logo.svg` (full) and `yana-logo-small.svg` (16 px). In the app it is recolored to match the active theme.

Fonts: [IBM Plex Sans and Plex Mono](https://github.com/IBM/plex) (SIL Open Font License), icons: [Lucide](https://lucide.dev) (ISC). License texts are in `assets/fonts/`.
