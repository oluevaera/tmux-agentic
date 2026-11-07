//! Drawing the board: tabs and column headings, the list, the shortcuts, and the preview.
use crate::agents::Who;
use crate::app::{App, Click, ConfirmAction, DROPDOWN, HISTORY, Mode, Tone};
use crate::detect::State;
use crate::keymap::{self, Action};
use crate::rows::{self, Detach, Kind, RowId};
use crate::sessions::Place;
use crate::text::{self, CARET, bold, cells, dim, filled, fit, tail};
use crate::tmux;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

/// Quiet bands for where you are and what you've marked, from the theme.
fn band() -> Color {
    text::palette().cursor
}

fn marked_band() -> Color {
    text::palette().marked
}

fn rule(frame: &mut Frame, area: Rect) {
    let line = "─".repeat(area.width as usize);
    frame.render_widget(Paragraph::new(line).style(Style::new().fg(text::palette().muted)), area);
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.regions.clear();
    let area = frame.area();
    frame.render_widget(Clear, area);
    // The popup's own background is set as it opens (popup.rs); painting it here too lets a theme changed since show.
    if let Some(background) = crate::config::get().background {
        frame.buffer_mut().set_style(area, Style::new().bg(background));
    }
    let inner = Rect {
        x: area.x + 2.min(area.width),
        y: area.y + 1.min(area.height),
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };
    let wanted = inner.height as usize * 60 / 100;
    // Below this the preview would show too little to read: the list gets the room.
    let preview = if wanted.saturating_sub(1) < 10 { 0 } else { wanted as u16 };
    let input = u16::from(app.searching());
    let [header, top_rule, input_line, list, foot_rule, footer, preview_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(input),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(preview),
    ])
    .areas(inner);
    draw_header(frame, app, header);
    rule(frame, top_rule);
    draw_input(frame, app, input_line);
    draw_list(frame, app, list);
    rule(frame, foot_rule);
    draw_footer(frame, app, footer);
    if preview > 0 {
        let [preview_rule, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(preview_area);
        rule(frame, preview_rule);
        draw_preview(frame, app, body);
    }
    draw_dropdown(frame, app, list, inner);
}

/// The folders for what's typed, under the Folder column and over whatever is there: below the row being typed in, or above
/// it near the bottom. Long paths show their end, the part that tells them apart.
fn draw_dropdown(frame: &mut Frame, app: &mut App, list: Rect, inner: Rect) {
    let Mode::Folder(edit) = &app.mode else { return };
    let shown: Vec<String> = edit.matches.iter().take(DROPDOWN).cloned().collect();
    let index = edit.index;
    let height = shown.len().max(1) as u16;
    let row = list.y + app.cursor.saturating_sub(app.offset) as u16;
    let y = if row + 1 + height <= inner.bottom() {
        row + 1
    } else {
        row.saturating_sub(height)
    };
    let x = list.x + 47; // two left of the Folder column, for the pointer
    let area = Rect {
        x,
        y,
        width: inner.right().saturating_sub(x),
        height,
    };
    frame.render_widget(Clear, area);
    let room = (area.width as usize).saturating_sub(2);
    if shown.is_empty() {
        let line = Line::styled("  no matching folder", dim());
        frame.render_widget(Paragraph::new(line).style(Style::new().bg(text::field())), area);
    }
    for (i, path) in shown.iter().enumerate() {
        let rect = Rect {
            y: y + i as u16,
            height: 1,
            ..area
        };
        let selected = i == index;
        let text = if cells(path) > room {
            format!("…{}", tail(path, room.saturating_sub(1)))
        } else {
            path.clone()
        };
        let line = format!("{}{text}", if selected { "› " } else { "  " });
        let style = if selected { bold().bg(band()) } else { Style::new().bg(text::field()) };
        frame.render_widget(Paragraph::new(line).style(style), rect);
        app.regions.insert(0, (rect, Click::Folder(i)));
    }
}

/// Keys and words in a line, remembering where each clickable part ended up.
struct Bar {
    spans: Vec<Span<'static>>,
    x: u16,
    y: u16,
}

impl Bar {
    fn new(area: Rect) -> Self {
        Bar {
            spans: Vec::new(),
            x: area.x,
            y: area.y,
        }
    }

    fn push(&mut self, text: String, style: Style) -> Rect {
        let width = cells(&text) as u16;
        let rect = Rect {
            x: self.x,
            y: self.y,
            width,
            height: 1,
        };
        self.x += width;
        self.spans.push(Span::styled(text, style));
        rect
    }
}

