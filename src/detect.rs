//! Finding agent panes and reading their state from what they show.
//! shortcut: matches on-screen text ("esc to interrupt", "esc to cancel", ⏺/• action lines); update the patterns if the
//! TUIs reword them.
use crate::agents::{Agent, Registry, Turn, sleep};
use crate::tmux;
use ratatui::style::{Color, Modifier, Style};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Waiting,
    Done,
    Running,
    Idle,
}

impl State {
    pub const ORDER: [State; 4] = [State::Waiting, State::Done, State::Running, State::Idle];

    pub fn key(self) -> &'static str {
        match self {
            State::Waiting => "waiting",
            State::Done => "done",
            State::Running => "running",
            State::Idle => "idle",
        }
    }

    pub fn parse(key: &str) -> Option<State> {
        Self::ORDER.into_iter().find(|s| s.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            State::Waiting => "Needs you",
            State::Done => "Done",
            State::Running => "Working",
            State::Idle => "Idle",
        }
    }

    pub fn color(self) -> Color {
        let palette = crate::text::palette();
        match self {
            State::Waiting => palette.needs_you,
            State::Done => palette.done,
            State::Running => palette.working,
            State::Idle => palette.idle,
        }
    }

    /// A shape of its own for each state, so they can be told apart without their color.
    pub fn dot(self) -> &'static str {
        match self {
            State::Waiting => "◆",
            State::Done => "✓",
            State::Running => "●",
            State::Idle => "○",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Pane {
    pub id: String,
    pub state: State,
    pub doing: String,
    pub tool: String,
    pub folder: String,
    pub name: String,
    pub session: String,
    pub window: String,
    pub window_id: String,
    pub window_index: u32,
    pub pane_index: u32,
    pub session_id: String,
    pub path: String,
    pub context: String,
}

impl Pane {
    /// Its conversation's name, or the agent's until it has one.
    pub fn shown_name(&self, agents: &Registry) -> String {
        if self.name.is_empty() {
            agents.name(&self.tool)
        } else {
            self.name.clone()
        }
    }

    /// As stored in the cache the next open starts from.
    pub fn line(&self) -> String {
        [
            self.id.as_str(),
            self.state.key(),
            &self.doing,
            &self.tool,
            &self.folder,
            &self.name,
            &self.session,
            &self.window,
            &self.window_id,
            &self.window_index.to_string(),
            &self.pane_index.to_string(),
            &self.session_id,
            &self.path,
            &self.context,
        ]
        .join("\t")
    }

    pub fn parse(line: &str) -> Option<Pane> {
        let f: Vec<&str> = line.split('\t').collect();
        let [
            id,
            state,
            doing,
            tool,
            folder,
            name,
            session,
            window,
            window_id,
            window_index,
            pane_index,
            session_id,
            path,
            context,
        ] = f[..]
        else {
            return None;
        };
        Some(Pane {
            id: id.into(),
            state: State::parse(state)?,
            doing: doing.into(),
            tool: tool.into(),
            folder: folder.into(),
            name: name.into(),
            session: session.into(),
            window: window.into(),
            window_id: window_id.into(),
            window_index: window_index.parse().ok()?,
            pane_index: pane_index.parse().ok()?,
            session_id: session_id.into(),
            path: path.into(),
            context: context.into(),
        })
    }
}

/// Whether one of `phrases` is on `line` where the TUIs print them: after a separator or a column gap ("· esc to
/// cancel", "(3s • esc to interrupt)", "or esc to cancel", OpenCode's "⬝⬝ esc interrupt"), never inside a message or a
/// command that merely quotes them.
fn ui_phrase(line: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| {
        line.match_indices(phrase).any(|(at, _)| {
            let before = &line[..at];
            let bare = before.trim_end_matches(' ');
            bare.is_empty() || before.ends_with("  ") || bare.ends_with(" or") || bare.ends_with(['(', '·', '•', '⬝', '■']) // ⬝ ■: OpenCode's spinner
        })
    })
}

