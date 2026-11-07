//! Starting agents and moving marked panes together.
use crate::agents::Registry;
use crate::config::{self, Split};
use crate::history::Conversation;
use crate::tmux;
use std::path::Path;
use std::process::Command;

/// The columns an agent needs to be of any use side by side. Below that, tiling its window gives it more room.
const NARROW: usize = 20;
/// The rows an agent needs stacked.
const SHORT: usize = 8;

impl Split {
    /// split-window's and join-pane's flag for it.
    pub fn flag(self) -> &'static str {
        match self {
            Split::Horizontal => "-h",
            Split::Vertical => "-v",
        }
    }

    /// The size it shares out, in tmux's format.
    fn room(self, of: &str) -> String {
        match self {
            Split::Horizontal => format!("#{{{of}_width}}"),
            Split::Vertical => format!("#{{{of}_height}}"),
        }
    }
}

/// Agents that go together sit side by side or stacked, if `panes` of them can in `room`, the window's width or height.
/// tmux's own layout keys rearrange them.
fn layout(split: Split, room: usize, panes: usize) -> &'static str {
    let (least, even) = match split {
        Split::Horizontal => (NARROW, "even-horizontal"),
        Split::Vertical => (SHORT, "even-vertical"),
    };
    if room / panes.max(1) < least { "tiled" } else { even }
}

/// Evens out a window of agents that were opened together.
pub fn spread(target: &str) {
    let split = config::get().board.split;
    let size = tmux::display(target, &format!("{} #{{window_panes}}", split.room("window"))).unwrap_or_default();
    let mut numbers = size.split(' ').filter_map(|n| n.parse().ok());
    if let (Some(room), Some(panes)) = (numbers.next(), numbers.next()) {
        tmux::ok(&["select-layout", "-t", target, layout(split, room, panes)]);
    }
}

pub fn lowest_window_name<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let used: Vec<&str> = names.collect();
    let free = (1..).find(|n| !used.contains(&format!("Agentic {n}").as_str()));
    format!("Agentic {}", free.unwrap_or(1))
}

pub fn next_window_name(session: &str) -> Option<String> {
    let names = tmux::out(&["list-windows", "-t", session, "-F", "#{window_name}"])?;
    Some(lowest_window_name(names.lines()))
}

/// A name for a new tmux session for a folder: its name ("home" for ~), made valid (tmux takes no "." or ":") and unique
/// ("api", "api-2", …).
pub fn new_session_name(folder: &str) -> String {
    let last = folder.trim_end_matches('/').rsplit('/').next().filter(|name| !name.is_empty());
    let base = if folder == tmux::home() {
        "home".to_string()
    } else {
        last.unwrap_or("agents").replace(['.', ':'], "-")
    };
    let mut name = base.clone();
    let mut i = 2;
    while tmux::ok(&["has-session", "-t", &format!("={name}")]) {
        name = format!("{base}-{i}");
        i += 1;
    }
    name
}

/// Replaces whatever runs in a pane with a login shell, in `folder`.
pub fn to_shell(pane: &str, folder: &str) -> Result<(), String> {
    // Named, or respawn-pane re-runs what the pane first ran, which may be the agent.
    let shell = tmux::option("default-shell").unwrap_or_else(|| "/bin/sh".into());
    let login = format!("{} -l", tmux::shell_quote(&shell));
    tmux::run(&["respawn-pane", "-k", "-t", pane, "-c", &tmux::name(folder), &login])
}

fn existing_folder(folder: &str) -> String {
    if Path::new(folder).is_dir() { folder.to_string() } else { tmux::home() }
}

/// Types `command` into a new pane's shell and switches to it.
fn go_run(pane: &str, command: &str) {
    tmux::run_in(pane, command);
    tmux::go(pane);
}

