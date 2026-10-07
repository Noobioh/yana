//! Document model and Markdown conversion.
//!
//! Anything the editor can't represent faithfully is kept verbatim: whole
//! blocks become `Kind::Raw`, inline constructs become spans with `style.raw`.
//! That way saving never silently drops content the user wrote elsewhere.

use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};
pub use pulldown_cmark::Alignment;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Paragraph,
    Heading(u8),
    List { ordered: bool, depth: u8 },
    Image { src: String, alt: String },
    /// Fenced code block; text is the code, without fences.
    Code { lang: String },
    /// GFM table: cells separated by '\t', rows by '\n'; the first row is the header.
    Table { align: Vec<Alignment> },
    Raw,
}

pub const CELL: char = '\t';
pub const ROW: char = '\n';

impl Kind {
    /// Blocks whose text is stored as-is: newlines allowed, no inline formatting.
    pub fn is_verbatim(&self) -> bool {
        matches!(self, Kind::Code { .. } | Kind::Raw)
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// Verbatim markdown source, written back unescaped.
    pub raw: bool,
    pub link: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub kind: Kind,
    pub spans: Vec<Span>,
}

impl Block {
    pub fn new(kind: Kind, spans: Vec<Span>) -> Self {
        let mut b = Block { kind, spans };
        b.normalize();
        b
    }

    pub fn paragraph() -> Self {
        Block::new(Kind::Paragraph, vec![])
    }

    pub fn raw(text: &str) -> Self {
        Block::verbatim(Kind::Raw, text)
    }

    pub fn table(align: Vec<Alignment>, rows: Vec<Vec<Vec<Span>>>) -> Self {
        let sep = |c: char| Span { text: c.into(), style: Style::default() };
        let mut spans = vec![];
        for (r, row) in rows.into_iter().enumerate() {
            if r > 0 {
                spans.push(sep(ROW));
            }
            for (c, cell) in row.into_iter().enumerate() {
                if c > 0 {
                    spans.push(sep(CELL));
                }
                spans.extend(cell);
            }
        }
        Block::new(Kind::Table { align }, spans)
    }

    /// Byte range of every table cell, by row.
    pub fn cells(&self) -> Vec<Vec<Range<usize>>> {
        let text = self.text();
        let mut rows = vec![vec![]];
        let mut start = 0;
        for (i, c) in text.char_indices().chain([(text.len(), ROW)]) {
            if c == CELL || c == ROW {
                rows.last_mut().unwrap().push(start..i);
                start = i + 1;
                if c == ROW {
                    rows.push(vec![]);
                }
            }
        }
        rows.pop();
        rows
    }

    /// Table as rows of cells of spans; inverse of `Block::table`.
    pub fn grid(&self) -> Vec<Vec<Vec<Span>>> {
        self.cells().into_iter().map(|row| row.into_iter().map(|c| self.slice(c)).collect()).collect()
    }

    pub fn verbatim(kind: Kind, text: &str) -> Self {
        Block::new(kind, vec![Span { text: text.into(), style: Style { raw: true, ..Style::default() } }])
    }

    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.spans.iter().map(|s| s.text.len()).sum()
    }

    /// Merge neighbours with equal style, drop empty spans.
    pub fn normalize(&mut self) {
        let mut out: Vec<Span> = Vec::with_capacity(self.spans.len());
        for s in self.spans.drain(..) {
            if s.text.is_empty() {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.style == s.style => last.text.push_str(&s.text),
                _ => out.push(s),
            }
        }
        self.spans = out;
    }

    /// Split spans so that a span boundary exists at `offset`; returns the
    /// index of the first span starting at or after `offset`.
    fn cut(&mut self, offset: usize) -> usize {
        let mut pos = 0;
        for i in 0..self.spans.len() {
            let len = self.spans[i].text.len();
            if offset == pos {
                return i;
            }
            if offset < pos + len {
                let tail = self.spans[i].text.split_off(offset - pos);
                let style = self.spans[i].style.clone();
                self.spans.insert(i + 1, Span { text: tail, style });
                return i + 1;
            }
            pos += len;
        }
        self.spans.len()
    }