/// State and "doing" of a pane's screen. Claude marks messages and tool calls with ⏺, Codex with •, OpenCode with →.
pub fn inspect_screen(screen: &str, agent: Option<&dyn Agent>) -> (State, String) {
    let lines: Vec<String> = screen.lines().filter(|l| !l.trim().is_empty()).map(str::to_lowercase).collect();
    let bottom = &lines[lines.len().saturating_sub(8)..];
    let any = |phrases: &[&str]| bottom.iter().any(|line| ui_phrase(line, phrases));
    // Waiting: a dialog's way out ("esc to cancel"; Codex's "esc back" and "esc skip") or OpenCode's permission choices
    // ("Allow once   Always allow   Reject"), which offer no esc.
    let state = if any(&["esc to cancel", "esc back", "esc skip", "allow once"]) {
        State::Waiting
    } else if any(&["esc to interrupt", "esc interrupt"]) || agent.is_some_and(|a| a.busy(screen)) {
        State::Running
    } else {
        State::Idle
    };
    let doing = screen.lines().rev().find_map(|line| {
        let line = line.trim_start();
        ["⏺ ", "• ", "→ "].iter().find_map(|mark| line.strip_prefix(mark))
    });
    (state, doing.unwrap_or_default().replace('\t', " "))
}

/// A pane's state and doing; `title` is its pane title when the caller has it already, else tmux is asked if its agent
/// keeps a log.
pub fn inspect(pane: &str, agents: &Registry, tool: &str, title: Option<&str>) -> (State, String) {
    let screen = tmux::capture(pane, SCREEN).unwrap_or_default();
    inspect_captured(pane, &screen, agents, tool, title)
}

/// How much of a screen `inspect` reads.
pub const SCREEN: &[&str] = &["-S", "-200"];

/// `inspect` for a screen captured already.
pub fn inspect_captured(pane: &str, screen: &str, agents: &Registry, tool: &str, title: Option<&str>) -> (State, String) {
    let agent = agents.get(tool);
    let (state, doing) = inspect_screen(screen, agent);
    let turn = agent.filter(|a| a.logs_turns()).and_then(|a| {
        let title = title.map_or_else(|| tmux::display(pane, "#{pane_title}").unwrap_or_default(), str::to_string);
        a.turn(pane, &a.title(&title), agents)
    });
    (with_turn(state, turn, agent.is_none_or(|a| a.records_all_work())), doing)
}

/// The screen's state, put right by the agent's own record where it keeps one: a turn under way is Working even while the
/// screen shows nothing busy (Codex running a tool), one that's over is Idle whatever the screen quotes, and one waiting
/// on you is Needs you whatever the screen shows. A record that doesn't say (Codex's and Claude's never do) leaves Needs
/// you to the screen.
/// `all_work`: whether the record shows everything the agent does (`records_all_work`); one that doesn't can't say a
/// busy screen is wrong.
fn with_turn(state: State, turn: Option<Turn>, all_work: bool) -> State {
    match (turn, state) {
        (Some(Turn::Asking), _) => State::Waiting,
        (Some(Turn::Running), State::Idle) => State::Running,
        (Some(Turn::Idle), State::Running) if all_work => State::Idle,
        (_, state) => state,
    }
}

fn moved_path() -> std::path::PathBuf {
    tmux::history_dir().join(format!("moved-{}", tmux::server_id()))
}

/// A tmux window, whether or not it has agents.
#[derive(Clone, PartialEq, Debug)]
pub struct Window {
    pub id: String,
    pub index: String,
    pub name: String,
    pub session_id: String,
    pub session: String,
}

pub fn windows() -> Option<Vec<Window>> {
    let list = tmux::query(&[
        "list-windows",
        "-a",
        "-F",
        "#{window_id}\t#{window_index}\t#{window_name}\t#{session_id}\t#{session_name}",
    ])?;
    let window = |line: &str| {
        let [id, index, name, session_id, session] = line.split('\t').collect::<Vec<_>>()[..] else {
            return None;
        };
        let text = str::to_string;
        Some(Window {
            id: text(id),
            index: text(index),
            name: text(name),
            session_id: text(session_id),
            session: text(session),
        })
    };
    Some(list.lines().filter_map(window).collect())
}