/// Header: the tabs with their counts, and the column headings.
fn draw_header(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut bar = Bar::new(area);
    bar.push("  ".into(), Style::new());
    let current = app.view.clone();
    let views = app.views();
    // tmux's own way: the open tab in brackets, nothing behind it. The number keys reach the tabs, on them only if
    // config.toml asks: dim, so the names stay what you read.
    let tab = |bar: &mut Bar, open: bool, place: u8, text: String, style: Style| {
        bar.push((if open { "[" } else { " " }).into(), style);
        let key = app.keys.label(Action::Tab(place));
        if app.tab_numbers && !key.is_empty() {
            bar.push(format!("{key}. "), dim());
        }
        bar.push(text, style);
        bar.push((if open { "]" } else { " " }).into(), style);
    };
    for (i, view) in views.iter().enumerate() {
        let start = bar.x;
        let label = match view.as_str() {
            "all" => "All".to_string(),
            "waiting" => "Needs you".to_string(),
            tool => app.agents.name(tool),
        };
        let count = app.count(view);
        let open = *view == current;
        // An empty tab is dimmed without its 0; Needs you is the one count in color, the yellow of its dot.
        let style = if open {
            bold()
        } else if view != "all" && count == 0 {
            dim()
        } else if view == "waiting" {
            bold().fg(State::Waiting.color())
        } else {
            Style::new()
        };
        let text = if view != "all" && count == 0 && !open {
            label
        } else {
            // Joined to its name by a dot, so it never reads as the next tab's key.
            format!("{label}·{count}")
        };
        tab(&mut bar, open, i as u8 + 1, text, style);
        let rect = Rect {
            x: start,
            y: area.y,
            width: bar.x - start,
            height: 1,
        };
        app.regions.push((rect, Click::Tab(view.clone())));
        bar.push("  ".into(), Style::new());
    }
    bar.push("  ".into(), Style::new());
    let start = bar.x;
    let open = current == HISTORY;
    tab(&mut bar, open, 0, "History".into(), if open { bold() } else { Style::new() });
    let rect = Rect {
        x: start,
        y: area.y,
        width: bar.x - start,
        height: 1,
    };
    app.regions.push((rect, Click::Tab(HISTORY.into())));
    let columns = if current == HISTORY {
        format!("  {:>4}  {:<12}{:<17}{}", "Age", "Agent", "Folder", "Conversation")
    } else if app.searching() {
        format!(
            "    {:<42}{:<11}{:<14}{:<11}{:<9}{}",
            "Agent", "Kind", "Folder", "Status", "Context", "Doing"
        )
    } else {
        format!(
            "      {:<32}{:<11}{:<14}{:<11}{:<9}{}",
            "Agent", "Kind", "Folder", "Status", "Context", "Doing"
        )
    };
    let lines = vec![Line::from(bar.spans), Line::raw(""), Line::styled(columns, dim())];
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let Mode::Search(input) = &app.mode else { return };
    let (prompt, text) = ("Search › ", input.text.as_str());
    let x = area.x + 2 + cells(prompt) as u16 + cells(&text[..input.cursor]) as u16;
    let line = Line::from(vec![Span::raw("  "), Span::styled(prompt, dim()), Span::raw(text.to_string())]);
    frame.render_widget(Paragraph::new(line), area);
    frame.set_cursor_position(Position::new(x.min(area.right().saturating_sub(1)), area.y));
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    app.regions.push((area, Click::List));
    let height = area.height as usize;
    if app.cursor < app.offset {
        app.offset = app.cursor;
    } else if height > 0 && app.cursor >= app.offset + height {
        app.offset = app.cursor + 1 - height;
    }
    app.offset = app.offset.min(app.rows.len().saturating_sub(height));
    for (i, row) in app.rows.iter().skip(app.offset).take(height).enumerate() {
        let y = area.y + i as u16;
        let marked = app.marks.contains(&row.id);
        let selected = i + app.offset == app.cursor;
        let band = if marked {
            Style::new().bg(marked_band())
        } else if selected {
            Style::new().bg(band())
        } else {
            Style::new()
        };
        let mut spans = vec![
            Span::raw(if selected { "›" } else { " " }),
            if marked {
                Span::styled("▌", Style::new().fg(crate::text::marker()))
            } else {
                Span::raw(" ")
            },
        ];
        spans.extend(row.line.spans.iter().cloned());
        // A name or folder being typed: the terminal's own cursor stands where it's typed, in a blank cell that keeps
        // the columns where they are.
        let mut caret = None;
        let mut before = 0;
        for span in &mut spans {
            if let Some(at) = span.content.find(CARET) {
                caret = Some(before + cells(&span.content[..at]));
                span.content = span.content.replace(CARET, " ").into();
            }
            before += cells(&span.content);
        }
        let line = Line::from(spans);
        let rect = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(line).style(band), rect);
        if let Some(column) = caret {
            frame.set_cursor_position(Position::new((rect.x + column as u16).min(rect.right().saturating_sub(1)), y));
        }
    }
}

pub struct Hint {
    pub key: String,
    pub label: String,
    pub press: Option<KeyEvent>,
    /// On a narrow footer the highest goes first; 0 always stays.
    pub drop: u8,
}

impl Hint {
    fn drops(mut self, rank: u8) -> Self {
        self.drop = rank;
        self
    }
}

fn hint(key: &str, label: &str, code: KeyCode) -> Hint {
    Hint {
        key: key.to_string(),
        label: label.to_string(),
        press: Some(KeyEvent::new(code, KeyModifiers::NONE)),
        drop: 1,
    }
}

/// An action's hint, under whichever key config.toml gives it; none (filtered out) when it has no key.
fn act(app: &App, action: Action, label: &str) -> Hint {
    match app.keys.first(action) {
        Some(press) => Hint {
            key: keymap::label(&press),
            label: label.to_string(),
            press: Some(press),
            drop: 1,
        },
        None => Hint {
            key: String::new(),
            label: String::new(),
            press: None,
            drop: 0,
        },
    }
}

fn note(label: &str) -> Hint {
    Hint {
        key: String::new(),
        label: label.to_string(),
        press: None,
        drop: 0,
    }
}

/// Where marked agents would go, in words: "main › 1 api", "session other", "a new window".
fn destination_name(app: &App) -> String {
    match app.destination() {
        Some(Place::Window(id)) => app
            .windows
            .iter()
            .find(|w| w.id == id)
            .map_or_else(|| "that window".to_string(), |w| format!("{} › {} {}", w.session, w.index, w.name)),
        Some(Place::Session(id)) => app
            .windows
            .iter()
            .find(|w| w.session_id == id)
            .map_or_else(|| "that session".to_string(), |w| format!("session {}", w.session)),
        Some(Place::NewSession) => "a new tmux session".to_string(),
        Some(Place::NewWindow) | None => "a new window".to_string(),
    }
}

/// Every footer has the same shape, read left to right: what's going on, the main action, what acts on this row, what
/// works anywhere; then, at the right edge, help and the way out. The last group is that right-hand one. Narrow, the
/// least used go first (the highest `drop`); the main action, help and the way out always stay.
pub fn shortcuts(app: &App) -> Vec<Vec<Hint>> {
    let mut groups = footer(app);
    for group in &mut groups {
        group.retain(|h| !h.key.is_empty() || !h.label.is_empty()); // an action left without a key
    }
    groups
}

