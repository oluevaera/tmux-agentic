//! The board's keys: what each action is bound to, from config.toml's [board.keys] over the defaults. The footer, the
//! help and the messages that name a key all ask here, so they show the keys you have.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Down,
    Up,
    First,
    Last,
    PrevTab,
    NextTab,
    Go,
    Mark,
    SwitchAgent,
    Detach,
    Rename,
    Kill,
    Compact,
    New,
    Folder,
    Search,
    Layout,
    Help,
    Close,
    /// A tab by its place in the bar, 1 the first; 0 is History.
    Tab(u8),
}

/// Every action: its name in config.toml and its default keys, in tmux's names.
pub const ACTIONS: [(Action, &str, &[&str]); 29] = [
    (Action::Down, "down", &["j", "Down"]),
    (Action::Up, "up", &["k", "Up"]),
    (Action::First, "first", &["g", "Home"]),
    (Action::Last, "last", &["G", "End"]),
    (Action::PrevTab, "prev_tab", &["h", "Left"]),
    (Action::NextTab, "next_tab", &["l", "Right"]),
    (Action::Go, "go", &["Enter"]),
    (Action::Mark, "mark", &["Space"]),
    (Action::SwitchAgent, "switch_agent", &["Tab"]),
    (Action::Detach, "detach", &["BTab"]),
    (Action::Rename, "rename", &["r"]),
    (Action::Kill, "kill", &["x"]),
    (Action::Compact, "compact", &["c"]),
    (Action::New, "new", &["n"]),
    (Action::Folder, "folder", &["o"]),
    (Action::Search, "search", &["/"]),
    (Action::Layout, "layout", &["f"]),
    (Action::Help, "help", &["?"]),
    (Action::Close, "close", &["q"]),
    (Action::Tab(1), "tab_1", &["1"]),
    (Action::Tab(2), "tab_2", &["2"]),
    (Action::Tab(3), "tab_3", &["3"]),
    (Action::Tab(4), "tab_4", &["4"]),
    (Action::Tab(5), "tab_5", &["5"]),
    (Action::Tab(6), "tab_6", &["6"]),
    (Action::Tab(7), "tab_7", &["7"]),
    (Action::Tab(8), "tab_8", &["8"]),
    (Action::Tab(9), "tab_9", &["9"]),
    (Action::Tab(0), "history", &["0"]),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Keymap {
    keys: Vec<(Action, Vec<KeyEvent>)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap {
            keys: ACTIONS
                .iter()
                .map(|(action, _, keys)| (*action, keys.iter().filter_map(|k| parse_key(k)).collect()))
                .collect(),
        }
    }
}

impl Keymap {
    /// What `key` does, if anything.
    pub fn action(&self, key: &KeyEvent) -> Option<Action> {
        self.keys
            .iter()
            .find(|(_, keys)| keys.iter().any(|k| same_key(k, key)))
            .map(|(action, _)| *action)
    }

    pub fn keys(&self, action: Action) -> &[KeyEvent] {
        self.keys.iter().find(|(a, _)| *a == action).map_or(&[], |(_, keys)| keys)
    }

    /// The key a hint shows and a click on it presses: the action's first.
    pub fn first(&self, action: Action) -> Option<KeyEvent> {
        self.keys(action).first().copied()
    }

    /// The action's first key as the footer shows it: "x", "↵", "⇥". Empty when it has none.
    pub fn label(&self, action: Action) -> String {
        self.first(action).map(|key| label(&key)).unwrap_or_default()
    }

    /// Two actions side by side, key by key, as the help shows them: "j k   ↓ ↑".
    pub fn pair(&self, a: Action, b: Action) -> String {
        let (a, b) = (self.keys(a), self.keys(b));
        (0..a.len().max(b.len()))
            .map(|i| {
                let shown: Vec<String> = [a.get(i), b.get(i)].into_iter().flatten().map(label).collect();
                shown.join(" ")
            })
            .collect::<Vec<_>>()
            .join("   ")
    }

    /// Takes `key` off every action: one the board can never see.
    pub fn drop_key(&mut self, key: &KeyEvent) {
        for (_, keys) in &mut self.keys {
            keys.retain(|k| !same_key(k, key));
        }
    }

