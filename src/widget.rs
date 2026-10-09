//! WYSIWYG editor widget: lays out and draws `Editor` blocks, turns mouse and
//! keyboard input into `Action`s. Uses the concrete cosmic-text buffer behind
//! iced's paragraph for exact caret, hit-test and selection geometry.

use crate::doc::{Alignment, Block, Kind};
use crate::editor::{Action, Editor, Mark, Motion, Pos};
use crate::theme::{self, Pal};
use iced::advanced::graphics::text::{Paragraph, cosmic_text};
use iced::advanced::image::{self as img, Renderer as _};
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::text::{self, Paragraph as _, Renderer as _};
use iced::advanced::widget::{Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, clipboard, layout, mouse};
use iced::keyboard::{self, Key, Modifiers, key::Named};
use iced::{Border, Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Size, Theme, alignment, font};
use std::ops::Range;
use std::path::Path;
use std::time::{Duration, Instant};

const SIZE: f32 = 16.0;
const LINE: f32 = 1.7;
const INDENT: f32 = 26.0;
const RAW_PAD: f32 = 10.0;
const MAX_IMAGE_H: f32 = 480.0;
const CELL_PAD: f32 = 8.0;
const MIN_COL: f32 = 48.0;
/// Clickable space below the last block (places the caret at the end).
const BOTTOM: f32 = 200.0;

pub fn view<'a, M: 'a>(editor: &'a Editor, base: &'a Path, pal: Pal, on_action: impl Fn(Action) -> M + 'a) -> Element<'a, M> {
    Element::new(EditorView { editor, base, pal, on_action: Box::new(on_action) })
}

struct EditorView<'a, M> {
    editor: &'a Editor,
    /// Note folder; image paths resolve against it.
    base: &'a Path,
    pal: Pal,
    on_action: Box<dyn Fn(Action) -> M + 'a>,
}

/// A block laid out at a given width.
struct Laid {
    block: Block,
    width: f32,
    /// Palette the spans were colored with (part of the cache key).
    pal: Pal,
    para: Paragraph,
    left: f32,
    top: f32,
    height: f32,
    image: Option<(img::Handle, Size)>,
    /// Table cells; empty for other blocks.
    cells: Vec<Cell>,
}

struct Cell {
    para: Paragraph,
    /// Cell box, relative to the block origin; text starts at `rect` + CELL_PAD.
    rect: Rectangle,
    /// Byte range of the cell's text in the block.
    range: Range<usize>,
    header: bool,
    /// Horizontal offset of the text for centered/right-aligned columns.
    shift: f32,
}

impl Cell {
    fn text_origin(&self) -> Point {
        Point::new(self.rect.x + CELL_PAD + self.shift, self.rect.y + CELL_PAD)
    }
}

#[derive(Default)]
struct State {
    laid: Vec<Laid>,
    dragging: bool,
    last_click: Option<(Instant, Pos)>,
    modifiers: Modifiers,
}

fn heading_size(level: u8) -> f32 {
    match level {
        1 => 26.0,
        2 => 22.0,
        3 => 18.0,
        _ => 16.0,
    }
}

fn block_size(b: &Block) -> f32 {
    match b.kind {
        Kind::Heading(n) => heading_size(n),
        Kind::Raw | Kind::Code { .. } => SIZE - 2.0,
        _ => SIZE,
    }
}

fn gap(b: &Block) -> f32 {
    match b.kind {
        Kind::List { .. } => 4.0,
        _ => 12.0,
    }
}

fn spans(b: &Block, pal: Pal) -> Vec<text::Span<'static, (), Font>> {
    b.spans
        .iter()
        .map(|s| {
            let st = &s.style;
            let mono = st.code || st.raw || b.kind.is_verbatim();
            let mut f = if mono { theme::MONO } else { theme::SANS };
            if st.bold || matches!(b.kind, Kind::Heading(_)) {
                f.weight = font::Weight::Semibold;
            }
            if st.italic {
                f.style = font::Style::Italic;
            }
            let span = text::Span::new(s.text.clone()).font(f);
            match () {
                _ if st.link.is_some() => span.color(pal.signal).underline(true),
                _ if st.raw && !b.kind.is_verbatim() => span.color(pal.faint),
                _ => span,
            }
        })
        .collect()
}

