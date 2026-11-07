//! What the keys do to a row or the selection: mark, go, rename, kill, compact, switch CLI, new agent, folder.
use super::*;

/// `finish-rename <pane> <tool>`: an empty /rename, seen through to the end. The board reads "ok" or "fail", a tab and
/// the outcome from stdout; once it has closed, tmux shows it.
pub fn finish_rename(pane: &str, tool: &str) {
    let agents = Registry::load();
    detect::send_command(pane, "/rename");
    let finished = agents.get(tool).and_then(|a| a.finish_rename(pane));
    let outcome = finished.unwrap_or_else(|| Ok(format!("Asked {} to name it", agents.name(tool))));
    let (how, said) = match &outcome {
        Ok(text) => ("ok", text),
        Err(text) => ("fail", text),
    };
    let mut out = std::io::stdout();
    if writeln!(out, "{how}\t{said}").and_then(|()| out.flush()).is_err() {
        tmux::display_message(said);
    }
}

impl App {
    pub(super) fn mark(&mut self) {
        self.confirm = None;
        let Some(id) = self.selected().cloned().filter(RowId::markable) else {
            return;
        };
        match self.marks.iter().position(|m| *m == id) {
            Some(at) => {
                self.marks.remove(at);
            }
            None => {
                // Staged is for one agent, marked is for the selection: a move of its own gives way to the selection's.
                if let RowId::Pane(pane) = &id
                    && self.staged.iter().any(|stage| stage.pane == *pane && stage.detach.is_some())
                {
                    self.change_stage(&pane.clone(), |stage| stage.detach = None);
                }
                self.marks.push(id);
            }
        }
        if self.marks.is_empty() {
            self.marks_detach = None;
        }
        self.switch = None;
        self.rebuild();
    }

    pub(super) fn enter(&mut self) {
        let Some(id) = self.selected().cloned() else {
            if !self.marks.is_empty() {
                self.open_marked();
            }
            return;
        };
        if matches!(id, RowId::Draft(_) | RowId::NewWindow(_) | RowId::NewSession) {
            let anchor = self.drafts.first().map(|draft| draft.anchor.clone());
            let drafts = std::mem::take(&mut self.drafts);
            self.apply(After::Start(start_groups(drafts)));
            if let Some(anchor) = anchor {
                self.select(&anchor);
            }
            return;
        }
        if self.confirm.take().is_some() {
            return;
        }
        if self.has_staged() {
            return self.apply_staged();
        }
        if !self.marks.is_empty() {
            return self.open_marked();
        }
        match id {
            RowId::Conversation(tool, conversation) => self.open_conversation(tool, conversation),
            RowId::Session(_) | RowId::Window(..) => {
                if !self.collapsed.remove(&id) {
                    self.collapsed.insert(id);
                }
                self.rebuild();
            }
            RowId::Pane(pane) => {
                self.quit = Some(After::Switch(pane));
            }
            _ => {}
        }
    }

    pub(super) fn marked_panes(&self) -> Vec<String> {
        let ids = self
            .marks
            .iter()
            .filter_map(|m| if let RowId::Pane(id) = m { Some(id.clone()) } else { None });
        ids.collect()
    }

    /// Where marked agents go: where Shift+Tab sent them, else where the cursor is. In the tree a session or a window takes
    /// them, and so does the window of the agent under the cursor; in the list, or with nothing to join, they get a new
    /// window together (None).
    pub fn destination(&self) -> Option<Place> {
        match (self.marks_detach, self.selected()) {
            (Some(Detach::Window), _) => Some(Place::NewWindow),
            (Some(Detach::Session), _) => Some(Place::NewSession),
            (None, Some(RowId::Session(session))) => Some(Place::Session(session.clone())),
            (None, Some(RowId::Window(_, window))) => Some(Place::Window(window.clone())),
            (None, Some(RowId::Pane(pane))) if !self.flat => self.pane(pane).map(|p| Place::Window(p.window_id.clone())),
            _ => None,
        }
    }

