//! The list's rows: a tree of sessions, their windows and the agents in them (or just the agents), with aligned columns.
use crate::agents::Registry;
use crate::detect::{Pane, State, Window};
use crate::history::Conversation;
use crate::sessions::Where;
use crate::text::{CARET, bold, cells, dim, filled, fit, tail};
use crate::tmux;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use std::collections::{HashMap, HashSet};

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum RowId {
    Session(String),
    Window(String, String),
    Pane(String),
    Conversation(String, String),
    Loading,
    /// A new agent waiting for Enter, by its id.
    Draft(u64),
    /// The new window new agents will share, in a session; "": the new tmux session's.
    NewWindow(String),
    /// The new tmux session new agents will share.
    NewSession,
}

impl RowId {
    pub fn markable(&self) -> bool {
        matches!(self, RowId::Pane(_) | RowId::Conversation(..))
    }
}

pub struct Row {
    pub id: RowId,
    pub line: Line<'static>,
    /// What search matches against: the text shown.
    pub text: String,
}

/// Where an agent is staged to go on its own.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Detach {
    Window,
    Session,
}

/// What's chosen for an agent until Enter applies it: any of a folder, another CLI and a place of its own.
#[derive(Clone, PartialEq, Debug)]
pub struct Stage {
    pub pane: String,
    pub folder: Option<String>,
    pub tool: Option<String>,
    pub detach: Option<Detach>,
}

/// A name being typed, mirrored in its row; tmux and the agent aren't touched until Enter.
#[derive(Clone, Debug)]
pub struct Mirror {
    pub kind: Kind,
    pub target: String,
    /// The name as typed, with a CARET where the cursor is.
    pub text: String,
    /// The old name, still whole: typing replaces it.
    pub whole: bool,
}

impl Mirror {
    /// How the name being typed looks in its row: the old one on the folder dropdown's dark blue until it's touched,
    /// like a selection.
    fn style(&self, base: Style) -> Style {
        if self.whole { base.bg(crate::text::field()) } else { base }
    }

    fn edits(&self, kind: Kind, target: &str) -> bool {
        self.kind == kind && self.target == target
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Session,
    Window,
    Pane,
}

pub struct Options<'a> {
    pub view: &'a str,
    pub flat: bool,
    /// Search rows: each with its full session/window path, nothing folded.
    pub compact: bool,
    pub collapsed: &'a HashSet<RowId>,
    pub mirror: Option<&'a Mirror>,
    /// The agent whose folder is being typed, and the text so far.
    pub folder: Option<(&'a str, &'a str)>,
    /// Changes chosen for agents, not applied yet.
    pub staged: &'a [Stage],
    /// Why the last change to an agent went wrong, by pane.
    pub failed: &'a HashMap<String, String>,
    /// Whole windows staged for a new tmux session.
    pub staged_windows: &'a [String],
    /// Every window: those without agents are places to send agents to.
    pub windows: &'a [Window],
    /// New agents waiting for Enter, drawn in the tree where they will start.
    pub drafts: &'a [NewAgent<'a>],
}

/// A new agent waiting for Enter.
pub struct NewAgent<'a> {
    pub id: u64,
    pub agent: &'a str,
    pub folder: &'a str,
    pub place: &'a Where,
    /// Its folder as it's being typed.
    pub typed: Option<&'a str>,
}

pub fn in_view(pane: &Pane, view: &str) -> bool {
    view == "all" || pane.tool == view || (view == "waiting" && matches!(pane.state, State::Waiting | State::Done))
}

fn row(id: RowId, spans: Vec<Span<'static>>) -> Row {
    let text = spans.iter().map(|s| s.content.as_ref()).collect();
    Row {
        id,
        line: Line::from(spans),
        text,
    }
}