/// Opens a new tmux session in `folder`, types `command` into its shell like you would, and switches to it. Gives its pane.
pub fn launch(folder: &str, command: &str) -> Result<String, String> {
    let folder = &existing_folder(folder);
    let failed = || "Could not open a window".to_string();
    let name = new_session_name(folder);
    let pane = tmux::out(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        &tmux::name(&name),
        "-n",
        "Agentic 1",
        "-c",
        &tmux::name(folder),
    ]);
    let pane = pane.ok_or_else(failed)?;
    go_run(&pane, command);
    Ok(pane)
}

/// The panes to move, each once, all of them still agents: nothing moves unless every one can.
fn checked<'a>(agents: &Registry, panes: &'a [String]) -> Result<Vec<&'a String>, String> {
    let mut list = Vec::new();
    for id in panes {
        if !tmux::is_pane(id) {
            return Err("Invalid agent pane".into());
        }
        if list.contains(&id) {
            continue;
        }
        let command = tmux::pane_command(id).ok_or_else(|| format!("Could not read {id}"))?;
        if !agents.tracks(&command) {
            return Err(format!("Agent in {id} is no longer running; nothing moved"));
        }
        list.push(id);
    }
    Ok(list)
}

/// The commands that put `panes` after `target`, in order, each taking its share of that pane's room. With too little
/// room for that, `tile`, the window is tiled as they arrive.
fn joins(split: Split, target: &str, tile: bool, panes: &[&String]) -> Vec<Vec<String>> {
    let mut steps = Vec::new();
    let mut after = target;
    for (i, id) in panes.iter().enumerate() {
        let left = panes.len() - i;
        let size = format!("{}%", left * 100 / (left + 1));
        let mut join = vec!["join-pane", "-d", split.flag()];
        if !tile {
            join.extend(["-l", &size]);
        }
        join.extend(["-s", id, "-t", after]);
        steps.push(join.iter().map(|arg| arg.to_string()).collect());
        if tile {
            steps.push(["select-layout", "-t", target, "tiled"].map(str::to_string).to_vec());
        }
        after = id;
    }
    steps
}

/// Puts `panes` after `target`: a pane, or a window's active one. Then they and the panes in line with them share the room
/// evenly.
fn join(target: &str, panes: &[&String]) -> Result<(), String> {
    let split = config::get().board.split;
    let size = tmux::display(target, &format!("{} #{{window_panes}}", split.room("window"))).ok_or("Could not read the window's size")?;
    let mut numbers = size.split(' ').filter_map(|n| n.parse::<usize>().ok());
    let (Some(room), Some(already)) = (numbers.next(), numbers.next()) else {
        return Err("Could not read the window's size".into());
    };
    let tile = layout(split, room, already + panes.len()) == "tiled";
    tmux::chain(&joins(split, target, tile, panes)).map_err(|error| format!("Could not move them all: {error}"))?;
    if let Some(last) = panes.last()
        && !tile
    {
        even_out(last);
    }
    Ok(())
}

/// Shares the room evenly between `pane` and the panes in line with it, leaving the rest of the window as it is.
fn even_out(pane: &str) {
    tmux::ok(&["select-layout", "-E", "-t", pane]);
}

/// A pane of `panes` ("id left top width height", as list-panes gives them) right beside `pane`, along a whole side: one
/// in line with it, which its room goes to as it closes.
fn beside(panes: &str, pane: &str) -> Option<String> {
    let all: Vec<(&str, [usize; 4])> = panes
        .lines()
        .filter_map(|line| {
            let mut words = line.split(' ');
            let id = words.next()?;
            let mut numbers = words.filter_map(|n| n.parse().ok());
            Some((id, [numbers.next()?, numbers.next()?, numbers.next()?, numbers.next()?]))
        })
        .collect();
    let [left, top, width, height] = all.iter().find(|(id, _)| *id == pane)?.1;
    all.iter()
        .find(|(id, [l, t, w, h])| {
            *id != pane
                && ((*t == top && *h == height && (l + w + 1 == left || left + width + 1 == *l))
                    || (*l == left && *w == width && (t + h + 1 == top || top + height + 1 == *t)))
        })
        .map(|(id, _)| id.to_string())
}

