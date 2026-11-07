//! Text helpers shared by the list, the preview and the footer: display widths, and tmux's ANSI output as styled lines.
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The colors in force: config.toml's theme.
pub fn palette() -> crate::theme::Palette {
    crate::config::get().theme.clone()
}

/// What stands out as yours: marks and staged changes.
pub fn marker() -> Color {
    palette().accent
}

/// Behind something you're typing into or choosing from: the folder dropdown, a name being renamed.
pub fn field() -> Color {
    palette().field
}
/// Where the cursor goes in a name or folder being typed in its row. Never shown: the terminal's own cursor stands there.
pub const CARET: char = '\u{E000}';

/// A cell filled with one of the terminal's own colors, its text in the terminal's background color: it follows the theme,
/// and stays solid on a transparent one, where plain colored text doesn't stand out. Bright, because a theme's normal
/// colors can be too close to its background to read this way.
pub fn filled(color: Color) -> Style {
    bold().fg(color).add_modifier(Modifier::REVERSED)
}

pub fn dim() -> Style {
    Style::new().fg(palette().muted)
}

pub fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

pub fn cells(s: &str) -> usize {
    s.width()
}

/// `s` padded to `w` columns; with `cut`, also cut to them, so a long name never moves the columns after it.
pub fn fit(s: &str, w: usize, cut: bool) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let width = c.width().unwrap_or(0);
        if cut && used + width > w {
            break;
        }
        used += width;
        out.push(c);
    }
    out.push_str(&" ".repeat(w.saturating_sub(used)));
    out
}

/// The last `w` columns of `s`.
pub fn tail(s: &str, w: usize) -> String {
    let mut used = 0;
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        used += c.width().unwrap_or(0);
        if used > w {
            break;
        }
        start = i;
    }
    s[start..].to_string()
}

/// `claude` as `Claude`.
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Wraps at spaces where it can, like `fold -s`.
pub fn fold(s: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for raw in s.lines() {
        let mut rest = raw.replace('\t', "  ");
        loop {
            if cells(&rest) <= width || width == 0 {
                lines.push(rest);
                break;
            }
            let mut used = 0;
            let mut cut = 0;
            let mut space = None;
            for (i, c) in rest.char_indices() {
                used += c.width().unwrap_or(0);
                if used > width {
                    break;
                }
                cut = i + c.len_utf8();
                if c == ' ' {
                    space = Some(cut);
                }
            }
            // A character wider than the line goes on one of its own, whole: never cut inside it.
            let at = match space.unwrap_or(cut) {
                0 => rest.chars().next().map_or(1, char::len_utf8),
                at => at,
            };
            lines.push(rest[..at].to_string());
            rest = rest[at..].to_string();
            if rest.is_empty() {
                break;
            }
        }
    }
    lines
}

/// An SGR sequence's codes, one per number: tmux also writes the colon form, where "4:3" is a curly underline (4 for
/// this) and "38:2::r:g:b" a colour (38;2;r;g;b here).
fn sgr_codes(params: &str) -> Vec<String> {
    let mut codes = Vec::new();
    for part in params.split(';') {
        match part.split_once(':') {
            Some(("38" | "48" | "58", _)) => {
                codes.extend(part.split(':').filter(|n| !n.is_empty()).map(str::to_string));
            }
            Some((head, _)) => codes.push(head.to_string()),
            None => codes.push(part.to_string()),
        }
    }
    codes
}

