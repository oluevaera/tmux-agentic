//! The board: its state and what each key does. State lives here, in one process, for as long as the popup is open.
use crate::agents::{Registry, Who};
use crate::config::Layout;
use crate::detect::{self, Pane, Window};
use crate::history::{self, Conversation, LiveHandoff, Opening};
use crate::input::Input;
use crate::keymap::{Action, Keymap, parse_key, same_key};
use crate::rows::{self, Detach, Kind, Mirror, NewAgent, Options, Row, RowId, Stage};
use crate::sessions::{self, Place, Where};
use crate::text::CARET;
use crate::{tmux, ui};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

mod actions;
mod apply;
mod keys;
mod notice;
mod workers;

pub use actions::finish_rename;
use apply::*;
use notice::Notice;
pub use notice::Tone;
use workers::*;

pub const LIVE_VIEWS: [&str; 2] = ["all", "waiting"];
pub const HISTORY: &str = "history";
/// How many folders the dropdown shows.
pub const DROPDOWN: usize = 8;
/// Esc pressed again this soon is still held down: a key repeats far faster.
const HELD: Duration = Duration::from_millis(200);

pub enum Msg {
    Event(Event),
    /// Time passing, while a message is on its clock: on a quiet screen nothing else would wake the board.
    Tick,
    /// config.toml was saved: it has been read again.
    Config,
    /// How something finished: what to say, and whether it went wrong.
    Outcome(Result<String, String>),
    /// How a compaction finished: the one outcome that lets another start.
    Compacted(Result<String, String>),
    /// Changes that didn't all go through: what to say, and what's left of them.
    Stopped(String, Ledger),
    /// The staged CLI changes' conversations, found (or one's reason not to go ahead): Enter goes on with them.
    Prepared(Result<Vec<LiveHandoff>, (String, String)>),
    /// `partial`: read without the Context column, which fills in right after.
    Panes {
        panes: Vec<Pane>,
        partial: bool,
    },
    Screen {
        pane: String,
        lines: Vec<String>,
    },
    Messages {
        row: RowId,
        messages: Vec<(Who, String)>,
    },
    History(Vec<Conversation>),
    Windows(Vec<Window>),
}