/// Closes a pane, and the panes in line with it share its room evenly.
pub fn close(pane: &str) -> bool {
    let panes = tmux::query(&[
        "list-panes",
        "-t",
        pane,
        "-F",
        "#{pane_id} #{pane_left} #{pane_top} #{pane_width} #{pane_height}",
    ]);
    let next = panes.and_then(|panes| beside(&panes, pane));
    if !tmux::ok(&["kill-pane", "-t", pane]) {
        return false;
    }
    if let Some(next) = next {
        even_out(&next);
    }
    true
}

/// Moves agent panes together into a new window. Checks every one first; only runs after the board closes, so moving
/// its host cannot interrupt it.
pub fn group(agents: &Registry, session: &str, panes: &[String]) -> Result<(), String> {
    let list = checked(agents, panes)?;
    let Some((first, rest)) = list.split_first() else { return Ok(()) };
    let window = next_window_name(session).ok_or("Could not read the session's windows")?;
    tmux::run(&["break-pane", "-d", "-s", first, "-t", &format!("{session}:"), "-n", &tmux::name(&window)])
        .map_err(|_| format!("Could not move {first}"))?;
    join(first, rest)?;
    focus(&list);
    Ok(())
}

/// The heading marked agents were sent to.
#[derive(Clone, PartialEq, Debug)]
pub enum Place {
    Window(String),
    Session(String),
    /// Marked agents staged with Shift+Tab: a new window of their own, together, or a new tmux session.
    NewWindow,
    NewSession,
}

/// Adds agents to an existing window next to its active pane, each after the one before, all in line sharing it evenly.
pub fn move_to_window(agents: &Registry, window: &str, panes: &[String]) -> Result<(), String> {
    let list = checked(agents, panes)?;
    let coming: Vec<&String> = list
        .iter()
        .copied()
        .filter(|id| tmux::display(id, "#{window_id}").as_deref() != Some(window))
        .collect();
    if !coming.is_empty() {
        join(window, &coming)?;
    }
    focus(&list);
    Ok(())
}

/// Agents into `session`. `together`, for a selection: one window for all of them, unless they are exactly one whole
/// window, which moves as it is, with its name and layout. Otherwise each agent gets a window of its own.
pub fn move_to_session(agents: &Registry, session: &str, panes: &[String], together: bool) -> Result<(), String> {
    let list = checked(agents, panes)?;
    let target = format!("{session}:");
    if together {
        let Some((first, rest)) = list.split_first() else { return Ok(()) };
        let windows: Vec<String> = list.iter().filter_map(|id| tmux::display(id, "#{window_id}")).collect();
        // Their panes can close between the check and here: an agent that exits as you press Enter.
        let Some(window) = windows.first() else {
            return Err("The agents are no longer there".into());
        };
        let whole =
            windows.iter().all(|w| w == window) && tmux::display(window, "#{window_panes}").as_deref() == Some(list.len().to_string().as_str());
        if whole {
            if tmux::display(window, "#{session_id}").as_deref() != Some(session) && !tmux::ok(&["move-window", "-s", window, "-t", &target]) {
                return Err(format!("Could not move window {window}"));
            }
        } else {
            let name = next_window_name(session).ok_or("Could not read the session's windows")?;
            tmux::run(&["break-pane", "-d", "-s", first, "-t", &target, "-n", &tmux::name(&name)]).map_err(|_| format!("Could not move {first}"))?;
            join(first, rest)?;
        }
        focus(&list);
        return Ok(());
    }
    for id in &list {
        if tmux::display(id, "#{window_panes} #{session_id}").as_deref() == Some(format!("1 {session}").as_str()) {
            continue; // already a window of its own there
        }
        let name = next_window_name(session).ok_or("Could not read the session's windows")?;
        if !tmux::ok(&["break-pane", "-d", "-s", id, "-t", &target, "-n", &tmux::name(&name)]) {
            return Err(format!("Could not move {id}"));
        }
    }
    focus(&list);
    Ok(())
}