fn agent_row(pane: &Pane, prefix: String, width: usize, o: &Options, agents: &Registry) -> Row {
    let editing = o.mirror.is_some_and(|m| m.edits(Kind::Pane, &pane.id));
    let mut title = if editing { pane.name.clone() } else { pane.shown_name(agents) };
    if o.compact {
        title = format!("{} › {} {} › {title}", pane.session, pane.window_index, pane.window);
    } else if editing && cells(&title) > width {
        // A name being typed keeps the column width too: when it is longer, its end shows, so the columns never move.
        title = format!("…{}", tail(&title, width - 1));
    }
    let title = fit(&title, width, !o.compact); // A search row is never cut: search only matches what it shows.
    let id = RowId::Pane(pane.id.clone());
    let tint = Style::new().fg(agents.tint(&pane.tool));
    let name = agents.name(&pane.tool);
    let stage = o.staged.iter().find(|stage| stage.pane == pane.id);
    let (kind, doing) = match stage.and_then(|stage| stage.tool.as_deref()) {
        Some(target) => (
            switched_to(target, 11, agents),
            format!("({name} conversation, continued in {})", agents.name(target)),
        ),
        None => {
            let doing = if cells(&pane.doing) > 50 {
                format!("{}…", fit(&pane.doing, 49, true).trim_end())
            } else {
                pane.doing.clone()
            };
            (vec![Span::styled(fit(&name, 11, true), tint)], doing)
        }
    };
    let hue = Style::new().fg(pane.state.color());
    let failed = o.failed.get(&pane.id);
    let dot = match failed {
        Some(_) => Span::styled("✗", filled(crate::text::palette().failed)),
        None => Span::styled(pane.state.dot(), hue),
    };
    let mut spans = vec![Span::styled(prefix, dim()), dot, Span::raw(" ")];
    // Only the name itself is highlighted, not the rest of its column.
    let name_style = o.mirror.filter(|_| editing).map_or(bold(), |m| m.style(bold()));
    let shown = title.trim_end();
    spans.push(Span::styled(shown.to_string(), name_style));
    spans.push(Span::raw(" ".repeat(cells(&title) - cells(shown))));
    spans.push(Span::raw("  "));
    spans.extend(kind);
    let detach = stage.and_then(|stage| stage.detach);
    let folder = match stage.and_then(|stage| stage.folder.as_ref()) {
        Some(folder) => Span::styled(
            fit(&format!("→ {}", tmux::folder_name(folder)), 12, true),
            bold().fg(crate::text::marker()),
        ),
        None => Span::styled(fit(&pane.folder, 12, true), dim()),
    };
    match o.folder.filter(|(typing, _)| *typing == pane.id) {
        Some((_, typed)) => spans.extend(typed_folder(typed)),
        None => spans.extend([
            folder,
            Span::raw("  "),
            Span::styled(fit(pane.state.label(), 11, false), hue),
            Span::raw(if detach.is_some() { String::new() } else { fit(&pane.context, 9, false) }), // room for its longer note
        ]),
    }
    match (failed, detach) {
        (Some(reason), _) => spans.push(Span::styled(reason.clone(), filled(crate::text::palette().failed))),
        (_, Some(Detach::Window)) => spans.push(Span::styled("open in a new tmux window", bold().fg(crate::text::marker()))),
        (_, Some(Detach::Session)) => spans.push(Span::styled("open in a new tmux session", bold().fg(crate::text::marker()))),
        (None, None) => spans.push(Span::styled(doing, dim())),
    }
    row(id, spans)
}

/// A folder being typed: the path (its end, where you type) over Folder, Status and Context.
fn typed_folder(typed: &str) -> [Span<'static>; 3] {
    let shown = if cells(typed) > 32 {
        format!("…{}", tail(typed, 31))
    } else {
        typed.to_string()
    };
    // The cursor right after what's typed, the rest of the room after it.
    let rest = 33 - cells(&shown);
    [Span::styled(shown, bold()), Span::raw(CARET.to_string()), Span::raw(" ".repeat(rest))]
}

