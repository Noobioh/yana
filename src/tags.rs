//! Tags: per-note list in the note's YAML front matter (`tags: [a, b]`),
//! colors for the whole repo in `.ez-notes/tags.toml`.

use iced::Color;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const AUTO: &str = "auto";

pub const PALETTE: [&str; 10] = [
    "#ef4444", "#f97316", "#eab308", "#22c55e", "#14b8a6", "#3b82f6", "#6366f1", "#a855f7", "#ec4899", "#64748b",
];

/// Front matter of a note. Only `tags` is understood; other lines are kept verbatim.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Front {
    pub tags: Vec<String>,
    rest: Vec<String>,
}

/// Split a note file into front matter and markdown body.
pub fn split(md: &str) -> (Front, &str) {
    let mut front = Front::default();
    let Some(after) = md.strip_prefix("---\n") else { return (front, md) };
    let mut pos = 0;
    let mut lines = vec![];
    let body = loop {
        let Some(line) = after[pos..].split_inclusive('\n').next() else { return (Front::default(), md) };
        pos += line.len();
        if line.trim_end() == "---" {
            break &after[pos..];
        }
        lines.push(line.trim_end_matches('\n'));
    };
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let Some(value) = line.strip_prefix("tags:") else {
            front.rest.push(line.to_string());
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            // block list:  tags:\n  - a\n  - b
            while let Some(item) = lines.get(i).and_then(|l| l.trim_start().strip_prefix("- ")) {
                front.tags.push(unquote(item));
                i += 1;
            }
        } else {
            let inner = value.trim_start_matches('[').trim_end_matches(']');
            front.tags.extend(inner.split(',').map(unquote).filter(|t| !t.is_empty()));
        }
    }
    (front, body.trim_start_matches('\n'))
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches(['"', '\'']).to_string()
}

impl Front {
    /// Front matter followed by `body`; no front matter block when there's nothing to write.
    pub fn render(&self, body: &str) -> String {
        if self.tags.is_empty() && self.rest.is_empty() {
            return body.to_string();
        }
        let mut out = String::from("---\n");
        if !self.tags.is_empty() {
            out.push_str(&format!("tags: [{}]\n", self.tags.join(", ")));
        }
        for l in &self.rest {
            out.push_str(l);
            out.push('\n');
        }
        out.push_str("---\n\n");
        out.push_str(body);
        out
    }
}

/// Tag names stay simple so they never need quoting in YAML.
pub fn valid_tag(name: &str) -> Option<String> {
    let t = name.trim().trim_start_matches('#').replace(' ', "-");
    let ok = !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || "-_/".contains(c));
    ok.then_some(t)
}

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct TagsFile {
    /// tag -> "#rrggbb" or "auto"
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

fn file(repo: &Path) -> PathBuf {
    repo.join(".ez-notes/tags.toml")
}

impl TagsFile {
    pub fn load(repo: &Path) -> Self {
        std::fs::read_to_string(file(repo)).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, repo: &Path) -> Result<(), String> {
        let f = file(repo);
        std::fs::create_dir_all(f.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(f, toml::to_string_pretty(self).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
    }

    pub fn setting(&self, tag: &str) -> &str {
        self.tags.get(tag).map_or(AUTO, String::as_str)
    }

    pub fn color(&self, tag: &str) -> Color {
        let hex = match self.setting(tag) {
            AUTO => auto(tag),
            hex => hex,
        };
        hex_color(hex).unwrap_or_else(|| hex_color(auto(tag)).unwrap())
    }
}

pub fn hex_color(hex: &str) -> Option<Color> {
    let v = u32::from_str_radix(hex.strip_prefix('#')?, 16).ok().filter(|_| hex.len() == 7)?;
    Some(Color::from_rgb8((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Stable palette color derived from the name (FNV-1a).
pub fn auto(tag: &str) -> &'static str {
    let h = tag.bytes().fold(0x811c9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x01000193));
    PALETTE[h as usize % PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter() {
        let (f, body) = split("---\ntitle: X\ntags: [work, \"idea\"]\n---\n\n# Hi\n");
        assert_eq!(f.tags, ["work", "idea"]);
        assert_eq!(body, "# Hi\n");
        assert_eq!(f.render(body), "---\ntags: [work, idea]\ntitle: X\n---\n\n# Hi\n");

        let (f, _) = split("---\ntags:\n  - a\n  - b\n---\nbody");
        assert_eq!(f.tags, ["a", "b"]);

        let (f, body) = split("# no front matter\n");
        assert_eq!((f.render(body).as_str(), body), ("# no front matter\n", "# no front matter\n"));

        // unterminated: leave untouched
        assert_eq!(split("---\nfoo\n").1, "---\nfoo\n");

        let mut f = Front::default();
        f.tags.push("x".into());
        assert_eq!(split(&f.render("b\n")).0.tags, ["x"]);
    }

    #[test]
    fn colors() {
        let mut t = TagsFile::default();
        assert_eq!(t.color("work"), hex_color(auto("work")).unwrap());
        t.tags.insert("work".into(), "#ff0000".into());
        assert_eq!(t.color("work"), Color::from_rgb8(255, 0, 0));
        assert_eq!(valid_tag(" #my tag "), Some("my-tag".into()));
        assert_eq!(valid_tag("a,b"), None);
    }
}
