//! Colors, by what they're for. The default, "terminal", takes them from the terminal's own sixteen, so the board follows
//! whatever theme the terminal has; a named theme brings its own, and config.toml's [theme] can change any one of them.
//! A theme colors only text, symbols and the border: what's behind them, the popup's background and the bands, is the
//! terminal's or set in [theme] for itself, never the theme's, so a theme never sits on a background it wasn't made for.
use ratatui::style::Color;

/// Every color the board and the status bar use.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub needs_you: Color,
    pub working: Color,
    pub done: Color,
    pub idle: Color,
    pub failed: Color,
    pub success: Color,
    /// What's yours: marks and staged changes.
    pub accent: Color,
    /// Secondary text and the rules between parts.
    pub muted: Color,
    pub border: Color,
    pub claude: Color,
    pub codex: Color,
    pub opencode: Color,
    /// The quiet bands behind the row you're on, marked rows, and what you're typing into: no theme's, see above.
    pub cursor: Color,
    pub marked: Color,
    pub field: Color,
}

/// The names config.toml gives each role, for changing one.
pub const ROLES: [&str; 15] = [
    "needs_you",
    "working",
    "done",
    "idle",
    "failed",
    "success",
    "accent",
    "muted",
    "border",
    "claude",
    "codex",
    "opencode",
    "cursor",
    "marked",
    "field",
];

impl Palette {
    pub fn role_mut(&mut self, role: &str) -> Option<&mut Color> {
        Some(match role {
            "needs_you" => &mut self.needs_you,
            "working" => &mut self.working,
            "done" => &mut self.done,
            "idle" => &mut self.idle,
            "failed" => &mut self.failed,
            "success" => &mut self.success,
            "accent" => &mut self.accent,
            "muted" => &mut self.muted,
            "border" => &mut self.border,
            "claude" => &mut self.claude,
            "codex" => &mut self.codex,
            "opencode" => &mut self.opencode,
            "cursor" => &mut self.cursor,
            "marked" => &mut self.marked,
            "field" => &mut self.field,
            _ => return None,
        })
    }

    /// A CLI's own color, for the ones a theme knows.
    pub fn agent(&self, command: &str) -> Option<Color> {
        match command {
            "claude" => Some(self.claude),
            "codex" => Some(self.codex),
            "opencode" => Some(self.opencode),
            _ => None,
        }
    }
}

/// The terminal's own colors, bright where a dark theme's normal ones are too close to its background. A terminal theme
/// has no subtle colors, only its sixteen hues, so the bands are fixed: tuned for a dark terminal.
fn terminal() -> Palette {
    Palette {
        needs_you: Color::LightYellow,
        working: Color::Green,
        done: Color::LightBlue,
        idle: Color::DarkGray,
        failed: Color::LightRed,
        success: Color::LightGreen,
        accent: Color::LightBlue,
        muted: Color::DarkGray,
        border: Color::DarkGray,
        claude: Color::Indexed(173),
        codex: Color::Cyan,
        opencode: Color::Indexed(141),
        cursor: Color::Rgb(0x26, 0x2c, 0x38),
        marked: Color::Rgb(0x1f, 0x2c, 0x40),
        field: Color::Rgb(0x1a, 0x1f, 0x29),
    }
}

/// A named theme's colors, as hex: its comment grey, then its hues. All dark themes: their colors are made to be read on
/// a dark background, and the background is the terminal's.
struct Colors {
    muted: u32,
    red: u32,
    green: u32,
    yellow: u32,
    blue: u32,
    orange: u32,
    purple: u32,
    cyan: u32,
}

/// Each role from its theme's colors; the bands stay as they are.
fn from(c: &Colors) -> Palette {
    Palette {
        needs_you: rgb(c.yellow),
        working: rgb(c.green),
        done: rgb(c.blue),
        idle: rgb(c.muted),
        failed: rgb(c.red),
        success: rgb(c.green),
        accent: rgb(c.blue),
        muted: rgb(c.muted),
        border: rgb(c.muted),
        claude: rgb(c.orange),
        codex: rgb(c.cyan),
        opencode: rgb(c.purple),
        ..terminal()
    }
}