/// The rows of a live tab.
pub fn live(panes: &[Pane], o: &Options, agents: &Registry) -> Vec<Row> {
    let mut list: Vec<Pane> = panes.iter().filter(|p| in_view(p, o.view)).cloned().collect();
    if let Some(m) = o.mirror {
        for pane in &mut list {
            match m.kind {
                Kind::Session if pane.session_id == m.target => pane.session = m.text.clone(),
                Kind::Window if pane.window_id == m.target => pane.window = m.text.clone(),
                Kind::Pane if pane.id == m.target => pane.name = m.text.clone(),
                _ => {}
            }
        }
    }
    let flat = o.flat && !o.compact;
    if flat {
        // What needs you first; the session order within each state.
        list.sort_by_key(|p| State::ORDER.iter().position(|s| *s == p.state));
    }
    if flat || o.compact {
        let (prefix, width) = if o.compact { ("", 40) } else { ("  ", 30) }; // the tree's indent goes to the name, so the columns stay put
        return list.iter().map(|p| agent_row(p, prefix.to_string(), width, o, agents)).collect();
    }
    let leaving = |pane: &Pane| o.staged.iter().find(|stage| stage.pane == pane.id).and_then(|stage| stage.detach);
    let mut session_agents: HashMap<&str, usize> = HashMap::new();
    let mut remaining: HashMap<(&str, &str), usize> = HashMap::new();
    for pane in &list {
        *session_agents.entry(&pane.session_id).or_default() += 1;
        // A row staged to leave is drawn where it's going, so it doesn't count here.
        *remaining.entry((&pane.session_id, &pane.window_id)).or_default() += usize::from(leaving(pane).is_none());
    }
    // New agents for a window come after its agents, so its branches carry on to them.
    let drafts_in = |place: &Where| o.drafts.iter().filter(|d| d.place == place).count();
    for ((_, window), left) in remaining.iter_mut() {
        *left += drafts_in(&Where::Pane(window.to_string()));
    }
    let mut windows: HashMap<&str, usize> = HashMap::new();
    for (session, _) in remaining.keys() {
        *windows.entry(session).or_default() += 1;
    }
    // Windows with no agents, as dim rows to send agents to; only All shows everything there is.
    let free: Vec<&Window> = o
        .windows
        .iter()
        .filter(|w| o.view == "all" && !list.iter().any(|p| p.window_id == w.id))
        .collect();
    for window in &free {
        *windows.entry(&window.session_id).or_default() += 1; // so the tree's branches carry on to them
    }
    // A new window for new agents is the session's last.
    let new_window_in = |session: &str| drafts_in(&Where::Window(session.to_string())) > 0;
    for (session, count) in windows.iter_mut() {
        *count += usize::from(new_window_in(session));
    }
    // New agents under `stem`, the branches of the rows above them, each with its own branch.
    let draft_rows = |place: &Where, stem: &str| -> Vec<Row> {
        let here: Vec<&NewAgent> = o.drafts.iter().filter(|d| d.place == place).collect();
        let note = if matches!(place, Where::Pane(_)) { "new pane" } else { "" };
        let mut rows = Vec::new();
        for (i, d) in here.iter().enumerate() {
            let branch = if i + 1 < here.len() { "├─ " } else { "└─ " };
            rows.extend(draft(d.id, d.agent, d.folder, note, &format!("{stem}{branch}"), d.typed, agents));
        }
        rows
    };
    let new_window = |session: &str| -> Vec<Row> {
        if !new_window_in(session) {
            return Vec::new();
        }
        let mut rows = vec![row(
            RowId::NewWindow(session.to_string()),
            vec![
                Span::styled("  └─ ", dim()),
                Span::styled("+ new window", bold().fg(crate::text::marker())),
            ],
        )];
        rows.extend(draft_rows(&Where::Window(session.to_string()), "     "));
        rows
    };
    let spare = |session: &str| -> Vec<Row> {
        let mut here = free.iter().filter(|w| w.session_id == session).peekable();
        let then = new_window_in(session);
        let mut rows = Vec::new();
        while let Some(w) = here.next() {
            let id = RowId::Window(w.session_id.clone(), w.id.clone());
            let more = here.peek().is_some() || then;
            let branch = if more { "├─" } else { "└─" };
            // Renamed in its row like any window: what's typed, on the field's blue while the old name is whole.
            let mut spans = match o.mirror.filter(|m| m.edits(Kind::Window, &w.id)) {
                Some(m) => vec![
                    Span::styled(format!("  {branch} {} ", w.index), dim()),
                    Span::styled(m.text.clone(), m.style(Style::new())),
                ],
                None => vec![Span::styled(format!("  {branch} {} {}", w.index, w.name), dim())],
            };
            if o.staged_windows.contains(&w.id) {
                spans = window_leaving(&w.index, &w.name);
            }
            rows.push(row(id, spans));
            rows.extend(draft_rows(&Where::Pane(w.id.clone()), if more { "  │  " } else { "     " }));
        }
        rows
    };
    // A window's new agents, once its agents are drawn.
    let window_drafts =
        |window: &str, last_of_session: bool| draft_rows(&Where::Pane(window.to_string()), if last_of_session { "     " } else { "  │  " });
    let mut rows = Vec::new();
    let mut session_end: Vec<Row> = Vec::new();
    let mut outside: Vec<Row> = Vec::new();
    let (mut last_session, mut last_window): (Option<&str>, Option<&str>) = (None, None);
    let (mut hide_session, mut hide_window, mut last_of_session) = (false, false, false);
    for pane in &list {
        if let Some(window) = last_window.filter(|window| *window != pane.window_id)
            && !hide_session
            && !hide_window
        {
            rows.extend(window_drafts(window, last_of_session));
        }
        if last_session != Some(pane.session_id.as_str()) {
            rows.append(&mut session_end);
            if let Some(previous) = last_session.filter(|_| !hide_session) {
                rows.extend(spare(previous));
                rows.extend(new_window(previous));
            }
            let id = RowId::Session(pane.session_id.clone());
            hide_session = o.collapsed.contains(&id);
            let n = session_agents[pane.session_id.as_str()];
            let name_style = o
                .mirror
                .filter(|m| m.edits(Kind::Session, &pane.session_id))
                .map_or(bold(), |m| m.style(bold()));
            let mut spans = vec![
                Span::styled(pane.session.clone(), name_style),
                Span::styled(format!("  {n} {}", if n == 1 { "agent" } else { "agents" }), dim()),
            ];
            if hide_session {
                spans.push(Span::raw(" ▸"));
            }
            rows.push(row(id, spans));
            last_window = None;
        }
        if last_window != Some(pane.window_id.as_str()) {
            let left = windows.get_mut(pane.session_id.as_str()).unwrap();
            *left -= 1;
            last_of_session = *left == 0;
            let id = RowId::Window(pane.session_id.clone(), pane.window_id.clone());
            hide_window = o.collapsed.contains(&id);
            if !hide_session {
                let name_style = o
                    .mirror
                    .filter(|m| m.edits(Kind::Window, &pane.window_id))
                    .map_or(Style::new(), |m| m.style(Style::new()));
                let mut spans = vec![
                    Span::styled(format!("  {}", if last_of_session { "└─ " } else { "├─ " }), dim()),
                    Span::raw(format!("{} ", pane.window_index)),
                    Span::styled(pane.window.clone(), name_style),
                ];
                if o.staged_windows.contains(&pane.window_id) {
                    spans = window_leaving(&pane.window_index.to_string(), &pane.window);
                }
                if hide_window {
                    spans.push(Span::raw(" ▸"));
                }
                rows.push(row(id, spans));
            }
        }
        let left = remaining.get_mut(&(pane.session_id.as_str(), pane.window_id.as_str())).unwrap();
        // Staged to leave, it steps out to the level it will live at and waits at the end of that level; the name takes up
        // the room, so the columns stay put.
        match leaving(pane) {
            Some(Detach::Window) if !hide_session => session_end.push(agent_row(pane, "  ├─ ".to_string(), 27, o, agents)),
            Some(Detach::Session) => outside.push(agent_row(pane, String::new(), 32, o, agents)),
            Some(_) => {}
            None => {
                *left -= 1;
                let prefix = format!(
                    "  {}{}",
                    if last_of_session { "   " } else { "│  " },
                    if *left > 0 { "├─ " } else { "└─ " }
                );
                if !hide_session && !hide_window {
                    rows.push(agent_row(pane, prefix, 24, o, agents));
                }
            }
        }
        last_session = Some(&pane.session_id);
        last_window = Some(&pane.window_id);
    }
    if let Some(window) = last_window
        && !hide_session
        && !hide_window
    {
        rows.extend(window_drafts(window, last_of_session));
    }
    rows.append(&mut session_end);
    if let Some(previous) = last_session.filter(|_| !hide_session) {
        rows.extend(spare(previous));
        rows.extend(new_window(previous));
    }
    // Sessions with no agents at all.
    let mut seen: Vec<&str> = list.iter().map(|p| p.session_id.as_str()).collect();
    for window in o.windows.iter().filter(|_| o.view == "all") {
        if seen.contains(&window.session_id.as_str()) {
            continue;
        }
        seen.push(&window.session_id);
        let id = RowId::Session(window.session_id.clone());
        let name = match o.mirror.filter(|m| m.edits(Kind::Session, &window.session_id)) {
            Some(m) => Span::styled(m.text.clone(), m.style(bold())),
            None => Span::styled(window.session.clone(), dim()),
        };
        rows.push(row(id, vec![name, Span::styled("  0 agents", dim())]));
        rows.extend(spare(&window.session_id));
        rows.extend(new_window(&window.session_id));
    }
    rows.append(&mut outside);
    // New agents for a new tmux session: it, its first window, and them in it.
    if drafts_in(&Where::Session) > 0 {
        let staged = |text: &str| Span::styled(text.to_string(), bold().fg(crate::text::marker()));
        rows.push(row(RowId::NewSession, vec![staged("+ new tmux session")]));
        rows.push(row(
            RowId::NewWindow(String::new()),
            vec![Span::styled("└─ ", dim()), staged("+ new window")],
        ));
        rows.extend(draft_rows(&Where::Session, "   "));
    }
    rows
}