/// Where we sent agents with /cd: pane, then (tool, folder). The last one sent wins.
fn moved_folders() -> HashMap<String, (String, String)> {
    let text = std::fs::read_to_string(moved_path()).unwrap_or_default();
    let split = |line: &str| {
        let mut f = line.splitn(3, '\t');
        Some((f.next()?.to_string(), (f.next()?.to_string(), f.next()?.to_string())))
    };
    text.lines().filter_map(split).collect()
}

/// The folder we last sent agent `pane` to.
pub fn moved_folder(pane: &str) -> Option<String> {
    moved_folders().remove(pane).map(|(_, folder)| folder)
}

pub fn remember_folder(pane: &str, tool: &str, folder: &str) {
    use std::io::Write;
    let _ = tmux::private_dir(&tmux::history_dir());
    let file = std::fs::OpenOptions::new().create(true).append(true).open(moved_path());
    if let Ok(mut file) = file {
        let _ = writeln!(file, "{pane}\t{tool}\t{folder}");
    }
}

pub fn done_panes() -> HashSet<String> {
    let text = std::fs::read_to_string(tmux::status_file()).unwrap_or_default();
    text.lines().filter_map(|l| l.strip_suffix("\tdone")).map(str::to_string).collect()
}

const FORMAT: &str = "#{pane_id}\t#{pane_current_command}\t#{pane_current_path}\t#{pane_title}\t#{session_name}\t#{window_name}\t#{window_id}\t#{window_index}\t#{pane_index}\t#{session_id}";

/// One row per agent, by session, window and pane. `context` fills the Context column, which costs a transcript lookup.
pub fn panes(agents: &Registry, context: bool) -> Option<Vec<Pane>> {
    let list = tmux::query(&["list-panes", "-a", "-F", FORMAT])?;
    let done = done_panes();
    let moved = moved_folders();
    let mut seen = HashSet::new();
    let rows: Vec<Vec<&str>> = list
        .lines()
        .map(|line| line.split('\t').collect::<Vec<_>>())
        .filter(|f| f.len() == 10 && agents.tracks(f[1]) && seen.insert(f[0]))
        .collect();
    let read = |f: &Vec<&str>| {
        let (id, tool, path, title) = (f[0], f[1], f[2], f[3]);
        let agent = agents.get(tool)?;
        let screen = tmux::capture(id, &["-S", "-200"]).unwrap_or_default();
        let (state, doing) = inspect_screen(&screen, Some(agent));
        let name = agent.title(title);
        // The quick first look skips the logs as it skips Context: finding them is the slow part.
        let turn = if context { agent.turn(id, &name, agents) } else { None };
        let mut state = with_turn(state, turn, agent.records_all_work());
        // Done until the status refresh sees its pane in view.
        if state == State::Idle && done.contains(id) {
            state = State::Done;
        }
        let context = if context {
            agent.context(id, &name, agents).unwrap_or_default()
        } else {
            String::new()
        };
        // Unless the process itself moves, tmux reports where it started: the agent's screen says otherwise, or we sent it there.
        let sent = |(to, _): &&(String, String)| to == tool && !agent.moves_itself();
        let moved = moved.get(id).filter(sent).map(|(_, folder)| folder.clone());
        let path = agent.folder(&screen).or(moved).unwrap_or_else(|| path.to_string());
        let path = path.as_str();
        let folder = tmux::folder_name(path);
        Some(Pane {
            id: id.into(),
            state,
            doing,
            tool: tool.into(),
            folder,
            name,
            session: f[4].into(),
            window: f[5].into(),
            window_id: f[6].into(),
            window_index: f[7].parse().unwrap_or(0),
            pane_index: f[8].parse().unwrap_or(0),
            session_id: f[9].into(),
            path: path.into(),
            context,
        })
    };
    let mut found: Vec<Pane> = std::thread::scope(|scope| {
        let reading: Vec<_> = rows.iter().map(|row| scope.spawn(|| read(row))).collect();
        reading.into_iter().filter_map(|t| t.join().ok().flatten()).collect()
    });
    found.sort_by(|a, b| (&a.session, &a.session_id, a.window_index, a.pane_index).cmp(&(&b.session, &b.session_id, b.window_index, b.pane_index)));
    Some(found)
}

