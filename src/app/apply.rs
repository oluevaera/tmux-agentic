//! Staged changes and running them: each step in order, with the board open or once it closes, and what's left when one fails.
use super::*;

/// Keeps track of staged changes while they run: what ran comes off, so what's left can go back to being staged, and a
/// failure is known by the agent it happened to.
#[derive(Default)]
pub struct Ledger {
    pub(super) stages: Vec<Stage>,
    pub(super) windows: Vec<String>,
    pub(super) failed: Vec<(String, String)>,
    /// Marked agents whose move didn't go through, and where they were going: marked again, the cursor back there.
    pub(super) marks: Vec<String>,
    pub(super) place: Option<Place>,
}

impl Ledger {
    pub(super) fn stage(&mut self, pane: &str) -> Option<&mut Stage> {
        self.stages.iter_mut().find(|stage| stage.pane == pane)
    }
}

/// What Enter does after every handoff has been checked, in this order.
pub(super) enum Step {
    Swap(LiveHandoff, Option<String>),
    Cd(Vec<CdMove>),
    Detach(Detach),
    Move,
    Open(Vec<Opening>),
}

/// Runs steps one at a time, each only once the one before it has succeeded. What's done stays done: an agent that was
/// swapped has already closed its original, whose conversation is in History.
pub(super) fn in_order<T>(steps: Vec<(String, T)>, mut run: impl FnMut(T) -> Result<(), String>, mut show: impl FnMut(&str)) -> Result<(), String> {
    let total = steps.len();
    for (i, (label, step)) in steps.into_iter().enumerate() {
        let number = i + 1;
        if total > 1 {
            show(&format!("Step {number} of {total}: {label}"));
        }
        if let Err(problem) = run(step) {
            let rest = match total - number {
                0 => String::new(),
                1 => format!(". Step {total} not run"),
                _ => format!(". Steps {}-{total} not run", number + 1),
            };
            return Err(format!("Stopped at step {number} of {total}: {problem}{rest}"));
        }
    }
    Ok(())
}

/// Forwards each agent's own /cd, checking again that it's idle with an empty input box. Says how it went.
pub(super) fn send_cds(agents: &Registry, moves: &[CdMove], ledger: &mut Ledger) -> Result<String, String> {
    let (mut moved, mut failed) = (Vec::new(), Vec::new());
    for (tool, pane, folder) in moves {
        let blocked = detect::rename_blocker(pane, agents);
        let command = agents.get(tool).and_then(|agent| agent.cd(folder));
        if blocked.is_none() && command.is_some_and(|command| detect::send_command(pane, &command)) {
            detect::remember_folder(pane, tool, folder);
            if let Some(stage) = ledger.stage(pane) {
                stage.folder = None;
            }
            moved.push((tool, folder));
        } else {
            let reason = blocked.unwrap_or_else(|| "it didn't take the command".to_string());
            ledger.failed.push((pane.clone(), reason.clone()));
            failed.push((agents.name(tool), reason));
        }
    }
    match (&moved[..], &failed[..]) {
        (_, [(name, reason)]) => Err(format!("{name} can't move: {reason}")),
        (_, [_, ..]) => Err(format!("{} agents can't move: see their rows", failed.len())),
        ([(tool, folder)], []) => Ok(format!("Asked {} to move to {}", agents.name(tool), tmux::tilde(folder))),
        _ => Ok(format!("Asked {} agents to move", moved.len())),
    }
}