/// Each agent in a window of its own, in its session.
pub fn own_windows(agents: &Registry, panes: &[String]) -> Result<(), String> {
    let mut by_session: Vec<(String, Vec<String>)> = Vec::new();
    for pane in panes {
        let session = tmux::display(pane, "#{session_id}").ok_or_else(|| format!("Could not read {pane}"))?;
        match by_session.iter_mut().find(|(known, _)| *known == session) {
            Some((_, list)) => list.push(pane.clone()),
            None => by_session.push((session, vec![pane.clone()])),
        }
    }
    by_session
        .iter()
        .try_for_each(|(session, list)| move_to_session(agents, session, list, false))
}

/// A new tmux session named after the first folder, holding whole windows, as they are, and these agents.
pub fn new_session(agents: &Registry, panes: &[String], windows: &[String], together: bool) -> Result<(), String> {
    let list = checked(agents, panes)?;
    let Some(first) = windows.first().or(list.first().copied()) else {
        return Ok(());
    };
    let folder = tmux::display(first, "#{pane_current_path}").unwrap_or_else(tmux::home);
    // Its first window only holds the session until the rest are in.
    let created = tmux::out(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{session_id} #{window_id}",
        "-s",
        &tmux::name(&new_session_name(&folder)),
        "-c",
        &tmux::name(&folder),
        "sleep 86400",
    ])
    .ok_or("Could not open a tmux session")?;
    let (session, placeholder) = created.split_once(' ').ok_or("Could not open a tmux session")?;
    let target = format!("{session}:");
    let moves: Vec<Vec<String>> = windows
        .iter()
        .map(|window| ["move-window", "-s", window, "-t", &target].map(str::to_string).to_vec())
        .collect();
    let moved = tmux::chain(&moves)
        .map_err(|error| format!("Could not move the windows: {error}"))
        .and_then(|()| move_to_session(agents, session, panes, together));
    tmux::ok(&["kill-window", "-t", placeholder]); // whether or not the move worked: never leave the placeholder behind
    if moved.is_ok() {
        tmux::go(first);
    }
    moved
}

fn focus(list: &[&String]) {
    if let Some(first) = list.first() {
        tmux::go(first);
    }
}

/// Where new agents go: a new pane in a window, a new window in a session, or a new tmux session.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Where {
    Pane(String),
    Window(String),
    Session,
}

/// Starts new agents, (agent, folder), together where they go: the first opens the window or session, the rest share it
/// with it. Ends on the first.
pub fn start_together(agents: &Registry, place: &Where, list: &[(String, String)]) -> Result<(), String> {
    let (mut first, mut last): (Option<String>, Option<String>) = (None, None);
    for (agent, folder) in list {
        let pane = match (&last, place) {
            (Some(last), _) => start(agents, agent, false, last, folder)?,
            (None, Where::Pane(window)) => start(agents, agent, false, window, folder)?,
            (None, Where::Window(session)) => start(agents, agent, true, session, folder)?,
            (None, Where::Session) => {
                remember_agent(agent);
                launch(folder, &agents.start(agent))?
            }
        };
        first.get_or_insert_with(|| pane.clone());
        last = Some(pane);
    }
    if let Some(first) = first {
        tmux::go(&first);
    }
    Ok(())
}

/// Starts `agent` in a new window of a session or a new pane in a window, typing its command like you would. Gives its
/// pane.
pub fn start(agents: &Registry, agent: &str, in_window: bool, target: &str, folder: &str) -> Result<String, String> {
    let folder = &existing_folder(folder);
    let failed = || "Could not open a window".to_string();
    let pane = if in_window {
        let name = next_window_name(target).ok_or_else(failed)?;
        tmux::open(&[
            "new-window",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &format!("{target}:"),
            "-n",
            &tmux::name(&name),
            "-c",
            &tmux::name(folder),
        ])
    } else {
        tmux::open(&[
            "split-window",
            config::get().board.split.flag(),
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            target,
            "-c",
            &tmux::name(folder),
        ])
    };
    let pane = pane.ok_or_else(failed)?;
    if !in_window {
        even_out(&pane);
    }
    remember_agent(agent);
    go_run(&pane, &agents.start(agent));
    Ok(pane)
}