    /// The marked conversations, to open; with none marked, the one under the cursor, so marked agents can join it.
    pub(super) fn marked_conversations(&self) -> Vec<Opening> {
        let mut conversations: Vec<&RowId> = self.marks.iter().filter(|mark| matches!(mark, RowId::Conversation(..))).collect();
        if conversations.is_empty()
            && !self.marks.is_empty()
            && let Some(row @ RowId::Conversation(..)) = self.selected()
        {
            conversations.push(row);
        }
        conversations
            .into_iter()
            .filter_map(|row| match row {
                RowId::Conversation(tool, id) => self.conversation(tool, id),
                _ => None,
            })
            .map(|c| Opening {
                tool: c.tool.clone(),
                id: c.id.clone(),
                folder: c.folder.clone(),
                target: c.tool.clone(),
                path: None,
            })
            .collect()
    }

    pub(super) fn open_marked(&mut self) {
        let list = self.marked_conversations();
        let panes = self.marked_panes();
        if !list.is_empty() {
            return self.apply(After::Resume { list, panes });
        }
        let after = match self.destination() {
            Some(place) => After::Move { place, panes },
            None => After::Group { panes },
        };
        self.apply(after);
    }

    /// Enter on a conversation: resume it, or open it with the agent Tab chose, once the board closes. Problems show in
    /// the footer and the board stays open.
    pub(super) fn open_conversation(&mut self, tool: String, id: String) {
        let row = RowId::Conversation(tool.clone(), id.clone());
        let Some(conversation) = self.conversation(&tool, &id).cloned() else {
            return;
        };
        if !std::path::Path::new(&conversation.folder).is_dir() {
            // Moved or deleted. Resuming elsewhere would fail: an agent looks a conversation up by its folder.
            self.fail(format!("Folder no longer exists: {}", tmux::tilde(&conversation.folder)));
            return;
        }
        let target = self
            .switch
            .as_ref()
            .filter(|(r, _)| *r == row)
            .map_or_else(|| tool.clone(), |(_, target)| target.clone());
        if target != tool
            && let Err(error) = history::conversation_command(&self.agents, &tool, &id, &target, None)
        {
            self.fail(error.replace(['\n', '\r', '\t'], " "));
            return;
        }
        let list = vec![Opening {
            tool,
            id,
            folder: conversation.folder,
            target,
            path: None,
        }];
        self.apply(After::Resume { list, panes: Vec::new() });
    }

    pub(super) fn rename_key(&mut self) {
        self.confirm = None;
        self.switch = None;
        let Some(id) = self.selected().cloned() else { return };
        let (kind, target) = match id {
            RowId::Session(session) => (Kind::Session, session),
            RowId::Window(_, window) => (Kind::Window, window),
            RowId::Pane(pane) => (Kind::Pane, pane),
            _ => return,
        };
        if kind == Kind::Pane {
            let tool = tmux::pane_command(&target).unwrap_or_default();
            let retitles = self.agents.get(&tool).is_some_and(|a| a.retitles());
            // An agent renamed through its own API is never typed into: no idle or empty-box check.
            if !retitles && let Some(reason) = detect::rename_blocker(&target, &self.agents) {
                self.fail(format!("Can't rename: {reason}"));
                return;
            }
            // Its name highlighted, as a heading's is: typing replaces it, ← or → keeps it to edit, Backspace empties it
            // so the agent picks one.
            let current = self.pane(&target).map(|p| p.name.clone()).filter(|name| !name.is_empty());
            self.mode = Mode::Rename(Rename {
                kind,
                target,
                input: Input::default(),
                selected: current,
            });
        } else {
            let format = if kind == Kind::Session { "#{session_name}" } else { "#{window_name}" };
            let Some(current) = tmux::display(&target, format) else { return };
            self.mode = Mode::Rename(Rename {
                kind,
                target,
                input: Input::default(),
                selected: Some(current),
            });
        }
        self.rebuild();
    }