pub(super) fn finish(after: After, agents: &Registry, origin: &str, say: &dyn Fn(&str), ledger: &mut Ledger) -> Result<(), String> {
    match after {
        After::Close => Ok(()),
        After::Switch(pane) => {
            // Remember where you were, for "back" (prefix + b).
            if pane != origin {
                tmux::ok(&["set", "-g", "@agentic-previous", origin]);
            }
            tmux::run(&["switch-client", "-t", &pane])
        }
        After::Group { panes } => {
            let session = tmux::origin_session(origin).ok_or("The board's session is gone")?;
            remember_move(move_selection(agents, &session, None, &panes), &panes, None, ledger)
        }
        After::Resume { list, panes } => open_together(agents, origin, &list, &panes),
        After::Apply {
            swaps,
            cds,
            mut detached,
            windows,
            panes,
            place,
            open,
        } => {
            // First: a swapped agent may be the pane the board was opened from.
            let session = tmux::origin_session(origin);
            let mut steps: Vec<(String, Step)> = swaps
                .into_iter()
                .map(|(handoff, folder)| (format!("starting {}", agents.name(&handoff.opening.target)), Step::Swap(handoff, folder)))
                .collect();
            if !cds.is_empty() {
                steps.push(("moving folders".to_string(), Step::Cd(cds)));
            }
            if detached.iter().any(|(_, to)| *to == Detach::Window) {
                steps.push(("opening new windows".to_string(), Step::Detach(Detach::Window)));
            }
            if !windows.is_empty() || detached.iter().any(|(_, to)| *to == Detach::Session) {
                steps.push(("opening a new tmux session".to_string(), Step::Detach(Detach::Session)));
            }
            if !open.is_empty() {
                steps.push(("opening conversations".to_string(), Step::Open(open)));
            } else if !panes.is_empty() {
                steps.push(("moving agents".to_string(), Step::Move));
            }
            let mut panes = panes;
            in_order(
                steps,
                |step| match step {
                    Step::Swap(handoff, folder) => {
                        let replaced = history::replace(agents, &handoff, folder.as_deref());
                        let pane = replaced.inspect_err(|error| ledger.failed.push((handoff.pane.clone(), error.clone())))?;
                        if let Some(stage) = ledger.stage(&handoff.pane) {
                            (stage.pane, stage.tool, stage.folder) = (pane.clone(), None, None);
                        }
                        if let Some(staged) = detached.iter_mut().find(|(staged, _)| *staged == handoff.pane) {
                            staged.0 = pane.clone();
                        }
                        if let Some(marked) = panes.iter_mut().find(|marked| **marked == handoff.pane) {
                            *marked = pane;
                        }
                        Ok(())
                    }
                    Step::Cd(cds) => send_cds(agents, &cds, ledger).map(|_| ()),
                    Step::Detach(to) => {
                        let chosen: Vec<String> = detached
                            .iter()
                            .filter(|(_, staged)| *staged == to)
                            .map(|(pane, _)| pane.clone())
                            .collect();
                        // Staged is per agent: each in a session of its own, and each whole window too. Each is told apart: one
                        // that went has nothing more to do, one that didn't stays staged with its own reason.
                        let mut first_error: Option<String> = None;
                        match to {
                            Detach::Window => {
                                let moved = sessions::own_windows(agents, &chosen);
                                for pane in &chosen {
                                    settle(ledger, pane, &moved);
                                }
                                first_error = moved.err();
                            }
                            Detach::Session => {
                                for pane in &chosen {
                                    let moved = sessions::new_session(agents, std::slice::from_ref(pane), &[], false);
                                    settle(ledger, pane, &moved);
                                    if let Err(error) = moved {
                                        first_error.get_or_insert(error);
                                    }
                                }
                                let mut left = Vec::new();
                                for window in &windows {
                                    if let Err(error) = sessions::new_session(agents, &[], std::slice::from_ref(window), false) {
                                        left.push(window.clone());
                                        first_error.get_or_insert(error);
                                    }
                                }
                                ledger.windows = left;
                            }
                        }
                        first_error.map_or(Ok(()), Err)
                    }
                    Step::Move => {
                        let session = session.clone().ok_or("The board's session is gone")?;
                        let moved = move_selection(agents, &session, place.as_ref(), &panes);
                        remember_move(moved, &panes, place.as_ref(), ledger)
                    }
                    Step::Open(list) => open_together(agents, origin, &list, &panes),
                },
                say,
            )
        }
        After::Start(groups) => {
            let steps = groups
                .into_iter()
                .map(|(place, list)| {
                    let what = match list.len() {
                        1 => "starting an agent".to_string(),
                        n => format!("starting {n} agents"),
                    };
                    (what, (place, list))
                })
                .collect();
            in_order(steps, |(place, list)| sessions::start_together(agents, &place, &list), say)
        }
        After::Move { place, panes } => {
            let session = tmux::origin_session(origin).ok_or("The board's session is gone")?;
            let moved = move_selection(agents, &session, Some(&place), &panes);
            remember_move(moved, &panes, Some(&place), ledger)
        }
    }
}