/// What the preview shows that takes a tmux call or a file read.
#[derive(Clone, PartialEq)]
pub enum Target {
    Nothing,
    Pane(String),
    Conversation(String, String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ConfirmAction {
    Kill,
    Compact,
}

#[derive(Clone, Debug)]
pub struct Rename {
    pub kind: Kind,
    pub target: String,
    pub input: Input,
    /// The current name, selected: typing replaces it, an arrow starts editing it at either end, Enter keeps it.
    pub selected: Option<String>,
}

pub struct FolderEdit {
    pub input: Input,
    pub suggested: Vec<String>,
    pub matches: Vec<String>,
    /// The highlighted one.
    pub index: usize,
    /// The agent being moved, in its own row; else it's the folder of the draft `draft`.
    pub pane: Option<String>,
    pub draft: u64,
    /// The folder it starts from, which . and .. are typed from.
    pub base: String,
}

pub enum Mode {
    Normal,
    Search(Input),
    Rename(Rename),
    Folder(FolderEdit),
}

/// Where a new agent lands, from where its draft sits in the tree.
#[derive(Clone, PartialEq, Debug)]
pub enum Spot {
    Pane { session: String, window: String },
    Window { session: String },
    Session,
}

impl Spot {
    pub fn place(&self) -> Where {
        match self {
            Spot::Pane { window, .. } => Where::Pane(window.clone()),
            Spot::Window { session } => Where::Window(session.clone()),
            Spot::Session => Where::Session,
        }
    }
}

#[derive(Clone)]
pub struct Draft {
    /// Its row's, for as long as it's there.
    pub id: u64,
    pub agent: String,
    pub spot: Spot,
    /// Where it was created: Shift+Tab comes back round to it.
    pub start: Spot,
    pub folder: String,
    pub anchor: RowId,
    pub place: String,
}

/// What runs once the board has closed: moving its host mid-draw could interrupt it.
/// A staged folder for an agent keeping its CLI: tool, pane, folder.
pub type CdMove = (String, String, String);

pub enum After {
    Close,
    Switch(String),
    Group {
        panes: Vec<String>,
    },
    /// Conversations in one new window, joined by any marked agents.
    Resume {
        list: Vec<Opening>,
        panes: Vec<String>,
    },
    /// Staged changes, then the marked agents to where the cursor is (a new window with no place).
    Apply {
        swaps: Vec<(LiveHandoff, Option<String>)>,
        cds: Vec<CdMove>,
        detached: Vec<(String, Detach)>,
        windows: Vec<String>,
        panes: Vec<String>,
        place: Option<Place>,
        /// Marked conversations: they open last, and the marked agents join them instead of moving on their own.
        open: Vec<Opening>,
    },
    /// New agents, (agent, folder), together in each place: sessions last, as starting one takes you there.
    Start(Vec<(Where, Vec<(String, String)>)>),
    Move {
        place: Place,
        panes: Vec<String>,
    },
}

#[derive(Default)]
pub struct Preview {
    pub lines: Vec<Line<'static>>,
    pub messages: Vec<(Who, String)>,
    /// Lines up from the bottom: it follows the newest output until scrolled.
    pub up: usize,
    pub height: usize,
    /// The newest screen, kept aside while scrolled up: shown once back at the bottom, as it's only sent once.
    pub held: Option<Vec<String>>,
}

/// A clickable part of the screen, recorded as it is drawn.
#[derive(Clone)]
pub enum Click {
    Tab(String),
    Key(KeyEvent),
    List,
    Preview,
    Folder(usize),
}

pub struct App {
    pub agents: Arc<Registry>,
    pub origin: String,
    pub panes: Vec<Pane>,
    pub windows: Vec<Window>,
    pub history: Option<Vec<Conversation>>,
    pub view: String,
    pub flat: bool,
    pub collapsed: HashSet<RowId>,
    pub rows: Vec<Row>,
    pub cursor: usize,
    pub offset: usize,
    pub marks: Vec<RowId>,
    /// Shift+Tab on a marked row: the whole selection, together, in a new window or a new tmux session.
    pub marks_detach: Option<Detach>,
    pub mode: Mode,
    pub switch: Option<(RowId, String)>,
    pub confirm: Option<(ConfirmAction, RowId)>,
    /// New agents waiting for Enter, in the order they were made.
    pub drafts: Vec<Draft>,
    /// The id the next draft gets.
    pub next_draft: u64,
    /// Nothing changes until Enter applies them all.
    pub staged: Vec<Stage>,
    /// Why the last change to an agent went wrong, by pane, until it's dismissed or tried again.
    pub failed: HashMap<String, String>,
    /// Staged changes from the plain list: the board stays to show a failure, and closes once they've gone through.
    closing: bool,
    /// Whole windows, likewise, for a new tmux session.
    pub staged_windows: Vec<String>,
    pub help: bool,
    /// How far the help is scrolled, and how far it can be: set as it's drawn.
    pub help_scroll: usize,
    pub help_most: usize,
    pub preview: Preview,
    pub regions: Vec<(Rect, Click)>,
    pub quit: Option<After>,
    /// What runs behind the board: closing it waits for these, so nothing stops halfway.
    work: Vec<std::thread::JoinHandle<()>>,
    /// Conversations found for the staged CLI changes, and whether that search is still on.
    handoffs: Vec<LiveHandoff>,
    preparing: bool,
    /// The message on the band.
    pub notice: Notice,
    compacting: bool,
    /// The board's keys, config.toml's over the defaults.
    pub keys: Keymap,
    /// Each tab's number key on it, as config.toml says.
    pub tab_numbers: bool,
    close_key: Option<KeyEvent>,
    /// tmux's prefix and the key that opens the board after it: pressed in the board, they close it.
    prefix_close: Option<(KeyEvent, KeyEvent)>,
    after_prefix: bool,
    /// When Esc was last pressed, and whether that came so soon after the one before that it's held down: Esc held to clear
    /// everything stops there, not closing the board, and the presses after it don't reach the agent under it.
    last_escape: Option<Instant>,
    escape_held: bool,
    /// When the last click landed, and on which row: by its id, as a refresh can reorder rows between two clicks.
    last_click: Option<(Instant, RowId)>,
    /// Whether the agents have been placed once: after that, agents coming and going never move the cursor.
    placed: bool,
    target: Target,
    tx: Sender<Msg>,
    wake_preview: Sender<Target>,
    wake_refresh: Sender<()>,
}

fn layout_path() -> std::path::PathBuf {
    tmux::history_dir().join("layout")
}

fn panes_path() -> std::path::PathBuf {
    tmux::history_dir().join(format!("panes-{}", tmux::server_id()))
}

/// Where it starts: config.toml's layout, or with "auto" wherever f left it last time (the list the first time).
fn read_flat() -> bool {
    match crate::config::get().board.layout {
        Layout::List => true,
        Layout::Tree => false,
        Layout::Auto => std::fs::read_to_string(layout_path()).map_or(true, |text| text.split_whitespace().next() == Some("on")),
    }
}

impl App {
    pub fn new(agents: Arc<Registry>, origin: String, tx: Sender<Msg>, wake_preview: Sender<Target>, wake_refresh: Sender<()>) -> Self {
        let cached = std::fs::read_to_string(panes_path()).unwrap_or_default();
        App {
            agents,
            origin,
            panes: cached.lines().filter_map(Pane::parse).collect(),
            windows: Vec::new(),
            history: history::cached(),
            view: "all".into(),
            flat: read_flat(),
            collapsed: HashSet::new(),
            rows: Vec::new(),
            cursor: 0,
            offset: 0,
            marks: Vec::new(),
            marks_detach: None,
            mode: Mode::Normal,
            switch: None,
            confirm: None,
            drafts: Vec::new(),
            next_draft: 0,
            staged: Vec::new(),
            failed: HashMap::new(),
            closing: false,
            staged_windows: Vec::new(),
            help: false,
            help_scroll: 0,
            help_most: 0,
            preview: Preview::default(),
            regions: Vec::new(),
            quit: None,
            work: Vec::new(),
            handoffs: Vec::new(),
            preparing: false,
            notice: Notice::default(),
            compacting: false,
            keys: crate::config::get().board.keys.clone(),
            tab_numbers: crate::config::get().board.tab_numbers,
            close_key: std::env::var("AGENTS_CLOSE_KEY").ok().and_then(|key| parse_key(&key)),
            prefix_close: std::env::var("AGENTS_PREFIX")
                .ok()
                .and_then(|key| parse_key(&key))
                .zip(std::env::var("AGENTS_OPEN_KEY").ok().and_then(|key| parse_key(&key))),
            after_prefix: false,
            last_escape: None,
            escape_held: false,
            last_click: None,
            placed: false,
            target: Target::Nothing,
            tx,
            wake_preview,
            wake_refresh,
        }
    }

    /// All, Needs you, then each installed agent: at most nine, so 1-9 reach every one (0 is History).
    pub fn views(&self) -> Vec<String> {
        LIVE_VIEWS
            .iter()
            .map(|v| v.to_string())
            .chain(self.agents.installed().iter().cloned())
            .take(9)
            .collect()
    }

    pub fn count(&self, view: &str) -> usize {
        self.panes.iter().filter(|p| rows::in_view(p, view)).count()
    }

    /// Empty tabs are dimmed, and h/l, their number and a click all pass them by.
    pub fn tab_open(&self, view: &str) -> bool {
        view == "all" || view == HISTORY || self.count(view) > 0
    }

    pub fn in_history(&self) -> bool {
        self.view == HISTORY
    }

    pub fn searching(&self) -> bool {
        matches!(self.mode, Mode::Search(_))
    }

    pub fn selected(&self) -> Option<&RowId> {
        self.rows.get(self.cursor).map(|row| &row.id)
    }

    fn past(&self) -> &[Conversation] {
        self.history.as_deref().unwrap_or_default()
    }

    pub fn pane(&self, id: &str) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    pub fn conversation(&self, tool: &str, id: &str) -> Option<&Conversation> {
        self.history.as_ref()?.iter().find(|c| c.tool == tool && c.id == id)
    }

    fn mirror(&self) -> Option<Mirror> {
        let Mode::Rename(rename) = &self.mode else { return None };
        // Renaming happens in the row itself, like a folder being typed: the terminal's cursor is there, nothing above the
        // list. An untouched name keeps it at its end.
        let (text, whole) = match &rename.selected {
            Some(name) => (format!("{name}{CARET}"), true),
            None => {
                let input = &rename.input;
                (format!("{}{CARET}{}", &input.text[..input.cursor], &input.text[input.cursor..]), false)
            }
        };
        Some(Mirror {
            kind: rename.kind,
            target: rename.target.clone(),
            text: text.replace(char::is_control, " "),
            whole,
        })
    }

    /// The list for the open tab, keeping the cursor on its row.
    pub fn rebuild(&mut self) {
        let keep = self.selected().cloned();
        let switch = self.switch.as_ref().map(|(row, target)| (row, target.as_str()));
        let mut list = if self.in_history() {
            match &self.history {
                Some(all) => rows::history(all, switch, &self.agents),
                None => rows::loading(),
            }
        } else {
            let mirror = self.mirror();
            let mut staged = self.staged.clone();
            if let Some(detach) = self.marks_detach {
                for pane in self.marked_panes() {
                    match staged.iter_mut().find(|stage| stage.pane == pane) {
                        Some(stage) => stage.detach = Some(detach),
                        None => staged.push(Stage {
                            pane,
                            folder: None,
                            tool: None,
                            detach: Some(detach),
                        }),
                    }
                }
            }
            // In the tree, new agents are drawn where they will start; elsewhere, under the row n was pressed on.
            let places: Vec<Where> = self.drafts.iter().map(|draft| draft.spot.place()).collect();
            let drafts: Vec<NewAgent> = if self.flat || self.searching() {
                Vec::new()
            } else {
                self.drafts
                    .iter()
                    .zip(&places)
                    .map(|(draft, place)| NewAgent {
                        id: draft.id,
                        agent: &draft.agent,
                        folder: &draft.folder,
                        place,
                        typed: self.typed_for(draft.id),
                    })
                    .collect()
            };
            let options = Options {
                drafts: &drafts,
                view: &self.view,
                flat: self.flat,
                compact: self.searching(),
                collapsed: &self.collapsed,
                mirror: mirror.as_ref(),
                staged: &staged,
                failed: &self.failed,
                staged_windows: &self.staged_windows,
                windows: &self.windows,
                folder: match &self.mode {
                    Mode::Folder(edit) => edit.pane.as_deref().map(|pane| (pane, edit.input.text.as_str())),
                    _ => None,
                },
            };
            rows::live(&self.panes, &options, &self.agents)
        };
        let unplaced: Vec<&Draft> = self
            .drafts
            .iter()
            .filter(|draft| !self.in_history() && !self.folded(draft) && !list.iter().any(|row| row.id == RowId::Draft(draft.id)))
            .collect();
        for draft in unplaced {
            let at = self.draft_row(draft, &list);
            let typed = self.typed_for(draft.id);
            let drawn = rows::draft(draft.id, &draft.agent, &draft.folder, &draft.place, "  ", typed, &self.agents);
            list.splice(at..at, drawn);
        }
        if let Mode::Search(input) = &self.mode {
            list.retain(|row| rows::matches(&row.text, &input.text));
        }
        self.rows = list;
        match keep.and_then(|id| self.rows.iter().position(|row| row.id == id)) {
            Some(at) => self.cursor = at,
            None => self.cursor = self.cursor.min(self.rows.len().saturating_sub(1)),
        }
        self.after_move(None);
    }

    /// Where a draft's row goes outside the tree: under the row n was pressed on, after the drafts already there.
    fn draft_row(&self, draft: &Draft, list: &[Row]) -> usize {
        let at = list.iter().position(|row| row.id == draft.anchor).map_or(list.len(), |at| at + 1);
        at + list[at..].iter().take_while(|row| matches!(row.id, RowId::Draft(_))).count()
    }

    /// In the tree, a draft whose session or window is folded, as the rest of it is.
    fn folded(&self, draft: &Draft) -> bool {
        let folded = |row: RowId| self.collapsed.contains(&row);
        !self.flat
            && !self.searching()
            && match &draft.spot {
                Spot::Pane { session, window } => folded(RowId::Session(session.clone())) || folded(RowId::Window(session.clone(), window.clone())),
                Spot::Window { session } => folded(RowId::Session(session.clone())),
                Spot::Session => false,
            }
    }

    /// A draft's folder as it's being typed.
    fn typed_for(&self, id: u64) -> Option<&str> {
        match &self.mode {
            Mode::Folder(edit) if edit.pane.is_none() && edit.draft == id => Some(edit.input.text.as_str()),
            _ => None,
        }
    }

    fn place_of(&self, spot: &Spot) -> String {
        match spot {
            Spot::Pane { window, .. } => match self.panes.iter().find(|p| p.window_id == *window) {
                Some(p) => format!("new pane in {} › {} {}", p.session, p.window_index, p.window),
                None => "new pane".to_string(),
            },
            Spot::Window { session } => {
                let name = self.panes.iter().find(|p| p.session_id == *session).map(|p| p.session.clone());
                let name = name.or_else(|| tmux::display(session, "#{session_name}")).unwrap_or_default();
                format!("new window in {name}")
            }
            Spot::Session => "open in a new tmux session".to_string(),
        }
    }

    pub fn moving_agent(&self) -> bool {
        matches!(&self.mode, Mode::Folder(edit) if edit.pane.is_some())
    }

    /// The draft the cursor is on.
    pub fn selected_draft(&self) -> Option<u64> {
        match self.selected() {
            Some(RowId::Draft(id)) => Some(*id),
            _ => None,
        }
    }

    pub fn draft(&self, id: u64) -> Option<&Draft> {
        self.drafts.iter().find(|draft| draft.id == id)
    }

    fn draft_mut(&mut self, id: u64) -> Option<&mut Draft> {
        self.drafts.iter_mut().find(|draft| draft.id == id)
    }

    /// The new window or session the cursor is on, which new agents will share.
    pub fn selected_group(&self) -> Option<Where> {
        match self.selected() {
            Some(RowId::NewWindow(session)) if !session.is_empty() => Some(Where::Window(session.clone())),
            Some(RowId::NewWindow(_) | RowId::NewSession) => Some(Where::Session),
            _ => None,
        }
    }

    /// The drafts going to `place`, in order.
    pub fn drafts_for(&self, place: &Where) -> Vec<u64> {
        self.drafts
            .iter()
            .filter(|draft| draft.spot.place() == *place)
            .map(|draft| draft.id)
            .collect()
    }

    /// The row that stands for where a draft goes: its own for a new pane, else the new window or session it shares.
    fn group_row(&self, id: u64) -> RowId {
        match self.draft(id).map(|draft| draft.spot.place()) {
            Some(Where::Window(session)) if self.selected_draft().is_none() => RowId::NewWindow(session),
            Some(Where::Session) if self.selected_draft().is_none() => RowId::NewSession,
            _ => RowId::Draft(id),
        }
    }

    /// Unfolds the session and window a draft goes in, so it can be seen there.
    fn reveal(&mut self, spot: &Spot) {
        match spot {
            Spot::Pane { session, window } => {
                self.collapsed.remove(&RowId::Session(session.clone()));
                self.collapsed.remove(&RowId::Window(session.clone(), window.clone()));
            }
            Spot::Window { session } => {
                self.collapsed.remove(&RowId::Session(session.clone()));
            }
            Spot::Session => {}
        }
    }

    /// Shift+Tab on a draft, or on a new window or session for all of its drafts: one level out, then back to where each
    /// was created.
    fn outdent_drafts(&mut self, ids: &[u64]) {
        for id in ids {
            let Some(draft) = self.draft(*id) else { continue };
            let spot = match &draft.spot {
                Spot::Pane { session, .. } => Spot::Window { session: session.clone() },
                Spot::Window { .. } => Spot::Session,
                Spot::Session => draft.start.clone(),
            };
            let place = self.place_of(&spot);
            self.reveal(&spot);
            if let Some(draft) = self.draft_mut(*id) {
                draft.spot = spot;
                draft.place = place;
            }
        }
        let row = ids.first().map(|id| self.group_row(*id));
        self.rebuild();
        if let Some(row) = row {
            self.select(&row);
        }
    }

    /// A row's effects once the cursor is on it: what was pending on the row it left is cancelled.
    fn after_move(&mut self, from: Option<RowId>) {
        let here = self.selected().cloned();
        if from.is_some() && from != here {
            self.preview.up = 0;
            if self.notice.text().starts_with("Can't rename") {
                self.notice.clear();
            }
        }
        if self.confirm.as_ref().is_some_and(|(_, row)| Some(row) != here.as_ref()) {
            self.confirm = None;
        }
        if self.switch.as_ref().is_some_and(|(row, _)| Some(row) != here.as_ref()) {
            self.switch = None;
            self.rebuild_keeping_cursor();
        }
        self.sync_preview();
    }

    fn rebuild_keeping_cursor(&mut self) {
        let at = self.cursor;
        self.rebuild();
        self.cursor = at.min(self.rows.len().saturating_sub(1));
    }

    fn sync_preview(&mut self) {
        let target = match self.selected() {
            Some(RowId::Pane(id)) if !self.help => Target::Pane(id.clone()),
            Some(RowId::Conversation(tool, id)) if !self.help => Target::Conversation(tool.clone(), id.clone()),
            _ => Target::Nothing,
        };
        if target != self.target {
            self.preview.lines.clear();
            self.preview.messages.clear();
            self.preview.up = 0;
            self.preview.held = None;
            self.target = target.clone();
            let _ = self.wake_preview.send(target);
        }
    }

    pub(super) fn show_screen(&mut self, lines: &[String]) {
        let mut style = ratatui::style::Style::default();
        self.preview.lines = lines.iter().map(|line| crate::text::ansi_line(line, &mut style)).collect();
    }

    fn move_to(&mut self, at: usize) {
        let from = self.selected().cloned();
        self.cursor = at.min(self.rows.len().saturating_sub(1));
        self.after_move(from);
    }

    pub fn select(&mut self, id: &RowId) {
        if let Some(at) = self.rows.iter().position(|row| row.id == *id) {
            self.move_to(at);
        }
    }

    fn first_item(&mut self) {
        let at = self
            .rows
            .iter()
            .position(|row| matches!(row.id, RowId::Pane(_) | RowId::Conversation(..)));
        self.move_to(at.unwrap_or(0));
    }

    /// Open where you are: on the agent pane the board was opened from, else (from a shell) the first agent in that
    /// window, else in that session, else the first agent.
    pub fn place_at_origin(&mut self) {
        self.placed = !self.panes.is_empty();
        self.rebuild();
        let window = std::env::var("AGENTS_ORIGIN_WINDOW").unwrap_or_default();
        let session = std::env::var("AGENTS_ORIGIN_SESSION").unwrap_or_default();
        let found = self.panes.iter().find(|p| p.id == self.origin);
        let found = found.or_else(|| self.panes.iter().find(|p| p.window_id == window));
        let found = found.or_else(|| self.panes.iter().find(|p| p.session_id == session));
        match found.map(|p| RowId::Pane(p.id.clone())) {
            Some(id) if self.rows.iter().any(|row| row.id == id) => self.select(&id),
            _ => self.first_item(),
        }
        self.sync_preview();
    }

    pub fn show_tab(&mut self, view: &str) {
        self.confirm = None;
        self.switch = None;
        if self.view != view {
            self.view = view.to_string();
            self.rebuild();
        }
        self.first_item();
    }

    fn step_tab(&mut self, forward: bool) {
        let mut tabs = self.views();
        tabs.push(HISTORY.to_string());
        let n = tabs.len();
        let at = tabs.iter().position(|v| *v == self.view).unwrap_or(0);
        let next = (1..=n)
            .map(|step| if forward { (at + step) % n } else { (at + n - step % n) % n })
            .find(|i| self.tab_open(&tabs[*i]));
        if let Some(view) = next.map(|i| tabs[i].clone()) {
            self.show_tab(&view);
        }
    }

    fn digit_tab(&mut self, digit: usize) {
        let view = if digit == 0 {
            Some(HISTORY.to_string())
        } else {
            self.views().get(digit - 1).cloned()
        };
        if let Some(view) = view.filter(|v| self.tab_open(v)) {
            self.show_tab(&view);
        }
    }

    pub fn open_tab(&mut self, view: &str) {
        if self.tab_open(view) {
            self.show_tab(view);
        }
    }

    fn toggle_flat(&mut self) {
        if self.in_history() {
            return;
        }
        self.flat = !self.flat;
        let _ = tmux::write_atomic(&layout_path(), if self.flat { "on\n" } else { "off\n" });
        self.rebuild();
    }

    pub fn on_msg(&mut self, msg: Msg) {
        self.expire(None);
        match msg {
            Msg::Event(Event::Key(key)) if key.kind != KeyEventKind::Release => self.on_key(key),
            Msg::Event(Event::Mouse(mouse)) => self.on_mouse(mouse),
            Msg::Event(_) | Msg::Tick => {}
            // Colors are read as they're drawn; the keys are kept, so they're taken again.
            Msg::Config => {
                self.keys = crate::config::get().board.keys.clone();
                self.tab_numbers = crate::config::get().board.tab_numbers;
                match crate::config::warnings().first() {
                    Some(first) => self.fail(first.clone()),
                    None => self.say("config.toml applied"),
                }
            }
            Msg::Panes { mut panes, partial } => {
                if partial {
                    for pane in &mut panes {
                        pane.context = self.pane(&pane.id).map_or_else(String::new, |old| old.context.clone());
                    }
                }
                self.staged.retain(|stage| panes.iter().any(|p| p.id == stage.pane));
                self.failed.retain(|pane, _| panes.iter().any(|p| p.id == *pane));
                // A window staged whole may have no agents at all: it goes only once the window does.
                self.staged_windows
                    .retain(|window| panes.iter().any(|p| p.window_id == *window) || self.windows.iter().any(|w| w.id == *window));
                self.marks.retain(|mark| match mark {
                    RowId::Pane(id) => panes.iter().any(|p| p.id == *id),
                    _ => true,
                });
                if self.marks.is_empty() {
                    self.marks_detach = None;
                }
                let first_look = !self.placed && !panes.is_empty();
                self.panes = panes;
                if first_look { self.place_at_origin() } else { self.rebuild() }
            }
            Msg::Screen { pane, lines } => {
                if self.target == Target::Pane(pane) {
                    if self.preview.up == 0 {
                        self.show_screen(&lines);
                    } else {
                        self.preview.held = Some(lines);
                    }
                }
            }
            Msg::Messages { row, messages } => {
                if self.selected() == Some(&row) {
                    self.preview.messages = messages;
                }
            }
            Msg::History(list) => {
                history::store(&list);
                self.history = Some(list);
                self.rebuild();
            }
            Msg::Windows(windows) => {
                self.windows = windows;
                self.rebuild();
            }
            Msg::Compacted(outcome) => {
                self.compacting = false;
                self.report(outcome);
                let _ = self.wake_refresh.send(());
            }
            Msg::Outcome(outcome) => {
                if self.closing && outcome.as_deref() == Ok("Done") {
                    self.quit = Some(After::Close);
                }
                self.report(outcome);
                let _ = self.wake_refresh.send(());
            }
            Msg::Prepared(prepared) => {
                self.preparing = false;
                match prepared {
                    Ok(handoffs) => {
                        self.handoffs = handoffs;
                        // Staged again while they were found: Enter decides again.
                        if self.handoffs_ready() {
                            self.apply_staged();
                        } else {
                            self.notice.clear();
                        }
                    }
                    Err((pane, error)) => {
                        self.failed = HashMap::from([(pane, error.clone())]);
                        self.fail(error);
                        self.rebuild();
                    }
                }
            }
            Msg::Stopped(text, ledger) => {
                self.closing = false;
                self.notice.stop(text, !ledger.failed.is_empty());
                self.failed.extend(ledger.failed);
                // What's left goes back, unless it was staged or marked again while the job ran: that newer one stands.
                for stage in ledger.stages {
                    let left = stage.folder.is_some() || stage.tool.is_some() || stage.detach.is_some();
                    if left && !self.staged.iter().any(|s| s.pane == stage.pane) {
                        self.staged.push(stage);
                    }
                }
                for window in ledger.windows {
                    if !self.staged_windows.contains(&window) {
                        self.staged_windows.push(window);
                    }
                }
                for pane in ledger.marks {
                    let mark = RowId::Pane(pane.clone());
                    if self.panes.iter().any(|p| p.id == pane) && !self.marks.contains(&mark) {
                        self.marks.push(mark);
                    }
                }
                let _ = self.wake_refresh.send(());
                self.rebuild();
                self.marks_detach = match ledger.place {
                    Some(Place::NewWindow) => Some(Detach::Window),
                    Some(Place::NewSession) => Some(Detach::Session),
                    _ => self.marks_detach,
                };
                let back = match ledger.place {
                    Some(Place::Session(session)) => Some(RowId::Session(session)),
                    Some(Place::Window(window)) => self
                        .windows
                        .iter()
                        .find(|w| w.id == window)
                        .map(|w| RowId::Window(w.session_id.clone(), window)),
                    Some(Place::NewWindow | Place::NewSession) | None => None,
                };
                if let Some(row) = back {
                    self.select(&row);
                }
            }
        }
    }
}

pub fn run() -> ExitCode {
    let agents = Arc::new(Registry::load());
    let origin = std::env::var("AGENTS_ORIGIN").unwrap_or_default();
    let (tx, rx) = channel();
    let (wake_preview, preview_wakes) = channel();
    let (wake_refresh, refresh_wakes) = channel();
    let mut app = App::new(agents.clone(), origin.clone(), tx.clone(), wake_preview, wake_refresh);
    app.place_at_origin();
    if let Some(first) = crate::config::warnings().first() {
        app.fail(first.clone());
    }
    tmux::connect(&std::env::var("AGENTS_ORIGIN_SESSION").unwrap_or_default());
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    start_workers(&agents, &tx, preview_wakes, refresh_wakes);
    let after = loop {
        if terminal.draw(|frame| ui::draw(frame, &mut app)).is_err() {
            break After::Close;
        }
        // An idle board sleeps until something happens; only a message showing needs waking.
        let msg = if app.notice.is_empty() {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            rx.recv_timeout(Duration::from_millis(500))
        };
        match msg {
            Ok(msg) => app.on_msg(msg),
            Err(RecvTimeoutError::Timeout) => app.on_msg(Msg::Tick),
            Err(RecvTimeoutError::Disconnected) => break After::Close,
        }
        // Everything already waiting before the next frame, so a held key never queues up behind drawing.
        while app.quit.is_none() {
            let Ok(msg) = rx.try_recv() else { break };
            app.on_msg(msg);
        }
        if let Some(after) = app.quit.take() {
            break after;
        }
    };
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    for work in app.work.drain(..) {
        let _ = work.join();
    }
    tmux::disconnect();
    let lines: Vec<String> = app.panes.iter().map(Pane::line).collect();
    let _ = tmux::write_atomic(&panes_path(), &lines.join("\n"));
    // Said, so done: a failure status would only have run-shell lay "returned 1" over the pane as well.
    if let Err(error) = finish(after, &agents, &origin, &tmux::display_message, &mut Ledger::default()) {
        tmux::display_message(&error);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
pub mod tests {
    use super::notice::tone;
    use super::*;
    use crate::detect::State;
    use crate::rows::tests::pane;
    use ratatui::crossterm::event::KeyEventState;

    pub fn app(panes: Vec<Pane>) -> App {
        let (tx, _rx) = channel();
        let (preview, _p) = channel();
        let (refresh, _r) = channel();
        let mut app = App::new(Arc::new(Registry::every_builtin()), "%0".into(), tx, preview, refresh);
        app.panes = panes;
        app.history = Some(Vec::new());
        app.flat = false;
        // As the board does as it opens with agents to show.
        app.placed = !app.panes.is_empty();
        app.rebuild();
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
    }

    fn key(app: &mut App, c: char) {
        press(app, KeyCode::Char(c));
    }

    pub fn sample() -> Vec<Pane> {
        vec![
            pane("%1", "a", 1, 0, State::Idle, "claude"),
            pane("%2", "a", 1, 1, State::Running, "codex"),
            pane("%3", "a", 2, 0, State::Waiting, "claude"),
            pane("%4", "b", 1, 0, State::Done, "codex"),
        ]
    }

    fn id(pane: &str) -> RowId {
        RowId::Pane(pane.to_string())
    }

    #[test]
    fn a_remapped_tab_key_opens_its_tab_and_the_digit_no_longer_does() {
        let mut app = app(sample());
        app.keys.bind(Action::Tab(0), vec![parse_key("y").unwrap()], &[]).unwrap();
        key(&mut app, '0');
        assert!(!app.in_history(), "0 is free now");
        key(&mut app, 'y');
        assert!(app.in_history());
    }

    #[test]
    fn j_and_k_stop_at_the_ends() {
        let mut app = app(sample());
        key(&mut app, 'k');
        assert_eq!(app.cursor, 0);
        key(&mut app, 'G');
        assert_eq!(app.cursor, app.rows.len() - 1);
        key(&mut app, 'j');
        assert_eq!(app.cursor, app.rows.len() - 1);
    }

    #[test]
    fn a_staged_window_without_agents_stays_staged_through_a_refresh() {
        let mut app = app(sample());
        app.windows.push(Window {
            id: "@free".into(),
            index: "9".into(),
            name: "spare".into(),
            session_id: "$a".into(),
            session: "a".into(),
        });
        app.staged_windows.push("@free".into());
        app.on_msg(Msg::Panes {
            panes: sample(),
            partial: false,
        });
        assert_eq!(app.staged_windows, ["@free"]);
    }

    #[test]
    fn clearing_marks_clears_where_they_were_going() {
        let mut app = app(sample());
        app.marks.push(id("%1"));
        app.marks_detach = Some(Detach::Window);
        press(&mut app, KeyCode::Esc);
        assert!(app.marks.is_empty() && app.marks_detach.is_none());
    }

    #[test]
    fn what_a_stopped_job_puts_back_doesnt_double_what_was_staged_again() {
        let mut app = app(sample());
        app.stage_folder("%1".into(), "/w/again".into());
        let ledger = Ledger {
            stages: app.staged.clone(),
            marks: vec!["%2".into()],
            ..Ledger::default()
        };
        app.marks.push(id("%2"));
        app.on_msg(Msg::Stopped("Stopped at step 1 of 1: no".into(), ledger));
        assert_eq!(app.staged.len(), 1);
        assert_eq!(app.marks, [id("%2")]);
    }

    #[test]
    fn only_a_compactions_own_outcome_lets_another_start() {
        let mut app = app(sample());
        app.compacting = true;
        app.on_msg(Msg::Outcome(Ok("Step 2 of 3".into())));
        assert!(app.compacting, "another job's progress");
        app.on_msg(Msg::Compacted(Ok("Asked Claude to compact".into())));
        assert!(!app.compacting);
    }

    #[test]
    fn the_cursor_stays_on_its_agent_when_a_refresh_moves_it() {
        let mut app = app(sample());
        app.select(&id("%3"));
        let mut panes = sample();
        panes.remove(0);
        app.on_msg(Msg::Panes { panes, partial: false });
        assert_eq!(app.selected(), Some(&id("%3")));
    }

    #[test]
    fn a_partial_refresh_keeps_the_context_column() {
        let mut panes = sample();
        panes[0].context = "11%".into();
        let mut app = app(panes);
        app.on_msg(Msg::Panes {
            panes: sample(),
            partial: true,
        });
        assert_eq!(app.pane("%1").unwrap().context, "11%");
    }

    #[test]
    fn headings_cannot_be_marked_and_enter_folds_them() {
        let mut app = app(sample());
        key(&mut app, ' ');
        assert!(app.marks.is_empty());
        let before = app.rows.len();
        press(&mut app, KeyCode::Enter);
        assert!(app.rows.len() < before);
        assert!(app.quit.is_none());
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.rows.len(), before);
    }

    #[test]
    fn marked_agents_go_where_the_cursor_is() {
        let mut app = app(sample());
        app.flat = true;
        app.rebuild();
        app.select(&id("%2"));
        key(&mut app, ' ');
        app.select(&id("%1"));
        key(&mut app, ' ');
        press(&mut app, KeyCode::Enter);
        let Some(After::Group { panes }) = app.quit.take() else {
            panic!("in the list an agent row groups them in a new window")
        };
        assert_eq!(panes, ["%2", "%1"]); // the order they were marked in
        app.flat = false;
        app.rebuild();
        app.select(&id("%4"));
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(app.quit.take(), Some(After::Move { place: Place::Window(w), .. }) if w == "@b1"),
            "in the tree they join the agent's window"
        );
        app.select(&RowId::Window("$b".into(), "@b1".into()));
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.quit.take(), Some(After::Move { place: Place::Window(w), .. }) if w == "@b1"));
        app.select(&RowId::Session("$b".into()));
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.quit.take(), Some(After::Move { place: Place::Session(s), .. }) if s == "$b"));
    }

    fn past(tool: &str, id: &str) -> Conversation {
        Conversation {
            tool: tool.into(),
            id: id.into(),
            time: 1,
            folder: "/w/past".into(),
            title: "Old".into(),
        }
    }

    fn opened(app: &mut App) -> (Vec<String>, Vec<String>) {
        let Some(After::Resume { list, panes, .. }) = app.quit.take() else {
            panic!("nothing opened")
        };
        (list.into_iter().map(|open| open.id).collect(), panes)
    }

    #[test]
    fn marked_agents_and_conversations_go_together() {
        let mut app = app(sample());
        app.history = Some(vec![past("claude", "one"), past("claude", "two")]);
        app.select(&id("%1"));
        key(&mut app, ' ');
        key(&mut app, '0');
        press(&mut app, KeyCode::Enter); // on a conversation that isn't marked
        assert_eq!(opened(&mut app), (vec!["one".to_string()], vec!["%1".to_string()]));
        key(&mut app, 'j');
        key(&mut app, ' ');
        key(&mut app, 'k');
        press(&mut app, KeyCode::Enter); // a marked one wins over the cursor
        assert_eq!(opened(&mut app), (vec!["two".to_string()], vec!["%1".to_string()]));
    }

    #[test]
    fn enter_goes_to_the_agent() {
        let mut app = app(sample());
        app.select(&id("%3"));
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.quit, Some(After::Switch(ref p)) if p == "%3"));
    }

    #[test]
    fn escape_clears_marks_before_closing() {
        let mut app = app(sample());
        app.select(&id("%1"));
        key(&mut app, ' ');
        press(&mut app, KeyCode::Esc);
        assert!(app.marks.is_empty() && app.quit.is_none());
        press(&mut app, KeyCode::Esc);
        assert!(app.quit.is_none(), "held down, it stops before closing");
        let_go(&mut app);
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.quit, Some(After::Close)), "pressed again, it closes");
    }

    /// As if Esc was let go of a while ago.
    fn let_go(app: &mut App) {
        app.last_escape = Instant::now().checked_sub(HELD * 2);
    }

    fn folders(app: &App) -> Vec<(String, String)> {
        app.staged
            .iter()
            .map(|stage| (stage.pane.clone(), stage.folder.clone().unwrap()))
            .collect()
    }

    #[test]
    fn a_failure_is_known_by_where_it_happened_not_by_its_wording() {
        assert_eq!(tone("Asked Claude to move to ~/api", true), Tone::Failed);
        assert_eq!(
            tone("Cannot move: the agent is running", false),
            Tone::Done,
            "the words alone decide nothing"
        );
        assert_eq!(tone("Working…", false), Tone::Working);
        assert_eq!(tone("Step 2 of 3: moving folders", false), Tone::Working);
        assert_eq!(tone("Done", false), Tone::Done);
    }

    #[test]
    fn a_failure_stays_until_escape_and_what_failed_is_staged_again() {
        let mut app = app(sample());
        let stage = |pane: &str, folder: Option<&str>| Stage {
            pane: pane.into(),
            folder: folder.map(str::to_string),
            tool: None,
            detach: None,
        };
        let ledger = Ledger {
            stages: vec![stage("%1", Some("/w/new")), stage("%2", None)], // %2 went through
            failed: vec![("%1".into(), "the agent is running".into())],
            ..Ledger::default()
        };
        app.on_msg(Msg::Stopped("Claude can't move: the agent is running".into(), ledger));
        assert_eq!(app.staged, [stage("%1", Some("/w/new"))]);
        assert!(app.rows.iter().any(|row| row.text.contains("✗") && row.id == id("%1")));
        key(&mut app, 'j');
        assert!(!app.notice.is_empty(), "moving on doesn't hide it");
        press(&mut app, KeyCode::Esc);
        assert!(app.notice.is_empty() && app.failed.is_empty(), "dismissed");
        assert_eq!(app.staged.len(), 1, "the change is still there");
        assert!(app.quit.is_none());
    }

    fn stopped(app: &mut App, on_a_row: bool) {
        let failed = if on_a_row {
            vec![("%1".to_string(), "the agent is running".to_string())]
        } else {
            Vec::new()
        };
        app.on_msg(Msg::Stopped("Stopped at step 1 of 2: no".into(), Ledger { failed, ..Ledger::default() }));
    }

    fn wait(app: &mut App, seconds: u64) {
        let text = app.notice.text.clone();
        app.notice.shown = Some((text, Instant::now() - Duration::from_secs(seconds)));
        app.expire(None);
    }

    #[test]
    fn a_refused_key_goes_with_the_next_key_or_in_a_few_seconds() {
        let mut app = app(sample());
        app.fail("Can't rename: the agent is running");
        key(&mut app, 'j');
        assert!(app.notice.is_empty());
        app.fail("Can't rename: the agent is running");
        wait(&mut app, 4);
        assert!(!app.notice.is_empty());
        wait(&mut app, 6);
        assert!(app.notice.is_empty());
    }

    #[test]
    fn escape_on_a_refusal_only_dismisses_it() {
        let mut app = app(sample());
        app.stage_folder("%1".into(), "/w/new".into());
        app.fail("Can't rename: the agent is running");
        press(&mut app, KeyCode::Esc);
        assert!(app.notice.is_empty());
        assert_eq!(app.staged.len(), 1, "the next layer is untouched");
        assert!(app.quit.is_none());
    }

    #[test]
    fn a_failures_band_goes_in_time_and_its_row_stays_until_dismissed() {
        let mut app = app(sample());
        stopped(&mut app, true);
        key(&mut app, 'j');
        assert!(!app.notice.is_empty(), "keys don't hide it");
        wait(&mut app, 7);
        assert!(!app.notice.is_empty());
        wait(&mut app, 9);
        assert!(app.notice.is_empty(), "the band is gone");
        assert_eq!(app.failed.len(), 1, "the row remembers");
        assert!(app.rows.iter().any(|row| row.text.contains("✗")));
        app.stage_folder("%1".into(), "/w/new".into());
        press(&mut app, KeyCode::Esc);
        assert!(app.failed.is_empty());
        assert_eq!(app.staged.len(), 1, "esc dismissed the failure, not the change");
    }

    #[test]
    fn a_failure_with_no_row_stays_until_dismissed() {
        let mut app = app(sample());
        stopped(&mut app, false);
        wait(&mut app, 60);
        key(&mut app, 'j');
        assert!(!app.notice.is_empty());
        press(&mut app, KeyCode::Esc);
        assert!(app.notice.is_empty() && app.quit.is_none());
    }

    #[test]
    fn a_failed_move_marks_them_again_with_the_cursor_where_they_were_going() {
        let mut app = app(sample());
        let ledger = Ledger {
            failed: vec![("%1".into(), "no space".into()), ("%2".into(), "no space".into())],
            marks: vec!["%1".into(), "%2".into()],
            place: Some(Place::Session("$b".into())),
            ..Ledger::default()
        };
        app.on_msg(Msg::Stopped("Could not move them all: no space".into(), ledger));
        assert_eq!(app.marks, [id("%1"), id("%2")]);
        assert_eq!(app.selected(), Some(&RowId::Session("$b".into())));
        assert_eq!(app.failed.len(), 2);
        assert!(app.tone() == Tone::Failed);
    }

    #[test]
    fn marking_a_staged_agent_hands_its_move_to_the_selection() {
        let mut app = app(sample());
        app.flat = false;
        app.rebuild();
        app.select(&id("%1"));
        app.stage_detach("%1".into());
        assert!(app.staged.iter().any(|s| s.pane == "%1" && s.detach.is_some()));
        key(&mut app, ' ');
        assert!(
            app.staged.iter().all(|s| s.detach.is_none()),
            "its own move is gone: it moves with the selection"
        );
        app.select(&id("%1"));
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.marks_detach, Some(Detach::Window), "Shift+Tab on a marked row is for the selection");
        assert!(app.staged.iter().all(|s| s.pane != "%1" || s.detach.is_none()));
        key(&mut app, ' ');
        assert!(app.marks.is_empty() && app.marks_detach.is_none());
    }

    #[test]
    fn an_action_on_a_marked_row_is_for_the_selection_and_elsewhere_for_its_row() {
        let mut app = app(sample());
        app.select(&id("%1"));
        key(&mut app, ' ');
        app.select(&id("%2"));
        key(&mut app, ' ');
        assert_eq!(app.targets(), [id("%1"), id("%2")]);
        app.select(&id("%3"));
        assert_eq!(app.targets(), [id("%3")]);
    }

    #[test]
    fn good_news_goes_with_the_next_key() {
        let mut app = app(sample());
        app.say("Asked Claude to move to ~/api");
        key(&mut app, 'j');
        assert!(app.notice.is_empty());
    }

    #[test]
    fn steps_run_in_order_and_a_failure_stops_the_rest() {
        let steps = |labels: &[&str]| labels.iter().map(|l| (l.to_string(), l.to_string())).collect::<Vec<_>>();
        let (mut ran, mut shown) = (Vec::new(), Vec::new());
        let outcome = in_order(
            steps(&["one", "two", "three", "four"]),
            |step| {
                ran.push(step.clone());
                if step == "two" { Err("it broke".to_string()) } else { Ok(()) }
            },
            |line| shown.push(line.to_string()),
        );
        assert_eq!(ran, ["one", "two"]);
        assert_eq!(shown, ["Step 1 of 4: one", "Step 2 of 4: two"]);
        assert_eq!(outcome, Err("Stopped at step 2 of 4: it broke. Steps 3-4 not run".to_string()));
        let failed_last = in_order(steps(&["one", "two"]), |s| if s == "two" { Err("no".into()) } else { Ok(()) }, |_| {});
        assert_eq!(failed_last, Err("Stopped at step 2 of 2: no".to_string()));
        let (mut count, mut shown) = (0, 0);
        assert_eq!(
            in_order(
                steps(&["only"]),
                |_| {
                    count += 1;
                    Ok(())
                },
                |_| shown += 1
            ),
            Ok(())
        );
        assert_eq!((count, shown), (1, 0), "one step needs no counting");
    }

    #[test]
    fn a_window_is_staged_whole_and_escape_drops_it() {
        let mut app = app(sample());
        let window = RowId::Window("$a".into(), "@a2".into());
        app.select(&window);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.staged_windows, ["@a2"]);
        assert!(app.has_staged());
        assert_eq!(app.selected(), Some(&window), "the cursor stays on it");
        press(&mut app, KeyCode::BackTab);
        assert!(!app.has_staged(), "and back");
        press(&mut app, KeyCode::BackTab);
        press(&mut app, KeyCode::Esc);
        assert!(!app.has_staged() && app.quit.is_none());
    }

    #[test]
    fn a_folder_and_a_cli_share_one_stage_and_it_stays_while_either_is_set() {
        let mut app = app(sample());
        app.stage_folder("%1".into(), "/w/new".into());
        app.change_stage("%1", |stage| stage.tool = Some("codex".into()));
        assert_eq!(app.staged.len(), 1);
        assert!(app.rows.iter().any(|r| r.text.contains("→ Codex") && r.text.contains("→ new")));
        app.stage_folder("%1".into(), "/w/api".into());
        assert_eq!(app.staged.len(), 1, "the CLI change is still there");
        app.change_stage("%1", |stage| stage.tool = None);
        assert!(app.staged.is_empty());
    }

    #[test]
    fn escape_leaves_a_search_before_touching_what_it_was_opened_over() {
        let mut app = app(sample());
        app.select(&id("%1"));
        key(&mut app, ' ');
        app.stage_folder("%1".into(), "/w/new".into());
        key(&mut app, '/');
        key(&mut app, 'A');
        assert!(app.searching());
        press(&mut app, KeyCode::Esc);
        assert!(!app.searching());
        assert_eq!(app.staged.len(), 1, "staged changes survive");
        assert_eq!(app.marks.len(), 1, "marks survive");
        assert!(app.quit.is_none());
        press(&mut app, KeyCode::Esc);
        assert!(app.staged.is_empty());
        assert_eq!(app.marks.len(), 1);
        press(&mut app, KeyCode::Esc);
        assert!(app.marks.is_empty() && app.quit.is_none());
        tree_draft(&mut app, Spot::Window { session: "$a".into() });
        key(&mut app, '/');
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.drafts.len(), 1, "the draft survives");
    }

    #[test]
    fn staged_changes_leave_the_board_free_and_escape_drops_them_before_closing() {
        let mut app = app(sample());
        app.select(&id("%1"));
        app.stage_folder("%1".into(), "/w/new".into());
        app.stage_folder("%2".into(), "/w/api".into()); // where it already is: no change
        assert_eq!(folders(&app), [("%1".to_string(), "/w/new".to_string())]);
        assert!(matches!(app.mode, Mode::Normal));
        key(&mut app, 'j');
        assert_eq!(app.selected(), Some(&id("%2")), "the cursor is free");
        press(&mut app, KeyCode::Esc);
        assert!(app.staged.is_empty() && app.quit.is_none());
        let_go(&mut app);
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.quit, Some(After::Close)));
    }

    #[test]
    fn staging_again_replaces_and_a_gone_agent_drops_out() {
        let mut app = app(sample());
        app.stage_folder("%1".into(), "/w/one".into());
        app.stage_folder("%1".into(), "/w/two".into());
        assert_eq!(folders(&app), [("%1".to_string(), "/w/two".to_string())]);
        app.stage_folder("%1".into(), "/w/api".into());
        assert!(app.staged.is_empty(), "back to its own folder");
        app.stage_folder("%1".into(), "/w/two".into());
        let mut panes = sample();
        panes.remove(0);
        app.on_msg(Msg::Panes { panes, partial: false });
        assert!(app.staged.is_empty());
    }

    #[test]
    fn confirmations_need_the_same_key_twice_on_the_same_row() {
        let mut app = app(sample());
        app.select(&id("%1"));
        key(&mut app, 'x');
        assert_eq!(app.confirm, Some((ConfirmAction::Kill, id("%1"))));
        key(&mut app, 'c'); // one never confirms the other
        assert_eq!(app.confirm, Some((ConfirmAction::Compact, id("%1"))));
        app.select(&id("%2"));
        assert_eq!(app.confirm, None);
        key(&mut app, 'x');
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.confirm, None);
        assert!(app.quit.is_none());
        key(&mut app, 'x');
        press(&mut app, KeyCode::Enter); // Enter cancels, it doesn't go to the agent
        assert_eq!(app.confirm, None);
        assert!(app.quit.is_none());
    }

    #[test]
    fn empty_tabs_stay_shut_and_keep_their_numbers() {
        let mut app = app(vec![pane("%1", "a", 1, 0, State::Idle, "claude")]);
        app.first_item();
        key(&mut app, '2'); // Needs you: nobody
        assert_eq!(app.view, "all");
        key(&mut app, 'l');
        assert_ne!(app.view, "waiting"); // passed by
        key(&mut app, '1');
        key(&mut app, 'h');
        assert_eq!(app.view, HISTORY);
        key(&mut app, 'h');
        assert_ne!(app.view, HISTORY);
    }

    #[test]
    fn every_tab_starts_on_its_first_agent() {
        let mut app = app(sample());
        key(&mut app, 'G');
        key(&mut app, '2');
        assert_eq!(app.selected(), Some(&id("%3")));
        assert_eq!(app.count("waiting"), 2, "Done waits for you too");
    }

    #[test]
    fn search_filters_every_agent_and_escape_restores_the_tree() {
        let mut app = app(sample());
        let tree = app.rows.len();
        key(&mut app, '/');
        for c in "%3".chars() {
            key(&mut app, c);
        }
        assert_eq!(app.rows.len(), 1);
        assert_eq!(app.selected(), Some(&id("%3")));
        press(&mut app, KeyCode::Esc);
        assert!(!app.searching());
        assert_eq!(app.rows.len(), tree);
        assert_eq!(app.selected(), Some(&id("%3")));
    }

    #[test]
    fn typing_a_search_never_triggers_keys() {
        let mut app = app(sample());
        key(&mut app, '/');
        for c in "qxrn".chars() {
            key(&mut app, c);
        }
        assert!(app.quit.is_none() && app.confirm.is_none() && app.drafts.is_empty());
        assert!(matches!(&app.mode, Mode::Search(input) if input.text == "qxrn"));
    }

    #[test]
    fn a_new_name_mirrors_in_its_row_until_escape() {
        let mut app = app(sample());
        app.mode = Mode::Rename(Rename {
            kind: Kind::Pane,
            target: "%1".into(),
            input: Input::default(),
            selected: None,
        });
        key(&mut app, 'h');
        key(&mut app, 'i');
        assert!(
            app.rows.iter().any(|r| r.text.contains(&format!("○ hi{CARET} "))),
            "typed in its row, the cursor there too"
        );
        assert_eq!(app.pane("%1").unwrap().name, "Agent %1");
        press(&mut app, KeyCode::Esc);
        assert!(!app.rows.iter().any(|r| r.text.contains(&format!("hi{CARET}"))));
    }

    #[test]
    fn a_selected_name_is_replaced_or_edited_at_either_end() {
        let renaming = |app: &mut App| {
            app.mode = Mode::Rename(Rename {
                kind: Kind::Window,
                target: "@a1".into(),
                input: Input::default(),
                selected: Some("old".into()),
            });
        };
        let text = |app: &App| match &app.mode {
            Mode::Rename(r) => (r.input.text.clone(), r.input.cursor),
            _ => panic!("not renaming"),
        };
        let mut app = app(sample());
        renaming(&mut app);
        key(&mut app, 'n');
        assert_eq!(text(&app), ("n".to_string(), 1));
        renaming(&mut app);
        press(&mut app, KeyCode::Left);
        assert_eq!(text(&app), ("old".to_string(), 0));
        renaming(&mut app);
        press(&mut app, KeyCode::Right);
        assert_eq!(text(&app), ("old".to_string(), 3));
        renaming(&mut app);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(text(&app), (String::new(), 0));
        // Ctrl+W at once, on the name the cursor sits after: its last word goes.
        let ctrl = |app: &mut App, c: char| app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        app.mode = Mode::Rename(Rename {
            kind: Kind::Window,
            target: "@a1".into(),
            input: Input::default(),
            selected: Some("fix the bug".into()),
        });
        ctrl(&mut app, 'w');
        assert_eq!(text(&app), ("fix the ".to_string(), 8));
        renaming(&mut app);
        ctrl(&mut app, 'u');
        assert_eq!(text(&app), (String::new(), 0));
        renaming(&mut app);
        ctrl(&mut app, 'z'); // nothing the box knows: the name stays whole
        assert!(matches!(&app.mode, Mode::Rename(r) if r.selected.as_deref() == Some("old")));
        for (code, cursor) in [(KeyCode::Home, 0), (KeyCode::End, 3)] {
            renaming(&mut app);
            press(&mut app, code);
            assert_eq!(text(&app), ("old".to_string(), cursor));
            key(&mut app, 'n');
            assert_eq!(text(&app).0, if cursor == 0 { "nold" } else { "oldn" });
        }
        renaming(&mut app);
        press(&mut app, KeyCode::Delete);
        assert!(matches!(&app.mode, Mode::Rename(r) if r.selected.is_none() && r.input.text.is_empty()));
        renaming(&mut app);
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
        assert!(matches!(&app.mode, Mode::Rename(r) if r.selected.as_deref() == Some("old")));
    }

    /// A draft for `agent` at `spot`, made from `anchor`, the cursor on it.
    fn add_draft(app: &mut App, agent: &str, spot: Spot, anchor: RowId) -> u64 {
        let id = app.next_draft;
        app.next_draft += 1;
        app.drafts.push(Draft {
            id,
            agent: agent.into(),
            start: spot.clone(),
            spot,
            folder: "/w".into(),
            anchor,
            place: String::new(),
        });
        app.rebuild();
        app.select(&RowId::Draft(id));
        id
    }

    fn tree_draft(app: &mut App, spot: Spot) -> u64 {
        add_draft(app, "claude", spot, id("%1"))
    }

    fn spot(app: &App) -> Spot {
        app.drafts.last().unwrap().spot.clone()
    }

    fn started(app: &App) -> &[(Where, Vec<(String, String)>)] {
        match &app.quit {
            Some(After::Start(groups)) => groups,
            _ => panic!("nothing started"),
        }
    }

    #[test]
    fn a_draft_escapes_back_to_where_n_was_pressed() {
        let mut app = app(sample());
        app.flat = true;
        app.rebuild();
        app.select(&id("%2"));
        add_draft(&mut app, "claude", Spot::Window { session: "$a".into() }, id("%2"));
        let at = app.rows.iter().position(|r| r.id == id("%2")).unwrap();
        assert_eq!(app.cursor, at + 1);
        press(&mut app, KeyCode::Esc);
        assert!(app.drafts.is_empty());
        assert_eq!(app.selected(), Some(&id("%2")));
    }

    #[test]
    fn escape_removes_the_draft_under_the_cursor_and_off_them_all() {
        let mut app = app(sample());
        let window = Spot::Window { session: "$a".into() };
        let first = tree_draft(&mut app, window.clone());
        let second = tree_draft(&mut app, window.clone());
        tree_draft(&mut app, window.clone());
        app.select(&RowId::Draft(second));
        press(&mut app, KeyCode::Esc);
        let left: Vec<u64> = app.drafts.iter().map(|d| d.id).collect();
        assert_eq!(left, [first, second + 1]);
        assert_eq!(app.selected(), Some(&RowId::Draft(first)), "back to the one made before it");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.selected(), Some(&RowId::Draft(second + 1)), "with none before, the one after");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.selected(), Some(&id("%1")), "with none left, where n was pressed");
        tree_draft(&mut app, window.clone());
        tree_draft(&mut app, Spot::Session);
        app.select(&id("%2"));
        press(&mut app, KeyCode::Esc);
        assert!(app.drafts.is_empty(), "off a draft, every one goes");
        assert!(app.quit.is_none());
    }

    #[test]
    fn n_on_a_draft_adds_another_just_like_it_after_it() {
        let mut app = app(sample());
        let window = Spot::Window { session: "$a".into() };
        let first = tree_draft(&mut app, window.clone());
        press(&mut app, KeyCode::Tab);
        tree_draft(&mut app, Spot::Session);
        app.select(&RowId::Draft(first));
        key(&mut app, 'n');
        let made: Vec<(&str, &Spot)> = app.drafts.iter().map(|d| (d.agent.as_str(), &d.spot)).collect();
        assert_eq!(made[1], (app.drafts[0].agent.as_str(), &window), "a copy, next to the one it came from");
        assert_eq!(app.selected(), Some(&RowId::Draft(app.drafts[1].id)));
        let rows: Vec<&RowId> = app.rows.iter().map(|r| &r.id).filter(|id| matches!(id, RowId::Draft(_))).collect();
        assert_eq!(
            rows,
            [&RowId::Draft(first), &RowId::Draft(app.drafts[1].id), &RowId::Draft(app.drafts[2].id)],
            "in that order"
        );
    }

    #[test]
    fn a_new_window_takes_n_shift_tab_and_escape_for_all_its_drafts() {
        let mut app = app(sample());
        let window = Spot::Window { session: "$a".into() };
        tree_draft(&mut app, window.clone());
        app.select(&RowId::NewWindow("$a".into()));
        key(&mut app, 'n');
        assert_eq!(app.drafts.len(), 2, "another in it");
        assert!(app.drafts.iter().all(|draft| draft.spot == window));
        app.select(&RowId::NewWindow("$a".into()));
        press(&mut app, KeyCode::BackTab);
        assert!(app.drafts.iter().all(|draft| draft.spot == Spot::Session), "the whole group out a level");
        assert_eq!(app.selected(), Some(&RowId::NewSession), "the cursor goes with it");
        press(&mut app, KeyCode::Esc);
        assert!(app.drafts.is_empty());
        assert!(app.quit.is_none());
    }

    #[test]
    fn a_draft_unfolds_where_it_goes() {
        let mut app = app(sample());
        let id = tree_draft(
            &mut app,
            Spot::Pane {
                session: "$a".into(),
                window: "@a1".into(),
            },
        );
        app.collapsed.insert(RowId::Session("$a".into()));
        app.rebuild();
        assert!(!app.rows.iter().any(|row| row.id == RowId::Draft(id)), "folded away");
        app.outdent_drafts(&[id]);
        assert!(app.collapsed.is_empty());
        assert!(app.rows.iter().any(|row| row.id == RowId::Draft(id)), "and there to see");
    }

    #[test]
    fn arrows_move_the_cursor_over_drafts() {
        let mut app = app(sample());
        tree_draft(
            &mut app,
            Spot::Pane {
                session: "$a".into(),
                window: "@a1".into(),
            },
        );
        let at = app.cursor;
        press(&mut app, KeyCode::Down);
        assert_eq!(app.cursor, at + 1);
        key(&mut app, 'k');
        key(&mut app, 'k');
        assert_eq!(app.cursor, at - 1);
        assert_eq!(
            spot(&app),
            Spot::Pane {
                session: "$a".into(),
                window: "@a1".into()
            },
            "the draft stays where it is"
        );
    }

    #[test]
    fn starting_a_draft_leaves_staged_changes_and_marks_as_they_were() {
        let mut app = app(sample());
        app.flat = false;
        app.stage_folder("%1".into(), "/w/new".into());
        app.marks.push(id("%2"));
        tree_draft(&mut app, Spot::Window { session: "$a".into() });
        press(&mut app, KeyCode::Enter);
        assert_eq!(started(&app).len(), 1, "the agent starts");
        assert_eq!(app.staged.len(), 1, "the staged change is still there to apply");
        assert_eq!(app.marks, [id("%2")], "and so is the mark");
    }

    #[test]
    fn shift_tab_steps_out_and_comes_back() {
        let mut app = app(sample());
        let start = Spot::Pane {
            session: "$a".into(),
            window: "@a2".into(),
        };
        tree_draft(&mut app, start.clone());
        press(&mut app, KeyCode::BackTab);
        assert_eq!(spot(&app), Spot::Window { session: "$a".into() });
        press(&mut app, KeyCode::BackTab);
        assert_eq!(spot(&app), Spot::Session);
        assert_eq!(app.drafts[0].place, "open in a new tmux session");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(spot(&app), start);
    }

    #[test]
    fn enter_on_a_draft_outside_every_session_makes_a_tmux_session() {
        let mut app = app(sample());
        tree_draft(&mut app, Spot::Session);
        press(&mut app, KeyCode::Enter);
        assert_eq!(started(&app)[0].0, Where::Session);
    }

    #[test]
    fn enter_starts_every_draft_together_by_place_new_sessions_last() {
        let mut app = app(sample());
        let pane = Spot::Pane {
            session: "$a".into(),
            window: "@a1".into(),
        };
        let window = Spot::Window { session: "$b".into() };
        add_draft(&mut app, "claude", Spot::Session, id("%1"));
        add_draft(&mut app, "codex", window.clone(), id("%1"));
        add_draft(&mut app, "codex", pane, id("%2"));
        add_draft(&mut app, "claude", window, id("%1"));
        press(&mut app, KeyCode::Enter);
        let agents = |list: &[(String, String)]| list.iter().map(|(agent, _)| agent.clone()).collect::<Vec<_>>();
        let groups: Vec<(Where, Vec<String>)> = started(&app).iter().map(|(at, list)| (at.clone(), agents(list))).collect();
        assert_eq!(
            groups,
            [
                (Where::Window("$b".into()), vec!["codex".to_string(), "claude".to_string()]),
                (Where::Pane("@a1".into()), vec!["codex".to_string()]),
                (Where::Session, vec!["claude".to_string()]),
            ]
        );
        assert!(app.drafts.is_empty());
    }

    #[test]
    fn arrows_in_the_flat_list_still_move_the_cursor() {
        let mut app = app(sample());
        app.flat = true;
        tree_draft(&mut app, Spot::Window { session: "$a".into() });
        let at = app.cursor;
        key(&mut app, 'k');
        assert_eq!(app.cursor, at - 1);
        assert_eq!(spot(&app), Spot::Window { session: "$a".into() });
    }

    #[test]
    fn enter_on_a_draft_starts_it() {
        let mut app = app(sample());
        let spot = Spot::Pane {
            session: "$a".into(),
            window: "@a1".into(),
        };
        add_draft(&mut app, "codex", spot, id("%2"));
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            started(&app),
            [(Where::Pane("@a1".into()), vec![("codex".to_string(), "/w".to_string())])]
        );
    }

    #[test]
    fn the_prefix_and_the_open_key_close_it_and_the_prefix_alone_does_nothing() {
        let mut app = app(sample());
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        app.prefix_close = Some((ctrl_s, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
        let start = app.cursor;
        app.on_key(ctrl_s);
        key(&mut app, 'j');
        assert!(
            app.quit.is_none() && app.cursor == start + 1,
            "the prefix then another key: that key, as ever"
        );
        app.on_key(ctrl_s);
        key(&mut app, 'a');
        assert!(matches!(app.quit, Some(After::Close)));
    }

    #[test]
    fn the_close_key_closes_from_any_screen() {
        let mut app = app(sample());
        app.close_key = parse_key("C-g");
        app.mode = Mode::Search(Input::default());
        app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert!(matches!(app.quit, Some(After::Close)));
    }
}
