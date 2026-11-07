//! Keys and the mouse: what each does in each mode.
use super::*;

impl App {
    pub fn on_key(&mut self, key: KeyEvent) {
        self.expire(Some(key.code));
        if key.code == KeyCode::Esc {
            let now = Instant::now();
            self.escape_held = self.last_escape.is_some_and(|last| now.duration_since(last) < HELD);
            self.last_escape = Some(now);
        }
        // tmux's prefix then the open key, as it opened the board: closes it. A prefix that's a board key stays one.
        if let Some((prefix, open)) = self.prefix_close.filter(|(prefix, _)| self.keys.action(prefix).is_none()) {
            if std::mem::take(&mut self.after_prefix) {
                if same_key(&open, &key) {
                    self.quit = Some(After::Close);
                    return;
                }
            } else if same_key(&prefix, &key) {
                self.after_prefix = true;
                return;
            }
        }
        if self.close_key.is_some_and(|close| same_key(&close, &key)) {
            self.quit = Some(After::Close);
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = Some(After::Close);
            return;
        }
        match &self.mode {
            Mode::Normal => self.key_normal(key),
            Mode::Search(_) => self.key_search(key),
            Mode::Rename(_) => self.key_rename(key),
            Mode::Folder(_) => self.key_folder(key),
        }
    }

    pub(super) fn key_normal(&mut self, key: KeyEvent) {
        // Kept as it is, whatever config.toml says: the way back out.
        if key.code == KeyCode::Esc {
            return self.escape();
        }
        let Some(action) = self.keys.action(&key) else { return };
        if matches!(action, Action::Detach) && !self.flat {
            if let Some(id) = self.selected_draft() {
                return self.outdent_drafts(&[id]);
            }
            if let Some(place) = self.selected_group() {
                return self.outdent_drafts(&self.drafts_for(&place));
            }
        }
        match action {
            Action::Detach if !self.flat && self.selected().is_some_and(|row| self.marks.contains(row)) => self.stage_marks_detach(),
            Action::Detach if !self.flat => match self.selected().cloned() {
                Some(RowId::Pane(pane)) => self.stage_detach(pane),
                Some(RowId::Window(_, window)) => self.stage_window(window),
                _ => {}
            },
            Action::Detach => {}
            Action::Down => self.move_to(self.cursor + 1),
            Action::Up => self.move_to(self.cursor.saturating_sub(1)),
            Action::First => self.move_to(0),
            Action::Last => self.move_to(usize::MAX),
            Action::PrevTab => self.step_tab(false),
            Action::NextTab => self.step_tab(true),
            Action::Layout => self.toggle_flat(),
            Action::Help => {
                self.help = !self.help;
                self.help_scroll = 0;
                self.sync_preview();
            }
            Action::Search => {
                self.confirm = None;
                self.switch = None;
                self.mode = Mode::Search(Input::default());
                self.rebuild();
            }
            Action::Rename => self.rename_key(),
            Action::Kill => self.x_key(),
            Action::Compact => self.c_key(),
            Action::New => self.n_key(),
            Action::Folder => self.o_key(),
            Action::Close => self.quit = Some(After::Close),
            Action::Mark => self.mark(),
            Action::SwitchAgent => self.tab_key(),
            Action::Go => self.enter(),
            Action::Tab(place) => self.digit_tab(usize::from(place)),
        }
    }

    pub(super) fn key_search(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Mode::Search(input) = &mut self.mode else { return };
        match key.code {
            KeyCode::Esc => return self.escape(),
            KeyCode::Enter => return self.enter(),
            KeyCode::Down => return self.move_to(self.cursor + 1),
            KeyCode::Up => return self.move_to(self.cursor.saturating_sub(1)),
            KeyCode::Char('j' | 'n') if ctrl => return self.move_to(self.cursor + 1),
            KeyCode::Char('k' | 'p') if ctrl => return self.move_to(self.cursor.saturating_sub(1)),
            _ if input.edit(key) => {}
            _ => return,
        }
        self.rebuild();
        if !self.rows.is_empty() && self.selected().is_some_and(|id| !id.markable()) {
            self.first_item();
        }
    }