/// Marked agents, together, where they were sent: a window, a session, a new window or session of their own, or with no
/// place a new window in the board's session.
pub(super) fn move_selection(agents: &Registry, session: &str, place: Option<&Place>, panes: &[String]) -> Result<(), String> {
    match place {
        Some(Place::Window(window)) => sessions::move_to_window(agents, window, panes),
        Some(Place::Session(target)) => sessions::move_to_session(agents, target, panes, true),
        Some(Place::NewSession) => sessions::new_session(agents, panes, &[], true),
        Some(Place::NewWindow) | None => sessions::group(agents, session, panes),
    }
}

/// Conversations in one new window, the marked agents joining them.
pub(super) fn open_together(agents: &Registry, origin: &str, list: &[Opening], panes: &[String]) -> Result<(), String> {
    // Nothing moves unless the conversations opened.
    let Some(first) = history::resume(agents, origin, list)? else {
        return Ok(());
    };
    if panes.is_empty() {
        return Ok(());
    }
    let window = tmux::display(&first, "#{window_id}").ok_or("The new window is gone")?;
    sessions::move_to_window(agents, &window, panes)?;
    sessions::spread(&window);
    Ok(())
}

/// What a staged agent's move came to: done, its stage has nothing left to move; failed, the reason by its row.
fn settle(ledger: &mut Ledger, pane: &str, moved: &Result<(), String>) {
    match moved {
        Ok(()) => {
            if let Some(stage) = ledger.stage(pane) {
                stage.detach = None;
            }
        }
        Err(error) => ledger.failed.push((pane.to_string(), error.clone())),
    }
}

/// One outcome for several: the single one as it is, or how many went and the first thing that went wrong.
pub(super) fn summary(outcomes: Vec<Result<String, String>>, all: impl Fn(usize) -> String) -> Result<String, String> {
    if outcomes.len() == 1 {
        return outcomes.into_iter().next().unwrap();
    }
    let failed: Vec<String> = outcomes.iter().filter_map(|o| o.as_ref().err().cloned()).collect();
    match failed.first() {
        None => Ok(all(outcomes.len())),
        Some(first) => Err(format!("{} of {} didn't go: {first}", failed.len(), outcomes.len())),
    }
}

/// A move of marked agents that went wrong: each one's row says so, and they're marked again to try again.
pub(super) fn remember_move(moved: Result<(), String>, panes: &[String], place: Option<&Place>, ledger: &mut Ledger) -> Result<(), String> {
    if let Err(error) = &moved {
        ledger.failed.extend(panes.iter().map(|pane| (pane.clone(), error.clone())));
        ledger.marks = panes.to_vec();
        ledger.place = place.cloned();
    }
    moved
}

impl App {
    pub(super) fn background(&mut self, work: impl FnOnce() -> Result<String, String> + Send + 'static) {
        self.background_as(Msg::Outcome, work);
    }

    /// The same, its outcome sent as `msg`.
    pub(super) fn background_as(&mut self, msg: fn(Result<String, String>) -> Msg, work: impl FnOnce() -> Result<String, String> + Send + 'static) {
        let tx = self.tx.clone();
        self.work.push(std::thread::spawn(move || {
            let _ = tx.send(msg(work()));
        }));
    }

    /// Runs staged changes in the background, putting back what's left if they stop.
    pub(super) fn attempt(&mut self, mut ledger: Ledger, work: impl FnOnce(&mut Ledger) -> Result<String, String> + Send + 'static) {
        self.failed.clear();
        let tx = self.tx.clone();
        self.work.push(std::thread::spawn(move || {
            let _ = tx.send(match work(&mut ledger) {
                Ok(text) => Msg::Outcome(Ok(text)),
                Err(text) => Msg::Stopped(text, ledger),
            });
        }));
    }

    /// Takes everything staged, to run.
    pub(super) fn take_staged(&mut self) -> Ledger {
        Ledger {
            stages: std::mem::take(&mut self.staged),
            windows: std::mem::take(&mut self.staged_windows),
            ..Ledger::default()
        }
    }

