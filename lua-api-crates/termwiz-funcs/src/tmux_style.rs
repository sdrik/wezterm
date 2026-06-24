//! Convert a tmux style string (containing `#[...]` style directives, as found
//! in tmux `status-left`/`status-right` and in the value of a `#{T:...}` format
//! subscription) into a list of wezterm [`FormatItem`]s, so the styled content
//! can be rendered in the wezterm status bar via `wezterm.format`.
//!
//! The parser is stateful: it tracks the running style plus a "default" style
//! (what `#[default]` reverts to) and a `push-default`/`pop-default` stack,
//! mirroring tmux's own style model. Format items are derived from the current
//! style at each text boundary. Parsing is tolerant: unknown or layout-only
//! keywords (e.g. `align=`, `range=`) are skipped rather than producing errors.

use crate::{FormatColor, FormatItem};
use std::str::FromStr;
use termwiz::cell::{AttributeChange, Blink, Intensity, Underline};
use termwiz::color::{AnsiColor, SrgbaTuple};

/// The full set of attributes a tmux `#[...]` run can establish. The
/// `base()` value is the default style: no attribute and no color override.
#[derive(Debug, Clone, PartialEq)]
struct Style {
    fg: Option<FormatColor>,
    bg: Option<FormatColor>,
    intensity: Intensity,
    italic: bool,
    underline: Underline,
    blink: Blink,
    reverse: bool,
    invisible: bool,
    strikethrough: bool,
}

impl Style {
    fn base() -> Self {
        Self {
            fg: None,
            bg: None,
            intensity: Intensity::Normal,
            italic: false,
            underline: Underline::None,
            blink: Blink::None,
            reverse: false,
            invisible: false,
            strikethrough: false,
        }
    }

    /// tmux `#[none]`: turn off all attributes but keep the current colors.
    fn clear_attributes(&mut self) {
        let fg = self.fg.take();
        let bg = self.bg.take();
        *self = Self::base();
        self.fg = fg;
        self.bg = bg;
    }

    /// Append the format items that establish this style, assuming the output is
    /// preceded by a `ResetAttributes` (so only the non-default fields matter).
    fn emit_into(&self, out: &mut Vec<FormatItem>) {
        if let Some(fg) = &self.fg {
            out.push(FormatItem::Foreground(fg.clone()));
        }
        if let Some(bg) = &self.bg {
            out.push(FormatItem::Background(bg.clone()));
        }
        if self.intensity != Intensity::Normal {
            out.push(FormatItem::Attribute(AttributeChange::Intensity(
                self.intensity,
            )));
        }
        if self.italic {
            out.push(FormatItem::Attribute(AttributeChange::Italic(true)));
        }
        if self.underline != Underline::None {
            out.push(FormatItem::Attribute(AttributeChange::Underline(
                self.underline,
            )));
        }
        if self.blink != Blink::None {
            out.push(FormatItem::Attribute(AttributeChange::Blink(self.blink)));
        }
        if self.reverse {
            out.push(FormatItem::Attribute(AttributeChange::Reverse(true)));
        }
        if self.invisible {
            out.push(FormatItem::Attribute(AttributeChange::Invisible(true)));
        }
        if self.strikethrough {
            out.push(FormatItem::Attribute(AttributeChange::StrikeThrough(true)));
        }
    }
}

/// A recognized, settable text attribute (the subset of tmux attributes that
/// has a wezterm format-item equivalent).
enum Attr {
    Bold,
    Dim,
    Italics,
    Reverse,
    Blink,
    Hidden,
    Strikethrough,
    Underscore(Underline),
}

/// Parse a tmux attribute name (without any `no` prefix). Returns `None` for
/// attributes that have no wezterm equivalent (`acs`, `overline`).
fn parse_attr(name: &str) -> Option<Attr> {
    Some(match name.to_ascii_lowercase().as_str() {
        "bold" | "bright" => Attr::Bold,
        "dim" => Attr::Dim,
        "italics" => Attr::Italics,
        "reverse" => Attr::Reverse,
        "blink" => Attr::Blink,
        "hidden" => Attr::Hidden,
        "strikethrough" => Attr::Strikethrough,
        "underscore" => Attr::Underscore(Underline::Single),
        "double-underscore" => Attr::Underscore(Underline::Double),
        "curly-underscore" => Attr::Underscore(Underline::Curly),
        "dotted-underscore" => Attr::Underscore(Underline::Dotted),
        "dashed-underscore" => Attr::Underscore(Underline::Dashed),
        _ => return None,
    })
}