/// A window staged for a new tmux session, stepped out to the sessions' level.
fn window_leaving(index: &str, name: &str) -> Vec<Span<'static>> {
    vec![
        Span::raw(fit(&format!("{index} {name}"), 34, true)),
        Span::styled("open in a new tmux session", bold().fg(crate::text::marker())),
    ]
}

fn ago(seconds: i64) -> String {
    match seconds {
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// The CLI a row is about to continue in: the arrow in the blue of every staged change, the name in its own color.
pub fn switched_to(target: &str, width: usize, agents: &Registry) -> Vec<Span<'static>> {
    vec![
        Span::styled("→ ", bold().fg(crate::text::marker())),
        Span::styled(fit(&agents.name(target), width - 2, true), bold().fg(agents.tint(target))),
    ]
}

/// History's rows, like the live list: the conversation bold, the age and folder dim.
pub fn history(list: &[Conversation], switch: Option<(&RowId, &str)>, agents: &Registry) -> Vec<Row> {
    let now = tmux::now();
    list.iter()
        .map(|c| {
            let id = RowId::Conversation(c.tool.clone(), c.id.clone());
            let switched = switch.filter(|(row, _)| **row == id);
            let agent = match switched {
                Some((_, target)) => switched_to(target, 11, agents),
                None => vec![Span::styled(fit(&agents.name(&c.tool), 11, true), Style::new().fg(agents.tint(&c.tool)))],
            };
            let title = if c.title.is_empty() { "(untitled)" } else { &c.title };
            let mut spans = vec![Span::styled(format!("{:>4}", ago(now - c.time)), dim()), Span::raw("  ")];
            spans.extend(agent);
            spans.extend([
                Span::raw(" "),
                Span::styled(fit(&tmux::folder_name(&c.folder), 16, true), dim()),
                Span::raw(" "),
                Span::styled(title.to_string(), bold()),
            ]);
            if let Some((_, target)) = switched {
                spans.push(Span::styled(
                    format!("  ({} conversation, continued in {})", agents.name(&c.tool), agents.name(target)),
                    dim(),
                ));
            }
            row(id, spans)
        })
        .collect()
}

pub fn loading() -> Vec<Row> {
    vec![row(RowId::Loading, vec![Span::styled("  Loading history…", dim())])]
}

/// The draft for a new agent: "+ New agent", its agent in the Kind column, where it will start in line with a staged row's note. While its
/// folder is typed: the path (its end, where you type) over Folder, Status and Context, and the matching folders under it.
pub fn draft(id: u64, agent: &str, folder: &str, place: &str, prefix: &str, typed: Option<&str>, agents: &Registry) -> Vec<Row> {
    let mut spans = vec![
        Span::styled(format!("{prefix}+ "), dim()),
        Span::styled(fit("New agent", 32 - cells(prefix), true), bold()),
        Span::raw("  "),
        Span::styled(fit(&agents.name(agent), 11, true), Style::new().fg(agents.tint(agent))),
    ];
    match typed {
        Some(typed) => spans.extend(typed_folder(typed)),
        None => spans.extend([Span::styled(fit(&tmux::folder_name(folder), 12, true), dim()), Span::raw(" ".repeat(13))]),
    }
    spans.push(Span::styled(place.to_string(), bold().fg(crate::text::marker())));
    vec![row(RowId::Draft(id), spans)]
}

/// fzf-like matching: every space-separated term is found in order, ignoring case unless it has capitals.
pub fn matches(text: &str, query: &str) -> bool {
    let lower = text.to_lowercase();
    query.split_whitespace().all(|term| {
        let exact = term.chars().any(char::is_uppercase);
        let mut letters = if exact {
            text.chars().collect::<Vec<_>>().into_iter()
        } else {
            lower.chars().collect::<Vec<_>>().into_iter()
        };
        term.chars().all(|wanted| letters.any(|c| c == wanted))
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn pane(id: &str, session: &str, window: u32, index: u32, state: State, tool: &str) -> Pane {
        Pane {
            id: id.into(),
            state,
            doing: "doing".into(),
            tool: tool.into(),
            folder: "api".into(),
            name: format!("Agent {id}"),
            session: session.into(),
            window: format!("w{window}"),
            window_id: format!("@{session}{window}"),
            window_index: window,
            pane_index: index,
            session_id: format!("${session}"),
            path: "/w/api".into(),
            context: String::new(),
        }
    }

    fn registry() -> Registry {
        Registry::load()
    }

    fn options<'a>(collapsed: &'a HashSet<RowId>) -> Options<'a> {
        Options {
            view: "all",
            flat: false,
            compact: false,
            collapsed,
            mirror: None,
            folder: None,
            staged: &[],
            failed: Box::leak(Box::default()),
            staged_windows: &[],
            windows: &[],
            drafts: &[],
        }
    }

    fn sample() -> Vec<Pane> {
        vec![
            pane("%1", "a", 1, 0, State::Idle, "claude"),
            pane("%2", "a", 1, 1, State::Running, "codex"),
            pane("%3", "a", 2, 0, State::Waiting, "claude"),
            pane("%4", "b", 1, 0, State::Done, "codex"),
        ]
    }

    #[test]
    fn tree_groups_sessions_windows_and_agents() {
        let none = HashSet::new();
        let rows = live(&sample(), &options(&none), &registry());
        let lines: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert!(lines[0].starts_with("a  3 agents"));
        assert!(lines[1].starts_with("  ├─ 1 w1"));
        assert!(lines[2].starts_with("  │  ├─ ○ Agent %1"));
        assert!(lines[3].starts_with("  │  └─ ● Agent %2"));
        assert!(lines[4].starts_with("  └─ 2 w2"));
        assert!(lines[5].starts_with("     └─ ◆ Agent %3"));
        assert!(lines[6].starts_with("b  1 agent"));
        assert!(lines[7].starts_with("  └─ 1 w1"));
        assert!(lines[8].starts_with("     └─ ✓ Agent %4"));
    }

    #[test]
    fn columns_line_up_in_tree_flat_and_whatever_the_name() {
        let none = HashSet::new();
        let mut panes = sample();
        panes[0].name = "a name much longer than any column would ever hold".into();
        panes[1].name = "日本語の名前".into();
        let tree = live(&panes, &options(&none), &registry());
        let flat = live(
            &panes,
            &Options {
                flat: true,
                ..options(&none)
            },
            &registry(),
        );
        let column = |row: &Row| {
            let at = row.text.find("api").unwrap();
            cells(&row.text[..at])
        };
        for row in tree.iter().filter(|r| matches!(r.id, RowId::Pane(_))).chain(flat.iter()) {
            assert_eq!(column(row), 47, "{}", row.text);
        }
    }

    #[test]
    fn flat_sorts_what_needs_you_first() {
        let none = HashSet::new();
        let rows = live(
            &sample(),
            &Options {
                flat: true,
                ..options(&none)
            },
            &registry(),
        );
        let ids: Vec<_> = rows.iter().map(|r| r.id.clone()).collect();
        let want = ["%3", "%4", "%2", "%1"].map(|id| RowId::Pane(id.to_string()));
        assert_eq!(ids, want);
    }

    #[test]
    fn folds_hide_children_and_keep_nested_folds() {
        let mut folded = HashSet::new();
        folded.insert(RowId::Session("$a".into()));
        let rows = live(&sample(), &options(&folded), &registry());
        assert_eq!(rows.len(), 4);
        assert!(rows[0].text.ends_with('▸'));
        assert_eq!(rows[1].id, RowId::Session("$b".into()));
        folded.clear();
        folded.insert(RowId::Window("$a".into(), "@a1".into()));
        let rows = live(&sample(), &options(&folded), &registry());
        assert_eq!(rows.iter().filter(|r| matches!(r.id, RowId::Pane(_))).count(), 2);
    }

    #[test]
    fn search_rows_carry_their_path_and_ignore_folds() {
        let mut folded = HashSet::new();
        folded.insert(RowId::Session("$a".into()));
        let rows = live(
            &sample(),
            &Options {
                compact: true,
                ..options(&folded)
            },
            &registry(),
        );
        assert_eq!(rows.len(), 4);
        assert!(rows[0].text.contains("a › 1 w1 › Agent %1"));
    }

    #[test]
    fn done_is_under_needs_you() {
        let none = HashSet::new();
        let rows = live(
            &sample(),
            &Options {
                view: "waiting",
                flat: true,
                ..options(&none)
            },
            &registry(),
        );
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn a_window_or_session_without_agents_shows_its_name_being_typed() {
        let none = HashSet::new();
        let window = |id: &str, session: &str| Window {
            id: id.into(),
            index: "9".into(),
            name: "spare".into(),
            session_id: format!("${session}"),
            session: session.into(),
        };
        let windows = [window("@free", "a"), window("@lone", "z")];
        let typing = |kind, target: &str| Mirror {
            kind,
            target: target.into(),
            text: "newname".into(),
            whole: false,
        };
        for (mirror, row) in [
            (typing(Kind::Window, "@free"), "  └─ 9 newname"),
            (typing(Kind::Session, "$z"), "newname  0 agents"),
        ] {
            let rows = live(
                &sample(),
                &Options {
                    windows: &windows,
                    mirror: Some(&mirror),
                    ..options(&none)
                },
                &registry(),
            );
            assert!(rows.iter().any(|r| r.text.starts_with(row)), "{row} missing");
        }
    }

    #[test]
    fn duplicate_names_do_not_merge_groups() {
        let none = HashSet::new();
        let mut panes = sample();
        let mirror = Mirror {
            kind: Kind::Session,
            target: "$b".into(),
            text: "a".into(),
            whole: false,
        };
        panes.truncate(4);
        let rows = live(
            &panes,
            &Options {
                mirror: Some(&mirror),
                ..options(&none)
            },
            &registry(),
        );
        assert_eq!(rows.iter().filter(|r| matches!(r.id, RowId::Session(_))).count(), 2);
    }

    #[test]
    fn a_long_name_being_typed_shows_its_end() {
        let none = HashSet::new();
        let mirror = Mirror {
            kind: Kind::Pane,
            target: "%1".into(),
            text: "x".repeat(60) + "end",
            whole: false,
        };
        let rows = live(
            &sample(),
            &Options {
                flat: true,
                mirror: Some(&mirror),
                ..options(&none)
            },
            &registry(),
        );
        assert!(rows.iter().any(|r| r.text.contains("xxxend  ")));
    }

    #[test]
    fn doing_is_cut_to_fifty_columns() {
        let none = HashSet::new();
        let mut panes = sample();
        panes[0].doing = "d".repeat(80);
        let rows = live(
            &panes,
            &Options {
                flat: true,
                ..options(&none)
            },
            &registry(),
        );
        let row = rows.iter().find(|r| r.id == RowId::Pane("%1".into())).unwrap();
        assert!(row.text.ends_with(&format!("{}…", "d".repeat(49))));
    }

    #[test]
    fn staged_to_leave_a_row_steps_out_a_level_and_the_columns_stay() {
        let none = HashSet::new();
        let stage = |detach| {
            [Stage {
                pane: "%1".into(),
                folder: None,
                tool: None,
                detach: Some(detach),
            }]
        };
        let row = |staged: &[Stage]| {
            let rows = live(&sample(), &Options { staged, ..options(&none) }, &registry());
            rows.iter().find(|r| r.id == RowId::Pane("%1".into())).unwrap().text.clone()
        };
        let (agent, window, session) = (row(&[]), row(&stage(Detach::Window)), row(&stage(Detach::Session)));
        assert!(agent.starts_with("  │  ├─ ○ Agent %1"));
        assert!(window.starts_with("  ├─ ○ Agent %1"));
        assert!(session.starts_with("○ Agent %1"));
        for text in [&agent, &window, &session] {
            assert_eq!(cells(&text[..text.find("api").unwrap()]), 47, "{text}");
        }
    }

    #[test]
    fn a_window_staged_for_a_session_steps_out_to_the_sessions_level() {
        let none = HashSet::new();
        let windows = ["@a2".to_string()];
        let rows = live(
            &sample(),
            &Options {
                staged_windows: &windows,
                ..options(&none)
            },
            &registry(),
        );
        let row = rows.iter().find(|r| r.id == RowId::Window("$a".into(), "@a2".into())).unwrap();
        assert!(row.text.starts_with("2 w2"), "{}", row.text);
        assert!(row.text.contains("open in a new tmux session"));
    }

    #[test]
    fn a_row_leaving_waits_at_the_end_of_where_it_is_going() {
        let none = HashSet::new();
        let order = |staged: &[Stage]| -> Vec<RowId> {
            live(&sample(), &Options { staged, ..options(&none) }, &registry())
                .into_iter()
                .map(|r| r.id)
                .collect()
        };
        let stage = |detach| {
            [Stage {
                pane: "%1".into(),
                folder: None,
                tool: None,
                detach: Some(detach),
            }]
        };
        let window = order(&stage(Detach::Window));
        let (at, next) = (
            window.iter().position(|id| *id == RowId::Pane("%1".into())).unwrap(),
            window.iter().position(|id| *id == RowId::Session("$b".into())).unwrap(),
        );
        assert_eq!(at + 1, next, "last in its session: {window:?}");
        let session = order(&stage(Detach::Session));
        assert_eq!(session.last(), Some(&RowId::Pane("%1".into())), "outside them all: {session:?}");
    }

    #[test]
    fn windows_and_sessions_without_agents_are_dim_rows_on_all_only() {
        let none = HashSet::new();
        let window = |id: &str, session: &str| Window {
            id: id.into(),
            index: "9".into(),
            name: "spare".into(),
            session_id: format!("${session}"),
            session: session.into(),
        };
        let windows = [window("@a1", "a"), window("@free", "a"), window("@lone", "z")];
        let rows = live(
            &sample(),
            &Options {
                windows: &windows,
                ..options(&none)
            },
            &registry(),
        );
        let id = |r: &Row| r.id.clone();
        let list: Vec<_> = rows.iter().map(id).collect();
        let free = RowId::Window("$a".into(), "@free".into());
        assert_eq!(
            list.iter().filter(|r| **r == RowId::Window("$a".into(), "@a1".into())).count(),
            1,
            "no second row for one with agents"
        );
        assert_eq!(
            list.iter().position(|r| *r == free).map(|at| at + 1),
            list.iter().position(|r| *r == RowId::Session("$b".into()))
        );
        assert!(rows.iter().any(|r| r.text.starts_with("z  0 agents")));
        assert!(rows.iter().any(|r| r.text.starts_with("  ├─ 2 w2")), "the branch carries on past it");
        assert!(rows.iter().any(|r| r.text.starts_with("  └─ 9 spare")));
        assert_eq!(list.last(), Some(&RowId::Window("$z".into(), "@lone".into())));
        let filtered = live(
            &sample(),
            &Options {
                view: "claude",
                windows: &windows,
                ..options(&none)
            },
            &registry(),
        );
        assert!(!filtered.iter().any(|r| r.id == free));
    }

    #[test]
    fn a_staged_folder_shows_in_its_row_and_the_columns_stay() {
        let none = HashSet::new();
        let staged = [Stage {
            pane: "%1".into(),
            folder: Some("/w/new".into()),
            tool: None,
            detach: None,
        }];
        let rows = live(
            &sample(),
            &Options {
                flat: true,
                staged: &staged,
                ..options(&none)
            },
            &registry(),
        );
        let row = rows.iter().find(|r| r.id == RowId::Pane("%1".into())).unwrap();
        assert!(row.text.contains("→ new"));
        assert!(row.text.contains("Idle"), "still an agent like the others");
        assert_eq!(cells(&row.text[..row.text.find("→ new").unwrap()]), 47);
        assert_eq!(cells(&row.text[..row.text.find("Idle").unwrap()]), 61);
    }

    #[test]
    fn a_folder_is_typed_in_place_without_moving_what_is_after_it() {
        let none = HashSet::new();
        let typing = live(
            &sample(),
            &Options {
                flat: true,
                folder: Some(("%1", "~/new")),
                ..options(&none)
            },
            &registry(),
        );
        let plain = live(
            &sample(),
            &Options {
                flat: true,
                ..options(&none)
            },
            &registry(),
        );
        let row = |rows: &[Row]| rows.iter().find(|r| r.id == RowId::Pane("%1".into())).unwrap().text.clone();
        let (typed, shown) = (row(&typing), row(&plain));
        assert!(typed.contains("~/new"));
        assert!(!typed.contains("Idle"));
        assert_eq!(cells(&typed[..typed.find("~/new").unwrap()]), 47, "where the folder was");
        assert_eq!(cells(&typed), cells(&shown));
        assert!(typed.ends_with("doing"));
    }

    #[test]
    fn the_cursor_follows_a_short_folder_and_the_room_comes_after_it() {
        let spans = typed_folder("~/a/");
        assert_eq!(spans[0].content, "~/a/");
        assert_eq!(spans[1].content, CARET.to_string(), "right after what's typed");
        let long = "~/".to_string() + &"x".repeat(60);
        let spans = typed_folder(&long);
        assert!(spans[0].content.starts_with('…') && spans[0].content.ends_with('x'));
        let total: usize = spans.iter().map(|s| cells(&s.content)).sum();
        assert_eq!(
            total,
            cells(&typed_folder("~/a/").iter().map(|s| s.content.to_string()).collect::<String>()),
            "the same room either way"
        );
    }

    #[test]
    fn a_draft_indents_without_moving_its_columns() {
        for prefix in ["", "  ", "     ", "  │  ├─ "] {
            let rows = draft(0, "claude", "/w/api", "x", prefix, None, &registry());
            let at = rows[0].text.find("api").unwrap();
            assert_eq!(cells(&rows[0].text[..at]), 47);
            assert!(rows[0].text.starts_with(&format!("{prefix}+ New agent")));
        }
    }

    #[test]
    fn new_agents_are_drawn_in_the_tree_where_they_will_start() {
        let none = HashSet::new();
        let (pane_in_w1, window_in_a, session) = (Where::Pane("@a1".into()), Where::Window("$a".into()), Where::Session);
        let new = |id, place| NewAgent {
            id,
            agent: "claude",
            folder: "/w/api",
            place,
            typed: None,
        };
        let drafts = [new(0, &pane_in_w1), new(1, &window_in_a), new(2, &window_in_a), new(3, &session)];
        let rows = live(
            &sample(),
            &Options {
                drafts: &drafts,
                ..options(&none)
            },
            &registry(),
        );
        let lines: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert!(lines[2].starts_with("  │  ├─ ○ Agent %1"));
        assert!(lines[3].starts_with("  │  ├─ ● Agent %2"), "no longer the window's last");
        assert!(lines[4].starts_with("  │  └─ + New agent"), "{}", lines[4]);
        assert!(lines[4].contains("new pane"));
        assert!(lines[5].starts_with("  ├─ 2 w2"), "no longer the session's last");
        assert!(lines[6].starts_with("  │  └─ ◆ Agent %3"));
        assert_eq!(rows[7].id, RowId::NewWindow("$a".into()));
        assert!(lines[7].starts_with("  └─ + new window"));
        assert!(lines[8].starts_with("     ├─ + New agent"));
        assert!(lines[9].starts_with("     └─ + New agent"));
        assert!(lines[10].starts_with("b  1 agent"));
        assert_eq!(rows[13].id, RowId::NewSession);
        assert_eq!(rows[14].id, RowId::NewWindow(String::new()));
        assert!(lines[14].starts_with("└─ + new window"));
        assert_eq!(rows[15].id, RowId::Draft(3));
        assert!(lines[15].starts_with("   └─ + New agent"));
        assert_eq!(rows.len(), 16);
    }

    #[test]
    fn matching_is_fuzzy_and_smart_about_case() {
        assert!(matches("main › 1 agents › Fix bug", "fix bg"));
        assert!(matches("Fix", "fix"));
        assert!(!matches("fix", "Fix"));
        assert!(!matches("abc", "acb"));
        assert!(matches("anything", ""));
    }

    #[test]
    fn ages_read_at_a_glance() {
        assert_eq!(ago(59), "0m");
        assert_eq!(ago(7200), "2h");
        assert_eq!(ago(172_800), "2d");
    }
}
