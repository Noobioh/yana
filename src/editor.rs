//! Editing state and operations on the document model. No UI here: the
//! widget translates input into `Action`s, `Editor::perform` applies them.

use crate::doc::{self, Alignment, Block, CELL, Kind, ROW, Style};
use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub block: usize,
    /// Byte offset into the block's text.
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    DocStart,
    DocEnd,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mark {
    Bold,
    Italic,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TableOp {
    AddRow,
    RemoveRow,
    AddColumn,
    RemoveColumn,
}

#[derive(Clone, Debug)]
pub enum Action {
    Focus(bool),
    Select { pos: Pos, extend: bool },
    SelectWord(Pos),
    SelectAll,
    Move(Motion, bool),
    Insert(String),
    Enter,
    Backspace,
    Delete,
    Toggle(Mark),
    /// Toggle block kind (Heading/List/Paragraph) for the selected blocks.
    SetKind(Kind),
    Indent,
    Outdent,
    SetLink(String),
    InsertImage { src: String, alt: String },
    InsertTable,
    /// Row/column edit on the table under the cursor.
    Table(TableOp),
    Paste(String),
    Undo,
    Redo,
    /// Asks the app to open its link prompt; no-op for the editor itself.
    RequestLink,
}

type Snapshot = (Vec<Block>, Pos);

pub struct Editor {
    pub blocks: Vec<Block>,
    pub cursor: Pos,
    pub anchor: Option<Pos>,
    pub focused: bool,
    /// Bumped on every content change; the app watches it for autosave.
    pub revision: u64,
    /// Marks toggled with a collapsed selection, applied to the next typed text.
    typing: Option<Style>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    coalesce: bool,
}

impl Editor {
    pub fn new(markdown: &str) -> Self {
        let mut blocks = doc::parse(markdown);
        if blocks.is_empty() {
            blocks.push(Block::paragraph());
        }
        Editor {
            blocks,
            cursor: Pos::default(),
            anchor: None,
            focused: false,
            revision: 0,
            typing: None,
            undo: vec![],
            redo: vec![],
            coalesce: false,
        }
    }

    pub fn markdown(&self) -> String {
        doc::serialize(&self.blocks)
    }

    /// Ordered, non-empty selection.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        (a != self.cursor).then(|| (a.min(self.cursor), a.max(self.cursor)))
    }

    /// Byte range of `block` covered by the selection.
    pub fn selected_range(&self, block: usize) -> Option<(usize, usize)> {
        let (a, b) = self.selection()?;
        if block < a.block || block > b.block {
            return None;
        }
        let start = if block == a.block { a.offset } else { 0 };
        let end = if block == b.block { b.offset } else { self.blocks[block].len() };
        Some((start, end))
    }

    pub fn selection_markdown(&self) -> Option<String> {
        let (a, b) = self.selection()?;
        let mut blocks: Vec<Block> = (a.block..=b.block)
            .map(|i| {
                let (s, e) = self.selected_range(i).unwrap();
                let blk = &self.blocks[i];
                match blk.kind {
                    // across cells or out of the table: whole rows, so it stays a valid table
                    Kind::Table { .. } if a.block != b.block || blk.text()[s..e].contains([CELL, ROW]) => table_rows(blk, s..e),
                    Kind::Table { .. } => Block::new(Kind::Paragraph, blk.slice(s..e)),
                    _ => Block::new(blk.kind.clone(), blk.slice(s..e)),
                }
            })
            .collect();
        if blocks.len() == 1 && !matches!(blocks[0].kind, Kind::Image { .. } | Kind::Table { .. }) {
            blocks[0].kind = Kind::Paragraph;
        }
        Some(doc::serialize(&blocks).trim_end().to_string())
    }

    pub fn link_at(&self, pos: Pos) -> Option<String> {
        let b = self.blocks.get(pos.block)?;
        let next = next_boundary(&b.text(), pos.offset);
        b.slice(pos.offset..next).first()?.style.link.clone()
    }

    /// Returns true when the document content changed.
    pub fn perform(&mut self, action: Action) -> bool {
        let rev = self.revision;
        let typing = self.typing.take();
        match action {
            Action::Focus(f) => {
                self.focused = f;
                self.typing = typing;
                return false;
            }
            Action::Select { pos, extend } => self.select(pos, extend),
            Action::SelectWord(pos) => {
                let pos = self.clamp(pos);
                let text = self.blocks[pos.block].text();
                let is_word = |c: char| c.is_alphanumeric() || c == '_';
                let start = text[..pos.offset].rfind(|c| !is_word(c)).map_or(0, |i| next_boundary(&text, i));
                let end = text[pos.offset..].find(|c| !is_word(c)).map_or(text.len(), |i| pos.offset + i);
                self.select(Pos { offset: start, ..pos }, false);
                self.select(Pos { offset: end, ..pos }, true);
            }
            Action::SelectAll => {
                let last = self.blocks.len() - 1;
                self.select(Pos::default(), false);
                self.select(Pos { block: last, offset: self.blocks[last].len() }, true);
            }
            Action::Move(m, extend) => {
                let pos = match (self.selection(), m, extend) {
                    (Some((a, _)), Motion::Left, false) => a,
                    (Some((_, b)), Motion::Right, false) => b,
                    _ => self.motion(m),
                };
                self.select(pos, extend);
            }
            Action::Undo => self.restore(true),
            Action::Redo => self.restore(false),
            Action::RequestLink => {}
            Action::Toggle(mark) => {
                if self.selection().is_some() {
                    self.edit(false, |e| e.toggle(mark));
                } else {
                    let mut s = typing.unwrap_or_else(|| self.typing_style());
                    match mark {
                        Mark::Bold => s.bold = !s.bold,
                        Mark::Italic => s.italic = !s.italic,
                    }
                    self.typing = Some(s);
                }
            }
            Action::Insert(text) => {
                let coalesce = text.chars().all(|c| !c.is_whitespace()) && self.selection().is_none();
                self.typing = typing;
                self.edit(coalesce, |e| e.insert(&text));
            }
            Action::Enter => self.edit(false, Self::enter),
            Action::Backspace => self.edit(false, Self::backspace),
            Action::Delete => self.edit(false, Self::delete_forward),
            Action::SetKind(kind) => self.edit(false, |e| e.set_kind(kind)),
            Action::Indent if self.in_table() => self.next_cell(true),
            Action::Outdent if self.in_table() => self.next_cell(false),
            Action::Indent if self.blocks[self.cursor.block].kind.is_verbatim() => self.edit(false, |e| e.insert("    ")),
            Action::Indent => self.edit(false, |e| e.indent(1)),
            Action::Outdent => self.edit(false, |e| e.indent(-1)),
            Action::SetLink(url) => self.edit(false, |e| e.set_link(url)),
            Action::InsertImage { src, alt } => self.edit(false, |e| e.insert_image(src, alt)),
            Action::InsertTable => self.edit(false, Self::insert_table),
            Action::Table(op) if self.in_table() => self.edit(false, |e| e.table_op(op)),
            Action::Table(_) => {}
            Action::Paste(text) => self.edit(false, |e| e.paste(&text)),
        }
        self.revision != rev
    }

    fn edit(&mut self, coalesce: bool, f: impl FnOnce(&mut Self)) {
        if !(coalesce && self.coalesce) {
            self.undo.push((self.blocks.clone(), self.cursor));
            // ponytail: full-document snapshots, fine for note-sized docs
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        f(self);
        if self.blocks.is_empty() {
            self.blocks.push(Block::paragraph());
        }
        self.cursor = self.clamp(self.cursor);
        self.coalesce = coalesce;
        self.revision += 1;
    }

    fn restore(&mut self, undo: bool) {
        let (from, to) = if undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
        if let Some((blocks, cursor)) = from.pop() {
            to.push((std::mem::replace(&mut self.blocks, blocks), self.cursor));
            self.cursor = self.clamp(cursor);
            self.anchor = None;
            self.coalesce = false;
            self.revision += 1;
        }
    }

    fn select(&mut self, pos: Pos, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = self.clamp(pos);
        self.coalesce = false;
    }

    pub fn clamp(&self, p: Pos) -> Pos {
        let block = p.block.min(self.blocks.len() - 1);
        let text = self.blocks[block].text();
        let mut offset = p.offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        Pos { block, offset }
    }

    fn end_of(&self, block: usize) -> Pos {
        Pos { block, offset: self.blocks[block].len() }
    }

    fn motion(&self, m: Motion) -> Pos {
        let c = self.cursor;
        let text = self.blocks[c.block].text();
        let last = self.blocks.len() - 1;
        match m {
            Motion::Left if c.offset > 0 => Pos { offset: prev_boundary(&text, c.offset), ..c },
            Motion::Right if c.offset < text.len() => Pos { offset: next_boundary(&text, c.offset), ..c },
            Motion::Left | Motion::WordLeft if c.offset == 0 && c.block > 0 => self.end_of(c.block - 1),
            Motion::Right | Motion::WordRight if c.offset == text.len() && c.block < last => {
                Pos { block: c.block + 1, offset: 0 }
            }
            Motion::WordLeft => {
                let t = text[..c.offset].trim_end_matches(|ch: char| !ch.is_alphanumeric());
                let start = t.rfind(|ch: char| !ch.is_alphanumeric()).map_or(0, |i| next_boundary(&text, i));
                Pos { offset: start, ..c }
            }
            Motion::WordRight => {
                let rest = &text[c.offset..];
                let skip = rest.len() - rest.trim_start_matches(|ch: char| !ch.is_alphanumeric()).len();
                let word = rest[skip..].find(|ch: char| !ch.is_alphanumeric()).unwrap_or(rest.len() - skip);
                Pos { offset: c.offset + skip + word, ..c }
            }
            Motion::Home if self.in_table() => Pos { offset: self.cell_range(c).start, ..c },
            Motion::End if self.in_table() => Pos { offset: self.cell_range(c).end, ..c },
            Motion::Home => Pos { offset: 0, ..c },
            Motion::End => self.end_of(c.block),
            Motion::DocStart => Pos::default(),
            Motion::DocEnd => self.end_of(last),
            Motion::Left | Motion::Right => c,
        }
    }

    /// Style typed text gets at the cursor: the style of the previous
    /// character, minus a link we're only touching the edge of.
    fn typing_style(&self) -> Style {
        let b = &self.blocks[self.cursor.block];
        if b.kind.is_verbatim() {
            return Style { raw: true, ..Style::default() };
        }
        let mut s = b.style_at(self.cursor.offset);
        if s.link.is_some() && self.link_at(self.cursor) != s.link {
            s.link = None;
        }
        s
    }

    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else { return false };
        let ranges: Vec<_> = (a.block..=b.block).map(|i| (i, self.selected_range(i).unwrap())).collect();
        self.anchor = None;
        self.cursor = a;
        if is_table(&self.blocks[a.block]) || is_table(&self.blocks[b.block]) {
            // no merging into or out of a table: clear what's selected, drop what's fully selected
            for (i, (s, e)) in ranges.into_iter().rev() {
                let blk = &mut self.blocks[i];
                if (s == 0 && e == blk.len()) || (i != a.block && i != b.block) {
                    self.blocks.remove(i);
                } else if is_table(blk) {
                    delete_keeping_cells(blk, s..e);
                } else {
                    blk.delete(s..e);
                }
            }
            if self.blocks.is_empty() {
                self.blocks.push(Block::paragraph());
            }
            return true;
        }
        if a.block == b.block {
            self.blocks[a.block].delete(a.offset..b.offset);
            return true;
        }
        let tail = self.blocks[b.block].split_off(b.offset);
        let last_is_image = matches!(self.blocks[b.block].kind, Kind::Image { .. });
        let first = &mut self.blocks[a.block];
        first.split_off(a.offset);
        if matches!(first.kind, Kind::Image { .. }) {
            first.kind = Kind::Paragraph;
        }
        if !last_is_image {
            first.spans.extend(tail);
        }
        self.blocks[a.block].normalize();
        self.blocks.drain(a.block + 1..=b.block);
        true
    }

    fn insert(&mut self, text: &str) {
        self.delete_selection();
        let c = self.cursor;
        if matches!(self.blocks[c.block].kind, Kind::Image { .. }) {
            self.blocks.insert(c.block + 1, Block::paragraph());
            self.cursor = Pos { block: c.block + 1, offset: 0 };
            return self.insert(text);
        }
        let style = self.typing.clone().unwrap_or_else(|| self.typing_style());
        let b = &mut self.blocks[c.block];
        b.insert(c.offset, text, style);
        self.cursor.offset += text.len();
        // markdown-style shortcuts at the start of a paragraph
        if text == " " && b.kind == Kind::Paragraph {
            let kind = match &b.text()[..self.cursor.offset] {
                "# " => Some(Kind::Heading(1)),
                "## " => Some(Kind::Heading(2)),
                "### " => Some(Kind::Heading(3)),
                "- " | "* " => Some(Kind::List { ordered: false, depth: 0 }),
                "1. " => Some(Kind::List { ordered: true, depth: 0 }),
                _ => None,
            };
            if let Some(kind) = kind {
                b.delete(0..self.cursor.offset);
                b.kind = kind;
                self.cursor.offset = 0;
            }
        }
    }

    fn enter(&mut self) {
        self.delete_selection();
        let c = self.cursor;
        let b = &mut self.blocks[c.block];
        match b.kind.clone() {
            // Enter on an empty last line leaves the code block
            k if k.is_verbatim() && c.offset == b.len() && b.text().ends_with('\n') => {
                b.delete(c.offset - 1..c.offset);
                self.blocks.insert(c.block + 1, Block::paragraph());
                self.cursor = Pos { block: c.block + 1, offset: 0 };
            }
            k if k.is_verbatim() => {
                b.insert(c.offset, "\n", Style { raw: true, ..Style::default() });
                self.cursor.offset += 1;
            }
            Kind::Image { .. } => {
                self.blocks.insert(c.block + 1, Block::paragraph());
                self.cursor = Pos { block: c.block + 1, offset: 0 };
            }
            // Enter adds a row; on an empty last row it leaves the table
            Kind::Table { .. } => {
                let rows = b.cells();
                let r = rows.iter().position(|row| c.offset <= row.last().unwrap().end).unwrap_or(0);
                if r > 0 && r + 1 == rows.len() && rows[r].iter().all(|x| x.is_empty()) {
                    self.table_op(TableOp::RemoveRow);
                    self.blocks.insert(c.block + 1, Block::paragraph());
                    self.cursor = Pos { block: c.block + 1, offset: 0 };
                } else {
                    self.table_op(TableOp::AddRow);
                }
            }
            // ```lang + Enter starts a code block
            Kind::Paragraph if c.offset == b.len() && b.text().starts_with("```") => {
                let lang = b.text()[3..].trim().to_string();
                *b = Block::verbatim(Kind::Code { lang }, "");
                self.cursor.offset = 0;
                if c.block + 1 == self.blocks.len() {
                    self.blocks.push(Block::paragraph());
                }
            }
            Kind::List { ordered, depth } if b.len() == 0 => {
                b.kind = if depth > 0 { Kind::List { ordered, depth: depth - 1 } } else { Kind::Paragraph };
            }
            Kind::Heading(_) if c.offset == 0 && b.len() > 0 => {
                self.blocks.insert(c.block, Block::paragraph());
                self.cursor.block += 1;
            }
            kind => {
                let tail = b.split_off(c.offset);
                let kind = if matches!(kind, Kind::List { .. }) { kind } else { Kind::Paragraph };
                self.blocks.insert(c.block + 1, Block::new(kind, tail));
                self.cursor = Pos { block: c.block + 1, offset: 0 };
            }
        }
    }

    fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let c = self.cursor;
        let prev_is_image = c.block > 0 && matches!(self.blocks[c.block - 1].kind, Kind::Image { .. });
        let prev_is_table = c.block > 0 && is_table(&self.blocks[c.block - 1]);
        let b = &mut self.blocks[c.block];
        match b.kind {
            Kind::Image { .. } => {
                self.blocks.remove(c.block);
                self.cursor = if c.block > 0 { self.end_of(c.block - 1) } else { Pos::default() };
            }
            // an empty table goes away; otherwise never delete a cell separator, step over it
            Kind::Table { .. } if c.offset == 0 && b.text().chars().all(|ch| ch == CELL || ch == ROW) => {
                *b = Block::paragraph();
            }
            Kind::Table { .. } if c.offset > 0 => {
                let text = b.text();
                let p = prev_boundary(&text, c.offset);
                if !text[p..].starts_with([CELL, ROW]) {
                    b.delete(p..c.offset);
                }
                self.cursor.offset = p;
            }
            Kind::Table { .. } => {}
            _ if c.offset > 0 => {
                let p = prev_boundary(&b.text(), c.offset);
                b.delete(p..c.offset);
                self.cursor.offset = p;
            }
            Kind::List { ordered, depth } if depth > 0 => b.kind = Kind::List { ordered, depth: depth - 1 },
            Kind::List { .. } | Kind::Heading(_) => b.kind = Kind::Paragraph,
            Kind::Code { .. } if b.len() == 0 => b.kind = Kind::Paragraph,
            Kind::Code { .. } => {}
            _ if c.block == 0 => {}
            _ if prev_is_image => {
                self.blocks.remove(c.block - 1);
                self.cursor.block -= 1;
            }
            _ if prev_is_table => {
                if b.len() == 0 {
                    self.blocks.remove(c.block);
                }
                self.cursor = self.end_of(c.block - 1);
            }
            _ => self.merge_into_previous(c.block),
        }
    }

    fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        let c = self.cursor;
        let b = &mut self.blocks[c.block];
        let text = b.text();
        if matches!(b.kind, Kind::Image { .. }) {
            self.blocks.remove(c.block);
        } else if is_table(b) {
            if c.offset < text.len() && !text[c.offset..].starts_with([CELL, ROW]) {
                b.delete(c.offset..next_boundary(&text, c.offset));
            }
        } else if c.offset < text.len() {
            b.delete(c.offset..next_boundary(&text, c.offset));
        } else if c.block + 1 < self.blocks.len() {
            if matches!(self.blocks[c.block + 1].kind, Kind::Image { .. }) {
                self.blocks.remove(c.block + 1);
            } else if is_table(&self.blocks[c.block + 1]) {
                if self.blocks[c.block].len() == 0 {
                    self.blocks.remove(c.block);
                }
            } else {
                self.merge_into_previous(c.block + 1);
            }
        }
    }

    fn merge_into_previous(&mut self, i: usize) {
        let spans = self.blocks.remove(i).spans;
        let prev = &mut self.blocks[i - 1];
        self.cursor = Pos { block: i - 1, offset: prev.len() };
        prev.spans.extend(spans);
        if prev.kind.is_verbatim() {
            prev.spans.iter_mut().for_each(|s| s.style = Style { raw: true, ..Style::default() });
        }
        prev.normalize();
    }

    fn selected_blocks(&self) -> std::ops::RangeInclusive<usize> {
        match self.selection() {
            Some((a, b)) => a.block..=b.block,
            None => self.cursor.block..=self.cursor.block,
        }
    }

    fn toggle(&mut self, mark: Mark) {
        let get = |s: &Style| match mark {
            Mark::Bold => s.bold,
            Mark::Italic => s.italic,
        };
        let ranges: Vec<_> = self.selected_blocks().filter_map(|i| Some((i, self.selected_range(i)?))).collect();
        let all = ranges
            .iter()
            .all(|&(i, (s, e))| self.blocks[i].slice(s..e).iter().all(|sp| get(&sp.style)));
        for (i, (s, e)) in ranges {
            if self.blocks[i].kind.is_verbatim() {
                continue;
            }
            self.blocks[i].map_style(s..e, |st| match mark {
                Mark::Bold => st.bold = !all,
                Mark::Italic => st.italic = !all,
            });
        }
    }

    fn set_kind(&mut self, kind: Kind) {
        if let Kind::Code { .. } = kind {
            return self.toggle_code();
        }
        let same = |a: &Kind, b: &Kind| match (a, b) {
            (Kind::List { ordered: x, .. }, Kind::List { ordered: y, .. }) => x == y,
            _ => a == b,
        };
        let range = self.selected_blocks();
        let all = range.clone().all(|i| same(&self.blocks[i].kind, &kind));
        for i in range {
            let b = &mut self.blocks[i];
            if matches!(b.kind, Kind::Image { .. } | Kind::Table { .. }) || b.kind.is_verbatim() {
                continue;
            }
            b.kind = match (&b.kind, &kind) {
                _ if all => Kind::Paragraph,
                (Kind::List { depth, .. }, Kind::List { ordered, .. }) => Kind::List { ordered: *ordered, depth: *depth },
                _ => kind.clone(),
            };
        }
    }

    /// Selected blocks become one code block (lines joined), or back into
    /// one paragraph per line when they're all code already.
    fn toggle_code(&mut self) {
        let range = self.selected_blocks();
        let (start, single) = (*range.start(), range.start() == range.end());
        let blocks: Vec<Block> = if range.clone().all(|i| matches!(self.blocks[i].kind, Kind::Code { .. })) {
            range
                .clone()
                .flat_map(|i| self.blocks[i].text().split('\n').map(String::from).collect::<Vec<_>>())
                .map(|line| Block::new(Kind::Paragraph, vec![doc::Span { text: line, style: Style::default() }]))
                .collect()
        } else {
            let text: Vec<String> = range
                .clone()
                .filter(|&i| !matches!(self.blocks[i].kind, Kind::Image { .. }))
                .map(|i| match self.blocks[i].kind {
                    Kind::Table { .. } => doc::serialize(&self.blocks[i..=i]).trim_end().to_string(),
                    _ => self.blocks[i].text(),
                })
                .collect();
            vec![Block::verbatim(Kind::Code { lang: String::new() }, &text.join("\n"))]
        };
        let offset = if single && blocks.len() == 1 { self.cursor.offset } else { 0 };
        self.blocks.splice(range, blocks);
        self.anchor = None;
        self.cursor = Pos { block: start, offset };
        if start + 1 == self.blocks.len() && self.blocks[start].kind.is_verbatim() {
            self.blocks.push(Block::paragraph());
        }
    }

    fn indent(&mut self, delta: i8) {
        for i in self.selected_blocks() {
            // a list item can be at most one level deeper than the item above it
            let max = match self.blocks.get(i.wrapping_sub(1)).map(|b| &b.kind) {
                Some(Kind::List { depth, .. }) => depth + 1,
                _ => 0,
            };
            if let Kind::List { depth, .. } = &mut self.blocks[i].kind {
                *depth = (*depth as i8 + delta).clamp(0, max as i8) as u8;
            }
        }
    }

    fn set_link(&mut self, url: String) {
        let link = (!url.trim().is_empty()).then(|| url.trim().to_string());
        if self.selection().is_none() {
            if let Some(url) = link {
                let style = Style { link: Some(url.clone()), ..Style::default() };
                let c = self.cursor;
                self.blocks[c.block].insert(c.offset, &url, style);
                self.cursor.offset += url.len();
            }
            return;
        }
        for i in self.selected_blocks() {
            let (s, e) = self.selected_range(i).unwrap();
            self.blocks[i].map_style(s..e, |st| st.link = link.clone());
        }
    }

    fn insert_image(&mut self, src: String, alt: String) {
        self.delete_selection();
        let c = self.cursor;
        let image = Block::new(Kind::Image { src, alt }, vec![]);
        let b = &mut self.blocks[c.block];
        let img = if b.kind == Kind::Paragraph && b.len() == 0 {
            *b = image;
            c.block
        } else if is_table(b) {
            self.blocks.insert(c.block + 1, image);
            c.block + 1
        } else if c.offset == 0 && !matches!(b.kind, Kind::Image { .. }) {
            self.blocks.insert(c.block, image);
            c.block
        } else {
            let tail = b.split_off(c.offset);
            let kind = if matches!(b.kind, Kind::Image { .. } | Kind::Heading(_)) { Kind::Paragraph } else { b.kind.clone() };
            self.blocks.insert(c.block + 1, image);
            if !tail.is_empty() {
                self.blocks.insert(c.block + 2, Block::new(kind, tail));
            }
            c.block + 1
        };
        if img + 1 == self.blocks.len() {
            self.blocks.push(Block::paragraph());
        }
        self.cursor = Pos { block: img + 1, offset: 0 };
    }

    fn in_table(&self) -> bool {
        is_table(&self.blocks[self.cursor.block])
    }

    /// (row, column) of the table cell holding `p`.
    fn cell_at(&self, p: Pos) -> (usize, usize) {
        let rows = self.blocks[p.block].cells();
        for (r, row) in rows.iter().enumerate() {
            if let Some(c) = row.iter().position(|cell| p.offset <= cell.end) {
                return (r, c);
            }
        }
        (rows.len() - 1, rows.last().map_or(0, |r| r.len() - 1))
    }

    fn cell_range(&self, p: Pos) -> Range<usize> {
        let (r, c) = self.cell_at(p);
        self.blocks[p.block].cells()[r][c].clone()
    }

    /// Tab / Shift+Tab: to the end of the next/previous cell; Tab in the last cell adds a row.
    fn next_cell(&mut self, forward: bool) {
        let c = self.cursor;
        let cells: Vec<Range<usize>> = self.blocks[c.block].cells().into_iter().flatten().collect();
        let k = cells.iter().position(|cell| c.offset <= cell.end).unwrap_or(0);
        match if forward { cells.get(k + 1) } else { k.checked_sub(1).map(|k| &cells[k]) } {
            Some(cell) => self.select(Pos { offset: cell.end, ..c }, false),
            None if forward => self.edit(false, |e| e.table_op(TableOp::AddRow)),
            None => {}
        }
    }

    /// Removing the last row or column removes the table.
    fn table_op(&mut self, op: TableOp) {
        let i = self.cursor.block;
        let Kind::Table { mut align } = self.blocks[i].kind.clone() else { return };
        let (mut r, mut c) = self.cell_at(self.cursor);
        let mut grid = self.blocks[i].grid();
        match op {
            TableOp::AddRow => {
                r += 1;
                c = 0;
                grid.insert(r, vec![vec![]; align.len()]);
            }
            TableOp::AddColumn => {
                c += 1;
                align.insert(c, Alignment::None);
                grid.iter_mut().for_each(|row| row.insert(c, vec![]));
            }
            TableOp::RemoveRow if grid.len() > 1 => {
                grid.remove(r);
                r = r.min(grid.len() - 1);
            }
            TableOp::RemoveColumn if align.len() > 1 => {
                align.remove(c);
                grid.iter_mut().for_each(|row| drop(row.remove(c)));
                c = c.min(align.len() - 1);
            }
            TableOp::RemoveRow | TableOp::RemoveColumn => {
                self.blocks[i] = Block::paragraph();
                self.cursor = Pos { block: i, offset: 0 };
                return;
            }
        }
        self.blocks[i] = Block::table(align, grid);
        self.anchor = None;
        let cell = self.blocks[i].cells()[r][c].clone();
        self.cursor = Pos { block: i, offset: cell.end };
    }

    fn insert_table(&mut self) {
        self.delete_selection();
        let c = self.cursor;
        let table = Block::table(vec![Alignment::None; 3], vec![vec![vec![]; 3]; 2]);
        let b = &mut self.blocks[c.block];
        let at = if b.kind == Kind::Paragraph && b.len() == 0 {
            *b = table;
            c.block
        } else {
            self.blocks.insert(c.block + 1, table);
            c.block + 1
        };
        if at + 1 == self.blocks.len() {
            self.blocks.push(Block::paragraph());
        }
        self.cursor = Pos { block: at, offset: 0 };
    }

    fn paste(&mut self, text: &str) {
        self.delete_selection();
        let c = self.cursor;
        if self.in_table() {
            let line = text.replace([CELL, ROW, '\r'], " ");
            let style = self.typing_style();
            self.blocks[c.block].insert(c.offset, &line, style);
            self.cursor.offset += line.len();
            return;
        }
        if self.blocks[c.block].kind.is_verbatim() {
            self.blocks[c.block].insert(c.offset, text, Style { raw: true, ..Style::default() });
            self.cursor.offset += text.len();
            return;
        }
        let mut parsed = doc::parse(text);
        if parsed.is_empty() {
            return;
        }
        let tail = self.blocks[c.block].split_off(c.offset);
        let mut idx = c.block;
        let first = parsed.remove(0);
        let cur = &mut self.blocks[idx];
        if first.kind == Kind::Paragraph && !matches!(cur.kind, Kind::Image { .. }) {
            cur.spans.extend(first.spans);
            cur.normalize();
        } else if cur.kind == Kind::Paragraph && cur.len() == 0 {
            *cur = first;
        } else {
            idx += 1;
            self.blocks.insert(idx, first);
        }
        for b in parsed {
            idx += 1;
            self.blocks.insert(idx, b);
        }
        let last = &mut self.blocks[idx];
        if matches!(last.kind, Kind::Image { .. }) || last.kind.is_verbatim() {
            if !tail.is_empty() {
                idx += 1;
                self.blocks.insert(idx, Block::new(Kind::Paragraph, tail));
                self.cursor = Pos { block: idx, offset: 0 };
            } else {
                self.cursor = self.end_of(idx);
            }
        } else {
            self.cursor = Pos { block: idx, offset: last.len() };
            last.spans.extend(tail);
            last.normalize();
        }
    }
}