struct StyledLine {
    text: String,
    visible: String,
    /// (byte offset in text, foreground, background)
    spans: Vec<(usize, Option<Color>, Option<Color>)>,
}

impl StyledLine {
    fn foreground(&self, at: usize) -> Option<Color> {
        self.spans.iter().rev().find(|span| span.0 <= at).and_then(|span| span.1)
    }

    /// Cut at the first background change after its "┃": past it is OpenCode's sidebar, not the box.
    fn inside_box(&self) -> String {
        let after = self.text.find('┃').map_or(0, |at| at + '┃'.len_utf8());
        let Some(first) = self.spans.iter().position(|span| span.0 >= after) else {
            return self.text.clone();
        };
        let back = self.spans[first].2;
        match self.spans[first..].iter().find(|span| span.2 != back) {
            // A blank box ends in spaces, which parse_input has already trimmed off the text but not off the spans.
            Some(span) => self.text[..span.0.min(self.text.len())].trim_end_matches(' ').to_string(),
            None => self.text.clone(),
        }
    }
}

fn styled(line: &str, style: &mut Style) -> StyledLine {
    let line = crate::text::without_osc(line).replace('\u{a0}', " ");
    let mut out = StyledLine {
        text: String::new(),
        visible: String::new(),
        spans: Vec::new(),
    };
    let mut rest = line.as_str();
    while !rest.is_empty() {
        let (part, after) = match rest.find("\x1b[") {
            Some(at) => (&rest[..at], &rest[at + 2..]),
            None => (rest, ""),
        };
        if !part.is_empty() {
            out.spans.push((out.text.len(), style.fg, style.bg));
            out.text.push_str(part);
            if !style.add_modifier.contains(Modifier::DIM) {
                out.visible.push_str(part);
            }
        }
        match after.find(|c: char| !c.is_ascii_digit() && c != ';' && c != ':') {
            Some(end) if after.as_bytes()[end] == b'm' => {
                crate::text::apply_sgr(style, &after[..end]);
                rest = &after[end + 1..];
            }
            Some(end) => rest = &after[end + 1..],
            None => rest = "",
        }
    }
    out
}

