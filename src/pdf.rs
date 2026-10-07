//! Export a note's block model to PDF, styled like the editor.

use crate::doc::{self, Block, Kind, Span};
use crate::theme;
use genpdf::elements::{Break, FrameCellDecorator, FramedElement, Image, LinearLayout, OrderedList, PaddedElement, Paragraph, StyledElement, TableLayout, UnorderedList};
use genpdf::fonts::{FontData, FontFamily};
use genpdf::style::{Color, Style};
use genpdf::error::Error;
use genpdf::render::Area;
use genpdf::{Alignment, Context, Element, Margins, RenderResult, Scale, SimplePageDecorator, Size};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::path::Path;

const LINK: Color = Color::Rgb(37, 99, 235);
const MUTED: Color = Color::Greyscale(90);
/// A4 minus the page margins, in mm.
const CONTENT: (f64, f64) = (210.0 - 40.0, 297.0 - 40.0);

pub fn export(title: &str, blocks: &[Block], note_dir: &Path, dest: &Path) -> Result<(), String> {
    let font = |b: &[u8]| FontData::new(b.to_vec(), None).map_err(|e| e.to_string());
    let sans = FontFamily {
        regular: font(theme::SANS_REGULAR)?,
        bold: font(theme::SANS_SEMIBOLD)?,
        italic: font(theme::SANS_ITALIC)?,
        bold_italic: font(theme::SANS_SEMIBOLD_ITALIC)?,
    };
    let mono = font(theme::MONO_REGULAR)?;
    let mut doc = genpdf::Document::new(sans);
    let mono = doc.add_font_family(FontFamily { regular: mono.clone(), bold: mono.clone(), italic: mono.clone(), bold_italic: mono });
    let mono = Style::new().with_font_family(mono);
    doc.set_title(title);
    doc.set_font_size(11);
    doc.set_line_spacing(1.35);
    let mut deco = SimplePageDecorator::new();
    deco.set_margins(20);
    doc.set_page_decorator(deco);

    doc.push(Paragraph::new(genpdf::style::StyledString::new(title, Style::new().bold().with_font_size(26))));
    doc.push(Break::new(0.6));

    let mut i = 0;
    while i < blocks.len() {
        let b = &blocks[i];
        let mut out = LinearLayout::vertical();
        match &b.kind {
            Kind::Paragraph if b.spans.is_empty() => out.push(Break::new(0.5)),
            Kind::Paragraph => out.push(para(&b.spans, Style::new(), mono)),
            Kind::Heading(n) => {
                let size = [22, 17, 14].get(*n as usize - 1).copied().unwrap_or(12);
                out.push(Break::new(0.4));
                out.push(para(&b.spans, Style::new().bold().with_font_size(size), mono));
            }
            Kind::List { .. } => {
                let (list, next) = list(blocks, i, mono);
                out.push(list);
                i = next;
                doc.push(PaddedElement::new(out, Margins::trbl(0, 0, 2, 0)));
                continue;
            }
            Kind::Raw | Kind::Table { .. } if let Some(t) = table(&doc::serialize(std::slice::from_ref(b)), mono) => {
                out.push(StyledElement::new(t, Style::new().with_font_size(10)))
            }
            Kind::Code { .. } | Kind::Raw | Kind::Table { .. } => out.push(code(&b.text(), mono)),
            Kind::Image { src, alt } => match image(&note_dir.join(src)) {
                Some(img) => out.push(img),
                None => out.push(Paragraph::new(genpdf::style::StyledString::new(
                    format!("[image: {}]", if alt.is_empty() { src } else { alt }),
                    Style::new().italic().with_color(MUTED),
                ))),
            },
        }
        doc.push(PaddedElement::new(out, Margins::trbl(0, 0, 2, 0)));
        i += 1;
    }
    doc.render_to_file(dest).map_err(|e| format!("could not write {}: {e}", dest.display()))
}

fn para(spans: &[Span], base: Style, mono: Style) -> Paragraph {
    let mut p = Paragraph::default();
    for s in spans {
        let mut style = base;
        if s.style.bold {
            style.set_bold();
        }
        if s.style.italic {
            style.set_italic();
        }
        if s.style.code {
            style.merge(mono);
        }
        if s.style.link.is_some() {
            style.set_color(LINK);
        }
        p.push_styled(s.text.clone(), style);
    }
    p
}