/// The next "n" starts with the agent last started this way.
pub fn remember_agent(agent: &str) {
    let _ = tmux::write_atomic(&tmux::history_dir().join("last-agent"), agent);
}

pub fn last_agent(agents: &Registry) -> Option<String> {
    let last = std::fs::read_to_string(tmux::history_dir().join("last-agent")).unwrap_or_default();
    let last = last.trim();
    let installed = agents.installed();
    installed.iter().find(|a| *a == last).or(installed.first()).cloned()
}

fn zoxide(args: &[&str]) -> Vec<String> {
    let output = Command::new("zoxide").args(args).output();
    output.map_or_else(
        |_| Vec::new(),
        |o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect(),
    )
}

/// Folder suggestions for a word: where you are, folders agents used, zoxide's, home.
pub fn suggestions(first: &str, history: &[Conversation]) -> Vec<String> {
    let mut all = vec![first.to_string()];
    all.extend(history.iter().map(|c| c.folder.clone()));
    all.extend(zoxide(&["query", "-l"]));
    all.push(tmux::home());
    dedup(all)
}

fn dedup(list: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    list.into_iter().filter(|item| seen.insert(item.clone())).collect()
}

fn split_path(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((base, prefix)) if !base.is_empty() => (base.to_string(), prefix.to_string()),
        Some((_, prefix)) => ("/".to_string(), prefix.to_string()),
        None => (".".to_string(), path.to_string()),
    }
}

fn child_folders(base: &str, prefix: &str, hidden: bool, case_sensitive: bool) -> Vec<String> {
    let start = if case_sensitive { prefix.to_string() } else { prefix.to_lowercase() };
    let mut found: Vec<String> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let compared = if case_sensitive { name.clone() } else { name.to_lowercase() };
            // metadata, not file_type: links to folders count as folders (macOS's /tmp, /var and /etc are links).
            compared.starts_with(&start) && (hidden || !name.starts_with('.')) && entry.path().is_dir()
        })
        .map(|entry| entry.path().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

/// The folders for what's typed. A path (/, ~ or .) lists the folders inside it that start with the last part, like shell
/// completion, anywhere on disk. Anything else searches the suggestions and everything zoxide knows. Shown with ~ for home.
/// A typed folder as a full path: ~ is home, and . or .. start from `base` (the agent's or the draft's folder), not from
/// wherever the board runs.
pub fn absolute(typed: &str, base: &str) -> String {
    let path = tmux::untilde(typed);
    if !path.starts_with('.') {
        return path;
    }
    // Tidied by its words alone (. dropped, .. a step up), not resolved: a link you went through stays as you see it.
    let mut tidy = std::path::PathBuf::new();
    for part in Path::new(base).join(&path).components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                tidy.pop();
            }
            other => tidy.push(other),
        }
    }
    tidy.to_string_lossy().into_owned()
}

pub fn dirs(typed: &str, suggested: &[String], base: &str) -> Vec<String> {
    let found = if typed.is_empty() {
        suggested.to_vec()
    } else if typed.starts_with(['/', '~', '.']) {
        // A trailing / says "inside": kept through tidying, which would drop it.
        let mut path = absolute(typed, base);
        if typed.ends_with('/') && !path.ends_with('/') {
            path.push('/');
        }
        let (base, prefix) = if path.ends_with('/') {
            (path.clone(), String::new())
        } else {
            split_path(&path)
        };
        let mut list = Vec::new();
        if Path::new(&path).is_dir() {
            list.push(if path == "/" {
                path.clone()
            } else {
                path.trim_end_matches('/').to_string()
            });
        }
        let mut children = child_folders(&base, &prefix, prefix.starts_with('.'), false);
        children.truncate(300);
        list.extend(children.into_iter().map(|c| c.replace("//", "/")));
        list
    } else {
        // The suggestions hold every folder zoxide knows already: matched here, as it would, not asked on each key.
        suggested.iter().filter(|folder| keywords_match(folder, typed)).cloned().collect()
    };
    dedup(found).iter().map(|path| tmux::tilde(path)).collect()
}