fn paragraph(spans: &[text::Span<'static, (), Font>], width: f32, size: f32, wrapping: text::Wrapping) -> Paragraph {
    Paragraph::with_spans(text::Text {
        content: spans,
        bounds: Size::new(width, f32::INFINITY),
        size: Pixels(size),
        line_height: text::LineHeight::Relative(LINE),
        font: theme::SANS,
        align_x: text::Alignment::Default,
        align_y: alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping,
    })
}

/// Table grid filling `width`: columns share it by content width, rows are as
/// tall as their tallest cell. Returns the cells and the total height.
fn table_cells(b: &Block, pal: Pal, width: f32) -> (Vec<Cell>, f32) {
    let Kind::Table { align } = &b.kind else { return (vec![], 0.0) };
    let rows = b.cells();
    let cols = align.len();
    let cell_spans = |r: usize, range: &Range<usize>| {
        let mut sp = spans(&Block::new(Kind::Paragraph, b.slice(range.clone())), pal);
        if r == 0 {
            sp.iter_mut().for_each(|s| s.font = s.font.map(|f| Font { weight: font::Weight::Semibold, ..f }));
        }
        sp
    };
    let mut natural = vec![MIN_COL; cols];
    for (r, row) in rows.iter().enumerate() {
        for (c, range) in row.iter().enumerate() {
            let w = paragraph(&cell_spans(r, range), f32::INFINITY, SIZE, text::Wrapping::None).min_bounds().width;
            natural[c] = natural[c].max(w + 2.0 * CELL_PAD);
        }
    }
    let total: f32 = natural.iter().sum();
    let col_w: Vec<f32> = natural.iter().map(|n| n * width / total).collect();
    let mut cells = vec![];
    let mut y = 0.0;
    for (r, row) in rows.iter().enumerate() {
        let first = cells.len();
        let mut x = 0.0;
        let mut h: f32 = SIZE * LINE;
        for (c, range) in row.iter().enumerate() {
            let inner = (col_w[c] - 2.0 * CELL_PAD).max(10.0);
            let para = paragraph(&cell_spans(r, range), inner, SIZE, text::Wrapping::WordOrGlyph);
            h = h.max(para.min_bounds().height);
            // align by moving the text origin, so caret and hit-testing need no special case
            let free = inner - para.min_bounds().width;
            let shift = match align[c] {
                Alignment::Center => free / 2.0,
                Alignment::Right => free,
                _ => 0.0,
            };
            cells.push(Cell { para, rect: Rectangle::new(Point::new(x, y), Size::new(col_w[c], 0.0)), shift, range: range.clone(), header: r == 0 });
            x += col_w[c];
        }
        let h = h + 2.0 * CELL_PAD;
        cells[first..].iter_mut().for_each(|cell| cell.rect.height = h);
        y += h;
    }
    (cells, y)
}

impl<M> EditorView<'_, M> {
    fn build(&self, b: &Block, width: f32, renderer: &iced::Renderer) -> Laid {
        let left = match b.kind {
            Kind::List { depth, .. } => INDENT * (depth as f32 + 1.0),
            Kind::Raw | Kind::Code { .. } => RAW_PAD,
            _ => 0.0,
        };
        let w = (width - left - if b.kind.is_verbatim() { RAW_PAD } else { 0.0 }).max(10.0);
        let size = block_size(b);
        let spans = spans(b, self.pal);
        let wrapping = if b.kind.is_verbatim() { text::Wrapping::Glyph } else { text::Wrapping::WordOrGlyph };
        let para = paragraph(&spans, w, size, wrapping);
        let mut height = para.min_bounds().height.max(size * LINE);
        let mut cells = vec![];
        if let Kind::Table { .. } = b.kind {
            (cells, height) = table_cells(b, self.pal, w);
        }
        let mut image = None;
        if let Kind::Image { src, .. } = &b.kind {
            let handle = img::Handle::from_path(self.base.join(src));
            if let Ok(alloc) = renderer.load_image(&handle) {
                let s = alloc.size();
                let (iw, ih) = (s.width as f32, s.height as f32);
                let scale = (w / iw).min(MAX_IMAGE_H / ih).min(1.0);
                let size = Size::new(iw * scale, ih * scale);
                height = size.height;
                image = Some((handle, size));
            }
        }
        Laid { block: b.clone(), width, pal: self.pal, para, left, top: 0.0, height, image, cells }
    }

    fn key_actions(&self, state: &State, key: &Key, mods: Modifiers, text: Option<&str>, clipboard: &mut dyn Clipboard) -> Option<Vec<Action>> {
        let (cmd, shift, alt) = (mods.command(), mods.shift(), mods.alt());
        let ed = self.editor;
        let a = match key.as_ref() {
            Key::Named(Named::Enter) => Action::Enter,
            Key::Named(Named::Backspace) if alt && ed.selection().is_none() => {
                return Some(vec![Action::Move(Motion::WordLeft, true), Action::Backspace]);
            }
            Key::Named(Named::Backspace) => Action::Backspace,
            Key::Named(Named::Delete) => Action::Delete,
            Key::Named(Named::ArrowLeft) => {
                Action::Move(if cmd { Motion::Home } else if alt { Motion::WordLeft } else { Motion::Left }, shift)
            }
            Key::Named(Named::ArrowRight) => {
                Action::Move(if cmd { Motion::End } else if alt { Motion::WordRight } else { Motion::Right }, shift)
            }
            Key::Named(Named::ArrowUp) if cmd => Action::Move(Motion::DocStart, shift),
            Key::Named(Named::ArrowDown) if cmd => Action::Move(Motion::DocEnd, shift),
            Key::Named(Named::ArrowUp) => Action::Select { pos: vertical(&state.laid, ed.cursor, false), extend: shift },
            Key::Named(Named::ArrowDown) => Action::Select { pos: vertical(&state.laid, ed.cursor, true), extend: shift },
            Key::Named(Named::Home) => Action::Move(Motion::Home, shift),
            Key::Named(Named::End) => Action::Move(Motion::End, shift),
            Key::Named(Named::Tab) => if shift { Action::Outdent } else { Action::Indent },
            Key::Named(Named::Escape) => Action::Focus(false),
            Key::Character(c) if cmd => match c.to_lowercase().as_str() {
                "b" => Action::Toggle(Mark::Bold),
                "i" => Action::Toggle(Mark::Italic),
                "k" => Action::RequestLink,
                "z" if shift => Action::Redo,
                "z" => Action::Undo,
                "y" => Action::Redo,
                "a" => Action::SelectAll,
                "c" | "x" => {
                    let md = ed.selection_markdown()?;
                    clipboard.write(clipboard::Kind::Standard, md);
                    if c == "c" {
                        return Some(vec![]);
                    }
                    Action::Delete
                }
                "v" => Action::Paste(clipboard.read(clipboard::Kind::Standard)?),
                _ => return None,
            },
            _ => match text {
                Some(t) if !cmd && !t.is_empty() && !t.chars().any(char::is_control) => Action::Insert(t.to_string()),
                _ => return None,
            },
        };
        Some(vec![a])
    }
}