/// Consecutive list blocks starting at `i` (same type at that depth) as one
/// nested list; returns it with the index of the first block after it.
fn list(blocks: &[Block], mut i: usize, mono: Style) -> (LinearLayout, usize) {
    let Kind::List { ordered, depth } = blocks[i].kind else { unreachable!() };
    let mut items: Vec<LinearLayout> = vec![];
    while let Some(Kind::List { ordered: o, depth: d }) = blocks.get(i).map(|b| &b.kind) {
        if *d < depth || (*d == depth && *o != ordered) {
            break;
        }
        if *d > depth {
            // nested list belongs to the previous item (or stands alone if there is none)
            let (nested, next) = list(blocks, i, mono);
            match items.last_mut() {
                Some(item) => item.push(nested),
                None => items.push(nested),
            }
            i = next;
            continue;
        }
        items.push(LinearLayout::vertical().element(para(&blocks[i].spans, Style::new(), mono)));
        i += 1;
    }
    let mut out = LinearLayout::vertical();
    if ordered {
        let mut l = OrderedList::new();
        items.into_iter().for_each(|it| l.push(it));
        out.push(l);
    } else {
        let mut l = UnorderedList::with_bullet(if depth % 2 == 0 { "•" } else { "◦" });
        items.into_iter().for_each(|it| l.push(it));
        out.push(l);
    }
    (out, i)
}

/// A raw block that is a GFM table, as a framed table; None for anything else.
fn table(src: &str, mono: Style) -> Option<TableLayout> {
    let mut ev = Parser::new_ext(src, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH);
    let Some(Event::Start(Tag::Table(aligns))) = ev.next() else { return None };
    // rows of cells, each cell as spans
    let mut rows: Vec<Vec<Vec<Span>>> = vec![];
    let mut style = doc::Style::default();
    let push = |rows: &mut Vec<Vec<Vec<Span>>>, text: &str, style: doc::Style| {
        if let Some(cell) = rows.last_mut().and_then(|r| r.last_mut()) {
            cell.push(Span { text: text.into(), style });
        }
    };
    for e in ev {
        match e {
            Event::Start(Tag::TableHead | Tag::TableRow) => rows.push(vec![]),
            Event::Start(Tag::TableCell) => rows.last_mut()?.push(vec![]),
            Event::Start(Tag::Strong) => style.bold = true,
            Event::End(TagEnd::Strong) => style.bold = false,
            Event::Start(Tag::Emphasis) => style.italic = true,
            Event::End(TagEnd::Emphasis) => style.italic = false,
            Event::Start(Tag::Link { dest_url, .. }) => style.link = Some(dest_url.to_string()),
            Event::End(TagEnd::Link) => style.link = None,
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => push(&mut rows, &t, style.clone()),
            Event::Code(t) => push(&mut rows, &t, doc::Style { code: true, ..style.clone() }),
            Event::SoftBreak | Event::HardBreak => push(&mut rows, " ", style.clone()),
            Event::End(TagEnd::Table) => break,
            _ => {}
        }
    }
    let cols = aligns.len();
    // column widths follow the longest cell, within limits so no column gets squeezed out
    let weights = (0..cols)
        .map(|c| rows.iter().filter_map(|r| r.get(c)).map(|s| s.iter().map(|s| s.text.chars().count()).sum()).max().unwrap_or(0).clamp(4, 40))
        .collect();
    let mut t = TableLayout::new(weights);
    t.set_cell_decorator(FrameCellDecorator::new(true, true, false));
    for (i, row) in rows.iter().enumerate() {
        let base = if i == 0 { Style::new().bold() } else { Style::new() };
        let cells = (0..cols).map(|c| {
            let align = match aligns[c] {
                pulldown_cmark::Alignment::Center => Alignment::Center,
                pulldown_cmark::Alignment::Right => Alignment::Right,
                _ => Alignment::Left,
            };
            let p = para(row.get(c).map_or(&[][..], |s| s), base, mono).aligned(align);
            Box::new(PaddedElement::new(p, Margins::trbl(1.5, 2, 1.5, 2))) as Box<dyn Element>
        });
        t.push_row(cells.collect()).ok()?;
    }
    Some(t)
}

/// Code/raw block in a frame. Like CSS `break-inside: avoid`: a block that
/// doesn't fit the rest of the page moves to the next one, unless it's taller
/// than a whole page anyway.
struct Code {
    lines: Vec<String>,
    style: Style,
    frame: FramedElement<PaddedElement<StyledElement<LinearLayout>>>,
    moved: bool,
}

const CODE_PAD: f64 = 3.0;