/// The text in a captured screen's input box, or None when it can't be read. Only Codex/Claude dim suggestions are
/// ignored. OpenCode uses a bordered textarea with a model row: its placeholder shares the muted separator color,
/// unlike typed text. Its sidebar shares the box's screen lines, so each line ends where the box's background does; the
/// slash command list above the box has a border of another color, so a new border color starts a new box.
pub fn parse_input(screen: &str) -> Option<String> {
    let mut style = Style::default();
    let mut input: Option<String> = None;
    let (mut in_box, mut border) = (false, None);
    let mut candidate: Option<String> = None;
    let mut body: Vec<String> = Vec::new();
    let [mut hint, mut muted, mut base]: [Vec<Option<Color>>; 3] = Default::default();
    for raw in screen.lines() {
        let mut line = styled(raw, &mut style);
        line.text.truncate(line.text.trim_end_matches(' ').len());
        let visible = line.visible.trim_end_matches(' ');
        if ["❯", "›"].iter().any(|p| visible == *p || visible.starts_with(&format!("{p} "))) {
            input = Some(visible.to_string());
        }
        if line.text.trim_start_matches(' ').starts_with('┃') {
            let edge = line.foreground(line.text.find('┃').unwrap_or(0));
            let cut = line.inside_box();
            if !in_box || edge != border {
                body.clear();
                hint.clear();
                muted.clear();
                base.clear();
            }
            border = edge;
            in_box = true;
            input = None;
            candidate = None;
            let text = cut.trim_start_matches(' ').strip_prefix('┃').unwrap_or_default();
            let text = text.strip_prefix(' ').unwrap_or(text);
            body.push(text.strip_prefix(' ').unwrap_or(text).to_string());
            hint.push(cut.find("Ask anything…").and_then(|at| line.foreground(at)));
            match cut.find('·') {
                Some(dot) => {
                    let after = dot + '·'.len_utf8();
                    let next = after + cut[after..].len() - cut[after..].trim_start_matches(' ').len();
                    muted.push(line.foreground(dot));
                    base.push(line.foreground(next));
                }
                None => {
                    muted.push(None);
                    base.push(None);
                }
            }
            continue;
        }
        if in_box {
            // shortcut: recognize the standard OpenCode box; unfamiliar or incomplete layouts stay unreadable.
            let n = body.len();
            let shell = |s: &str| s == "Shell" || s.starts_with("Shell ");
            if line.text.trim_start_matches(' ').starts_with('╹')
                && n >= 4
                && body[0].is_empty()
                && body[n - 2].is_empty()
                && !body[n - 1].is_empty()
                && !shell(&body[n - 1])
            {
                let mut text = body[1..n - 2].join("\n");
                if n == 4 && text.starts_with("Ask anything…") && muted[n - 1].is_some() && muted[n - 1] != base[n - 1] && hint[1] == muted[n - 1] {
                    text.clear();
                }
                candidate = Some(if text.is_empty() { "›".to_string() } else { format!("› {text}") });
            }
            in_box = false;
        }
        if candidate.is_some() && line.text.split_whitespace().any(|word| word == "commands") {
            input = candidate.clone();
        }
    }
    let input = input?;
    Some(if input == "❯" || input == "›" { String::new() } else { input })
}

pub fn pane_input(pane: &str) -> Option<String> {
    parse_input(&tmux::capture(pane, &["-J", "-e"])?)
}

/// Why an agent can't be typed into right now, or nothing: it must be idle with an empty input box, so a command never
/// lands in a draft or a busy turn.
pub fn rename_blocker(pane: &str, agents: &Registry) -> Option<String> {
    let tool = tmux::pane_command(pane).unwrap_or_default();
    let (state, _) = inspect(pane, agents, &tool, None);
    if state != State::Idle {
        return Some(format!("the agent is {}", state.key()));
    }
    match pane_input(pane) {
        None => Some("its input box can't be read".into()),
        Some(text) if !text.is_empty() => Some("its input box has text".into()),
        _ => None,
    }
}

/// Why a live agent can't hand its conversation over. Every agent needs an empty box, not only those that may type
/// /export: the new CLI closes its pane, and a draft in it would go with it.
pub fn live_blocker(pane: &str, agents: &Registry) -> Option<String> {
    if !tmux::is_pane(pane) {
        return Some("select an agent pane".into());
    }
    let tool = tmux::pane_command(pane).unwrap_or_default();
    if agents.get(&tool).is_none() {
        return Some("the agent is no longer available".into());
    }
    rename_blocker(pane, agents)
}

fn shows(pane: &str, command: &str) -> bool {
    pane_input(pane).is_some_and(|text| text == format!("› {command}") || text == format!("❯ {command}"))
}