#[rustfmt::skip]
const THEMES: [(&str, Colors); 10] = [
    ("catppuccin-mocha", Colors { muted: 0x6c7086, red: 0xf38ba8, green: 0xa6e3a1, yellow: 0xf9e2af, blue: 0x89b4fa, orange: 0xfab387, purple: 0xcba6f7, cyan: 0x94e2d5 }),
    ("tokyo-night", Colors { muted: 0x565f89, red: 0xf7768e, green: 0x9ece6a, yellow: 0xe0af68, blue: 0x7aa2f7, orange: 0xff9e64, purple: 0xbb9af7, cyan: 0x7dcfff }),
    ("gruvbox-dark", Colors { muted: 0x928374, red: 0xfb4934, green: 0xb8bb26, yellow: 0xfabd2f, blue: 0x83a598, orange: 0xfe8019, purple: 0xd3869b, cyan: 0x8ec07c }),
    ("nord", Colors { muted: 0x616e88, red: 0xbf616a, green: 0xa3be8c, yellow: 0xebcb8b, blue: 0x81a1c1, orange: 0xd08770, purple: 0xb48ead, cyan: 0x88c0d0 }),
    // Dracula has no blue: its purple stands in, and its pink for purple.
    ("dracula", Colors { muted: 0x6272a4, red: 0xff5555, green: 0x50fa7b, yellow: 0xf1fa8c, blue: 0xbd93f9, orange: 0xffb86c, purple: 0xff79c6, cyan: 0x8be9fd }),
    // Rosé Pine has no green: pine stands in, and foam for blue.
    ("rose-pine", Colors { muted: 0x6e6a86, red: 0xeb6f92, green: 0x31748f, yellow: 0xf6c177, blue: 0x9ccfd8, orange: 0xebbcba, purple: 0xc4a7e7, cyan: 0x9ccfd8 }),
    ("solarized-dark", Colors { muted: 0x586e75, red: 0xdc322f, green: 0x859900, yellow: 0xb58900, blue: 0x268bd2, orange: 0xcb4b16, purple: 0x6c71c4, cyan: 0x2aa198 }),
    ("one-dark", Colors { muted: 0x5c6370, red: 0xe06c75, green: 0x98c379, yellow: 0xe5c07b, blue: 0x61afef, orange: 0xd19a66, purple: 0xc678dd, cyan: 0x56b6c2 }),
    ("kanagawa", Colors { muted: 0x727169, red: 0xe46876, green: 0x98bb6c, yellow: 0xe6c384, blue: 0x7e9cd8, orange: 0xffa066, purple: 0x957fb8, cyan: 0x7fb4ca }),
    ("everforest", Colors { muted: 0x859289, red: 0xe67e80, green: 0xa7c080, yellow: 0xdbbc7f, blue: 0x7fbbb3, orange: 0xe69875, purple: 0xd699b6, cyan: 0x83c092 }),
];

/// Every theme's name, for config.toml's warnings and template.
pub fn names() -> Vec<&'static str> {
    ["terminal"].into_iter().chain(THEMES.iter().map(|(name, _)| *name)).collect()
}

pub fn named(name: &str) -> Option<Palette> {
    match name {
        "terminal" => Some(terminal()),
        _ => THEMES.iter().find(|(n, _)| *n == name).map(|(_, colors)| from(colors)),
    }
}

impl Default for Palette {
    fn default() -> Self {
        terminal()
    }
}

fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const ANSI: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];

/// A color as config.toml writes it: "#89b4fa", one of the terminal's sixteen ("yellow", "brightblack"), or "colour173".
pub fn parse_color(name: &str) -> Option<Color> {
    let name = name.trim().to_lowercase();
    if let Some(hex) = name.strip_prefix('#').filter(|h| h.len() == 6) {
        return u32::from_str_radix(hex, 16).ok().map(rgb);
    }
    if let Some(i) = ANSI.iter().position(|n| *n == name) {
        return Some(SIXTEEN[i]);
    }
    if let Some(i) = name.strip_prefix("bright").and_then(|rest| ANSI.iter().position(|n| *n == rest)) {
        return Some(SIXTEEN[i + 8]);
    }
    let index: u8 = name.strip_prefix("colour").or_else(|| name.strip_prefix("color"))?.parse().ok()?;
    Some(SIXTEEN.get(index as usize).copied().unwrap_or(Color::Indexed(index)))
}

/// The terminal's sixteen, by index, as the palette names them: so "brightyellow" in config.toml is the same color as the
/// terminal theme's own.
const SIXTEEN: [Color; 16] = [
    Color::Black,
    Color::Red,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::Gray,
    Color::DarkGray,
    Color::LightRed,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightBlue,
    Color::LightMagenta,
    Color::LightCyan,
    Color::White,
];

/// A color as tmux writes it, for the status bar and the popup's styles.
pub fn tmux(color: Color) -> String {
    let index = match color {
        Color::Rgb(r, g, b) => return format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(n) if n >= 16 => return format!("colour{n}"),
        Color::Indexed(n) => n,
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Reset => return "default".into(),
    };
    let name = ANSI[(index % 8) as usize];
    if index >= 8 { format!("bright{name}") } else { name.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_theme_fills_every_role_and_leaves_the_bands_alone() {
        for name in names() {
            let mut palette = named(name).unwrap();
            for role in ROLES {
                assert!(palette.role_mut(role).is_some(), "{name}: {role}");
            }
            assert_eq!(
                (palette.cursor, palette.marked, palette.field),
                (terminal().cursor, terminal().marked, terminal().field),
                "{name}"
            );
        }
    }

    #[test]
    fn the_terminal_theme_is_the_terminals_own_colors() {
        let palette = named("terminal").unwrap();
        assert_eq!(
            (palette.needs_you, palette.working, palette.done),
            (Color::LightYellow, Color::Green, Color::LightBlue)
        );
        assert_eq!(tmux(palette.needs_you), "brightyellow");
        assert_eq!(tmux(palette.idle), "brightblack");
        assert_eq!(tmux(palette.claude), "colour173");
    }

    #[test]
    fn colors_read_and_write_the_way_tmux_does() {
        assert_eq!(parse_color("#89B4FA"), Some(Color::Rgb(0x89, 0xb4, 0xfa)));
        assert_eq!(parse_color("brightblack"), Some(Color::DarkGray));
        assert_eq!(parse_color("colour11"), Some(Color::LightYellow));
        assert_eq!(parse_color("colour173"), Some(Color::Indexed(173)));
        assert_eq!(parse_color("blu"), None);
        assert_eq!(tmux(Color::Rgb(0x1e, 0x1e, 0x2e)), "#1e1e2e");
        assert_eq!(tmux(Color::Indexed(4)), "blue");
        for name in ["yellow", "brightblack", "brightwhite", "colour173"] {
            assert_eq!(parse_color(name).map(tmux).as_deref(), Some(name), "round trip");
        }
    }
}