fn is_table(b: &Block) -> bool {
    matches!(b.kind, Kind::Table { .. })
}

/// The header plus every row that `range` touches, as a table of its own.
fn table_rows(b: &Block, range: Range<usize>) -> Block {
    let Kind::Table { align } = &b.kind else { return b.clone() };
    let grid = b.grid();
    let touched = b.cells().into_iter().enumerate().skip(1).filter(|(_, row)| range.start < row.last().unwrap().end && range.end > row[0].start);
    let mut rows = vec![grid[0].clone()];
    rows.extend(touched.map(|(r, _)| grid[r].clone()));
    Block::table(align.clone(), rows)
}

/// Delete `range` from a table block, but keep the cell/row separators in it.
fn delete_keeping_cells(b: &mut Block, range: Range<usize>) {
    let text = b.text();
    let mut end = range.end;
    for (i, _) in text[range.clone()].rmatch_indices([CELL, ROW]) {
        b.delete(range.start + i + 1..end);
        end = range.start + i;
    }
    b.delete(range.start..end);
}

// ponytail: char boundaries, not grapheme clusters; emoji ZWJ sequences
// take several Backspaces. Swap in unicode-segmentation if that bothers.
fn prev_boundary(s: &str, i: usize) -> usize {
    s[..i].char_indices().last().map_or(0, |(j, _)| j)
}