    pub fn insert(&mut self, offset: usize, text: &str, style: Style) {
        let i = self.cut(offset);
        self.spans.insert(i, Span { text: text.into(), style });
        self.normalize();
    }

    pub fn delete(&mut self, range: Range<usize>) {
        let a = self.cut(range.start);
        let b = self.cut(range.end);
        self.spans.drain(a..b);
        self.normalize();
    }

    pub fn slice(&self, range: Range<usize>) -> Vec<Span> {
        let mut b = self.clone();
        let a = b.cut(range.start);
        let e = b.cut(range.end);
        b.spans.drain(a..e).collect()
    }

    pub fn split_off(&mut self, offset: usize) -> Vec<Span> {
        let i = self.cut(offset);
        let tail = self.spans.split_off(i);
        self.normalize();
        tail
    }

    pub fn map_style(&mut self, range: Range<usize>, f: impl Fn(&mut Style)) {
        let a = self.cut(range.start);
        let b = self.cut(range.end);
        self.spans[a..b].iter_mut().for_each(|s| f(&mut s.style));
        self.normalize();
    }

    /// Style of the character before `offset` (what typing continues with).
    pub fn style_at(&self, offset: usize) -> Style {
        let mut pos = 0;
        for s in &self.spans {
            pos += s.text.len();
            if offset <= pos && offset > pos - s.text.len() {
                return s.style.clone();
            }
        }
        self.spans.first().map(|s| s.style.clone()).unwrap_or_default()
    }
}

// ---------------------------------------------------------------- parsing

pub fn parse(src: &str) -> Vec<Block> {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let ev: Vec<(Event, Range<usize>)> = Parser::new_ext(src, opts).into_offset_iter().collect();
    let mut blocks = vec![];
    let mut i = 0;
    while i < ev.len() {
        let end = end_of(&ev, i);
        match &ev[i].0 {
            Event::Start(Tag::Paragraph) => blocks.extend(inline(src, &ev[i + 1..end], Kind::Paragraph)),
            Event::Start(Tag::Heading { level, .. }) => {
                blocks.extend(inline(src, &ev[i + 1..end], Kind::Heading(*level as u8)))
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                let code: String = ev[i + 1..end]
                    .iter()
                    .filter_map(|(e, _)| if let Event::Text(t) = e { Some(t.as_ref()) } else { None })
                    .collect();
                blocks.push(Block::verbatim(Kind::Code { lang }, code.strip_suffix('\n').unwrap_or(&code)));
            }
            Event::Start(Tag::Table(align)) => match table(src, &ev[i..=end], align) {
                Some(b) => blocks.push(b),
                None => blocks.push(Block::raw(src[ev[i].1.clone()].trim_end())),
            },
            Event::Start(Tag::List(_)) => match list(src, &ev, i, 0) {
                Some(items) => blocks.extend(items),
                None => blocks.push(Block::raw(src[ev[i].1.clone()].trim_end())),
            },
            _ => blocks.push(Block::raw(src[ev[i].1.clone()].trim_end())),
        }
        i = end + 1;
    }
    blocks
}

/// Index of the `End` matching the `Start` at `i` (or `i` itself for leaf events).
fn end_of(ev: &[(Event, Range<usize>)], i: usize) -> usize {
    if !matches!(ev[i].0, Event::Start(_)) {
        return i;
    }
    let mut depth = 0;
    for (j, (e, _)) in ev.iter().enumerate().skip(i) {
        match e {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            }
            _ => {}
        }
    }
    ev.len() - 1
}