impl<M> Widget<M, Theme, iced::Renderer> for EditorView<'_, M> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        let width = Some(limits.max().width).filter(|w| w.is_finite()).unwrap_or(800.0);
        let state = tree.state.downcast_mut::<State>();
        // ponytail: cache keyed by block index; inserting near the top reshapes everything below
        let mut old: Vec<Option<Laid>> = std::mem::take(&mut state.laid).into_iter().map(Some).collect();
        let mut y = 0.0;
        for (i, b) in self.editor.blocks.iter().enumerate() {
            let cached = old.get_mut(i).and_then(|o| o.take_if(|l| l.block == *b && l.width == width && l.pal == self.pal));
            let mut l = cached.unwrap_or_else(|| self.build(b, width, renderer));
            if matches!(b.kind, Kind::Heading(_)) && i > 0 {
                y += 8.0;
            }
            if b.kind.is_verbatim() {
                y += RAW_PAD;
            }
            l.top = y;
            y += l.height + gap(b) + if b.kind.is_verbatim() { RAW_PAD } else { 0.0 };
            state.laid.push(l);
        }
        layout::Node::new(Size::new(width, y + BOTTOM))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let pal = self.pal;
        let selection = pal.selection();
        let quad = |r: Rectangle| renderer::Quad { bounds: r, ..Default::default() };
        let mut counters: Vec<(bool, usize)> = vec![];
        for (i, l) in state.laid.iter().enumerate() {
            let origin = Point::new(bounds.x + l.left, bounds.y + l.top);
            let marker = match l.block.kind {
                Kind::List { ordered, depth } => {
                    let d = depth as usize;
                    counters.truncate(d + 1);
                    counters.resize(d + 1, (ordered, 0));
                    if counters[d].0 != ordered {
                        counters[d] = (ordered, 0);
                    }
                    counters[d].1 += 1;
                    Some(if ordered { format!("{}.", counters[d].1) } else { ["•", "◦", "▪"][d % 3].into() })
                }
                _ => {
                    counters.clear();
                    None
                }
            };
            if origin.y > viewport.y + viewport.height || origin.y + l.height + RAW_PAD < viewport.y {
                continue;
            }
            match &l.block.kind {
                Kind::Raw | Kind::Code { .. } => renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle::new(
                            Point::new(bounds.x, origin.y - RAW_PAD),
                            Size::new(bounds.width, l.height + 2.0 * RAW_PAD),
                        ),
                        border: Border { radius: 4.0.into(), ..Border::default() },
                        ..Default::default()
                    },
                    pal.raised,
                ),
                Kind::Table { .. } => {
                    for cell in &l.cells {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: cell.rect + origin_vec(origin),
                                border: Border { color: pal.line, width: 1.0, radius: 0.0.into() },
                                ..Default::default()
                            },
                            if cell.header { pal.raised } else { Color::TRANSPARENT },
                        );
                    }
                }
                Kind::Image { src, .. } => match &l.image {
                    Some((handle, size)) => {
                        renderer.draw_image(img::Image::new(handle.clone()), Rectangle::new(origin, *size), *viewport)
                    }
                    None => renderer.fill_text(
                        label(format!("⚠ image not found: {src}"), l.width),
                        origin,
                        pal.faint,
                        *viewport,
                    ),
                },
                _ => {}
            }
            if let Kind::Code { lang } = &l.block.kind {
                let mut t = label(lang.clone(), 200.0);
                t.size = Pixels(11.0);
                t.align_x = text::Alignment::Right;
                let at = Point::new(bounds.x + bounds.width - 8.0, origin.y - RAW_PAD + 2.0);
                renderer.fill_text(t, at, pal.faint, *viewport);
            }
            if let Some(m) = marker {
                let mut t = label(m, INDENT);
                t.align_x = text::Alignment::Right;
                renderer.fill_text(t, Point::new(origin.x - 6.0, origin.y), style.text_color, *viewport);
            }
            if let Some((s, e)) = self.editor.selected_range(i) {
                let rects = match l.image {
                    Some((_, size)) => vec![Rectangle::new(Point::ORIGIN, size)],
                    None => highlight(l, s, e),
                };
                for r in rects {
                    renderer.fill_quad(quad(r + origin_vec(origin)), selection);
                }
            }
            if l.cells.is_empty() {
                renderer.fill_paragraph(&l.para, origin, style.text_color, *viewport);
            }
            for cell in &l.cells {
                renderer.fill_paragraph(&cell.para, cell.text_origin() + origin_vec(origin), style.text_color, *viewport);
            }
        }
        if self.editor.focused {
            if let Some(l) = state.laid.get(self.editor.cursor.block) {
                let origin = Point::new(bounds.x + l.left, bounds.y + l.top);
                if let Some((_, size)) = l.image {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle::new(origin, size),
                            border: Border { color: pal.signal, width: 2.0, radius: 2.0.into() },
                            ..Default::default()
                        },
                        Color::TRANSPARENT,
                    );
                } else {
                    let (x, y, h) = caret(l, self.editor.cursor.offset);
                    let r = Rectangle::new(Point::new(origin.x + x, origin.y + y), Size::new(1.5, h));
                    renderer.fill_quad(quad(r), style.text_color);
                }
            }
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let mut actions = vec![];
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => match cursor.position_in(bounds) {
                Some(p) if !state.laid.is_empty() => {
                    let pos = pos_at(&state.laid, p);
                    if state.modifiers.command() {
                        if let Some(url) = self.editor.link_at(pos) {
                            open_url(&url);
                        }
                    }
                    let double = state
                        .last_click
                        .is_some_and(|(t, q)| q == pos && t.elapsed() < Duration::from_millis(400));
                    state.last_click = Some((Instant::now(), pos));
                    state.dragging = true;
                    if !self.editor.focused {
                        actions.push(Action::Focus(true));
                    }
                    actions.push(if double {
                        Action::SelectWord(pos)
                    } else {
                        Action::Select { pos, extend: state.modifiers.shift() }
                    });
                    shell.capture_event();
                }
                _ if self.editor.focused => actions.push(Action::Focus(false)),
                _ => {}
            },
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.dragging => {
                if let Some(p) = cursor.position() {
                    let local = Point::new(p.x - bounds.x, p.y - bounds.y);
                    let pos = pos_at(&state.laid, local);
                    if pos != self.editor.cursor {
                        actions.push(Action::Select { pos, extend: true });
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => state.dragging = false,
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => state.modifiers = *m,
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, text, .. }) if self.editor.focused => {
                if let Some(a) = self.key_actions(state, key, *modifiers, text.as_deref(), clipboard) {
                    actions = a;
                    shell.capture_event();
                }
            }
            _ => {}
        }
        for a in actions {
            shell.publish((self.on_action)(a));
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) { mouse::Interaction::Text } else { mouse::Interaction::None }
    }
}