/// zoxide's match: each word, ignoring case, in the path in order, the last within the folder's own name.
fn keywords_match(folder: &str, typed: &str) -> bool {
    let path = folder.to_lowercase();
    let words: Vec<String> = typed.split_whitespace().map(str::to_lowercase).collect();
    let Some(last) = words.last() else { return true };
    let name = path.rsplit('/').next().unwrap_or_default();
    if !name.contains(last.as_str()) {
        return false;
    }
    let mut rest = path.as_str();
    for word in &words {
        match rest.find(word.as_str()) {
            Some(at) => rest = &rest[at + word.len()..],
            None => return false,
        }
    }
    true
}

/// Tab while typing a folder: a path completes to the longest prefix its matching folders share (with a "/" once only one
/// matches); a word becomes the best folder zoxide or past agents know for it.
pub fn complete_folder(typed: &str, history: &[Conversation]) -> Option<String> {
    let done = if typed.starts_with(['~', '/', '.']) {
        let path = tmux::untilde(typed);
        let (base, prefix) = if path.ends_with('/') {
            (path.clone(), String::new())
        } else {
            split_path(&path)
        };
        let matches = child_folders(&base, &prefix, true, true);
        match &matches[..] {
            [] => return None,
            [one] => format!("{}/", one.replace("//", "/")),
            [first, rest @ ..] => {
                let mut common = first.replace("//", "/");
                for other in rest {
                    let other = other.replace("//", "/");
                    while !other.starts_with(&common) {
                        common.pop();
                    }
                }
                common
            }
        }
    } else {
        let lower = typed.to_lowercase();
        let known = zoxide(&["query", "--", typed]).into_iter().next();
        known.or_else(|| history.iter().map(|c| c.folder.clone()).find(|f| f.to_lowercase().contains(&lower)))?
    };
    Some(tmux::tilde(&done))
}