/// tmux keywords that wezterm recognizes but cannot represent as an inline
/// format item (colors aside): status-line layout and interactivity. These are
/// skipped. Note `none`/`default`/`push-default`/... are handled before this.
fn is_ignored_keyword(kw: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "us=", "align=", "fill=", "width=", "pad=", "list=", "range=",
    ];
    if PREFIXES.iter().any(|p| {
        kw.get(..p.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(p))
    }) {
        return true;
    }
    const STANDALONE: &[&str] = &[
        "acs", "overline", "ignore", "noignore", "noattr", "nolist", "norange", "noalign",
    ];
    STANDALONE.iter().any(|s| kw.eq_ignore_ascii_case(s))
}

/// Case-insensitive prefix strip that respects char boundaries.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Map a tmux color spec (`red`, `colour46`, `#ff0000`, `default`, ...) to a
/// [`FormatColor`]. Returns `None` if it can't be parsed (caller ignores it).
fn tmux_color_to_format_color(s: &str) -> Option<FormatColor> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("default") || s.eq_ignore_ascii_case("terminal") {
        return Some(FormatColor::Default);
    }

    // colourN / colorN, or a bare palette index.
    let lower = s.to_ascii_lowercase();
    let index = lower
        .strip_prefix("colour")
        .or_else(|| lower.strip_prefix("color"))
        .or(Some(lower.as_str()))
        .and_then(|digits| digits.parse::<u16>().ok());
    if let Some(n) = index {
        return (n <= 255)
            .then(|| palette_index_to_format_color(n as u8))
            .flatten();
    }

    // Named ANSI colors map to palette indices so they follow the user's theme.
    let ansi_index = match lower.as_str() {
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" => 5,
        "cyan" => 6,
        "white" => 7,
        "brightblack" => 8,
        "brightred" => 9,
        "brightgreen" => 10,
        "brightyellow" => 11,
        "brightblue" => 12,
        "brightmagenta" => 13,
        "brightcyan" => 14,
        "brightwhite" => 15,
        _ => {
            // #rrggbb, rgb:.., or any X11 name that SrgbaTuple understands.
            return SrgbaTuple::from_str(s)
                .ok()
                .map(|_| FormatColor::Color(s.to_string()));
        }
    };
    ansi_color_from_u8(ansi_index).map(FormatColor::AnsiColor)
}

fn palette_index_to_format_color(n: u8) -> Option<FormatColor> {
    if n < 16 {
        ansi_color_from_u8(n).map(FormatColor::AnsiColor)
    } else {
        Some(FormatColor::Color(palette_index_to_hex(n)))
    }
}

/// Palette indices 0-15 map to the corresponding [`AnsiColor`] so they render
/// with the user's theme colors.
fn ansi_color_from_u8(n: u8) -> Option<AnsiColor> {
    use AnsiColor::*;
    Some(match n {
        0 => Black,
        1 => Maroon,
        2 => Green,
        3 => Olive,
        4 => Navy,
        5 => Purple,
        6 => Teal,
        7 => Silver,
        8 => Grey,
        9 => Red,
        10 => Lime,
        11 => Yellow,
        12 => Blue,
        13 => Fuchsia,
        14 => Aqua,
        15 => White,
        _ => return None,
    })
}

/// Convert palette indices 16-255 to an `#rrggbb` string using the standard
/// xterm-256 layout (which matches wezterm's default palette): a 6×6×6 color
/// cube (16-231) followed by a 24-step grayscale ramp (232-255).
fn palette_index_to_hex(n: u8) -> String {
    const RAMP: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
    let (r, g, b) = if n < 232 {
        let i = n - 16;
        (
            RAMP[(i / 36) as usize],
            RAMP[((i / 6) % 6) as usize],
            RAMP[(i % 6) as usize],
        )
    } else {
        let v = 8 + 10 * (n - 232);
        (v, v, v)
    };
    format!("#{r:02x}{g:02x}{b:02x}")
}