    pub(super) fn key_rename(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Mode::Rename(rename) = &mut self.mode else { return };
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return self.rebuild();
            }
            KeyCode::Enter => return self.save_name(),
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End => {
                if let Some(name) = rename.selected.take() {
                    rename.input.set(&name);
                    if matches!(key.code, KeyCode::Left | KeyCode::Home) {
                        rename.input.home();
                    }
                } else {
                    rename.input.edit(key);
                }
            }
            KeyCode::Backspace | KeyCode::Delete if rename.selected.take().is_some() => rename.input.set(""),
            KeyCode::Char(_) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                rename.selected = None;
                rename.input.edit(key);
            }
            // Ctrl+W, Ctrl+U and the like work on the name where the cursor is, at its end, as in a shell.
            _ if rename.selected.is_some() => {
                let name = rename.selected.take().unwrap_or_default();
                rename.input.set(&name);
                if !rename.input.edit(key) {
                    rename.selected = Some(name);
                    return;
                }
            }
            _ if rename.input.edit(key) => {}
            _ => return,
        }
        self.rebuild();
    }

    pub(super) fn key_folder(&mut self, key: KeyEvent) {
        let Mode::Folder(edit) = &mut self.mode else { return };
        match key.code {
            KeyCode::Esc => {
                let row = edit.pane.take().map_or(RowId::Draft(edit.draft), RowId::Pane);
                self.mode = Mode::Normal;
                self.rebuild();
                return self.select(&row);
            }
            KeyCode::Enter => return self.set_draft_folder(),
            KeyCode::Down => {
                edit.index = (edit.index + 1).min(edit.matches.len().min(DROPDOWN).saturating_sub(1));
                return;
            }
            KeyCode::Up => {
                edit.index = edit.index.saturating_sub(1);
                return;
            }
            KeyCode::Right => match edit.matches.get(edit.index).cloned() {
                Some(path) => edit.input.set(&format!("{path}/")),
                None => {
                    edit.input.edit(key);
                }
            },
            KeyCode::Left => edit.input.set(&sessions::updir(&edit.input.text)),
            KeyCode::Tab => {
                let done = sessions::complete_folder(&edit.input.text, self.history.as_deref().unwrap_or_default());
                match done {
                    Some(done) => edit.input.set(&done),
                    None => return,
                }
            }
            _ if edit.input.edit(key) => {}
            _ => return,
        }
        edit.matches = sessions::dirs(&edit.input.text, &edit.suggested, &edit.base);
        edit.index = 0;
        self.rebuild();
    }

    pub(super) fn escape(&mut self) {
        if self.help {
            self.help = false;
            return self.sync_preview();
        }
        // Whatever was opened last goes first: a search never costs what it was opened on top of.
        if self.searching() {
            self.mode = Mode::Normal;
            return self.rebuild();
        }
        // The draft under the cursor, or the new window or session's, else every draft.
        if let Some(place) = self.selected_group() {
            return self.remove_drafts(&self.drafts_for(&place));
        }
        if let Some(id) = self.selected_draft() {
            return self.remove_drafts(&[id]);
        }
        if !self.drafts.is_empty() {
            self.drafts.clear();
            return self.rebuild();
        }
        if self.switch.take().is_some() {
            return self.rebuild();
        }
        if self.confirm.take().is_some() {
            return;
        }
        if !self.failed.is_empty() || (!self.notice.is_empty() && self.tone() == Tone::Failed) {
            if self.tone() == Tone::Failed {
                self.notice.clear();
            }
            self.failed.clear();
            return self.rebuild();
        }
        if self.has_staged() {
            self.staged.clear();
            self.staged_windows.clear();
            return self.rebuild();
        }
        if !self.marks.is_empty() {
            self.marks.clear();
            self.marks_detach = None;
            return self.rebuild();
        }
        // Held down, it has cleared everything: closing takes a press of its own.
        if !self.escape_held {
            self.quit = Some(After::Close);
        }
    }

    /// Removes drafts, the cursor going to the one made before them; with none before, the one after, else back where n
    /// was pressed.
    fn remove_drafts(&mut self, ids: &[u64]) {
        let Some(at) = self.drafts.iter().position(|draft| ids.contains(&draft.id)) else {
            return;
        };
        let anchor = self.drafts[at].anchor.clone();
        self.drafts.retain(|draft| !ids.contains(&draft.id));
        let near = at.checked_sub(1).or((at < self.drafts.len()).then_some(at));
        let row = near.map(|near| RowId::Draft(self.drafts[near].id));
        self.rebuild();
        match row.filter(|row| self.rows.iter().any(|r| r.id == *row)) {
            Some(row) => self.select(&row),
            None => self.select(&anchor),
        }
    }

    pub(super) fn on_mouse(&mut self, mouse: MouseEvent) {
        let position = Position::new(mouse.column, mouse.row);
        let hit = self
            .regions
            .iter()
            .find(|(rect, _)| rect.contains(position))
            .map(|(rect, click)| (*rect, click.clone()));
        let in_list = |rect: Rect| (mouse.row - rect.y) as usize;
        if matches!(self.mode, Mode::Folder(_)) && matches!(hit, Some((_, Click::List))) {
            return; // the row being typed in stays put
        }
        match (mouse.kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Some((_, Click::Folder(at)))) => {
                if let Mode::Folder(edit) = &mut self.mode {
                    edit.index = at;
                }
                self.set_draft_folder();
            }
            (MouseEventKind::ScrollUp, Some((_, Click::Preview))) if self.help => self.help_scroll = self.help_scroll.saturating_sub(1),
            (MouseEventKind::ScrollDown, Some((_, Click::Preview))) if self.help => {
                self.help_scroll = (self.help_scroll + 1).min(self.help_most);
            }
            (MouseEventKind::ScrollUp, Some((_, Click::Preview))) => {
                let most = self.preview.lines.len().saturating_sub(self.preview.height);
                self.preview.up = (self.preview.up + 1).min(most);
            }
            (MouseEventKind::ScrollDown, Some((_, Click::Preview))) => {
                self.preview.up = self.preview.up.saturating_sub(1);
                if self.preview.up == 0
                    && let Some(lines) = self.preview.held.take()
                {
                    self.show_screen(&lines);
                }
            }
            (MouseEventKind::ScrollUp, Some((_, Click::List))) => self.move_to(self.cursor.saturating_sub(1)),
            (MouseEventKind::ScrollDown, Some((_, Click::List))) => self.move_to(self.cursor + 1),
            (MouseEventKind::Down(MouseButton::Left), Some((_, Click::Tab(view)))) => self.open_tab(&view),
            (MouseEventKind::Down(MouseButton::Left), Some((_, Click::Key(key)))) => self.on_key(key),
            (MouseEventKind::Down(button), Some((rect, Click::List))) => {
                let at = self.offset + in_list(rect);
                if at >= self.rows.len() {
                    return;
                }
                self.move_to(at);
                if button == MouseButton::Right {
                    self.mark();
                } else if button == MouseButton::Left {
                    let row = self.rows[at].id.clone();
                    let double = self
                        .last_click
                        .as_ref()
                        .is_some_and(|(when, last)| *last == row && when.elapsed() < Duration::from_millis(400));
                    self.last_click = Some((Instant::now(), row));
                    if double {
                        self.enter();
                    }
                }
            }
            _ => {}
        }
    }
}