/// Without its OSC sequences (ESC ] ... BEL or ESC \\): links tmux passes on from what a pane shows, which aren't text.
pub fn without_osc(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains("\x1b]") {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find("\x1b]") {
        out.push_str(&rest[..at]);
        let body = &rest[at + 2..];
        let end = [body.find('\x07').map(|e| e + 1), body.find("\x1b\\").map(|e| e + 2)]
            .into_iter()
            .flatten()
            .min();
        rest = end.map_or("", |end| &body[end..]);
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

/// One SGR sequence's parameters applied to a style.
pub fn apply_sgr(style: &mut Style, params: &str) {
    let codes: Vec<u16> = sgr_codes(params).iter().map(|c| c.parse().unwrap_or(0)).collect();
    let mut i = 0;
    while i < codes.len() {
        let code = codes[i];
        match code {
            0 => *style = Style::default(),
            1 => *style = style.add_modifier(Modifier::BOLD),
            2 => *style = style.add_modifier(Modifier::DIM),
            3 => *style = style.add_modifier(Modifier::ITALIC),
            4 => *style = style.add_modifier(Modifier::UNDERLINED),
            7 => *style = style.add_modifier(Modifier::REVERSED),
            9 => *style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => *style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => *style = style.remove_modifier(Modifier::ITALIC),
            24 => *style = style.remove_modifier(Modifier::UNDERLINED),
            27 => *style = style.remove_modifier(Modifier::REVERSED),
            29 => *style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style.fg = Some(Color::Indexed((code - 30) as u8)),
            39 => style.fg = None,
            40..=47 => style.bg = Some(Color::Indexed((code - 40) as u8)),
            49 => style.bg = None,
            90..=97 => style.fg = Some(Color::Indexed((code - 90 + 8) as u8)),
            100..=107 => style.bg = Some(Color::Indexed((code - 100 + 8) as u8)),
            38 | 48 | 58 => {
                let color = match codes.get(i + 1) {
                    Some(5) => {
                        let c = codes.get(i + 2).map(|&n| Color::Indexed(n as u8));
                        i += 2;
                        c
                    }
                    Some(2) => {
                        let c = match (codes.get(i + 2), codes.get(i + 3), codes.get(i + 4)) {
                            (Some(&r), Some(&g), Some(&b)) => Some(Color::Rgb(r as u8, g as u8, b as u8)),
                            _ => None,
                        };
                        i += 4;
                        c
                    }
                    _ => None,
                };
                match code {
                    38 => style.fg = color,
                    48 => style.bg = color,
                    _ => {}
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// One line of `capture-pane -e` as a styled line. `style` carries over: tmux only prints what changed.
pub fn ansi_line(s: &str, style: &mut Style) -> Line<'static> {
    let mut spans = Vec::new();
    let mut text = String::new();
    let s = without_osc(s);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            text.push(c);
            continue;
        }
        if chars.peek() != Some(&'[') {
            chars.next();
            continue;
        }
        chars.next();
        let mut params = String::new();
        let mut last = None;
        for c in chars.by_ref() {
            if c.is_ascii_alphabetic() || c == '~' {
                last = Some(c);
                break;
            }
            params.push(c);
        }
        if last == Some('m') {
            if !text.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut text), *style));
            }
            apply_sgr(style, &params);
        }
    }
    if !text.is_empty() {
        spans.push(Span::styled(text, *style));
    }
    Line::from(spans)
}

/// Without its SGR sequences.
pub fn strip_ansi(s: &str) -> String {
    let mut style = Style::default();
    ansi_line(s, &mut style).spans.iter().map(|span| span.content.as_ref()).collect()
}

/// Runs of empty lines as one: an agent in a tall pane draws its answer at the top and its prompt at the bottom,
/// with a gap taller than the preview, which shows the bottom.
pub fn squeeze(screen: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut blank = false;
    for line in screen.lines() {
        let empty = strip_ansi(line).trim().is_empty();
        if empty && blank {
            continue;
        }
        blank = empty;
        out.push(line.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_counts_columns_not_bytes() {
        assert_eq!(fit("héllo", 7, true), "héllo  ");
        assert_eq!(fit("→ Codex long", 7, true), "→ Codex");
        assert_eq!(cells(&fit("日本語", 5, true)), 5);
    }

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ab", 3), "ab");
    }

    #[test]
    fn fold_breaks_at_spaces() {
        assert_eq!(fold("one two three", 8), ["one two ", "three"]);
        assert_eq!(fold("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn ansi_styles_carry_over_lines() {
        let mut style = Style::default();
        let line = ansi_line("a\x1b[1;38;5;173mb", &mut style);
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[1].style.fg, Some(Color::Indexed(173)));
        let next = ansi_line("c\x1b[0md", &mut style);
        assert_eq!(next.spans[0].style.fg, Some(Color::Indexed(173)));
        assert_eq!(next.spans[1].style.fg, None);
    }

    #[test]
    fn squeeze_keeps_one_empty_line() {
        assert_eq!(squeeze("a\n\n \n\x1b[0m\nb\n\nc"), ["a", "", "b", "", "c"]);
    }

    #[test]
    fn a_character_wider_than_the_line_gets_one_of_its_own() {
        assert_eq!(fold("日本", 1), ["日", "本"]);
        assert_eq!(fold("a日", 1), ["a", "日"]);
    }

    #[test]
    fn links_and_colon_codes_from_a_capture_dont_leak_into_the_text() {
        let mut style = Style::default();
        let line = ansi_line("see \x1b]8;;https://example.com\x1b\\the link\x1b]8;;\x1b\\ done", &mut style);
        let text: String = line.spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, "see the link done");
        let mut style = Style::default();
        let line = ansi_line("\x1b[31m\x1b[4:3mred, curly", &mut style);
        assert_eq!(line.spans[0].style.fg, Some(Color::Indexed(1)), "the curly underline keeps the colour");
        assert_eq!(sgr_codes("38:2::255:0:0;1"), ["38", "2", "255", "0", "0", "1"]);
        assert_eq!(without_osc("a\x1b]0;title\x07b"), "ab");
    }
}