/// Types slash command `command` into agent `pane` and submits it. Callers have checked that its input box holds no
/// text, but stray spaces pass that check and would turn "/export …" into a plain message to the model. A row of
/// Backspaces (one per column; a no-op on an empty box) removes them. Not Ctrl+U or Ctrl+C: both can be rebound, and
/// Ctrl+C on an empty box arms "press again to exit".
pub fn send_command(pane: &str, command: &str) -> bool {
    let width = tmux::display(pane, "#{pane_width}").unwrap_or_default();
    if !tmux::ok(&["send-keys", "-t", pane, "-N", &width, "BSpace"]) || !tmux::send_text(pane, command) {
        return false;
    }
    // Enter only once the agent shows the command in its input box, and a moment later: Codex takes fast input as a
    // paste, and an Enter inside a paste becomes a new line instead of sending.
    let seen = (0..20).any(|_| {
        let shown = shows(pane, command);
        if !shown {
            sleep(0.05);
        }
        shown
    });
    if !seen {
        // Not shown: take back what was typed rather than leave it in the box (it was empty before).
        tmux::ok(&["send-keys", "-t", pane, "-N", &command.chars().count().to_string(), "BSpace"]);
        return false;
    }
    sleep(0.2);
    if !tmux::send_key(pane, "Enter") {
        return false;
    }
    sleep(0.3);
    // OpenCode's path list took the Enter and finished the path: the next one sends it.
    if shows(pane, &format!("{command}/")) {
        tmux::send_key(pane, "Enter");
    // Still in the box (the Enter became a new line): take the new line back out and send once more.
    } else if shows(pane, command) {
        tmux::send_key(pane, "BSpace");
        sleep(0.3);
        tmux::send_key(pane, "Enter");
    }
    true
}