/// Left in the folder chooser: the typed path's parent folder (a search becomes the home folder).
pub fn updir(typed: &str) -> String {
    let path = if typed == "/" {
        "/".to_string()
    } else {
        tmux::untilde(typed.trim_end_matches('/'))
    };
    let parent = if path.starts_with(['/', '.']) {
        Path::new(&path)
            .parent()
            .map_or_else(|| "/".to_string(), |p| p.to_string_lossy().into_owned())
    } else {
        tmux::home()
    };
    let parent = if parent.is_empty() { ".".to_string() } else { parent };
    let parent = if parent == "/" { parent } else { format!("{parent}/") };
    tmux::tilde(&parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joined_panes_share_the_target_evenly_unless_tiled() {
        let (a, b, c) = ("%1".to_string(), "%2".to_string(), "%3".to_string());
        let even = joins(Split::Horizontal, "@1", false, &[&a, &b, &c]);
        let sizes: Vec<&str> = even.iter().map(|step| step[4].as_str()).collect();
        assert_eq!(sizes, ["75%", "66%", "50%"]);
        assert_eq!(even[1][8], "%1", "each after the one before");
        let tiled = joins(Split::Horizontal, "@1", true, &[&a, &b, &c]);
        assert_eq!(tiled.len(), 6);
        assert!(tiled.iter().all(|step| !step.contains(&"-l".to_string())));
        assert_eq!(tiled[1], ["select-layout", "-t", "@1", "tiled"]);
        assert!(joins(Split::Horizontal, "@1", false, &[]).is_empty());
        assert_eq!(joins(Split::Vertical, "@1", false, &[&a])[0][2], "-v");
    }

    #[test]
    fn a_closing_pane_gives_its_room_to_one_in_line() {
        // %1 | %2 | %3 above %4, the width of all three.
        let panes = "%1 0 0 39 20\n%2 40 0 39 20\n%3 80 0 40 20\n%4 0 21 120 20\n";
        assert_eq!(beside(panes, "%2").as_deref(), Some("%1"));
        assert_eq!(beside(panes, "%1").as_deref(), Some("%2"));
        assert_eq!(beside(panes, "%4"), None, "touches all three, matches none");
        assert_eq!(beside("%1 0 0 60 20\n%2 0 21 60 20\n", "%2").as_deref(), Some("%1"), "stacked");
        assert_eq!(beside(panes, "%9"), None);
    }

    #[test]
    fn agents_sit_side_by_side_until_that_leaves_them_too_narrow() {
        assert_eq!(layout(Split::Horizontal, 200, 10), "even-horizontal");
        assert_eq!(layout(Split::Horizontal, 200, 11), "tiled");
        assert_eq!(layout(Split::Horizontal, 80, 4), "even-horizontal");
        assert_eq!(layout(Split::Horizontal, 80, 5), "tiled");
        assert_eq!(layout(Split::Vertical, 50, 6), "even-vertical");
        assert_eq!(layout(Split::Vertical, 50, 7), "tiled");
    }

    #[test]
    fn a_session_name_is_never_empty() {
        assert_eq!(new_session_name("/"), "agents");
        assert_eq!(new_session_name("/w/my.api/"), "my-api");
    }

    #[test]
    fn windows_use_the_lowest_free_number() {
        assert_eq!(lowest_window_name(["zsh", "Agentic 1", "Agentic 3"].into_iter()), "Agentic 2");
        assert_eq!(lowest_window_name(["Agentic x"].into_iter()), "Agentic 1");
        let many: Vec<String> = (1..=10).map(|n| format!("Agentic {n}")).collect();
        assert_eq!(lowest_window_name(many.iter().map(String::as_str)), "Agentic 11");
    }

    #[test]
    fn paths_list_subfolders_and_complete() {
        let root = std::env::temp_dir().join(format!("agentic-dirs-{}", std::process::id()));
        for folder in ["alpha/one", "alpine", ".hidden", "beta"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        std::fs::write(root.join("file"), "").unwrap();
        let base = root.to_string_lossy().into_owned();
        let names = |list: Vec<String>| -> Vec<String> { list.iter().map(|p| p.rsplit('/').next().unwrap().to_string()).collect() };
        let listed = dirs(&format!("{base}/"), &[], "/w");
        assert_eq!(names(listed[1..].to_vec()), ["alpha", "alpine", "beta"]);
        assert_eq!(names(dirs(&format!("{base}/AL"), &[], "/w")), ["alpha", "alpine"]);
        assert_eq!(names(dirs(&format!("{base}/.h"), &[], "/w")), [".hidden"]);
        // . starts from the agent's folder, wherever the board runs.
        assert_eq!(absolute("./alpha", &base), format!("{base}/alpha"));
        assert_eq!(names(dirs("./al", &[], &base)), ["alpha", "alpine"]);
        let known = ["/w/tmux-agentic".to_string(), "/w/agentic/docs".to_string(), "/home/Agents".to_string()];
        assert_eq!(
            dirs("agentic", &known, &base),
            ["/w/tmux-agentic"],
            "the last word in the folder's own name"
        );
        assert_eq!(dirs("w doc", &known, &base), ["/w/agentic/docs"], "the words in order");
        assert!(dirs("doc w", &known, &base).is_empty());
        assert_eq!(dirs("AGENTS", &known, &base), ["/home/Agents"], "case ignored");
        assert_eq!(complete_folder(&format!("{base}/al"), &[]), Some(format!("{base}/alp")));
        assert_eq!(complete_folder(&format!("{base}/be"), &[]), Some(format!("{base}/beta/")));
        assert_eq!(complete_folder(&format!("{base}/zz"), &[]), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn updir_goes_up_and_stops_at_the_top() {
        assert_eq!(updir("/a/b/"), "/a/");
        assert_eq!(updir("/a"), "/");
        assert_eq!(updir("/"), "/");
    }
}