fn footer(app: &App) -> Vec<Vec<Hint>> {
    let esc = |label: &str| hint("esc", label, KeyCode::Esc).drops(0);
    let enter = |label: &str| hint("↵", label, KeyCode::Enter).drops(0);
    // Enter while typing is always Enter; elsewhere it's whatever goes.
    let go = |label: &str| act(app, Action::Go, label).drops(0);
    let help = || act(app, Action::Help, "help").drops(0);
    let selected = app.selected();
    // Typing or choosing: only what that takes, and the way back.
    if matches!(app.mode, Mode::Folder(_)) {
        let leave = if app.moving_agent() { "cancel" } else { "keep folder" };
        return vec![
            vec![
                enter(if app.moving_agent() { "choose folder" } else { "set folder" }),
                hint("← →", "up, into", KeyCode::Right).drops(2),
                hint("⇥", "complete", KeyCode::Tab).drops(1),
            ],
            vec![esc(leave)],
        ];
    }
    if let Mode::Rename(rename) = &app.mode {
        // Emptying the name only means something for an agent that picks its own.
        let tool = app.pane(&rename.target).map(|p| p.tool.as_str());
        let picks = rename.kind == Kind::Pane && !tool.and_then(|t| app.agents.get(t)).is_some_and(|a| a.retitles());
        let mut line = vec![enter("save")];
        if picks {
            let name = tool.map_or_else(|| "the agent".to_string(), |t| app.agents.name(t));
            line.push(hint("⌫", &format!("empty to let {name} pick"), KeyCode::Backspace));
        }
        return vec![line, vec![esc("keep old name")]];
    }
    if app.searching() {
        let main = if app.marks.is_empty() {
            enter("go to")
        } else {
            enter(&marked_enter(app))
        };
        return vec![vec![note("type to filter"), main], vec![esc("back")]];
    }
    if let Some((action, row)) = &app.confirm {
        let again = match (action, row) {
            (ConfirmAction::Compact, _) => act(app, Action::Compact, "compact"),
            (_, RowId::Conversation(..)) => act(app, Action::Kill, "remove"),
            _ => act(app, Action::Kill, "kill"),
        };
        return vec![vec![again.drops(0)], vec![esc("cancel")]];
    }
    if let Some(RowId::Draft(_)) = selected {
        let start = if app.drafts.len() > 1 { "start all" } else { "start" };
        let mut line = vec![
            go(start),
            act(app, Action::SwitchAgent, "agent").drops(2),
            act(app, Action::Folder, "folder").drops(2),
        ];
        if !app.flat {
            line.push(act(app, Action::Detach, "out a level").drops(3));
        }
        line.push(act(app, Action::New, "another").drops(4));
        return vec![line, vec![esc("remove")]];
    }
    if app.selected_group().is_some() {
        let line = vec![
            go("start all"),
            act(app, Action::New, "another").drops(2),
            act(app, Action::Detach, "out a level").drops(3),
        ];
        return vec![line, vec![esc("remove")]];
    }
    // Something waiting on you: staged changes, marked agents, failures. Their way out drops them.
    if app.has_staged() {
        let word = if app.failed.is_empty() { "apply" } else { "retry" };
        let changes = match app.staged.len() + app.staged_windows.len() {
            1 => format!("{word} 1 change"),
            n => format!("{word} {n} changes"),
        };
        let apply = if app.marks.is_empty() {
            changes
        } else {
            format!("{changes}, {}", marked_enter(app))
        };
        let mut line = vec![go(&apply)];
        if matches!(selected, Some(RowId::Pane(_))) {
            line.push(act(app, Action::Folder, "folder").drops(2));
            line.push(act(app, Action::SwitchAgent, "switch agent").drops(3));
            if !app.flat {
                line.push(act(app, Action::Detach, "window or session").drops(4));
            }
        }
        return vec![line, vec![help(), esc(if app.failed.is_empty() { "drop changes" } else { "dismiss" })]];
    }
    if !app.marks.is_empty() {
        return marked_hints(app);
    }
    if !app.failed.is_empty() {
        return vec![vec![note(&format!("{} failed", app.failed.len()))], vec![help(), esc("dismiss")]];
    }
    if selected == Some(&RowId::Loading) {
        let tabs = Hint {
            key: format!("{}/{}", app.keys.label(Action::PrevTab), app.keys.label(Action::NextTab)),
            ..act(app, Action::NextTab, "switch tab")
        };
        return vec![vec![tabs.drops(1)], vec![help(), esc("close")]];
    }
    // The everyday footer: on an agent, a heading or a conversation.
    let tool = match selected {
        Some(RowId::Pane(id)) => app.pane(id).map(|p| p.tool.clone()),
        Some(RowId::Conversation(tool, _)) => Some(tool.clone()),
        _ => None,
    };
    let can_switch = tool.is_some_and(|t| app.agents.switchable(&t));
    let mut main = match selected {
        Some(RowId::Conversation(..)) => "open".to_string(),
        Some(RowId::Pane(_)) | None => "go to".to_string(),
        _ => "fold".to_string(),
    };
    if can_switch && let Some((_, target)) = &app.switch {
        main = format!("open with {}", app.agents.name(target));
    }
    // Only the most used, so the footer reads at a glance; ? lists every key.
    let mut row = vec![go(&main)];
    match selected {
        // Continuing a conversation in another CLI is what sets this apart: it comes first, wherever it works.
        Some(RowId::Pane(_)) => {
            if can_switch {
                row.push(act(app, Action::SwitchAgent, "switch agent").drops(1));
            }
            row.push(act(app, Action::Mark, "mark").drops(1));
            row.push(act(app, Action::Rename, "rename").drops(2));
        }
        Some(RowId::Conversation(..)) => {
            if can_switch {
                row.push(act(app, Action::SwitchAgent, "switch agent").drops(1));
            }
            row.push(act(app, Action::Mark, "mark").drops(1));
        }
        Some(RowId::Window(..) | RowId::Session(_)) => row.push(act(app, Action::Rename, "rename").drops(2)),
        _ => {}
    }
    let mut anywhere = Vec::new();
    if !app.in_history() {
        anywhere.push(act(app, Action::New, "new").drops(2));
    }
    vec![row, anywhere, vec![help(), esc("close")]]
}

/// Where Enter takes marked agents, in words.
fn marked_enter(app: &App) -> String {
    let on_conversation = matches!(app.selected(), Some(RowId::Conversation(..)));
    if on_conversation || app.marks.iter().any(|mark| matches!(mark, RowId::Conversation(..))) {
        return "open together".to_string();
    }
    let agents = app.marks.iter().filter(|mark| matches!(mark, RowId::Pane(_))).count();
    format!("move {agents} into {}", destination_name(app))
}

