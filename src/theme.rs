//! App palette (user-editable, stored in config.toml), fonts, icons and
//! widget styles from the ez-notes style sheet.

use iced::widget::{button, container, pick_list, rule, svg, text, text_input};
use iced::{Background, Border, Color, Font, Theme, font, overlay};
use serde::{Deserialize, Serialize};

pub const SANS: Font = Font::with_name("IBM Plex Sans");
pub const MONO: Font = Font::with_name("IBM Plex Mono");
pub const ICONS: Font = Font::with_name("lucide");

pub const FONTS: [&[u8]; 7] = [
    include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Italic.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-SemiBoldItalic.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../assets/fonts/lucide.ttf"),
];

const LOGO: &str = include_str!("../assets/icons/yana-logo.svg");
const LOGO_SMALL: &str = include_str!("../assets/icons/yana-logo-small.svg");
/// Colors the logo artwork is drawn in; swapped for the palette at runtime.
const ART_SIGNAL: &str = "#C8F169";
const ART_GROUND: &str = "#0F1012";

pub fn hex(c: Color) -> String {
    let [r, g, b, _] = c.into_rgba8();
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// The Yana logo in the current palette: `small` is the 16 px variant (no dots, thicker strokes).
pub fn logo(pal: &Pal, small: bool) -> svg::Handle {
    let src = if small { LOGO_SMALL } else { LOGO };
    svg::Handle::from_memory(src.replace(ART_SIGNAL, &hex(pal.signal)).replace(ART_GROUND, &hex(pal.ground)).into_bytes())
}

/// Window icon from the small logo (Windows/Linux title bar and taskbar; macOS uses the app bundle icon).
pub fn window_icon() -> Option<iced::window::Icon> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_str(LOGO_SMALL, &usvg::Options::default()).ok()?;
    let size = 64;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let scale = size as f32 / tree.size().width();
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    iced::window::icon::from_rgba(rgba, size, size).ok()
}

pub fn weight(w: font::Weight) -> Font {
    Font { weight: w, ..SANS }
}

/// Palette as hex strings, the way it's stored and edited.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Colors {
    pub ground: String,
    pub panel: String,
    pub raised: String,
    pub line: String,
    pub signal: String,
    pub tag: String,
    pub danger: String,
    pub text: String,
    pub muted: String,
    pub faint: String,
}

/// (key, label) for the theme editor, in display order.
pub const FIELDS: [(&str, &str); 10] = [
    ("ground", "Ground"),
    ("panel", "Panel"),
    ("raised", "Raised"),
    ("line", "Line"),
    ("signal", "Signal / primary"),
    ("tag", "Tag"),
    ("danger", "Destructive"),
    ("text", "Text"),
    ("muted", "Muted"),
    ("faint", "Faint"),
];

impl Default for Colors {
    fn default() -> Self {
        Colors::dark()
    }
}

impl Colors {
    fn from(v: [&str; 10]) -> Self {
        let s = |i: usize| v[i].to_string();
        Colors {
            ground: s(0),
            panel: s(1),
            raised: s(2),
            line: s(3),
            signal: s(4),
            tag: s(5),
            danger: s(6),
            text: s(7),
            muted: s(8),
            faint: s(9),
        }
    }

    pub fn dark() -> Self {
        Colors::from(["#0F1012", "#17181B", "#202226", "#2A2C31", "#C8F169", "#5ED3C0", "#FF8A80", "#ECEDEE", "#9A9DA4", "#80848C"])
    }