    /// Binds `action` to `keys` instead of its own. A key another action has by default moves here; a key that another
    /// setting already took, or one the board keeps for itself, says so and stays where it was.
    pub fn bind(&mut self, action: Action, keys: Vec<KeyEvent>, taken: &[(KeyEvent, Action)]) -> Result<(), String> {
        for key in &keys {
            if let Some(why) = reserved(key) {
                return Err(format!("can't have {}: it's kept for {why}", label(key)));
            }
            if let Some((_, other)) = taken.iter().find(|(k, a)| same_key(k, key) && *a != action) {
                return Err(format!("can't have {}: {} has it already", label(key), name(*other)));
            }
        }
        for (other, theirs) in &mut self.keys {
            if *other != action {
                theirs.retain(|k| !keys.iter().any(|key| same_key(k, key)));
            }
        }
        if let Some((_, mine)) = self.keys.iter_mut().find(|(a, _)| *a == action) {
            *mine = keys;
        }
        Ok(())
    }
}

pub fn name(action: Action) -> &'static str {
    ACTIONS.iter().find(|(a, _, _)| *a == action).map_or("", |(_, name, _)| name)
}

pub fn by_name(name: &str) -> Option<Action> {
    ACTIONS.iter().find(|(_, n, _)| *n == name).map(|(action, _, _)| *action)
}

/// Keys no setting can take, and why: the way back out, and a sure way to close.
fn reserved(key: &KeyEvent) -> Option<&'static str> {
    match key.code {
        KeyCode::Esc => Some("going back"),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some("closing"),
        _ => None,
    }
}

/// A key as the footer and help show it.
pub fn label(key: &KeyEvent) -> String {
    let base = match key.code {
        KeyCode::Enter => "↵".to_string(),
        KeyCode::Tab => "⇥".to_string(),
        KeyCode::BackTab => "⇧⇥".to_string(),
        KeyCode::Char(' ') => "␣".to_string(),
        KeyCode::Up => "↑".to_string(),
        KeyCode::Down => "↓".to_string(),
        KeyCode::Left => "←".to_string(),
        KeyCode::Right => "→".to_string(),
        KeyCode::Backspace => "⌫".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pgup".to_string(),
        KeyCode::PageDown => "pgdn".to_string(),
        KeyCode::Delete => "del".to_string(),
        KeyCode::Insert => "ins".to_string(),
        KeyCode::F(n) => format!("F{n}"),
        KeyCode::Char(c) => c.to_string(),
        _ => "?".to_string(),
    };
    let mut shown = String::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        shown.push_str("C-");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        shown.push_str("M-");
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) && !matches!(key.code, KeyCode::Char(_) | KeyCode::BackTab) {
        shown.push_str("S-");
    }
    shown + &base
}