fn marked_hints(app: &App) -> Vec<Vec<Hint>> {
    let agents = app.marks.iter().filter(|mark| matches!(mark, RowId::Pane(_))).count();
    let mut line = vec![
        note(&format!("{} marked", app.marks.len())),
        act(app, Action::Go, &marked_enter(app)).drops(0),
    ];
    // On a marked row these act on all of it: the count says so.
    if app.selected().is_some_and(|row| app.marks.contains(row)) {
        let all = app.marks.len();
        let kill = if agents == 0 { format!("remove {all}") } else { format!("kill {all}") };
        line.push(act(app, Action::Mark, "unmark").drops(1));
        if agents > 0 && !app.flat {
            let next = match app.marks_detach {
                None => format!("{agents} to a new window"),
                Some(Detach::Window) => format!("{agents} to a new session"),
                Some(Detach::Session) => "back".to_string(),
            };
            line.push(act(app, Action::Detach, &next).drops(3));
        }
        if agents > 0 {
            line.push(act(app, Action::Compact, &format!("compact {agents}")).drops(5));
        }
        line.push(act(app, Action::Kill, &kill).drops(4));
    } else {
        line.push(act(app, Action::Mark, "mark").drops(1));
    }
    let leave = if app.failed.is_empty() { "clear marks" } else { "dismiss" };
    vec![
        line,
        vec![act(app, Action::Help, "help").drops(0), hint("esc", leave, KeyCode::Esc).drops(0)],
    ]
}

/// A confirmation, or the outcome of the last action, for the band that takes the footer's place.
pub fn notice(app: &App) -> (String, Tone) {
    if let Some((action, row)) = &app.confirm {
        let text = match (action, row) {
            // Agents only: c sends nothing to a marked conversation.
            (ConfirmAction::Compact, _) if compacted(app) > 1 => {
                format!("Compact {} agents? {} again.", compacted(app), app.keys.label(Action::Compact))
            }
            (ConfirmAction::Compact, RowId::Pane(id)) => format!("Compact \"{}\"? {} again.", shown_name(app, id), app.keys.label(Action::Compact)),
            (_, _) if app.targets().len() > 1 => {
                let agents = app.targets().iter().filter(|t| matches!(t, RowId::Pane(_))).count();
                let conversations = app.targets().len() - agents;
                match (agents, conversations) {
                    (a, 0) => format!("Kill {a} agents? Their tmux panes will close."),
                    (0, c) => format!(
                        "Remove {c} conversations from History? {} again (they can be restored)",
                        app.keys.label(Action::Kill)
                    ),
                    (a, c) => format!("Kill {a} agents and remove {c} conversations? {} again", app.keys.label(Action::Kill)),
                }
            }
            (_, RowId::Conversation(tool, id)) => {
                let title = app.conversation(tool, id).map_or("", |c| c.title.as_str());
                format!(
                    "Remove \"{title}\" from History? {} again (it can be restored)",
                    app.keys.label(Action::Kill)
                )
            }
            (_, RowId::Pane(id)) => format!("Kill \"{}\"? Its tmux pane will close.", shown_name(app, id)),
            _ => String::new(),
        };
        return (text, Tone::Ask);
    }
    (app.notice.text().to_string(), app.tone())
}

/// How many agents a compaction would go to: marked conversations aren't.
fn compacted(app: &App) -> usize {
    app.targets().iter().filter(|target| matches!(target, RowId::Pane(_))).count()
}

/// The agent by the name its row shows, not its pane id.
fn shown_name(app: &App, id: &str) -> String {
    app.pane(id).map_or_else(|| id.to_string(), |p| p.shown_name(&app.agents))
}

fn hint_width(h: &Hint) -> usize {
    if h.key.is_empty() {
        cells(&h.label)
    } else {
        cells(&h.key) + 1 + cells(&h.label)
    }
}

/// A message fills the whole row in the theme's color for it. The keys come back when it goes; one being typed into keeps
/// them.
fn draw_band(frame: &mut Frame, app: &mut App, area: Rect, message: &str, tone: Tone) {
    // Under way needs no alarm and takes no status color: blue and green already mean done and working on the rows.
    let (glyph, style) = match tone {
        Tone::Working => ("◐", bold()),
        Tone::Done => ("✓", filled(text::palette().success)),
        Tone::Ask => ("?", filled(text::palette().needs_you)),
        Tone::Failed => ("✗", filled(text::palette().failed)),
    };
    let hints = match tone {
        Tone::Failed if app.lasting_failure() && app.has_staged() => {
            vec![act(app, Action::Go, "retry"), hint("esc", "dismiss", KeyCode::Esc)]
        }
        Tone::Failed if app.lasting_failure() => vec![hint("esc", "dismiss", KeyCode::Esc)],
        Tone::Ask => vec![hint("esc", "cancel", KeyCode::Esc)],
        _ => Vec::new(),
    };
    let right: usize = hints.iter().map(|h| hint_width(h) + 2).sum();
    let width = area.width as usize;
    let room = width.saturating_sub(right + 4);
    let text = if cells(message) > room {
        format!("{}…", fit(message, room.saturating_sub(1), true).trim_end())
    } else {
        message.to_string()
    };
    let mut bar = Bar::new(area);
    bar.push(format!(" {glyph} {text}"), style);
    bar.push(" ".repeat(width.saturating_sub(cells(&text) + 3 + right)), style);
    for hint in hints {
        let start = bar.x;
        bar.push(format!("{} {}  ", hint.key, hint.label), style);
        if let Some(press) = hint.press {
            let rect = Rect {
                x: start,
                y: area.y,
                width: bar.x - start,
                height: 1,
            };
            app.regions.push((rect, Click::Key(press)));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(bar.spans)), area);
}