fn next_boundary(s: &str, i: usize) -> usize {
    i + s[i..].chars().next().map_or(0, char::len_utf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(md: &str) -> Editor {
        Editor::new(md)
    }

    fn type_str(e: &mut Editor, s: &str) {
        for ch in s.chars() {
            e.perform(Action::Insert(ch.to_string()));
        }
    }

    #[test]
    fn typing_shortcuts_and_marks() {
        let mut e = ed("");
        type_str(&mut e, "# Title");
        e.perform(Action::Enter);
        type_str(&mut e, "- one");
        e.perform(Action::Enter);
        e.perform(Action::Toggle(Mark::Bold));
        type_str(&mut e, "two");
        e.perform(Action::Enter);
        e.perform(Action::Enter); // empty item exits list
        type_str(&mut e, "end");
        assert_eq!(e.markdown(), "# Title\n\n- one\n- **two**\n\nend\n");
    }

    #[test]
    fn selection_ops() {
        let mut e = ed("hello world\n\nsecond para\n");
        e.perform(Action::Select { pos: Pos { block: 0, offset: 6 }, extend: false });
        e.perform(Action::Select { pos: Pos { block: 1, offset: 7 }, extend: true });
        e.perform(Action::Toggle(Mark::Italic));
        assert_eq!(e.markdown(), "hello *world*\n\n*second* para\n");
        e.perform(Action::Backspace);
        assert_eq!(e.markdown(), "hello para\n");
        e.perform(Action::Undo);
        assert_eq!(e.markdown(), "hello *world*\n\n*second* para\n");
        e.perform(Action::Redo);
        assert_eq!(e.markdown(), "hello para\n");
    }

    #[test]
    fn backspace_merges_and_unlists() {
        let mut e = ed("a\n\n- b\n");
        e.perform(Action::Select { pos: Pos { block: 1, offset: 0 }, extend: false });
        e.perform(Action::Backspace);
        assert_eq!(e.markdown(), "a\n\nb\n");
        e.perform(Action::Backspace);
        assert_eq!(e.markdown(), "ab\n");
        assert_eq!(e.cursor, Pos { block: 0, offset: 1 });
    }

    #[test]
    fn images_and_paste() {
        let mut e = ed("before after\n");
        e.perform(Action::Select { pos: Pos { block: 0, offset: 7 }, extend: false });
        e.perform(Action::InsertImage { src: "a.png".into(), alt: "".into() });
        assert_eq!(e.markdown(), "before\n\n![](a.png)\n\nafter\n");
        e.perform(Action::Paste("**x** y\n\n## H".into()));
        assert_eq!(e.markdown(), "before\n\n![](a.png)\n\n**x** y\n\n## Hafter\n");
    }

    #[test]
    fn code_blocks() {
        let mut e = ed("");
        type_str(&mut e, "```rust");
        e.perform(Action::Enter);
        e.perform(Action::Toggle(Mark::Bold)); // ignored inside code
        type_str(&mut e, "let *x* = 1;");
        e.perform(Action::Enter);
        type_str(&mut e, "x");
        e.perform(Action::Enter);
        e.perform(Action::Enter); // empty last line exits
        type_str(&mut e, "after");
        assert_eq!(e.markdown(), "```rust\nlet *x* = 1;\nx\n```\n\nafter\n");

        let mut e = ed("one\n\ntwo\n");
        e.perform(Action::SelectAll);
        e.perform(Action::SetKind(Kind::Code { lang: String::new() }));
        assert_eq!(e.markdown(), "```\none\ntwo\n```\n");
        e.perform(Action::SetKind(Kind::Code { lang: String::new() }));
        assert_eq!(e.markdown(), "one\n\ntwo\n");
    }

    #[test]
    fn tables() {
        let mut e = ed("");
        e.perform(Action::InsertTable);
        type_str(&mut e, "a");
        e.perform(Action::Indent);
        type_str(&mut e, "b");
        e.perform(Action::Indent);
        e.perform(Action::Indent); // last header cell -> new row
        type_str(&mut e, "1");
        assert_eq!(e.markdown(), "| a | b |  |\n|---|---|---|\n| 1 |  |  |\n");
        // Backspace and Delete never eat a separator
        e.perform(Action::Backspace);
        e.perform(Action::Backspace);
        e.perform(Action::Delete);
        assert_eq!(e.markdown(), "| a | b |  |\n|---|---|---|\n|  |  |  |\n");
        assert_eq!(e.cursor, Pos { block: 0, offset: 4 }, "stepped back over the row separator");
        e.perform(Action::Select { pos: Pos { block: 0, offset: 5 }, extend: false }); // row 1, column 0
        e.perform(Action::Table(TableOp::RemoveColumn));
        e.perform(Action::Table(TableOp::AddRow));
        assert_eq!(e.markdown(), "| b |  |\n|---|---|\n|  |  |\n|  |  |\n");
        // Enter on the empty last row leaves the table
        e.perform(Action::Enter);
        type_str(&mut e, "after");
        assert_eq!(e.markdown(), "| b |  |\n|---|---|\n|  |  |\n\nafter\n");
        // a selection from inside the table into the paragraph keeps the table intact
        e.perform(Action::Select { pos: Pos { block: 0, offset: 1 }, extend: false });
        e.perform(Action::Select { pos: Pos { block: 1, offset: 2 }, extend: true });
        e.perform(Action::Delete);
        assert_eq!(e.markdown(), "| b |  |\n|---|---|\n|  |  |\n\nter\n");
    }

    #[test]
    fn table_copy_and_select_all() {
        let mut e = ed("| a | b |\n|---|---|\n| c | d |\n| e | f |\n");
        // "a\tb\nc\td\ne\tf": inside one cell copies plain text, across cells whole rows
        e.perform(Action::Select { pos: Pos { block: 0, offset: 4 }, extend: false });
        e.perform(Action::Select { pos: Pos { block: 0, offset: 5 }, extend: true });
        assert_eq!(e.selection_markdown().unwrap(), "c");
        e.perform(Action::Select { pos: Pos { block: 0, offset: 7 }, extend: true });
        assert_eq!(e.selection_markdown().unwrap(), "| a | b |\n|---|---|\n| c | d |");
        // the note is only a table: select all + Enter/paste/image must not panic
        e.perform(Action::SelectAll);
        e.perform(Action::Enter);
        e.perform(Action::SelectAll);
        e.perform(Action::Paste("x".into()));
        e.perform(Action::SelectAll);
        e.perform(Action::InsertImage { src: "a.png".into(), alt: String::new() });
        assert_eq!(e.markdown(), "![](a.png)\n");
    }

    #[test]
    fn indent_limited_by_previous_item() {
        let mut e = ed("- a\n- b\n");
        e.perform(Action::Indent);
        assert_eq!(e.markdown(), "- a\n- b\n", "first item can't indent");
        e.perform(Action::Move(Motion::DocEnd, false));
        e.perform(Action::Indent);
        e.perform(Action::Indent);
        assert_eq!(e.markdown(), "- a\n    - b\n");
    }
}