    /// A change to windows, sessions or panes. The tree is for managing them, so there it runs with the board open and
    /// you stay to make more, the footer following along; anywhere else it closes the board and takes you there.
    pub(super) fn apply(&mut self, after: After) {
        // Only Apply runs what's staged, and starting an agent uses no marks: whatever an action doesn't use stays as
        // it was, not taken along and dropped once it works.
        let ledger = if matches!(after, After::Apply { .. }) {
            self.take_staged()
        } else {
            Ledger::default()
        };
        let keeps_marks = matches!(after, After::Start(_));
        // Unit tests only look at what would happen.
        if cfg!(test) || matches!(after, After::Close | After::Switch(_)) {
            self.quit = Some(after);
            return;
        }
        // Where the board takes you somewhere (the list, a conversation), it closes once that has worked, not before: a
        // failure has to show. The tree stays.
        let jump = self.flat || matches!(after, After::Resume { .. });
        let stay = !jump;
        self.closing = jump;
        // The cursor goes with what moved, so it's easy to find where it lands.
        let moving = match &after {
            After::Apply { panes, detached, .. } => panes.first().or(detached.first().map(|(pane, _)| pane)),
            After::Move { panes, .. } | After::Group { panes, .. } => panes.first(),
            _ => None,
        };
        let moving = moving.cloned();
        if !keeps_marks {
            self.marks.clear();
            self.marks_detach = None;
        }
        self.say("Working…");
        self.rebuild();
        if let Some(pane) = moving {
            self.select(&RowId::Pane(pane));
        }
        let (agents, origin, tx) = (self.agents.clone(), self.origin.clone(), self.tx.clone());
        self.attempt(ledger, move |ledger| {
            let say = |line: &str| {
                let _ = tx.send(Msg::Outcome(Ok(line.to_string())));
            };
            let outcome = tmux::staying(stay, || finish(after, &agents, &origin, &say, ledger));
            outcome.map(|()| "Done".to_string())
        });
    }

    pub fn has_staged(&self) -> bool {
        !self.staged.is_empty() || !self.staged_windows.is_empty()
    }

    /// Shift+Tab on a window heading: a new tmux session, or back.
    pub(super) fn stage_window(&mut self, window: String) {
        match self.staged_windows.iter().position(|staged| *staged == window) {
            Some(at) => {
                self.staged_windows.remove(at);
            }
            None => self.staged_windows.push(window),
        }
        self.confirm = None;
        self.notice.clear();
        self.rebuild();
    }

    pub(super) fn change_stage(&mut self, pane: &str, change: impl FnOnce(&mut Stage)) {
        if !self.staged.iter().any(|stage| stage.pane == pane) {
            self.staged.push(Stage {
                pane: pane.to_string(),
                folder: None,
                tool: None,
                detach: None,
            });
        }
        if let Some(stage) = self.staged.iter_mut().find(|stage| stage.pane == pane) {
            change(stage);
        }
        self.staged
            .retain(|stage| stage.folder.is_some() || stage.tool.is_some() || stage.detach.is_some());
        self.notice.clear();
        self.rebuild();
    }

    /// A folder chosen for an agent waits with the others; its own folder means no change.
    pub(super) fn stage_folder(&mut self, pane: String, folder: String) {
        let change = self.pane(&pane).filter(|p| p.path != folder).map(|_| folder);
        self.change_stage(&pane, |stage| stage.folder = change);
    }

    /// Tab: the next CLI to continue this agent in; back on its own, there is no change.
    pub(super) fn stage_tool(&mut self, pane: String) {
        let Some(tool) = self.pane(&pane).map(|p| p.tool.clone()).filter(|t| self.agents.switchable(t)) else {
            return;
        };
        let stage = self.staged.iter().find(|stage| stage.pane == pane);
        let current = stage.and_then(|stage| stage.tool.clone()).unwrap_or_else(|| tool.clone());
        if let Some(next) = self.agents.next_installed(&current) {
            self.confirm = None;
            self.change_stage(&pane, |stage| stage.tool = (next != tool).then_some(next));
        }
    }

    /// Shift+Tab in the tree: a new window, a new tmux session, then no change. An agent alone in its window has a window.
    /// Shift+Tab on a marked row: the whole selection to one new window, then one new tmux session, then back.
    pub(super) fn stage_marks_detach(&mut self) {
        self.marks_detach = match self.marks_detach {
            None => Some(Detach::Window),
            Some(Detach::Window) => Some(Detach::Session),
            Some(Detach::Session) => None,
        };
        self.confirm = None;
        self.rebuild();
    }