fn origin_vec(p: Point) -> iced::Vector {
    iced::Vector::new(p.x, p.y)
}

fn label(content: String, width: f32) -> text::Text<String, Font> {
    text::Text {
        content,
        bounds: Size::new(width, SIZE * LINE),
        size: Pixels(SIZE),
        line_height: text::LineHeight::Relative(LINE),
        font: theme::SANS,
        align_x: text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
    }
}

/// Block text offset -> (buffer line, byte index in line). Buffer lines split on '\n'.
fn to_line(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset];
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    (before.matches('\n').count(), offset - start)
}

fn from_line(text: &str, line: usize, index: usize) -> usize {
    let start: usize = text.split('\n').take(line).map(|l| l.len() + 1).sum();
    (start + index).min(text.len())
}

/// Caret (x, top, height) relative to the block origin.
fn caret(l: &Laid, offset: usize) -> (f32, f32, f32) {
    let text = l.block.text();
    match l.cells.iter().find(|c| offset <= c.range.end) {
        Some(cell) => {
            let o = cell.text_origin();
            let (x, y, h) = caret_in(&cell.para, &text[cell.range.clone()], offset - cell.range.start, SIZE);
            (o.x + x, o.y + y, h)
        }
        None => caret_in(&l.para, &text, offset, block_size(&l.block)),
    }
}

