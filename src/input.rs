//! A line of text being typed: search, a new name, a folder.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Default, Clone, Debug)]
pub struct Input {
    pub text: String,
    /// In bytes, always on a character boundary.
    pub cursor: usize,
}

impl Input {
    pub fn new(text: &str) -> Self {
        Input {
            text: text.to_string(),
            cursor: text.len(),
        }
    }

    pub fn set(&mut self, text: &str) {
        *self = Input::new(text);
    }

    /// The editing keys every text field shares; false for any other key.
    pub fn edit(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Char('u') if ctrl => self.delete_to_start(),
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => self.insert(c),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(),
            _ => return false,
        }
        true
    }

    fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn backspace(&mut self) {
        if let Some((at, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.text.remove(at);
            self.cursor = at;
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    fn left(&mut self) {
        if let Some((at, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.cursor = at;
        }
    }

    fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Ctrl+W: back over the word before the cursor.
    fn delete_word(&mut self) {
        let before = self.text[..self.cursor].trim_end();
        let start = before.rfind(' ').map_or(0, |at| at + 1);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Ctrl+U: everything before the cursor.
    fn delete_to_start(&mut self) {
        self.text.replace_range(..self.cursor, "");
        self.cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_stays_on_character_boundaries() {
        let mut input = Input::new("añb");
        let edit = |input: &mut Input, code| assert!(input.edit(KeyEvent::new(code, KeyModifiers::NONE)));
        edit(&mut input, KeyCode::Left);
        edit(&mut input, KeyCode::Left);
        edit(&mut input, KeyCode::Backspace);
        edit(&mut input, KeyCode::Char('é'));
        assert_eq!(input.text, "éñb");
        edit(&mut input, KeyCode::Right);
        edit(&mut input, KeyCode::Delete);
        assert_eq!(input.text, "éñ");
        edit(&mut input, KeyCode::Home);
        assert_eq!(input.cursor, 0);
        edit(&mut input, KeyCode::Delete);
        assert_eq!(input.text, "ñ");
        edit(&mut input, KeyCode::End);
        assert_eq!(input.cursor, "ñ".len());
        edit(&mut input, KeyCode::Delete);
        assert_eq!(input.text, "ñ");
        edit(&mut input, KeyCode::Backspace);
        edit(&mut input, KeyCode::Backspace);
        assert!(input.text.is_empty());
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn deleting_words() {
        let mut input = Input::new("one two ");
        assert!(input.edit(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)));
        assert_eq!(input.text, "one ");
        assert!(input.edit(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
        assert_eq!(input.text, "");
    }

    #[test]
    fn unhandled_keys_leave_the_input_unchanged() {
        let mut input = Input::new("añb");
        for key in [
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        ] {
            assert!(!input.edit(key));
            assert_eq!(input.text, "añb");
            assert_eq!(input.cursor, "añb".len());
        }
    }
}