pub fn clear_local_command(pane: &str, command: &str) {
    if shows(pane, command) {
        tmux::send_key(pane, "C-u");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(screen: &str) -> State {
        inspect_screen(screen, None).0
    }

    #[test]
    fn states_need_the_tui_phrase_not_a_message_quoting_it() {
        assert_eq!(state("✻ Thinking… (3s · esc to interrupt)"), State::Running);
        assert_eq!(state("  enter to confirm · esc to cancel"), State::Waiting);
        assert_eq!(state("Press enter or esc to cancel"), State::Waiting);
        assert_eq!(state("⬝⬝ esc interrupt"), State::Running);
        assert_eq!(state("esc to cancel"), State::Waiting);
        assert_eq!(state("Allow once   Always allow   Reject"), State::Waiting);
        assert_eq!(state("⏺ the phrase esc to cancel means waiting"), State::Idle);
        assert_eq!(state("grep 'esc to interrupt' file"), State::Idle);
    }

    #[test]
    fn only_the_bottom_of_the_screen_counts() {
        let mut screen = String::from("· esc to interrupt\n");
        screen.push_str(&"line\n".repeat(8));
        assert_eq!(state(&screen), State::Idle);
    }

    #[test]
    fn doing_is_the_last_marked_line() {
        let (_, doing) = inspect_screen("⏺ first\n  • second\tpart\nplain", None);
        assert_eq!(doing, "second part");
    }

    #[test]
    fn panes_survive_the_cache() {
        let pane = Pane {
            id: "%1".into(),
            state: State::Done,
            doing: String::new(),
            tool: "claude".into(),
            folder: "api".into(),
            name: "Fix".into(),
            session: "main".into(),
            window: "w".into(),
            window_id: "@1".into(),
            window_index: 2,
            pane_index: 3,
            session_id: "$1".into(),
            path: "/w/api".into(),
            context: "4%".into(),
        };
        assert_eq!(Pane::parse(&pane.line()), Some(pane));
    }

    #[test]
    fn a_blank_line_of_opencodes_box_beside_its_sidebar_is_read() {
        let blank = format!("\x1b[34m  ┃\x1b[49m\x1b[48;5;236m{}\x1b[49m\x1b[48;5;234m  sidebar", " ".repeat(160));
        let empty = format!(
            "\x1b[34m  ┃\x1b[49m\x1b[48;5;236m{}\x1b[49m\x1b[48;5;234m{}",
            " ".repeat(160),
            " ".repeat(10)
        );
        for line in [blank, empty] {
            let screen = format!("{line}\n  ┃  hello\n{line}\n  ┃  Build · Big\n  ╹▀▀▀\n  ctrl+p commands");
            assert_eq!(parse_input(&screen).as_deref(), Some("› hello"));
        }
    }

    #[test]
    fn input_box_reads_prompts() {
        assert_eq!(parse_input("❯ ").as_deref(), Some(""));
        assert_eq!(parse_input("› hello").as_deref(), Some("› hello"));
        assert_eq!(parse_input("no box here"), None);
        // Claude pads its prompt with a non-breaking space; a dim suggestion is not typed text.
        assert_eq!(parse_input("❯\u{a0}\x1b[2msuggestion\x1b[0m").as_deref(), Some(""));
    }

    fn opencode_box(text_line: &str) -> String {
        let bar = "\x1b[34m┃\x1b[39m";
        [
            format!("  {bar}"),
            format!("  {bar}  {text_line}"),
            format!("  {bar}"),
            format!("  {bar}  \x1b[37mBuild\x1b[90m · \x1b[39mmodel"),
            "  ╹▀▀▀▀".to_string(),
            "      tab agents  ctrl+p commands".to_string(),
        ]
        .join("\n")
    }

    #[test]
    fn opencode_placeholder_counts_as_empty_but_typed_text_does_not() {
        assert_eq!(parse_input(&opencode_box("\x1b[90mAsk anything…\x1b[39m")).as_deref(), Some(""));
        assert_eq!(parse_input(&opencode_box("\x1b[39mfix the bug")).as_deref(), Some("› fix the bug"));
        assert_eq!(
            parse_input(&opencode_box("\x1b[39mAsk anything… really")).as_deref(),
            Some("› Ask anything… really")
        );
    }

    #[test]
    fn opencode_sidebar_is_not_part_of_the_box() {
        let bar = "\x1b[34m┃\x1b[39m";
        let screen = [
            format!("{bar}\x1b[48;5;1m    \x1b[48;5;2m sidebar"),
            format!("{bar}\x1b[48;5;1m  typed\x1b[48;5;2m sidebar"),
            format!("{bar}\x1b[48;5;1m    \x1b[48;5;2m sidebar"),
            format!("{bar}\x1b[48;5;1m  Build\x1b[90m · \x1b[39mmodel\x1b[48;5;2m sidebar"),
            "╹▀▀▀".to_string(),
            "commands".to_string(),
        ]
        .join("\n");
        assert_eq!(parse_input(&screen).as_deref(), Some("› typed"));
    }

    #[test]
    fn the_log_says_whether_a_turn_runs_and_the_screen_whether_it_waits_on_you() {
        assert_eq!(
            with_turn(State::Idle, Some(Turn::Running), true),
            State::Running,
            "a tool running, nothing busy on screen"
        );
        assert_eq!(with_turn(State::Running, Some(Turn::Idle), true), State::Idle, "a quoted busy phrase");
        assert_eq!(
            with_turn(State::Waiting, Some(Turn::Running), true),
            State::Waiting,
            "an approval: no log has it"
        );
        assert_eq!(
            with_turn(State::Waiting, Some(Turn::Idle), true),
            State::Waiting,
            "a dialog outside any turn"
        );
        assert_eq!(
            with_turn(State::Running, Some(Turn::Asking), true),
            State::Waiting,
            "OpenCode says it asks"
        );
        assert_eq!(with_turn(State::Idle, None, true), State::Idle, "no log: the screen alone");
        assert_eq!(
            with_turn(State::Running, Some(Turn::Idle), false),
            State::Running,
            "a record that misses some work (Claude's /compact) can't call a busy screen idle"
        );
    }

    #[test]
    fn colon_codes_and_following_codes_read_right() {
        assert_eq!(parse_input("❯ \x1b[4:3mtyped").as_deref(), Some("❯ typed"));
        let mut style = Style::default();
        crate::text::apply_sgr(&mut style, "38;5;240;1");
        assert_eq!(style.fg, Some(Color::Indexed(240)), "as without the 1 after it");
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }
}