fn draw_footer(frame: &mut Frame, app: &mut App, area: Rect) {
    let (message, tone) = notice(app);
    if !message.is_empty() && matches!(app.mode, Mode::Normal) {
        return draw_band(frame, app, area, &message, tone);
    }
    let room = (area.width as usize).saturating_sub(4);
    let mut groups = shortcuts(app);
    let width = |groups: &Vec<Vec<Hint>>| {
        let parts = groups.iter().filter(|g| !g.is_empty());
        parts
            .map(|g| g.iter().map(hint_width).sum::<usize>() + 2 * g.len().saturating_sub(1) + 4)
            .sum::<usize>()
    };
    // Short of room, the least used go first: the highest drop, the rightmost of those.
    while width(&groups) > room {
        let worst = groups
            .iter()
            .enumerate()
            .flat_map(|(g, group)| group.iter().enumerate().map(move |(i, h)| (h.drop, g, i)))
            .filter(|(drop, ..)| *drop > 0)
            .max_by_key(|(drop, g, i)| (*drop, *g, *i));
        let Some((_, g, i)) = worst else { break };
        groups[g].remove(i);
    }
    let right = groups.pop().unwrap_or_default();
    let mut bar = Bar::new(area);
    let draw = |bar: &mut Bar, hints: &[Hint], app: &mut App| {
        for (i, h) in hints.iter().enumerate() {
            if i > 0 {
                bar.push("  ".into(), Style::new());
            }
            let start = bar.x;
            if !h.key.is_empty() {
                bar.push(h.key.to_string(), bold());
                bar.push(" ".into(), Style::new());
            }
            bar.push(h.label.clone(), Style::new());
            if let Some(press) = h.press {
                let rect = Rect {
                    x: start,
                    y: area.y,
                    width: bar.x - start,
                    height: 1,
                };
                app.regions.push((rect, Click::Key(press)));
            }
        }
    };
    bar.push("  ".into(), Style::new());
    for group in groups.iter().filter(|g| !g.is_empty()) {
        if bar.x > area.x + 2 {
            bar.push("    ".into(), Style::new());
        }
        draw(&mut bar, group, app);
    }
    // Help and the way out keep their place at the right edge.
    let right_width = right.iter().map(hint_width).sum::<usize>() + 2 * right.len().saturating_sub(1);
    let gap = (area.right() as usize).saturating_sub(bar.x as usize + right_width + 2).max(4);
    bar.push(" ".repeat(gap), Style::new());
    draw(&mut bar, &right, app);
    frame.render_widget(Paragraph::new(Line::from(bar.spans)), area);
}

fn state_label(state: State) -> Span<'static> {
    Span::styled(format!("{} {}", state.dot(), state.label()), Style::new().fg(state.color()))
}

fn pane_header(app: &App, id: &str) -> Option<Line<'static>> {
    let pane = app.pane(id)?;
    let kind = app.agents.name(&pane.tool);
    let staged = app.staged.iter().find(|stage| stage.pane == pane.id);
    let detach = staged
        .and_then(|stage| stage.detach)
        .or(app.marks.contains(&RowId::Pane(pane.id.clone())).then_some(app.marks_detach).flatten());
    let mut changes = Vec::new();
    if let Some(target) = staged.and_then(|stage| stage.tool.as_ref()) {
        changes.push(Span::raw("   "));
        changes.extend(rows::switched_to(target, app.agents.name(target).chars().count() + 2, &app.agents));
    }
    if let Some(folder) = staged.and_then(|stage| stage.folder.as_ref()) {
        changes.push(Span::styled(format!("   → {}", tmux::tilde(folder)), bold().fg(crate::text::marker())));
    }
    if let Some(detach) = detach {
        let place = if detach == Detach::Window {
            "new tmux window"
        } else {
            "new tmux session"
        };
        changes.push(Span::styled(format!("   open in a {place}"), bold().fg(crate::text::marker())));
    }
    let mut line = vec![
        Span::styled(pane.shown_name(&app.agents), bold()),
        Span::raw("  "),
        state_label(pane.state),
        Span::raw("  "),
        Span::styled(kind, Style::new().fg(app.agents.tint(&pane.tool))),
        Span::raw("  "),
        Span::styled(
            format!("{}   {} › {} › pane {}", pane.folder, pane.session, pane.window_index, pane.pane_index),
            dim(),
        ),
    ];
    line.extend(changes);
    Some(Line::from(line))
}

/// How long ago a conversation was last active, in words: "just now", "1 minute ago", "3 hours ago".
fn last_active(seconds: i64) -> String {
    let (count, unit) = match seconds {
        s if s < 60 => return "just now".into(),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86_400 => (s / 3600, "hour"),
        s => (s / 86_400, "day"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

fn conversation_lines(app: &App, tool: &str, id: &str, width: usize) -> Vec<Line<'static>> {
    let Some(c) = app.conversation(tool, id) else { return Vec::new() };
    let seconds = tmux::now() - c.time;
    let ago = last_active(seconds);
    let title = if c.title.is_empty() { "(untitled)".to_string() } else { c.title.clone() };
    let tint = Style::new().fg(app.agents.tint(tool));
    let mut lines = vec![
        Line::from(vec![
            Span::styled(title, bold()),
            Span::raw("  "),
            Span::styled(app.agents.name(tool), tint),
            Span::raw("  "),
            Span::styled(format!("{}   last active {ago}", tmux::tilde(&c.folder)), dim()),
        ]),
        Line::raw(""),
    ];
    // How it started (your first message, when there were more) and how it ended (your last one, the answer): what it was
    // about, and where it left off.
    for (who, text) in &app.preview.messages {
        if text.is_empty() {
            continue;
        }
        let (label, most) = match who {
            Who::First => (Line::styled("Started with", dim()), 4),
            Who::You => (Line::styled("You", bold()), 8),
            Who::Agent => (Line::styled(app.agents.name(tool), tint.add_modifier(Modifier::BOLD)), 30),
        };
        lines.push(label);
        lines.extend(
            text::fold(text, width.saturating_sub(4))
                .into_iter()
                .take(most)
                .map(|line| Line::raw(format!("  {line}"))),
        );
        lines.push(Line::raw(""));
    }
    lines
}

fn node_lines(app: &App, session: &str, window: Option<&str>) -> Vec<Line<'static>> {
    let list: Vec<_> = app
        .panes
        .iter()
        .filter(|p| p.session_id == session && window.is_none_or(|w| p.window_id == w))
        .collect();
    let Some(first) = list.first() else { return Vec::new() };
    let place = if window.is_some() {
        format!("{} › {} {}", first.session, first.window_index, first.window)
    } else {
        first.session.clone()
    };
    let n = list.len();
    let mut lines = vec![Line::from(vec![
        Span::styled(place, bold()),
        Span::styled(
            format!(
                "  {n} {}   {} folds it   {} renames it",
                if n == 1 { "agent" } else { "agents" },
                app.keys.label(Action::Go),
                app.keys.label(Action::Rename)
            ),
            dim(),
        ),
    ])];
    for pane in list {
        let name = pane.shown_name(&app.agents);
        lines.push(Line::from(vec![
            Span::styled(pane.state.dot(), Style::new().fg(pane.state.color())),
            Span::raw(format!(" {}  ", fit(&name, 24, true))),
            Span::styled(format!("{} {}   ", pane.window_index, pane.window), dim()),
            Span::styled(pane.state.label(), Style::new().fg(pane.state.color())),
        ]));
    }
    lines
}

/// A new window or session and the new agents that will share it.
fn group_lines(app: &App) -> Vec<Line<'static>> {
    let Some(place) = app.selected_group() else { return Vec::new() };
    let ids = app.drafts_for(&place);
    let place = ids
        .first()
        .and_then(|id| app.draft(*id))
        .map(|draft| draft.place.clone())
        .unwrap_or_default();
    let mut lines = vec![
        Line::from(vec![Span::styled("as a  ", dim()), Span::styled(place, bold().fg(crate::text::marker()))]),
        Line::raw(""),
    ];
    for draft in ids.iter().filter_map(|id| app.draft(*id)) {
        lines.push(Line::from(vec![
            Span::styled(
                fit(&app.agents.name(&draft.agent), 11, true),
                Style::new().fg(app.agents.tint(&draft.agent)),
            ),
            Span::raw(format!(" {}", tmux::tilde(&draft.folder))),
        ]));
    }
    lines
}