    pub fn light() -> Self {
        Colors::from(["#F4F4F1", "#FFFFFF", "#ECECE8", "#DCDCD7", "#4D7C0F", "#0F8C7C", "#C9372C", "#1A1B1E", "#5C5F66", "#8A8D94"])
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut String> {
        Some(match key {
            "ground" => &mut self.ground,
            "panel" => &mut self.panel,
            "raised" => &mut self.raised,
            "line" => &mut self.line,
            "signal" => &mut self.signal,
            "tag" => &mut self.tag,
            "danger" => &mut self.danger,
            "text" => &mut self.text,
            "muted" => &mut self.muted,
            "faint" => &mut self.faint,
            _ => return None,
        })
    }

    pub fn get(&self, key: &str) -> &str {
        match key {
            "ground" => &self.ground,
            "panel" => &self.panel,
            "raised" => &self.raised,
            "line" => &self.line,
            "signal" => &self.signal,
            "tag" => &self.tag,
            "danger" => &self.danger,
            "text" => &self.text,
            "muted" => &self.muted,
            "faint" => &self.faint,
            _ => "",
        }
    }

    /// Parsed palette; a field that isn't valid hex falls back to the dark default.
    pub fn pal(&self) -> Pal {
        let d = Colors::dark();
        let c = |v: &str, def: &str| crate::tags::hex_color(v.trim()).or_else(|| crate::tags::hex_color(def)).unwrap();
        Pal {
            ground: c(&self.ground, &d.ground),
            panel: c(&self.panel, &d.panel),
            raised: c(&self.raised, &d.raised),
            line: c(&self.line, &d.line),
            signal: c(&self.signal, &d.signal),
            tag: c(&self.tag, &d.tag),
            danger: c(&self.danger, &d.danger),
            text: c(&self.text, &d.text),
            muted: c(&self.muted, &d.muted),
            faint: c(&self.faint, &d.faint),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pal {
    pub ground: Color,
    pub panel: Color,
    pub raised: Color,
    pub line: Color,
    pub signal: Color,
    pub tag: Color,
    pub danger: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
}

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::from_rgba(a.r + (b.r - a.r) * t, a.g + (b.g - a.g) * t, a.b + (b.b - a.b) * t, a.a + (b.a - a.a) * t)
}

pub fn alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

/// Black or white, whichever reads on `bg`.
pub fn on(bg: Color) -> Color {
    if bg.relative_luminance() > 0.45 { Color::BLACK } else { Color::WHITE }
}

impl Pal {
    pub fn theme(&self) -> Theme {
        Theme::custom(
            "ez-notes",
            iced::theme::Palette {
                background: self.ground,
                text: self.text,
                primary: self.signal,
                success: self.signal,
                warning: self.tag,
                danger: self.danger,
            },
        )
    }

    /// Lighter on dark themes, darker on light ones.
    fn lift(&self, c: Color, t: f32) -> Color {
        let towards = if self.ground.relative_luminance() < 0.5 { Color::WHITE } else { Color::BLACK };
        mix(c, towards, t)
    }

    pub fn selection(&self) -> Color {
        alpha(self.signal, 0.3)
    }

    // ---- containers

    pub fn panel(self) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(self.panel.into()),
            border: Border { color: self.line, width: 1.0, radius: 12.0.into() },
            text_color: Some(self.text),
            ..container::Style::default()
        }
    }

    pub fn group(self) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(self.raised.into()),
            border: Border { color: self.line, width: 1.0, radius: 8.0.into() },
            ..container::Style::default()
        }
    }

    pub fn ground(self) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(self.ground.into()),
            text_color: Some(self.text),
            ..container::Style::default()
        }
    }

    pub fn fill(self, bg: Color, radius: f32) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(bg.into()),
            border: Border { radius: radius.into(), ..Border::default() },
            ..container::Style::default()
        }
    }

    pub fn rule(self) -> impl Fn(&Theme) -> rule::Style {
        move |_| rule::Style { color: self.line, radius: 0.0.into(), fill_mode: rule::FillMode::Full, snap: true }
    }

    // ---- buttons (36 px high, radius 8)

    fn button_style(&self, bg: Option<Color>, fg: Color, border: Color) -> button::Style {
        button::Style {
            background: bg.map(Background::from),
            text_color: fg,
            border: Border { color: border, width: if border.a > 0.0 { 1.0 } else { 0.0 }, radius: 8.0.into() },
            ..button::Style::default()
        }
    }

    pub fn primary(self) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, s| {
            let bg = match s {
                button::Status::Hovered => mix(self.signal, Color::WHITE, 0.25),
                button::Status::Pressed => mix(self.signal, Color::BLACK, 0.12),
                button::Status::Disabled => alpha(self.signal, 0.4),
                button::Status::Active => self.signal,
            };
            self.button_style(Some(bg), on(self.signal), Color::TRANSPARENT)
        }
    }

    pub fn secondary(self) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, s| {
            let (bg, fg) = match s {
                button::Status::Hovered => (self.lift(self.raised, 0.04), self.text),
                button::Status::Pressed => (self.lift(self.raised, 0.08), self.text),
                button::Status::Disabled => (self.raised, self.faint),
                button::Status::Active => (self.raised, self.text),
            };
            self.button_style(Some(bg), fg, self.line)
        }
    }

    pub fn ghost(self) -> impl Fn(&Theme, button::Status) -> button::Style {
        self.ghost_with(self.text, false)
    }

    /// Ghost button; `active` marks the current choice (toolbar state).
    pub fn ghost_with(self, fg: Color, active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, s| {
            let bg = match s {
                _ if active => Some(self.lift(self.raised, 0.06)),
                button::Status::Hovered => Some(self.lift(self.raised, 0.03)),
                button::Status::Pressed => Some(self.lift(self.raised, 0.07)),
                _ => None,
            };
            let fg = match s {
                button::Status::Disabled => self.faint,
                _ if active => self.signal,
                _ => fg,
            };
            self.button_style(bg, fg, Color::TRANSPARENT)
        }
    }

    pub fn danger(self) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, s| {
            let bg = match s {
                button::Status::Hovered => Some(alpha(self.danger, 0.12)),
                button::Status::Pressed => Some(alpha(self.danger, 0.2)),
                _ => None,
            };
            let fg = if s == button::Status::Disabled { self.faint } else { self.danger };
            self.button_style(bg, fg, Color::TRANSPARENT)
        }
    }

    /// Tree rows: 32 px, radius 6.
    pub fn row(self, selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, s| {
            let bg = match s {
                _ if selected => Some(alpha(self.signal, 0.12)),
                button::Status::Hovered | button::Status::Pressed => Some(self.raised),
                _ => None,
            };
            button::Style {
                background: bg.map(Background::from),
                text_color: self.text,
                border: Border { radius: 6.0.into(), ..Border::default() },
                ..button::Style::default()
            }
        }
    }

    /// Bare clickable text (chip label, chip ×).
    pub fn bare(self, fg: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, s| button::Style {
            text_color: if s == button::Status::Hovered { self.text } else { fg },
            ..button::Style::default()
        }
    }

    pub fn swatch(self, c: Color, selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, _| button::Style {
            background: Some(c.into()),
            text_color: on(c),
            border: Border { color: if selected { self.text } else { self.line }, width: if selected { 2.0 } else { 1.0 }, radius: 6.0.into() },
            ..button::Style::default()
        }
    }

    // ---- inputs

    pub fn input(self) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
        move |_, s| text_input::Style {
            background: self.raised.into(),
            border: Border {
                color: if matches!(s, text_input::Status::Focused { .. }) { self.signal } else { self.line },
                width: 1.0,
                radius: 8.0.into(),
            },
            icon: self.muted,
            placeholder: self.faint,
            value: self.text,
            selection: self.selection(),
        }
    }

    pub fn pick(self) -> impl Fn(&Theme, pick_list::Status) -> pick_list::Style {
        move |_, _| pick_list::Style {
            text_color: self.text,
            placeholder_color: self.faint,
            handle_color: self.muted,
            background: Color::TRANSPARENT.into(),
            border: Border::default(),
        }
    }

    pub fn menu(self) -> impl Fn(&Theme) -> overlay::menu::Style {
        move |_| overlay::menu::Style {
            background: self.raised.into(),
            border: Border { color: self.line, width: 1.0, radius: 8.0.into() },
            text_color: self.text,
            selected_text_color: on(self.signal),
            selected_background: self.signal.into(),
            shadow: Default::default(),
        }
    }
}