    pub(super) fn stage_detach(&mut self, pane: String) {
        let alone = tmux::display(&pane, "#{window_panes}").as_deref() == Some("1");
        let current = self.staged.iter().find(|stage| stage.pane == pane).and_then(|stage| stage.detach);
        let next = match current {
            None if !alone => Some(Detach::Window),
            None | Some(Detach::Window) => Some(Detach::Session),
            Some(Detach::Session) => None,
        };
        self.confirm = None;
        self.change_stage(&pane, |stage| stage.detach = next);
    }

    /// Enter with changes staged. Folders alone just need a /cd, so the board stays open. Every handoff is checked first:
    /// if one can't go ahead, nothing happens and everything stays staged.
    pub(super) fn apply_staged(&mut self) {
        let cds: Vec<CdMove> = self
            .staged
            .iter()
            .filter(|stage| stage.tool.is_none())
            .filter_map(|stage| Some((self.pane(&stage.pane)?.tool.clone(), stage.pane.clone(), stage.folder.clone()?)))
            .collect();
        let panes = self.marked_panes();
        // A marked agent goes where the cursor is, whatever else was staged for it.
        let detached: Vec<(String, Detach)> = self
            .staged
            .iter()
            .filter(|stage| !panes.contains(&stage.pane))
            .filter_map(|stage| Some((stage.pane.clone(), stage.detach?)))
            .collect();
        // Only /cds and nothing marked: sent straight. Marked conversations open in the full run, after them.
        let only_cds = panes.is_empty()
            && detached.is_empty()
            && self.staged_windows.is_empty()
            && self.staged.iter().all(|stage| stage.tool.is_none())
            && !self.marks.iter().any(|mark| matches!(mark, RowId::Conversation(..)));
        if only_cds {
            let ledger = self.take_staged();
            self.say("Moving…");
            self.rebuild();
            let agents = self.agents.clone();
            return self.attempt(ledger, move |ledger| send_cds(&agents, &cds, ledger));
        }
        // Finding a conversation can take seconds (an /export typed and waited for), so it runs behind the board, which
        // comes back here once they're all found. Until then nothing is taken: one that can't go ahead leaves it all staged.
        if !self.handoffs_ready() {
            if !self.preparing {
                self.prepare_handoffs();
            }
            return;
        }
        let mut swaps = Vec::new();
        for stage in &self.staged {
            if let Some(target) = &stage.tool {
                let found = self.handoffs.iter().position(|h| h.pane == stage.pane && h.opening.target == *target);
                let Some(handoff) = found.map(|i| self.handoffs.swap_remove(i)) else {
                    continue;
                };
                // Its real folder: what tmux reports can be out of date after a /cd.
                swaps.push((handoff, stage.folder.clone().or(self.pane(&stage.pane).map(|p| p.path.clone()))));
            }
        }
        self.handoffs.clear();
        let place = if panes.is_empty() { None } else { self.destination() };
        // Marked conversations open too, after the staged changes, with the marked agents joining them.
        let open = self.marked_conversations();
        let after = After::Apply {
            swaps,
            cds,
            detached,
            windows: self.staged_windows.clone(),
            panes,
            place,
            open,
        };
        self.apply(after);
    }

    /// The staged CLI changes, each as (agent pane, new CLI).
    fn staged_tools(&self) -> Vec<(String, String)> {
        self.staged
            .iter()
            .filter_map(|stage| Some((stage.pane.clone(), stage.tool.clone()?)))
            .collect()
    }

    /// Whether every staged CLI change has its conversation found.
    pub(super) fn handoffs_ready(&self) -> bool {
        let found = |(pane, target): &(String, String)| self.handoffs.iter().any(|h| h.pane == *pane && h.opening.target == *target);
        self.staged_tools().iter().all(found)
    }

    fn prepare_handoffs(&mut self) {
        let wanted = self.staged_tools();
        self.preparing = true;
        self.handoffs.clear();
        self.say("Getting the conversations ready…");
        let (agents, tx) = (self.agents.clone(), self.tx.clone());
        self.work.push(std::thread::spawn(move || {
            let prepared = wanted
                .iter()
                .map(|(pane, target)| history::prepare_live(&agents, pane, target).map_err(|error| (pane.clone(), error)))
                .collect();
            let _ = tx.send(Msg::Prepared(prepared));
        }));
    }
}