fn draft_lines(app: &App) -> Vec<Line<'static>> {
    let Some(draft) = app.selected_draft().and_then(|id| app.draft(id)) else {
        return Vec::new();
    };
    vec![
        Line::from(vec![
            Span::styled("New ", bold()),
            Span::styled(app.agents.name(&draft.agent), bold().fg(app.agents.tint(&draft.agent))),
            Span::styled(" agent", bold()),
        ]),
        Line::raw(""),
        Line::from(vec![Span::styled("in", dim()), Span::raw(format!("    {}", tmux::tilde(&draft.folder)))]),
        Line::from(vec![
            Span::styled("as a  ", dim()),
            Span::styled(draft.place.clone(), bold().fg(crate::text::marker())),
        ]),
    ]
}

/// A group of keys and what they do; an action left without a key isn't listed.
fn help_group(title: &str, keys: &[(String, String)], width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(title.to_string(), bold()), Line::raw("")];
    for (key, text) in keys.iter().filter(|(key, _)| !key.is_empty()) {
        lines.push(Line::from(vec![
            Span::styled(fit(key, width, false), bold()),
            Span::styled(text.to_string(), dim()),
        ]));
    }
    lines
}

/// The width of the keys in the help's left and right columns: every description in a column starts in the same place.
const LEFT: usize = 11;
const RIGHT: usize = 20;

fn help_statuses() -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled("Statuses", bold()), Line::raw("")];
    let meaning = [
        (State::Waiting, "asking you something, like a permission"),
        (State::Running, "in the middle of a turn"),
        (State::Done, "finished while you were elsewhere"),
        (State::Idle, "waiting for your next message"),
    ];
    for (state, text) in meaning {
        let label = format!("{} {}", state.dot(), state.label());
        lines.push(Line::from(vec![
            Span::styled(fit(&label, RIGHT, false), Style::new().fg(state.color())),
            Span::styled(text, dim()),
        ]));
    }
    lines
}

/// Every key, in four groups: side by side when the preview is wide enough.
/// The tabs' keys for the help: "1-9 0" as they come, each tab's own when remapped, History's only if it has one.
fn tab_keys(app: &App) -> (String, String) {
    let tabs: Vec<String> = (1..=9).map(|place| app.keys.label(Action::Tab(place))).collect();
    let history = app.keys.label(Action::Tab(0));
    let defaults = tabs.iter().enumerate().all(|(i, key)| *key == (i + 1).to_string()) && history == "0";
    let keys = if defaults {
        "1-9 0".to_string()
    } else {
        let bound: Vec<&str> = tabs.iter().chain([&history]).map(String::as_str).filter(|k| !k.is_empty()).collect();
        bound.join(" ")
    };
    let text = if history.is_empty() {
        "open a tab by its place"
    } else {
        "open a tab by its place; the last is History"
    };
    (keys, text.into())
}

fn help_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let key = |action| app.keys.label(action);
    let agent = help_group(
        "This agent",
        &[
            // In the footer's order: the most used first.
            (key(Action::Go), "go to it, fold a heading, or apply staged changes".into()),
            (key(Action::SwitchAgent), "continue its conversation in another agent (staged)".into()),
            (
                key(Action::Mark),
                format!("mark it; {} groups marked, or moves them to a heading", key(Action::Go)),
            ),
            (key(Action::Rename), "rename it, its window or its session".into()),
            (key(Action::Folder), "move it to another folder, if idle (staged)".into()),
            (key(Action::Detach), "tree: to a new window or session (staged)".into()),
            (key(Action::Compact), "compact its conversation (press twice)".into()),
            (key(Action::Kill), "kill it, or remove a conversation (press twice)".into()),
        ],
        LEFT,
    );
    let moving = help_group(
        "Moving around",
        &[
            (app.keys.pair(Action::Down, Action::Up), "move down or up".into()),
            (app.keys.pair(Action::PrevTab, Action::NextTab), "previous or next tab".into()),
            (app.keys.pair(Action::First, Action::Last), "first or last row".into()),
            tab_keys(app),
            (key(Action::Search), "search every agent".into()),
        ],
        RIGHT,
    );
    let anywhere = help_group(
        "Anywhere",
        &[
            (key(Action::New), "new agent in a new pane, window or session; on one, another".into()),
            (key(Action::Folder), "on a new agent: choose its folder".into()),
            (key(Action::Layout), "switch between the list and the tree".into()),
            (key(Action::Help), "show or hide this help".into()),
            ("esc".into(), "back one step at a time, the last opened first".into()),
            // Ctrl+C always closes, whatever close is bound to.
            (
                Some(key(Action::Close)).filter(|k| !k.is_empty()).unwrap_or_else(|| "C-c".into()),
                "close".into(),
            ),
            ("mouse".into(), "click selects, double-click goes, right-click marks".into()),
            ("wheel".into(), "scroll the preview, or move through the list".into()),
        ],
        LEFT,
    );
    // tmux's own keys, outside the board: shown as config.toml sets them, left out when unbound.
    let tmux_keys = crate::config::get().keys.clone();
    let prefixed = |key: &Option<String>| key.as_ref().map(|k| format!("prefix {k}"));
    let mut outside = Vec::new();
    if let Some(open) = prefixed(&tmux_keys.open) {
        outside.push((open, "open or close this board".to_string()));
    }
    if let Some(root) = tmux_keys.open_root.clone() {
        outside.push((root, "the same, without the prefix".into()));
    }
    if let Some(back) = prefixed(&tmux_keys.back) {
        outside.push((back, "back to where you were before the board took you".into()));
    }
    let mut anywhere = anywhere;
    if !outside.is_empty() {
        anywhere.push(Line::raw(""));
        anywhere.extend(help_group("In tmux", &outside, LEFT));
    }
    let statuses = help_statuses();
    let mut lines = Vec::new();
    if width < 120 {
        for group in [agent, moving, anywhere, statuses] {
            lines.extend(group);
            lines.push(Line::raw(""));
        }
        return lines;
    }
    for (left, right) in [(agent, moving), (anywhere, statuses)] {
        for i in 0..left.len().max(right.len()) {
            let mut spans = left.get(i).map_or_else(Vec::new, |line| line.spans.clone());
            let used: usize = spans.iter().map(|s| cells(&s.content)).sum();
            spans.push(Span::raw(" ".repeat(65usize.saturating_sub(used))));
            spans.extend(right.get(i).map_or_else(Vec::new, |line| line.spans.clone()));
            lines.push(Line::from(spans));
        }
        lines.push(Line::raw(""));
    }
    lines
}