fn code(text: &str, mono: Style) -> Code {
    let style = mono.with_font_size(9);
    let lines: Vec<String> = text.split('\n').map(|l| if l.is_empty() { " ".into() } else { l.replace('\t', "    ") }).collect();
    let mut layout = LinearLayout::vertical();
    lines.iter().for_each(|l| layout.push(Paragraph::new(l.clone())));
    // line height comes from the element style, not the string's
    let frame = FramedElement::new(PaddedElement::new(StyledElement::new(layout, style), Margins::all(CODE_PAD)));
    Code { lines, style, frame, moved: false }
}

impl Element for Code {
    fn render(&mut self, context: &Context, area: Area<'_>, style: Style) -> Result<RenderResult, Error> {
        if !self.moved {
            self.moved = true;
            let style = style.and(self.style);
            let width = f64::from(area.size().width) - 2.0 * CODE_PAD;
            // wrapped rows per line, estimated from the text width
            let rows: f64 = self.lines.iter().map(|l| (f64::from(style.str_width(&context.font_cache, l)) / width).ceil().max(1.0)).sum();
            let height = rows * f64::from(style.line_height(&context.font_cache)) + 2.0 * CODE_PAD;
            let fresh_page = f64::from(area.size().height) >= CONTENT.1 - 1.0;
            if !fresh_page && height > f64::from(area.size().height) && height <= CONTENT.1 {
                // nothing drawn: the document starts a new page and renders us again
                return Ok(RenderResult { size: Size::new(0, 0), has_more: true });
            }
        }
        self.frame.render(context, area, style)
    }
}

/// genpdf can't embed images with alpha, so flatten onto white; scale to fit the page.
fn image(path: &Path) -> Option<Image> {
    let img = image023::open(path).ok()?.to_rgba8();
    let mut rgb = image023::RgbImage::new(img.width(), img.height());
    for (dst, src) in rgb.pixels_mut().zip(img.pixels()) {
        let a = src[3] as u32;
        *dst = image023::Rgb([0, 1, 2].map(|c| ((src[c] as u32 * a + 255 * (255 - a)) / 255) as u8));
    }
    // 96 dpi: same size as on screen, unless that's too big for the page
    let (w, h) = (rgb.width() as f64 * 25.4 / 96.0, rgb.height() as f64 * 25.4 / 96.0);
    let fit = (CONTENT.0 / w).min(CONTENT.1 * 0.9 / h).min(1.0);
    Some(Image::from_dynamic_image(image023::DynamicImage::ImageRgb8(rgb)).ok()?.with_dpi(96.0).with_scale(Scale::new(fit, fit)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn code_block_moves_to_next_page() {
        let md = format!("{}```\n{}```\n", "filler paragraph\n\n".repeat(30), "line\n".repeat(15)) + &format!("```\n{}```\n", "taller than a page\n".repeat(120));
        let dest = std::env::temp_dir().join("yana-pdf-break.pdf");
        super::export("Break", &crate::doc::parse(&md), &std::env::temp_dir(), &dest).unwrap();
    }

    #[test]
    fn exports_every_block_kind() {
        let md = "# Title\n\nplain **bold** *it* `code` [link](http://x.y)\n\n- a\n- b\n    1. nested\n- c\n\n```rust\nfn main() {\n\tlet x = 1;\n}\n```\n\n| Name | Qty | Note |\n|:---|---:|:---:|\n| **apple** | 3 | `fresh` [link](http://x.y) |\n| a much longer cell that has to wrap onto several lines in the pdf | 12 | - |\n| short row |\n\n![missing](nope.png)\n\n![alpha](yana-pdf-test.png)\n";
        let dir = std::env::temp_dir();
        // translucent image: genpdf rejects alpha unless we flatten it
        image023::RgbaImage::from_pixel(400, 200, image023::Rgba([200, 241, 105, 128])).save(dir.join("yana-pdf-test.png")).unwrap();
        let dest = dir.join("yana-pdf-test.pdf");
        super::export("Test note", &crate::doc::parse(md), &dir, &dest).unwrap();
        let bytes = std::fs::read(&dest).unwrap();
        assert!(bytes.starts_with(b"%PDF") && bytes.len() > 1000);
        assert!(super::image(&dir.join("yana-pdf-test.png")).is_some());
        let mono = genpdf::style::Style::new();
        assert!(super::table("| a | b |\n|---|---|\n| 1 | 2 |", mono).is_some());
        assert!(super::table("> just a quote", mono).is_none());
    }
}