struct Parser {
    items: Vec<FormatItem>,
    text: String,
    current: Style,
    default: Style,
    default_stack: Vec<Style>,
    last_emitted: Style,
}

impl Parser {
    fn new() -> Self {
        Self {
            items: vec![],
            text: String::new(),
            current: Style::base(),
            default: Style::base(),
            default_stack: vec![],
            last_emitted: Style::base(),
        }
    }

    /// Emit the buffered text, prefixed by the style transition if the current
    /// style differs from what was last written.
    fn flush_text(&mut self) {
        if self.text.is_empty() {
            return;
        }
        if self.current != self.last_emitted {
            self.items.push(FormatItem::ResetAttributes);
            self.current.emit_into(&mut self.items);
            self.last_emitted = self.current.clone();
        }
        self.items
            .push(FormatItem::Text(std::mem::take(&mut self.text)));
    }

    fn set_attr(&mut self, attr: Attr, on: bool) {
        match attr {
            Attr::Bold => {
                self.current.intensity = if on {
                    Intensity::Bold
                } else {
                    Intensity::Normal
                }
            }
            Attr::Dim => {
                self.current.intensity = if on {
                    Intensity::Half
                } else {
                    Intensity::Normal
                }
            }
            Attr::Italics => self.current.italic = on,
            Attr::Reverse => self.current.reverse = on,
            Attr::Blink => self.current.blink = if on { Blink::Slow } else { Blink::None },
            Attr::Hidden => self.current.invisible = on,
            Attr::Strikethrough => self.current.strikethrough = on,
            Attr::Underscore(u) => self.current.underline = if on { u } else { Underline::None },
        }
    }

    fn apply_keyword(&mut self, kw: &str) {
        // Standalone state keywords.
        if kw.eq_ignore_ascii_case("default") {
            self.current = self.default.clone();
            return;
        }
        if kw.eq_ignore_ascii_case("none") {
            self.current.clear_attributes();
            return;
        }
        if kw.eq_ignore_ascii_case("push-default") {
            self.default_stack.push(self.default.clone());
            self.default = self.current.clone();
            return;
        }
        if kw.eq_ignore_ascii_case("pop-default") {
            self.default = self.default_stack.pop().unwrap_or_else(Style::base);
            return;
        }
        if kw.eq_ignore_ascii_case("set-default") {
            self.default = self.current.clone();
            return;
        }

        // Colors. A malformed color leaves the previous one untouched.
        if let Some(rest) = strip_prefix_ci(kw, "fg=") {
            if let Some(c) = tmux_color_to_format_color(rest) {
                self.current.fg = Some(c);
            }
            return;
        }
        if let Some(rest) = strip_prefix_ci(kw, "bg=") {
            if let Some(c) = tmux_color_to_format_color(rest) {
                self.current.bg = Some(c);
            }
            return;
        }

        // Layout/interactivity keywords with no inline equivalent (this also
        // catches the `no...` layout resets before the generic negation below).
        if is_ignored_keyword(kw) {
            return;
        }

        // Attribute negation: strip `no`, clear the attribute.
        if let Some(name) = strip_prefix_ci(kw, "no") {
            if let Some(attr) = parse_attr(name) {
                self.set_attr(attr, false);
            }
            return;
        }

        // Plain attribute: set it. Unknown keywords are ignored.
        if let Some(attr) = parse_attr(kw) {
            self.set_attr(attr, true);
        }
    }

    fn apply_spec(&mut self, spec: &str) {
        for kw in spec.split(|c: char| c == ',' || c.is_whitespace()) {
            let kw = kw.trim();
            if !kw.is_empty() {
                self.apply_keyword(kw);
            }
        }
    }