    pub(super) fn save_name(&mut self) {
        let Mode::Rename(rename) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        if rename.selected.is_some() {
            self.say("Name unchanged");
            return self.rebuild();
        }
        let name = rename.input.text.trim().to_string();
        let target = rename.target;
        if rename.kind != Kind::Pane {
            let (command, word) = if rename.kind == Kind::Session {
                ("rename-session", "session")
            } else {
                ("rename-window", "window")
            };
            if name.is_empty() {
                self.say("Rename cancelled: name is empty");
            } else {
                // -- so a name starting with - isn't read as an option.
                match tmux::run(&[command, "-t", &target, "--", &tmux::name(&name)]) {
                    Ok(()) => {
                        self.say(format!("Renamed {word} to: {name}"));
                        for pane in &mut self.panes {
                            if rename.kind == Kind::Session && pane.session_id == target {
                                pane.session = name.clone();
                            } else if rename.kind == Kind::Window && pane.window_id == target {
                                pane.window = name.clone();
                            }
                        }
                    }
                    Err(error) => self.fail(format!("Not renamed: {error}")),
                }
            }
            return self.rebuild();
        }
        let tool = tmux::pane_command(&target).unwrap_or_default();
        let agents = self.agents.clone();
        if agents.get(&tool).is_some_and(|a| a.retitles()) {
            self.say("Renaming…");
            self.background(move || match agents.get(&tool) {
                Some(agent) => agent.retitle(&target, &name, &agents),
                None => Err("Not renamed: the agent is no longer available".into()),
            });
        } else if let Some(reason) = detect::rename_blocker(&target, &agents) {
            self.fail(format!("Not renamed: {reason}"));
        } else if !name.is_empty() {
            // Known name: once the agent has "/rename", set the pane title so every view shows it at once; the agent sets
            // the same title a moment later when it handles it. Not before: the agent may never get it.
            self.say("Renaming…");
            self.background(move || {
                if !detect::send_command(&target, &format!("/rename {name}")) {
                    return Err("Not renamed: the agent didn't take /rename".into());
                }
                tmux::ok(&["select-pane", "-t", &target, "-T", &tmux::name(&name)]);
                Ok(format!("Renamed to: {name}"))
            });
        } else {
            // The agent picks the name, or its finish_rename finishes the job.
            self.say("Renaming…");
            let tx = self.tx.clone();
            // Its own process, not self.background: a dialog can take 15 seconds, and closing the board doesn't wait for it.
            // Its own process group, so closing the popup doesn't hang it up either.
            std::thread::spawn(move || {
                let start = |me| {
                    let mut command = Command::new(me);
                    command.args(["finish-rename", &target, &tool]);
                    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0);
                    command.spawn()?.wait_with_output()
                };
                let said = std::env::current_exe()
                    .and_then(start)
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
                let outcome = match said.as_deref().map(|line| line.split_once('\t')) {
                    Ok(Some(("ok", text))) => Ok(text.to_string()),
                    Ok(Some((_, text))) => Err(text.to_string()),
                    _ => Err("Not renamed: could not ask the agent".into()),
                };
                let _ = tx.send(Msg::Outcome(outcome));
            });
        }
        self.rebuild();
    }

    /// What an action on the cursor's row is for: on a marked row, the whole selection, else just that row.
    pub fn targets(&self) -> Vec<RowId> {
        match self.selected() {
            Some(row) if self.marks.contains(row) => self.marks.clone(),
            Some(row) => vec![row.clone()],
            None => Vec::new(),
        }
    }

    pub(super) fn x_key(&mut self) {
        self.switch = None;
        let Some(id) = self.selected().cloned() else { return };
        if !matches!(id, RowId::Pane(_) | RowId::Conversation(..)) {
            return;
        }
        if self.confirm != Some((ConfirmAction::Kill, id.clone())) {
            self.confirm = Some((ConfirmAction::Kill, id));
            return;
        }
        self.confirm = None;
        let targets = self.targets();
        let mut outcomes = Vec::new();
        for target in &targets {
            outcomes.push(match target {
                RowId::Conversation(tool, conversation) => self.remove_conversation(tool, conversation),
                RowId::Pane(pane) => self.kill(pane),
                _ => continue,
            });
        }
        self.marks.retain(|mark| !targets.contains(mark));
        if self.marks.is_empty() {
            self.marks_detach = None;
        }
        self.rebuild();
        let _ = self.wake_refresh.send(());
        let agents = targets.iter().filter(|t| matches!(t, RowId::Pane(_))).count();
        self.report(summary(outcomes, |n| match agents {
            0 => format!("Removed {n} conversations"),
            a if a == n => format!("Killed {n} agents"),
            _ => format!("Killed and removed all {n}"),
        }));
    }

    /// Closes an agent's pane. The last pane on the server becomes a shell instead: closing it would end tmux itself.
    pub(super) fn kill(&mut self, pane: &str) -> Result<String, String> {
        // Recheck after confirmation; the agent may have exited to its shell in the meantime.
        let command = tmux::pane_command(pane).unwrap_or_default();
        if !self.agents.tracks(&command) {
            return Err("Agent is no longer running; nothing killed".into());
        }
        // Sessions counted from the list: #{server_sessions} is empty before tmux 3.4.
        let sessions = tmux::query(&["list-sessions", "-F", "#{session_id}"]).map(|list| list.lines().count());
        let outcome = if tmux::display(pane, "#{window_panes} #{session_windows}").as_deref() == Some("1 1") && sessions == Some(1) {
            let folder = self.pane(pane).map_or_else(tmux::home, |p| p.path.clone());
            match sessions::to_shell(pane, &folder) {
                Ok(()) => Ok(format!("Killed agent in {pane}; left a shell")),
                Err(error) => Err(format!("Could not kill the selected agent: {error}")),
            }
        } else if sessions::close(pane) {
            Ok(format!("Killed agent in {pane}"))
        } else {
            Err("Could not kill the selected agent".into())
        };
        if outcome.is_ok() {
            self.panes.retain(|p| p.id != pane);
        }
        outcome
    }

    /// The conversation of a live pane is never removed; its row leaves History at once. Asked afresh, not from the
    /// context cache, which may not have every pane yet: a pane of the same agent in its folder whose conversation can't
    /// be told counts as having it open.
    pub(super) fn remove_conversation(&mut self, tool: &str, id: &str) -> Result<String, String> {
        let Some(agent) = self.agents.get(tool) else {
            return Err("Not removed: its agent is no longer available".into());
        };
        let folder = self
            .history
            .iter()
            .flatten()
            .find(|c| c.tool == tool && c.id == id)
            .map(|c| c.folder.as_str());
        let live = self
            .panes
            .iter()
            .filter(|p| p.tool == tool)
            .any(|p| match history::live_conversation(&self.agents, tool, &p.id, false) {
                Some(open) => open == id,
                None => folder.is_none_or(|f| f == p.path),
            });
        if live {
            return Err("Not removed: a live pane may have it open; close that first".into());
        }
        if !agent.removes() {
            return Err(format!("Not removed: {} conversations can't be removed", self.agents.name(tool)));
        }
        let outcome = agent.remove(id);
        if outcome.is_ok()
            && let Some(list) = &mut self.history
        {
            list.retain(|c| !(c.tool == tool && c.id == id));
            history::store(list);
        }
        outcome
    }

    pub(super) fn c_key(&mut self) {
        self.switch = None;
        if self.compacting {
            return;
        }
        let Some(row @ RowId::Pane(_)) = self.selected().cloned() else { return };
        if self.confirm != Some((ConfirmAction::Compact, row.clone())) {
            self.confirm = Some((ConfirmAction::Compact, row));
            return;
        }
        self.confirm = None;
        self.compacting = true;
        self.say("Requesting compaction…");
        let panes: Vec<String> = self
            .targets()
            .into_iter()
            .filter_map(|t| if let RowId::Pane(p) = t { Some(p) } else { None })
            .collect();
        let agents = self.agents.clone();
        self.background_as(Msg::Compacted, move || {
            let outcomes = panes.iter().map(|pane| {
                let tool = tmux::pane_command(pane).unwrap_or_default();
                let reason = if agents.tracks(&tool) {
                    detect::rename_blocker(pane, &agents)
                } else {
                    Some("the agent is no longer available".into())
                };
                match reason {
                    Some(reason) => Err(format!("Can't compact: {reason}")),
                    None if detect::send_command(pane, "/compact") => Ok(format!("Asked {} to compact", agents.name(&tool))),
                    None => Err("Could not send /compact to the selected agent".into()),
                }
            });
            summary(outcomes.collect(), |n| format!("Asked {n} agents to compact"))
        });
    }

    pub(super) fn tab_key(&mut self) {
        let Some(id) = self.selected().cloned() else { return };
        if let RowId::Draft(on) = id {
            let next = self.draft(on).and_then(|draft| self.agents.next_installed(&draft.agent));
            if let (Some(next), Some(draft)) = (next, self.drafts.iter_mut().find(|draft| draft.id == on)) {
                draft.agent = next;
            }
            return self.rebuild();
        }
        if let RowId::Pane(pane) = id {
            return self.stage_tool(pane);
        }
        let RowId::Conversation(tool, _) = &id else { return };
        let tool = tool.clone();
        if !self.marks.is_empty() || !self.agents.switchable(&tool) {
            return;
        }
        let current = self
            .switch
            .as_ref()
            .filter(|(row, _)| *row == id)
            .map_or_else(|| tool.clone(), |(_, target)| target.clone());
        if let Some(next) = self.agents.next_installed(&current) {
            self.switch = (next != tool).then_some((id, next)); // back on its own agent, there is no switch
            self.confirm = None;
            self.rebuild();
        }
    }

    /// n: a draft row for a new agent, in the selected row's folder. Flat list: a new window in the session the board was
    /// opened from. Tree: it starts in the selected window or session, and the arrows move it.
    pub(super) fn n_key(&mut self) {
        self.confirm = None;
        self.switch = None;
        let id = self.selected().cloned().unwrap_or(RowId::Loading);
        if self.in_history() {
            return;
        }
        // On a draft: another just like it, right after it. On a new window or session: another like its last.
        let like = match &id {
            RowId::Draft(on) => Some(*on),
            _ => self.selected_group().and_then(|place| self.drafts_for(&place).last().copied()),
        };
        if let Some(on) = like {
            let Some(at) = self.drafts.iter().position(|draft| draft.id == on) else {
                return;
            };
            let copy = Draft {
                id: self.next_draft,
                ..self.drafts[at].clone()
            };
            self.next_draft += 1;
            let new = copy.id;
            self.drafts.insert(at + 1, copy);
            self.rebuild();
            return self.select(&RowId::Draft(new));
        }
        let Some(home) = tmux::origin_session(&self.origin) else {
            return;
        };
        let here = Spot::Window { session: home };
        let spot = match &id {
            _ if self.flat => here,
            RowId::Session(session) => Spot::Window { session: session.clone() },
            RowId::Window(session, window) => Spot::Pane {
                session: session.clone(),
                window: window.clone(),
            },
            RowId::Pane(pane) => self.pane(pane).map_or(here, |p| Spot::Pane {
                session: p.session_id.clone(),
                window: p.window_id.clone(),
            }),
            _ => here,
        };
        let from = match &id {
            RowId::Session(session) => session,
            RowId::Window(_, window) => window,
            RowId::Pane(pane) => pane,
            _ => &self.origin,
        };
        let known = self.pane(from).map(|pane| pane.path.clone());
        let Some(folder) = known.or_else(|| tmux::display(from, "#{pane_current_path}")) else {
            return;
        };
        // On an agent, its CLI; else the one last started this way.
        let own = self
            .pane(from)
            .map(|pane| pane.tool.clone())
            .filter(|tool| self.agents.installed().contains(tool));
        let Some(agent) = own.or_else(|| sessions::last_agent(&self.agents)) else {
            return;
        };
        let place = self.place_of(&spot);
        self.reveal(&spot);
        let new = self.next_draft;
        self.next_draft += 1;
        self.drafts.push(Draft {
            id: new,
            agent,
            start: spot.clone(),
            spot,
            folder,
            anchor: id,
            place,
        });
        self.rebuild();
        self.select(&RowId::Draft(new));
    }

    /// o on an agent: its folder for its own /cd, which keeps the conversation. Only when it's idle with an empty input box,
    /// as for any command typed into an agent.
    pub(super) fn cd_folder(&mut self, pane: &str) -> Option<String> {
        self.confirm = None;
        self.switch = None;
        let agent = self.pane(pane).cloned()?;
        if self.agents.get(&agent.tool).is_none_or(|a| a.cd("").is_none()) {
            self.fail(format!("Cannot move: {} has no /cd", self.agents.name(&agent.tool)));
            return None;
        }
        if let Some(reason) = detect::rename_blocker(pane, &self.agents) {
            self.fail(format!("Cannot move: {reason}"));
            return None;
        }
        let staged = self.staged.iter().find(|stage| stage.pane == pane).and_then(|stage| stage.folder.clone());
        Some(staged.unwrap_or(agent.path))
    }

    /// o on the draft row: its folder is typed in place and the matching folders drop down under its Folder column, the
    /// agents staying around them.
    pub(super) fn o_key(&mut self) {
        let (folder, pane, draft) = match self.selected().cloned() {
            Some(RowId::Pane(pane)) => match self.cd_folder(&pane) {
                Some(folder) => (folder, Some(pane), 0),
                None => return,
            },
            Some(RowId::Draft(id)) => match self.draft(id) {
                Some(draft) => (draft.folder.clone(), None, id),
                None => return,
            },
            _ => return,
        };
        let suggested = sessions::suggestions(&folder, self.past());
        // The trailing slash lists what's inside, so there is something to pick from the start.
        let input = Input::new(&format!("{}/", tmux::tilde(&folder).trim_end_matches('/')));
        let matches = sessions::dirs(&input.text, &suggested, &folder);
        self.mode = Mode::Folder(FolderEdit {
            input,
            suggested,
            matches,
            index: 0,
            pane,
            draft,
            base: folder,
        });
        self.rebuild();
    }

    /// Enter while typing the draft's folder: the highlighted one, else the typed one.
    pub(super) fn set_draft_folder(&mut self) {
        let Mode::Folder(edit) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        let picked = edit.matches.get(edit.index).unwrap_or(&edit.input.text);
        // Its trailing slash goes, except the root's, which is all slash.
        let trim = |path: &str| match path.trim_end_matches('/') {
            "" if path.starts_with('/') => "/".to_string(),
            rest => rest.to_string(),
        };
        let mut typed = trim(&sessions::absolute(picked, &edit.base));
        // A folder's name rather than a path: the best match zoxide or past agents know.
        if !std::path::Path::new(&typed).is_dir() && !typed.starts_with(['/', '.']) {
            let known = sessions::complete_folder(&typed, self.past());
            typed = known.map_or(typed, |found| trim(&tmux::untilde(&found)));
        }
        let exists = !typed.is_empty() && std::path::Path::new(&typed).is_dir();
        if !exists {
            self.fail(format!("No folder {}", tmux::tilde(&typed)));
        }
        if let Some(pane) = edit.pane {
            if exists {
                self.stage_folder(pane.clone(), typed);
            }
            self.rebuild();
            return self.select(&RowId::Pane(pane));
        }
        if exists {
            if let Some(draft) = self.drafts.iter_mut().find(|draft| draft.id == edit.draft) {
                draft.folder = typed;
            }
            self.notice.clear();
        }
        self.rebuild();
        self.select(&RowId::Draft(edit.draft));
    }
}

/// Drafts as what Enter starts: those going to the same window, or to the same new window or session, start together
/// there, in the order they were made. New sessions go last, as starting one takes you there.
fn start_groups(drafts: Vec<Draft>) -> Vec<(Where, Vec<(String, String)>)> {
    let mut groups: Vec<(Where, Vec<(String, String)>)> = Vec::new();
    for draft in drafts {
        let place = draft.spot.place();
        let agent = (draft.agent, draft.folder);
        match groups.iter_mut().find(|(at, _)| *at == place) {
            Some((_, list)) => list.push(agent),
            None => groups.push((place, vec![agent])),
        }
    }
    groups.sort_by_key(|(place, _)| *place == Where::Session);
    groups
}