/// Lists made of single-paragraph items (with nesting) become List blocks;
/// anything fancier returns None so the caller keeps the whole list raw.
fn list(src: &str, ev: &[(Event, Range<usize>)], i: usize, depth: u8) -> Option<Vec<Block>> {
    let Event::Start(Tag::List(start)) = &ev[i].0 else { return None };
    let ordered = start.is_some();
    let end = end_of(ev, i);
    let mut out = vec![];
    let mut j = i + 1;
    while j < end {
        let item_end = end_of(ev, j);
        if !matches!(ev[j].0, Event::Start(Tag::Item)) {
            return None;
        }
        let mut k = j + 1;
        let mut content = vec![];
        let mut paragraphs = 0;
        let mut nested = vec![];
        while k < item_end {
            let e = end_of(ev, k);
            match &ev[k].0 {
                Event::Start(Tag::List(_)) => nested.extend(list(src, ev, k, depth + 1)?),
                _ if !nested.is_empty() => return None, // content after a nested list
                Event::Start(Tag::Paragraph) => {
                    paragraphs += 1;
                    content.extend_from_slice(&ev[k + 1..e]);
                }
                Event::Start(Tag::Image { .. }) => return None,
                Event::Start(t) if is_block_tag(t) => return None,
                Event::Rule | Event::Html(_) => return None,
                _ => content.extend_from_slice(&ev[k..=e]),
            }
            k = e + 1;
        }
        if paragraphs > 1 {
            return None;
        }
        out.extend(inline(src, &content, Kind::List { ordered, depth }));
        out.extend(nested);
        j = item_end + 1;
    }
    Some(out)
}

/// Table events (Start(Table) ..= End(Table)) -> Table block.
fn table(src: &str, ev: &[(Event, Range<usize>)], align: &[Alignment]) -> Option<Block> {
    let mut rows: Vec<Vec<Vec<Span>>> = vec![];
    let mut j = 1;
    while j < ev.len() {
        match &ev[j].0 {
            Event::Start(Tag::TableHead | Tag::TableRow) => rows.push(vec![]),
            Event::Start(Tag::TableCell) => {
                let e = end_of(ev, j);
                // a non-Paragraph kind keeps images inline (as raw source)
                let cell = inline(src, &ev[j + 1..e], Kind::Table { align: vec![] }).pop()?;
                rows.last_mut()?.push(cell.spans);
                j = e;
            }
            _ => {}
        }
        j += 1;
    }
    for row in &mut rows {
        row.resize(align.len(), vec![]);
    }
    Some(Block::table(align.to_vec(), rows))
}

fn is_block_tag(t: &Tag) -> bool {
    matches!(
        t,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::BlockQuote(_)
            | Tag::CodeBlock(_)
            | Tag::HtmlBlock
            | Tag::List(_)
            | Tag::Item
            | Tag::FootnoteDefinition(_)
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_)
    )
}

/// Turn inline events into one block (several when a paragraph contains images).
fn inline(src: &str, ev: &[(Event, Range<usize>)], kind: Kind) -> Vec<Block> {
    let mut blocks = vec![];
    let mut spans = vec![];
    let (mut bold, mut italic) = (0, 0);
    let mut link: Option<String> = None;
    let mut j = 0;
    while j < ev.len() {
        let style = Style { bold: bold > 0, italic: italic > 0, link: link.clone(), ..Style::default() };
        let raw = |r: &Range<usize>| Span {
            text: src[r.clone()].to_string(),
            style: Style { raw: true, ..Style::default() },
        };
        match &ev[j].0 {
            Event::Text(t) => spans.push(Span { text: t.to_string(), style }),
            Event::Code(t) => spans.push(Span { text: t.to_string(), style: Style { code: true, ..style } }),
            Event::SoftBreak => spans.push(Span { text: " ".into(), style }),
            Event::Start(Tag::Strong) => bold += 1,
            Event::End(TagEnd::Strong) => bold -= 1,
            Event::Start(Tag::Emphasis) => italic += 1,
            Event::End(TagEnd::Emphasis) => italic -= 1,
            Event::Start(Tag::Link { link_type, dest_url, title, .. }) => {
                let e = end_of(ev, j);
                let has_image = ev[j..e].iter().any(|(e, _)| matches!(e, Event::Start(Tag::Image { .. })));
                let simple = matches!(link_type, LinkType::Inline | LinkType::Autolink | LinkType::Reference
                    | LinkType::Collapsed | LinkType::Shortcut);
                if has_image || !title.is_empty() || !simple || link.is_some() {
                    spans.push(raw(&ev[j].1));
                    j = e;
                } else {
                    link = Some(dest_url.to_string());
                }
            }
            Event::End(TagEnd::Link) => link = None,
            Event::Start(Tag::Image { dest_url, title, .. }) if kind == Kind::Paragraph && title.is_empty() => {
                let e = end_of(ev, j);
                let alt: String = ev[j..e]
                    .iter()
                    .filter_map(|(e, _)| match e {
                        Event::Text(t) | Event::Code(t) => Some(t.to_string()),
                        _ => None,
                    })
                    .collect();
                let before = std::mem::take(&mut spans);
                if before.iter().any(|s| !s.text.trim().is_empty()) {
                    blocks.push(Block::new(kind.clone(), before));
                }
                blocks.push(Block::new(Kind::Image { src: dest_url.to_string(), alt }, vec![]));
                j = e;
            }
            // Strikethrough, nested images, math, html, footnotes, hard breaks, task markers:
            // keep their source verbatim.
            Event::Start(_) => {
                spans.push(raw(&ev[j].1));
                j = end_of(ev, j);
            }
            Event::End(_) => {}
            Event::TaskListMarker(done) => spans.push(Span {
                text: if *done { "[x] " } else { "[ ] " }.into(),
                style: Style { raw: true, ..Style::default() },
            }),
            _ => spans.push(raw(&ev[j].1)),
        }
        j += 1;
    }
    let had_image = !blocks.is_empty();
    if !had_image || spans.iter().any(|s| !s.text.trim().is_empty()) {
        if had_image {
            // text following an image in the same paragraph starts fresh
            if let Some(first) = spans.first_mut() {
                first.text = first.text.trim_start().to_string();
            }
        }
        blocks.push(Block::new(kind, spans));
    }
    blocks
}

