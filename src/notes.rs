//! Notes on disk: a note is a folder `<Name>/` holding `<Name>.md` plus its
//! assets; any other folder is just a folder and can nest without limit.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub enum Node {
    Folder { name: String, path: PathBuf, children: Vec<Node> },
    Note { name: String, path: PathBuf, tags: Vec<String> },
}

impl Node {
    pub fn name(&self) -> &str {
        match self {
            Node::Folder { name, .. } | Node::Note { name, .. } => name,
        }
    }
}

pub fn md_path(note_dir: &Path) -> PathBuf {
    let name = note_dir.file_name().unwrap_or_default().to_string_lossy();
    note_dir.join(format!("{name}.md"))
}

pub fn is_note(dir: &Path) -> bool {
    md_path(dir).is_file()
}

pub fn scan(dir: &Path) -> Vec<Node> {
    let mut nodes: Vec<Node> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok().filter(|n| !n.starts_with('.'))?;
            let path = e.path();
            Some(if is_note(&path) {
                Node::Note { tags: note_tags(&path), name, path }
            } else {
                Node::Folder { name, children: scan(&path), path }
            })
        })
        .collect();
    nodes.sort_by_key(|n| (matches!(n, Node::Note { .. }), n.name().to_lowercase()));
    nodes
}

fn note_tags(dir: &Path) -> Vec<String> {
    let md = fs::read_to_string(md_path(dir)).unwrap_or_default();
    crate::tags::split(&md.replace("\r\n", "\n")).0.tags
}

/// Reject names that would escape the folder or be invisible.
pub fn valid_name(name: &str) -> Result<&str, String> {
    let n = name.trim();
    if n.is_empty() || n.starts_with('.') || n.contains(['/', '\\', ':']) {
        return Err(format!("invalid name: {name:?}"));
    }
    Ok(n)
}

fn fresh(path: PathBuf) -> io::Result<PathBuf> {
    if path.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", path.display())));
    }
    Ok(path)
}

pub fn create_note(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let dir = fresh(parent.join(name))?;
    fs::create_dir_all(&dir)?;
    fs::write(md_path(&dir), "")?; // the title is the folder name, shown above the note
    Ok(dir)
}

pub fn create_folder(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let dir = fresh(parent.join(name))?;
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(".gitkeep"), "")?; // git doesn't track empty folders
    Ok(dir)
}

pub fn rename(path: &Path, new_name: &str) -> io::Result<PathBuf> {
    let new = fresh(path.with_file_name(new_name))?;
    let old_md = md_path(path);
    let was_note = old_md.is_file();
    fs::rename(path, &new)?;
    if was_note {
        fs::rename(new.join(old_md.file_name().unwrap()), md_path(&new))?;
    }
    Ok(new)
}

/// Copy an image into the note folder; returns the (deduplicated) file name.
pub fn import_image(note_dir: &Path, src: &Path) -> io::Result<String> {
    let stem = src.file_stem().unwrap_or_default().to_string_lossy();
    let ext = src.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut name = format!("{stem}{ext}");
    let mut i = 1;
    while note_dir.join(&name).exists() {
        name = format!("{stem}-{i}{ext}");
        i += 1;
    }
    fs::copy(src, note_dir.join(&name))?;
    Ok(name)
}

/// Copy the notes tree (without `.git`) to `dest`.
pub fn export(from: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        if e.file_name() == ".git" {
            continue;
        }
        let to = dest.join(e.file_name());
        if e.file_type()?.is_dir() {
            export(&e.path(), &to)?;
        } else {
            fs::copy(e.path(), to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_lifecycle() {
        let root = std::env::temp_dir().join(format!("ez-notes-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = create_folder(&root, "Work").unwrap();
        let note = create_note(&folder, "Plan").unwrap();
        assert!(md_path(&note).ends_with("Work/Plan/Plan.md"));
        let img = root.join("pic.png");
        fs::write(&img, b"x").unwrap();
        assert_eq!(import_image(&note, &img).unwrap(), "pic.png");
        assert_eq!(import_image(&note, &img).unwrap(), "pic-1.png");
        let renamed = rename(&note, "Roadmap").unwrap();
        assert!(md_path(&renamed).is_file() && renamed.join("pic-1.png").is_file());
        let tree = scan(&root);
        assert!(matches!(&tree[0], Node::Folder { children, .. } if matches!(&children[0], Node::Note { name, .. } if name == "Roadmap")));
        assert!(valid_name("../x").is_err() && valid_name(".hidden").is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