    fn parse(mut self, input: &str) -> Vec<FormatItem> {
        let chars: Vec<char> = input.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '#' && i + 1 < chars.len() {
                match chars[i + 1] {
                    '[' => {
                        // A style spec applies to the text that follows it, so
                        // flush what we have under the previous style first.
                        self.flush_text();
                        i += 2;
                        let start = i;
                        while i < chars.len() && chars[i] != ']' {
                            i += 1;
                        }
                        let spec: String = chars[start..i].iter().collect();
                        if i < chars.len() {
                            i += 1; // consume the ']'
                        }
                        self.apply_spec(&spec);
                        continue;
                    }
                    '#' => {
                        self.text.push('#');
                        i += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            self.text.push(chars[i]);
            i += 1;
        }
        self.flush_text();
        self.items
    }
}

/// Parse a tmux style string into wezterm format items.
pub fn tmux_style_to_format_items(input: &str) -> Vec<FormatItem> {
    Parser::new().parse(input)
}

#[cfg(test)]
mod tests {
    use super::tmux_style_to_format_items;
    use crate::{FormatColor, FormatItem};
    use termwiz::cell::{AttributeChange, Intensity};
    use termwiz::color::AnsiColor;

    fn fg(c: FormatColor) -> FormatItem {
        FormatItem::Foreground(c)
    }
    fn text(s: &str) -> FormatItem {
        FormatItem::Text(s.to_string())
    }

    #[test]
    fn plain_text_has_no_style_items() {
        assert_eq!(tmux_style_to_format_items("hello"), vec![text("hello")]);
    }

    #[test]
    fn named_fg_uses_ansi_palette() {
        // tmux "red" is palette index 1, which wezterm names Maroon.
        assert_eq!(
            tmux_style_to_format_items("#[fg=red]foo"),
            vec![
                FormatItem::ResetAttributes,
                fg(FormatColor::AnsiColor(AnsiColor::Maroon)),
                text("foo"),
            ]
        );
    }

    #[test]
    fn indexed_color_becomes_truecolor() {
        assert_eq!(
            tmux_style_to_format_items("#[fg=colour46]x"),
            vec![
                FormatItem::ResetAttributes,
                fg(FormatColor::Color("#00ff00".to_string())),
                text("x"),
            ]
        );
    }

    #[test]
    fn attribute_removal_is_handled() {
        assert_eq!(
            tmux_style_to_format_items("#[bold]a#[nobold]b"),
            vec![
                FormatItem::ResetAttributes,
                FormatItem::Attribute(AttributeChange::Intensity(Intensity::Bold)),
                text("a"),
                FormatItem::ResetAttributes,
                text("b"),
            ]
        );
    }

    #[test]
    fn doubled_hash_is_literal() {
        assert_eq!(tmux_style_to_format_items("a##b"), vec![text("a#b")]);
    }

    #[test]
    fn multiple_keywords_in_one_spec() {
        assert_eq!(
            tmux_style_to_format_items("#[fg=blue,bg=black,bold]z"),
            vec![
                FormatItem::ResetAttributes,
                fg(FormatColor::AnsiColor(AnsiColor::Navy)),
                FormatItem::Background(FormatColor::AnsiColor(AnsiColor::Black)),
                FormatItem::Attribute(AttributeChange::Intensity(Intensity::Bold)),
                text("z"),
            ]
        );
    }

    #[test]
    fn push_and_pop_default() {
        // A and C take the pushed default (red); B is blue; D is base after pop.
        let red = || fg(FormatColor::AnsiColor(AnsiColor::Maroon));
        let blue = || fg(FormatColor::AnsiColor(AnsiColor::Navy));
        assert_eq!(
            tmux_style_to_format_items(
                "#[fg=red]#[push-default]A#[fg=blue]B#[default]C#[pop-default]#[default]D"
            ),
            vec![
                FormatItem::ResetAttributes,
                red(),
                text("A"),
                FormatItem::ResetAttributes,
                blue(),
                text("B"),
                FormatItem::ResetAttributes,
                red(),
                text("C"),
                FormatItem::ResetAttributes,
                text("D"),
            ]
        );
    }

    #[test]
    fn layout_keywords_are_ignored() {
        assert_eq!(
            tmux_style_to_format_items("#[align=right]x"),
            vec![text("x")]
        );
    }
}