/// A Lucide icon glyph.
pub fn icon<'a>(codepoint: char) -> text::Text<'a> {
    text(codepoint.to_string()).font(ICONS).size(16).line_height(1.0)
}

pub mod i {
    pub const FILE_PLUS: char = '\u{e0cd}';
    pub const FOLDER_PLUS: char = '\u{e0de}';
    pub const CHEVRON_DOWN: char = '\u{e071}';
    pub const CHEVRON_RIGHT: char = '\u{e073}';
    pub const BOOK: char = '\u{e062}';
    pub const CHECK: char = '\u{e070}';
    pub const REFRESH: char = '\u{e149}';
    pub const UPLOAD: char = '\u{e19e}';
    pub const PLUS: char = '\u{e141}';
    pub const MINUS: char = '\u{e120}';
    pub const PENCIL: char = '\u{e1f9}';
    pub const TRASH: char = '\u{e18e}';
    pub const LIST: char = '\u{e10c}';
    pub const LIST_ORDERED: char = '\u{e1d1}';
    pub const LINK: char = '\u{e108}';
    pub const IMAGE: char = '\u{e0f9}';
    pub const CODE: char = '\u{e097}';
    pub const PALETTE: char = '\u{e1dd}';
    pub const X: char = '\u{e1b2}';
    pub const ALERT: char = '\u{e193}';
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logos() {
        assert!(window_icon().is_some(), "small logo rasterizes");
        let mut c = Colors::light();
        c.signal = "#123456".into();
        let pal = c.pal();
        assert_eq!(hex(pal.signal), "#123456");
        assert!(LOGO.contains(ART_SIGNAL) && LOGO.contains(ART_GROUND), "artwork colors to recolor");
        let _ = logo(&pal, false);
    }

    #[test]
    fn palette_parsing() {
        assert_eq!(Colors::dark().pal().signal, Color::from_rgb8(0xC8, 0xF1, 0x69));
        let mut c = Colors::light();
        *c.get_mut("signal").unwrap() = "nope".into();
        assert_eq!(c.pal().signal, Colors::dark().pal().signal, "invalid hex falls back");
        assert_eq!(c.get("ground"), "#F4F4F1");
        let saved: Colors = toml::from_str("ground = \"#000000\"").unwrap();
        assert_eq!((saved.ground.as_str(), saved.panel.as_str()), ("#000000", "#17181B"), "missing keys use defaults");
    }
}