// ---------------------------------------------------------------- serializing

pub fn serialize(blocks: &[Block]) -> String {
    let mut out = String::new();
    // (ordered, counter) per list depth
    let mut counters: Vec<(bool, usize)> = vec![];
    let mut prev_list = false;
    let mut top_ordered = None;
    for b in blocks {
        let line = match &b.kind {
            Kind::Paragraph if b.spans.is_empty() => continue,
            Kind::Paragraph => block_escape(&inline_md(&b.spans)).trim_end().to_string(),
            Kind::Heading(n) => format!("{} {}", "#".repeat(*n as usize), inline_md(&b.spans)).trim_end().to_string(),
            Kind::List { ordered, depth } => {
                // never deeper than one below the previous item, or it would indent into a code block
                let d = (*depth as usize).min(counters.len());
                counters.truncate(d + 1);
                while counters.len() <= d {
                    counters.push((*ordered, 0));
                }
                if counters[d].0 != *ordered {
                    counters[d] = (*ordered, 0);
                }
                counters[d].1 += 1;
                let marker = if *ordered { format!("{}.", counters[d].1) } else { "-".into() };
                let body = block_escape(&inline_md(&b.spans));
                format!("{}{} {}", "    ".repeat(d), marker, body).trim_end().to_string()
            }
            Kind::Image { src, alt } => {
                let src = if src.contains([' ', '(', ')']) { format!("<{src}>") } else { src.clone() };
                format!("![{}]({})", escape(alt), src)
            }
            Kind::Code { lang } => {
                let code = b.text();
                let fence = "`".repeat(longest_run(&code, '`').max(2) + 1);
                format!("{fence}{lang}\n{code}\n{fence}")
            }
            Kind::Table { align } => {
                let row = |cells: &[Range<usize>]| {
                    let cells: Vec<String> = cells.iter().map(|c| escape_pipes(&inline_md(&b.slice(c.clone())))).collect();
                    format!("| {} |", cells.join(" | "))
                };
                let rule = align.iter().map(|a| match a {
                    Alignment::None => "---",
                    Alignment::Left => ":---",
                    Alignment::Center => ":---:",
                    Alignment::Right => "---:",
                });
                let mut lines: Vec<String> = b.cells().iter().map(|r| row(r)).collect();
                lines.insert(1.min(lines.len()), format!("|{}|", rule.collect::<Vec<_>>().join("|")));
                lines.join("\n")
            }
            Kind::Raw => b.text(),
        };
        let is_list = matches!(b.kind, Kind::List { .. });
        // a top-level list of the other type must start after a blank line
        let mut tight = is_list && prev_list;
        match b.kind {
            Kind::List { ordered, depth: 0 } => {
                tight &= top_ordered == Some(ordered);
                top_ordered = Some(ordered);
            }
            Kind::List { .. } => {}
            _ => {
                counters.clear();
                top_ordered = None;
            }
        }
        if !out.is_empty() {
            out.push_str(if tight { "\n" } else { "\n\n" });
        }
        out.push_str(&line);
        prev_list = is_list;
    }
    out.push('\n');
    out
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\*_[]`<>&~".contains(c) {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

/// Escape characters that would turn the start of a paragraph into another block.
fn block_escape(s: &str) -> String {
    let s = s.trim_start();
    if s.starts_with(['#', '-', '+', '=']) {
        return format!("\\{s}");
    }
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && s[digits..].starts_with(['.', ')']) {
        return format!("{}\\{}", &s[..digits], &s[digits..]);
    }
    s.to_string()
}

fn inline_md(spans: &[Span]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < spans.len() {
        // group consecutive spans sharing a link
        let link = &spans[i].style.link;
        let j = i + spans[i..].iter().take_while(|s| &s.style.link == link).count();
        let body = marks_md(&spans[i..j]);
        match link {
            Some(url) => {
                let url = if url.contains([' ', '(', ')']) { format!("<{url}>") } else { url.clone() };
                out.push_str(&format!("[{body}]({url})"));
            }
            None => out.push_str(&body),
        }
        i = j;
    }
    out
}

/// Bold/italic markers around runs, keeping whitespace outside the markers.
fn marks_md(spans: &[Span]) -> String {
    let mut out = String::new();
    let mut open: Vec<&str> = vec![];
    let mut pending_ws = String::new();
    for s in spans {
        let want: Vec<&str> = [("**", s.style.bold), ("*", s.style.italic)]
            .into_iter()
            .filter_map(|(m, on)| on.then_some(m))
            .collect();
        let core = s.text.trim();
        // close everything from the first open mark this span doesn't want
        let close = |open: &mut Vec<&str>, out: &mut String| {
            if let Some(k) = open.iter().position(|m| !want.contains(m)) {
                for m in open.drain(k..).rev() {
                    out.push_str(m);
                }
            }
        };
        if core.is_empty() && !s.style.code && !s.style.raw {
            close(&mut open, &mut out);
            pending_ws.push_str(&s.text);
            continue;
        }
        let lead = &s.text[..s.text.len() - s.text.trim_start().len()];
        let trail = &s.text[s.text.trim_end().len()..];
        let (lead, core, trail) = if s.style.code || s.style.raw { ("", s.text.as_str(), "") } else { (lead, core, trail) };
        close(&mut open, &mut out);
        out.push_str(&escape(&pending_ws));
        out.push_str(&escape(lead));
        pending_ws.clear();
        for m in want {
            if !open.contains(&m) {
                out.push_str(m);
                open.push(m);
            }
        }
        if s.style.raw {
            out.push_str(core);
        } else if s.style.code {
            let ticks = longest_run(core, '`') + 1;
            let fence = "`".repeat(ticks);
            let pad = if core.starts_with('`') || core.ends_with('`') { " " } else { "" };
            out.push_str(&format!("{fence}{pad}{core}{pad}{fence}"));
        } else {
            out.push_str(&escape(core));
        }
        pending_ws.push_str(trail);
    }
    for m in open.into_iter().rev() {
        out.push_str(m);
    }
    out.push_str(&escape(&pending_ws));
    out
}