fn caret_in(para: &Paragraph, text: &str, offset: usize, size: f32) -> (f32, f32, f32) {
    let (line, idx) = to_line(text, offset);
    let runs: Vec<_> = para.buffer().layout_runs().filter(|r| r.line_i == line).collect();
    for (k, run) in runs.iter().enumerate() {
        let end = run.glyphs.last().map_or(0, |g| g.end);
        if idx < end || k + 1 == runs.len() {
            let x = run
                .glyphs
                .iter()
                .find(|g| idx >= g.start && idx < g.end)
                .map(|g| g.x)
                .unwrap_or_else(|| run.glyphs.last().map_or(0.0, |g| g.x + g.w));
            return (x, run.line_top, run.line_height);
        }
    }
    (0.0, 0.0, size * LINE)
}

fn highlight(l: &Laid, s: usize, e: usize) -> Vec<Rectangle> {
    let text = l.block.text();
    if l.cells.is_empty() {
        return highlight_in(&l.para, &text, s, e, block_size(&l.block));
    }
    let mut rects = vec![];
    for cell in &l.cells {
        let r = &cell.range;
        let (cs, ce) = (s.max(r.start), e.min(r.end));
        // partly selected cells, and fully selected empty ones (as a sliver)
        if cs < ce || (r.is_empty() && s <= r.start && r.end < e) {
            let o = origin_vec(cell.text_origin());
            rects.extend(highlight_in(&cell.para, &text[r.clone()], cs - r.start, ce - r.start, SIZE).into_iter().map(|x| x + o));
        }
    }
    rects
}

fn highlight_in(para: &Paragraph, text: &str, s: usize, e: usize, size: f32) -> Vec<Rectangle> {
    let ((l1, i1), (l2, i2)) = (to_line(text, s), to_line(text, e));
    let (c1, c2) = (cosmic_text::Cursor::new(l1, i1), cosmic_text::Cursor::new(l2, i2));
    let mut rects: Vec<Rectangle> = para
        .buffer()
        .layout_runs()
        .filter_map(|run| {
            let (x, w) = run.highlight(c1, c2)?;
            Some(Rectangle::new(Point::new(x, run.line_top), Size::new(w.max(4.0), run.line_height)))
        })
        .collect();
    if rects.is_empty() && text.is_empty() {
        // selected empty block: show a sliver so the selection reads as continuous
        rects.push(Rectangle::new(Point::ORIGIN, Size::new(4.0, size * LINE)));
    }
    rects
}