/// tmux's name for a key as a key event: `M-a`, `C-g`, `C-M-x`, `F5`, `M-Up`, `BTab`. None for "none".
pub fn parse_key(name: &str) -> Option<KeyEvent> {
    if name == "none" {
        return None;
    }
    let mut rest = name;
    let mut mods = KeyModifiers::NONE;
    loop {
        if let Some(r) = rest.strip_prefix("C-") {
            mods |= KeyModifiers::CONTROL;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("M-") {
            mods |= KeyModifiers::ALT;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("S-") {
            mods |= KeyModifiers::SHIFT;
            rest = r;
        } else {
            break;
        }
    }
    let lower = rest.to_lowercase();
    let code = match lower.as_str() {
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "enter" => KeyCode::Enter,
        "tab" if mods.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
        "tab" => KeyCode::Tab,
        "btab" => KeyCode::BackTab,
        "space" => KeyCode::Char(' '),
        "escape" | "esc" => KeyCode::Esc,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "ppage" | "pageup" | "pgup" => KeyCode::PageUp,
        "npage" | "pagedown" | "pgdn" => KeyCode::PageDown,
        "dc" | "delete" => KeyCode::Delete,
        "ic" | "insert" => KeyCode::Insert,
        "bspace" => KeyCode::Backspace,
        _ => {
            // tmux has F1 to F12.
            if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse().ok()).filter(|n| (1..=12).contains(n)) {
                KeyCode::F(n)
            } else {
                let mut chars = rest.chars();
                let c = chars.next()?;
                // tmux takes S- only on keys that aren't characters: a shifted letter is the capital.
                if chars.next().is_some() || mods.contains(KeyModifiers::SHIFT) {
                    return None;
                }
                KeyCode::Char(c)
            }
        }
    };
    Some(KeyEvent::new(code, mods))
}

/// The same key, whatever the terminal says about Shift: G comes as a capital, with or without it.
pub fn same_key(a: &KeyEvent, b: &KeyEvent) -> bool {
    // Shift is in a character itself (G), and Shift+Tab always comes with it; on any other key it makes another key.
    let shifted = !matches!(a.code, KeyCode::Char(_) | KeyCode::BackTab);
    let significant = KeyModifiers::CONTROL | KeyModifiers::ALT | if shifted { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
    a.code == b.code && a.modifiers & significant == b.modifiers & significant
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str]) -> Vec<KeyEvent> {
        names.iter().filter_map(|k| parse_key(k)).collect()
    }

    #[test]
    fn key_names() {
        assert_eq!(parse_key("M-a"), Some(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT)));
        assert_eq!(
            parse_key("C-M-x"),
            Some(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT | KeyModifiers::CONTROL))
        );
        assert_eq!(parse_key("F5"), Some(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)));
        assert_eq!(parse_key("M-Up"), Some(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)));
        assert_eq!(parse_key("BTab").map(|k| k.code), Some(KeyCode::BackTab));
        assert_eq!(parse_key("S-Tab").map(|k| k.code), Some(KeyCode::BackTab));
        assert_eq!(parse_key("none"), None);
        assert_eq!(parse_key("NoSuchKey"), None);
    }

    #[test]
    fn shift_makes_another_key_except_on_characters_and_shift_tab() {
        let up = parse_key("Up").unwrap();
        let shift_up = parse_key("S-Up").unwrap();
        assert!(!same_key(&up, &shift_up), "S-Up isn't Up");
        assert!(same_key(
            &parse_key("G").unwrap(),
            &KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT)
        ));
        assert!(same_key(
            &parse_key("BTab").unwrap(),
            &KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)
        ));
        assert_eq!(label(&shift_up), "S-↑");
        let mut map = Keymap::default();
        map.bind(Action::First, vec![parse_key("g").unwrap(), shift_up], &[]).unwrap();
        assert_eq!(map.action(&up), Some(Action::Up), "plain Up stays with up");
    }

    #[test]
    fn key_names_tmux_has_no_key_for_are_refused() {
        assert_eq!(parse_key("F0"), None);
        assert_eq!(parse_key("F13"), None);
        assert_eq!(parse_key("S-a"), None);
        assert!(parse_key("F12").is_some() && parse_key("S-F5").is_some() && parse_key("S-Left").is_some());
    }

    #[test]
    fn every_action_has_its_default_keys_and_they_show_as_before() {
        let map = Keymap::default();
        assert_eq!(map.action(&KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT)), Some(Action::Last));
        assert_eq!(map.action(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)), Some(Action::Detach));
        assert_eq!(map.action(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)), None, "C-x isn't x");
        assert_eq!(map.label(Action::Mark), "␣");
        assert_eq!(map.pair(Action::Down, Action::Up), "j k   ↓ ↑");
        assert!(ACTIONS.iter().all(|(action, _, _)| !map.keys(*action).is_empty()));
    }

    #[test]
    fn a_key_moves_to_the_action_it_is_given_to() {
        let mut map = Keymap::default();
        map.bind(Action::Search, keys(&["j", "C-f"]), &[]).unwrap();
        assert_eq!(map.action(&parse_key("j").unwrap()), Some(Action::Search));
        assert_eq!(map.action(&parse_key("C-f").unwrap()), Some(Action::Search));
        assert_eq!(map.action(&parse_key("/").unwrap()), None, "its old key goes");
        assert_eq!(map.keys(Action::Down), keys(&["Down"]).as_slice(), "down keeps the rest");
    }

    #[test]
    fn a_key_two_settings_want_or_the_board_keeps_stays_put() {
        let mut map = Keymap::default();
        let taken = [(parse_key("z").unwrap(), Action::Kill)];
        assert!(
            map.bind(Action::Rename, keys(&["z"]), &taken)
                .unwrap_err()
                .contains("kill has it already")
        );
        assert!(map.bind(Action::Rename, keys(&["C-c"]), &[]).unwrap_err().contains("kept for closing"));
        assert!(map.bind(Action::Close, keys(&["Escape"]), &[]).is_err());
        assert_eq!(map, Keymap::default());
    }
}