/// The preview: one fixed line saying which agent it is and where it lives, then its screen, kept at its newest output;
/// on a session or window, the agents in it; with ? on, every key.
fn draw_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    app.regions.push((area, Click::Preview));
    let width = area.width as usize;
    let selected = app.selected().cloned();
    let fixed = if app.help {
        help_lines(app, width)
    } else {
        match &selected {
            Some(RowId::Conversation(tool, id)) => conversation_lines(app, tool, id, width),
            Some(RowId::Session(session)) => node_lines(app, session, None),
            Some(RowId::Window(session, window)) => node_lines(app, session, Some(window)),
            Some(RowId::Draft(_)) => draft_lines(app),
            Some(RowId::NewWindow(_) | RowId::NewSession) => group_lines(app),
            _ => Vec::new(),
        }
    };
    if app.help {
        // Taller than the room at a narrow width: it scrolls, and says so while there's more.
        let most = fixed.len().saturating_sub(area.height as usize);
        app.help_scroll = app.help_scroll.min(most);
        app.help_most = most;
        frame.render_widget(Paragraph::new(fixed).scroll((app.help_scroll as u16, 0)), area);
        if app.help_scroll < most && area.height > 0 {
            let last = Rect {
                y: area.bottom() - 1,
                height: 1,
                ..area
            };
            frame.render_widget(Paragraph::new(Line::styled("↓ scroll for more", dim())).alignment(Alignment::Right), last);
        }
        return;
    }
    let Some(RowId::Pane(id)) = selected else {
        frame.render_widget(Paragraph::new(fixed), area);
        return;
    };
    let mut lines = Vec::new();
    lines.extend(pane_header(app, &id));
    // The row and the band have only so much room: here the reason is whole.
    if let Some(reason) = app.failed.get(&id) {
        lines.push(Line::from(Span::styled(format!(" ✗ {reason} "), filled(text::palette().failed))));
    }
    let height = (area.height as usize).saturating_sub(lines.len());
    app.preview.height = height;
    let total = app.preview.lines.len();
    let end = total.saturating_sub(app.preview.up);
    lines.extend(app.preview.lines[end.saturating_sub(height)..end].iter().cloned());
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::FolderEdit;
    use crate::app::tests::{app, sample};
    use crate::input::Input;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn the_list_comes_first_then_the_shortcuts_then_the_preview() {
        let mut app = app(sample());
        app.select(&RowId::Pane("%1".into()));
        let lines = screen(&mut app, 130, 40);
        let find = |text: &str| {
            lines
                .iter()
                .position(|line| line.contains(text))
                .unwrap_or_else(|| panic!("{text} missing:\n{}", lines.join("\n")))
        };
        assert!(find("All·4") < find("Agent %1"));
        assert!(find("Agent %1") < find("? help"));
        assert!(find("? help") < find("pane 0"));
        assert!(lines[find("Kind")].contains("Status"));
    }

    #[test]
    fn a_short_popup_gives_the_preview_room_to_the_list() {
        let mut app = app(sample());
        let lines = screen(&mut app, 130, 16);
        assert!(lines.iter().any(|line| line.contains("? help")));
    }

    #[test]
    fn after_a_failed_move_the_footer_offers_the_move_again() {
        let mut app = app(sample());
        app.marks.push(RowId::Pane("%1".into()));
        app.failed.insert("%1".into(), "no space".into());
        let lines = screen(&mut app, 150, 30);
        let footer = lines.iter().find(|line| line.contains("1 marked")).unwrap();
        assert!(footer.contains("↵ move") && footer.contains("esc dismiss"), "{footer}");
    }

    #[test]
    fn a_message_takes_the_footers_place_and_a_long_one_keeps_its_way_out() {
        let mut app = app(sample());
        app.fail("Could not reach the agent");
        app.failed.insert("%1".into(), "it broke".into());
        let lines = screen(&mut app, 70, 30);
        let footer = lines.iter().find(|line| line.contains("✗ Could not reach the agent")).unwrap();
        assert!(footer.contains("esc dismiss"));
        assert!(!footer.contains("? help"));
        app.fail(format!("Could not {}", "reach the agent ".repeat(9)));
        let lines = screen(&mut app, 70, 30);
        let footer = lines.iter().find(|line| line.contains("✗ Could not")).unwrap();
        assert!(footer.contains('…') && footer.contains("esc dismiss"));
    }

    #[test]
    fn remapped_keys_show_in_the_footer_the_help_and_the_messages() {
        let mut app = app(sample());
        let key = |name| keymap::parse_key(name).unwrap();
        app.keys.bind(Action::Kill, vec![key("z")], &[]).unwrap();
        app.keys.bind(Action::Mark, vec![key("v")], &[]).unwrap();
        app.keys.bind(Action::Layout, vec![], &[]).unwrap();
        app.rebuild();
        app.select(&RowId::Pane("%1".into()));
        let lines = screen(&mut app, 150, 40);
        let footer = lines.iter().find(|line| line.contains("? help")).unwrap();
        assert!(footer.contains("v mark") && !footer.contains("␣ mark"), "{footer}");
        assert!(footer.contains("r rename"), "rename is offered on an agent: {footer}");
        assert!(footer.contains("⇥ switch agent"), "and continuing it in another CLI: {footer}");
        assert!(!footer.contains(" tree"), "an action without a key has no hint: {footer}");
        app.on_key(key("z"));
        let (text, _) = notice(&app);
        assert!(text.contains("Its tmux pane will close"), "{text}");
        app.help = true;
        app.confirm = None;
        let lines = screen(&mut app, 150, 50);
        assert!(lines.iter().any(|line| line.contains("z ") && line.contains("kill it, or remove")));
        assert!(!lines.iter().any(|line| line.contains("switch between the list and the tree")));
    }

    #[test]
    fn tabs_show_their_number_keys_only_when_asked() {
        let mut app = app(sample());
        let header = |app: &mut App| screen(app, 150, 40).into_iter().find(|line| line.contains("History")).unwrap();
        assert!(!header(&mut app).contains("1 All"), "off by default");
        app.tab_numbers = true;
        let line = header(&mut app);
        assert!(
            line.contains("[1. All·") && line.contains("2. Needs you") && line.contains("0. History"),
            "{line}"
        );
        app.keys.bind(Action::Tab(1), vec![keymap::parse_key("a").unwrap()], &[]).unwrap();
        app.keys.bind(Action::Tab(0), vec![], &[]).unwrap();
        let line = header(&mut app);
        assert!(
            line.contains("[a. All·") && line.contains("  History"),
            "a remapped tab shows its key, an unbound one none: {line}"
        );
    }

    #[test]
    fn the_help_shows_the_tab_keys_as_they_are() {
        let mut app = app(sample());
        assert_eq!(tab_keys(&app).0, "1-9 0");
        app.keys.bind(Action::Tab(1), vec![keymap::parse_key("a").unwrap()], &[]).unwrap();
        assert_eq!(tab_keys(&app).0, "a 2 3 4 5 6 7 8 9 0");
        app.keys.bind(Action::Tab(0), vec![], &[]).unwrap();
        assert_eq!(tab_keys(&app), ("a 2 3 4 5 6 7 8 9".to_string(), "open a tab by its place".to_string()));
        for place in 1..=9 {
            app.keys.bind(Action::Tab(place), vec![], &[]).unwrap();
        }
        assert_eq!(tab_keys(&app).0, "", "no row at all");
    }

    #[test]
    fn help_too_tall_for_the_room_scrolls_and_says_so() {
        let mut app = app(sample());
        app.help = true;
        let lines = screen(&mut app, 106, 38);
        assert!(lines.iter().any(|l| l.contains("↓ scroll for more")), "{}", lines.join("\n"));
        assert!(!lines.iter().any(|l| l.contains("asking you something")), "the end is below");
        app.help_scroll = app.help_most;
        let lines = screen(&mut app, 106, 38);
        assert!(
            lines.iter().any(|l| l.contains("asking you something")),
            "scrolled to the end: {}",
            lines.join("\n")
        );
        assert!(!lines.iter().any(|l| l.contains("↓ scroll for more")));
    }

    #[test]
    fn help_replaces_the_preview() {
        let mut app = app(sample());
        app.help = true;
        let lines = screen(&mut app, 150, 50);
        assert!(lines.iter().any(|line| line.contains("This agent") && line.contains("Moving around")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("◆ Needs you") && line.contains("asking you something"))
        );
    }

    #[test]
    fn the_dropdown_covers_rows_instead_of_pushing_them() {
        let mut app = app(sample());
        let row = RowId::Pane("%1".to_string());
        app.select(&row);
        let before = screen(&mut app, 130, 40);
        app.mode = Mode::Folder(FolderEdit {
            input: Input::new("~/"),
            suggested: Vec::new(),
            matches: vec!["~/alpha".into(), "~/beta".into()],
            index: 1,
            pane: Some("%1".into()),
            draft: 0,
            base: "/w".into(),
        });
        app.rebuild();
        let after = screen(&mut app, 130, 40);
        let find = |lines: &[String], text: &str| lines.iter().position(|line| line.contains(text)).unwrap();
        assert_eq!(app.selected(), Some(&row), "the cursor stays on the agent");
        assert_eq!(find(&after, "~/alpha"), find(&before, "Agent %1") + 1, "right under its row");
        assert_eq!(find(&after, "~/beta"), find(&after, "~/alpha") + 1);
        for name in ["Agent %2", "Agent %3", "Agent %4"] {
            assert_eq!(find(&after, name), find(&before, name), "{name} didn't move");
        }
        assert!(
            after.iter().any(|line| line.contains("Agent %1") && line.contains("~/")),
            "typed in its own row"
        );
        assert!(app.regions.iter().any(|(_, click)| matches!(click, Click::Folder(1))));
    }

    #[test]
    fn near_the_bottom_the_dropdown_opens_upward() {
        let mut app = app(sample());
        app.select(&RowId::Pane("%4".to_string()));
        let before = screen(&mut app, 130, 18);
        let row = before.iter().position(|line| line.contains("Agent %4")).unwrap();
        app.mode = Mode::Folder(FolderEdit {
            input: Input::new("~/"),
            suggested: Vec::new(),
            matches: (0..8).map(|i| format!("~/folder{i}")).collect(),
            index: 0,
            pane: Some("%4".into()),
            draft: 0,
            base: "/w".into(),
        });
        let after = screen(&mut app, 130, 18);
        let last = after.iter().position(|line| line.contains("~/folder7")).unwrap();
        assert!(last < row, "above its row:\n{}", after.join("\n"));
    }

    #[test]
    fn hints_and_tabs_are_clickable() {
        let mut app = app(sample());
        screen(&mut app, 130, 40);
        assert!(app.regions.iter().any(|(_, click)| matches!(click, Click::Tab(view) if view == HISTORY)));
        assert!(
            app.regions
                .iter()
                .any(|(_, click)| matches!(click, Click::Key(key) if key.code == KeyCode::Char('?')))
        );
    }

    #[test]
    fn last_active_reads_as_words() {
        assert_eq!(last_active(20), "just now");
        assert_eq!(last_active(60), "1 minute ago");
        assert_eq!(last_active(3 * 3600 + 5), "3 hours ago");
        assert_eq!(last_active(86_400), "1 day ago");
    }

    #[test]
    fn help_names_the_tmux_keys_outside_the_board() {
        let app = app(sample());
        let text: String = help_lines(&app, 150).iter().map(|line| line.to_string() + "\n").collect();
        assert!(
            text.contains("prefix a") && !text.contains("M-a") && text.contains("open or close this board"),
            "{text}"
        );
        assert!(text.contains("prefix b") && text.contains("back to where you were"), "{text}");
    }
}