/// `|` ends a table cell, even inside code spans; escape those not escaped yet.
fn escape_pipes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut backslashes = 0;
    for c in s.chars() {
        if c == '|' && backslashes % 2 == 0 {
            out.push('\\');
        }
        backslashes = if c == '\\' { backslashes + 1 } else { 0 };
        out.push(c);
    }
    out
}

fn longest_run(s: &str, c: char) -> usize {
    let (mut best, mut cur) = (0, 0);
    for ch in s.chars() {
        cur = if ch == c { cur + 1 } else { 0 };
        best = best.max(cur);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(md: &str) -> String {
        let once = serialize(&parse(md));
        assert_eq!(parse(&once), parse(md), "model changed:\n{md}\n---\n{once}");
        assert_eq!(serialize(&parse(&once)), once, "not stable:\n{once}");
        once
    }

    #[test]
    fn inline_styles() {
        assert_eq!(roundtrip("plain **bold** *it* ***both*** `co*de`\n"), "plain **bold** *it* ***both*** `co*de`\n");
        assert_eq!(roundtrip("**bold *both* bold** tail\n"), "**bold *both* bold** tail\n");
        roundtrip("a [link **bold**](http://x.y/a_b) b\n");
        roundtrip("escapes \\* \\_ \\[x\\] 1 < 2 & ~\n");
        roundtrip("`` a`b ``\n");
    }

    #[test]
    fn blocks() {
        let md = "# Title\n\n## Sub\n\npara one\n\n- a\n- b\n    - nested *it*\n        1. deep\n- c\n\n1. one\n2. two\n\n![alt text](photo.png)\n\nafter\n";
        assert_eq!(roundtrip(md), md);
    }

    #[test]
    fn block_start_escapes() {
        roundtrip("\\# not heading\n\n\\- not list\n\n1\\. not ordered\n");
    }

    #[test]
    fn raw_preserved() {
        let md = "> quote\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\n~~strike~~ text\n\nline  \nbreak\n\n- [ ] task\n";
        let out = roundtrip(md);
        for part in ["> quote", "| 1 | 2 |", "~~strike~~", "- [ ] task"] {
            assert!(out.contains(part), "lost {part:?} in:\n{out}");
        }
    }

    #[test]
    fn code_blocks() {
        let md = "```rust\nfn main() {\n\n    println!(\"*hi*\");\n}\n```\n\ntext\n";
        assert_eq!(roundtrip(md), md);
        let b = parse(md);
        assert_eq!(b[0].kind, Kind::Code { lang: "rust".into() });
        assert_eq!(b[0].text(), "fn main() {\n\n    println!(\"*hi*\");\n}");
        // fences longer than any backtick run inside; indented code becomes fenced
        assert_eq!(roundtrip("````\na ``` b\n````\n"), "````\na ``` b\n````\n");
        assert_eq!(roundtrip("    indented\n"), "```\nindented\n```\n");
        assert_eq!(roundtrip("```\n```\n"), "```\n\n```\n");
    }

    #[test]
    fn tables() {
        let md = "| a | **b** |\n|---|:---:|\n| `x\\|y` | [l](u) |\n|  | 2 |\n";
        assert_eq!(roundtrip(md), md);
        let b = &parse(md)[0];
        assert_eq!(b.kind, Kind::Table { align: vec![Alignment::None, Alignment::Center] });
        assert_eq!(b.text(), "a\tb\nx|y\tl\n\t2");
        assert_eq!(b.cells()[2], vec![10..10, 11..12]);
        // ragged rows are padded to the header
        assert_eq!(roundtrip("| a | b |\n|---|---|\n| 1 |\n"), "| a | b |\n|---|---|\n| 1 |  |\n");
    }

    #[test]
    fn image_inside_paragraph_splits() {
        let b = parse("before ![a](x.png) after\n");
        assert_eq!(b.len(), 3);
        assert_eq!(b[1].kind, Kind::Image { src: "x.png".into(), alt: "a".into() });
    }

    #[test]
    fn span_ops() {
        let mut b = Block::new(Kind::Paragraph, vec![Span { text: "hello world".into(), style: Style::default() }]);
        b.map_style(0..5, |s| s.bold = true);
        assert_eq!(b.spans.len(), 2);
        b.insert(5, "!", Style::default());
        assert_eq!(b.text(), "hello! world");
        b.delete(3..8);
        assert_eq!(b.text(), "helorld");
        let tail = b.split_off(3);
        assert_eq!((b.text(), tail[0].text.as_str()), ("hel".into(), "orld"));
    }
}