/// Point relative to the block origin -> byte offset.
fn hit(l: &Laid, p: Point) -> usize {
    let text = l.block.text();
    if l.cells.is_empty() {
        return hit_in(&l.para, &text, p);
    }
    // the row under p (or the last), then the column under p (or the last)
    let last = l.cells.last().unwrap();
    let row_y = l.cells.iter().find(|c| p.y < c.rect.y + c.rect.height).unwrap_or(last).rect.y;
    let row: Vec<&Cell> = l.cells.iter().filter(|c| c.rect.y == row_y).collect();
    let cell = row.iter().find(|c| p.x < c.rect.x + c.rect.width).unwrap_or(row.last().unwrap());
    let o = cell.text_origin();
    // clicks/probes in the cell padding go to the nearest text line
    let y = (p.y - o.y).clamp(0.0, (cell.para.min_bounds().height - 1.0).max(0.0));
    cell.range.start + hit_in(&cell.para, &text[cell.range.clone()], Point::new(p.x - o.x, y))
}

fn hit_in(para: &Paragraph, text: &str, p: Point) -> usize {
    match para.buffer().hit(p.x, p.y) {
        Some(c) => from_line(text, c.line, c.index),
        None if p.y < 0.0 => 0,
        None => text.len(),
    }
}

/// Point relative to the widget -> document position.
fn pos_at(laid: &[Laid], p: Point) -> Pos {
    let i = laid.iter().position(|l| p.y < l.top + l.height + gap(&l.block)).unwrap_or(laid.len() - 1);
    let l = &laid[i];
    let offset = if l.image.is_some() || matches!(l.block.kind, Kind::Image { .. }) {
        0
    } else {
        hit(l, Point::new(p.x - l.left, (p.y - l.top).clamp(0.0, (l.height - 1.0).max(0.0))))
    };
    Pos { block: i, offset }
}

/// Position one visual line above/below the caret.
fn vertical(laid: &[Laid], c: Pos, down: bool) -> Pos {
    let Some(l) = laid.get(c.block) else { return c };
    let (x, y, h) = caret(l, c.offset);
    let mut ty = if down { y + h + 1.0 } else { y - 1.0 };
    if let Some(cell) = l.cells.iter().find(|cell| c.offset <= cell.range.end) {
        let top = cell.text_origin().y;
        if ty < top || ty >= top + cell.para.min_bounds().height {
            // past the cell's text (into its padding): step to the row above/below
            ty = if down { cell.rect.y + cell.rect.height + 1.0 } else { cell.rect.y - 1.0 };
        }
    }
    let is_image = |l: &Laid| matches!(l.block.kind, Kind::Image { .. });
    if !is_image(l) && ty >= 0.0 && ty < l.height {
        return Pos { offset: hit(l, Point::new(x, ty)), ..c };
    }
    let j = if down { c.block + 1 } else { c.block.wrapping_sub(1) };
    let Some(m) = laid.get(j) else {
        return Pos { offset: if down { l.block.len() } else { 0 }, ..c };
    };
    let offset = if is_image(m) {
        0
    } else {
        hit(m, Point::new(x + l.left - m.left, if down { 1.0 } else { m.height - 1.0 }))
    };
    Pos { block: j, offset }
}

pub fn open_url(url: &str) {
    // only web/mail links; never hand a local path to the OS opener
    if !["http://", "https://", "mailto:"].iter().any(|s| url.starts_with(s)) {
        return;
    }
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laid(b: Block, pal: Pal) -> Laid {
        let (cells, mut height) = table_cells(&b, pal, 600.0);
        let para = paragraph(&spans(&b, pal), 600.0, SIZE, text::Wrapping::WordOrGlyph);
        if cells.is_empty() {
            height = para.min_bounds().height.max(SIZE * LINE);
        }
        Laid { block: b, width: 600.0, pal, para, left: 0.0, top: 0.0, height, image: None, cells }
    }

    #[test]
    fn arrows_move_between_table_rows() {
        let pal = theme::Colors::dark().pal();
        let laid: Vec<Laid> = crate::doc::parse("above\n\n| a | b |\n|---|---|\n| c | d |\n| e | f |\n\nbelow\n")
            .into_iter()
            .map(|b| laid(b, pal))
            .collect();
        let at = |offset| Pos { block: 1, offset };
        // "a\tb\nc\td\ne\tf": b=2, d=6, f=10 (caret after the letter)
        assert_eq!(vertical(&laid, at(3), true), at(7), "b -> d");
        assert_eq!(vertical(&laid, at(7), true), at(11), "d -> f");
        assert_eq!(vertical(&laid, at(11), true).block, 2, "last row -> next block");
        assert_eq!(vertical(&laid, at(7), false), at(3), "d -> b");
        assert_eq!(vertical(&laid, at(3), false).block, 0, "header -> previous block");
    }
}
