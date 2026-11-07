//! Past conversations: listing, resuming, and handing a conversation to another agent.
use crate::agents::Registry;
use crate::detect;
use crate::sessions;
use crate::tmux;
use regex_lite::Regex;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, PartialEq, Debug)]
pub struct Conversation {
    pub tool: String,
    pub id: String,
    pub time: i64,
    pub folder: String,
    pub title: String,
}

impl Conversation {
    pub fn parse(line: &str) -> Option<Conversation> {
        let mut f = line.splitn(5, '\t');
        Some(Conversation {
            tool: f.next()?.to_string(),
            id: f.next()?.to_string(),
            time: f.next()?.parse().ok()?,
            folder: f.next()?.to_string(),
            title: f.next().unwrap_or_default().to_string(),
        })
    }

    pub fn line(&self) -> String {
        format!("{}\t{}\t{}\t{}\t{}", self.tool, self.id, self.time, self.folder, self.title)
    }
}

/// Which folders History leaves out: config.toml's pattern (the system temp folders, where throwaway sessions live, by
/// default), or `AGENTS_HISTORY_HIDE`'s when set.
fn hidden() -> Option<Regex> {
    let pattern = std::env::var("AGENTS_HISTORY_HIDE").unwrap_or_else(|_| crate::config::get().history.hide.clone());
    if pattern.is_empty() { None } else { Regex::new(&pattern).ok() }
}

/// All tools' past conversations, newest first.
pub fn past(agents: &Registry) -> Vec<Conversation> {
    let hide = hidden();
    let mut all = everything(agents);
    all.retain(|c| !hide.as_ref().is_some_and(|h| h.is_match(&c.folder)));
    all
}

/// The same, hidden folders too: a live pane in one is still found.
fn everything(agents: &Registry) -> Vec<Conversation> {
    let mut all: Vec<Conversation> = agents.all().flat_map(|a| a.history()).collect();
    all.sort_by_key(|c| std::cmp::Reverse(c.time));
    all.truncate(crate::config::get().history.limit);
    all
}

fn cache_path() -> PathBuf {
    tmux::history_dir().join("history.tsv")
}

/// What the last build found, so History opens at once and catches up.
pub fn cached() -> Option<Vec<Conversation>> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    Some(text.lines().filter_map(Conversation::parse).collect())
}

pub fn store(conversations: &[Conversation]) {
    let lines: Vec<String> = conversations.iter().map(Conversation::line).collect();
    let _ = tmux::write_atomic(&cache_path(), &lines.join("\n"));
}

/// `everything` from up to ten seconds ago: every agent's Context lookup needs it, and one read is plenty for them all.
fn recent_past(agents: &Registry) -> Vec<Conversation> {
    type Recent = Option<(Instant, Vec<Conversation>)>;
    static RECENT: LazyLock<Mutex<Recent>> = LazyLock::new(Default::default);
    let mut recent = RECENT.lock().unwrap();
    match &*recent {
        Some((at, list)) if at.elapsed() < Duration::from_secs(10) => list.clone(),
        _ => {
            let list = everything(agents);
            *recent = Some((Instant::now(), list.clone()));
            list
        }
    }
}

/// `etime` as seconds: [[dd-]hh:]mm:ss.
fn elapsed(etime: &str) -> i64 {
    let (days, clock) = etime.split_once('-').map_or((0, etime), |(d, rest)| (d.parse().unwrap_or(0), rest));
    let seconds = clock.split(':').fold(0, |total, part| total * 60 + part.parse::<i64>().unwrap_or(0));
    days * 86_400 + seconds
}

/// Only an id that follows a resume option: a handed-off agent's command line also carries the id of the conversation it
/// was handed, and short options like -s can mean something else to another agent.
fn resume_ids(command_line: &str, tool: &str) -> Vec<String> {
    static RESUME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(--resume|-r|resume|--session|-s)[ =]+([A-Za-z0-9_-]{8,})").unwrap());
    // Each agent's own: Codex's -s is its sandbox, not a session.
    let options: &[&str] = match tool {
        "claude" => &["--resume", "-r"],
        "codex" => &["resume"],
        "opencode" => &["--session", "-s"],
        _ => &["--resume", "-r", "resume", "--session", "-s"],
    };
    RESUME
        .captures_iter(command_line)
        .filter(|c| options.contains(&&c[1]))
        .map(|c| c[2].to_string())
        .collect()
}

/// The conversation among `list` in `folder`, with `title` (unless `any_title`), written to since `since`. Only when
/// there is exactly one.
fn the_one(list: &[Conversation], tool: &str, folders: &[&str], title: &str, since: i64, resumed: &str, any_title: bool) -> Option<String> {
    let mut matching = list.iter().filter(|c| {
        c.tool == tool && folders.contains(&c.folder.as_str()) && (c.title == title || any_title) && (c.time >= since || c.id == resumed)
    });
    let found = matching.next()?;
    matching.next().is_none().then(|| found.id.clone())
}

/// The saved conversation running in an agent pane, found without typing anything into it: the id on the agent's command
/// line (claude --resume <id>, codex resume <id>, opencode --session <id>), else the one conversation in the pane's
/// folder whose title is the pane's title and that was written to since the agent started. Codex can replace a resumed
/// thread with /clear, so its current title must match instead of trusting argv. With `any_title` (agents whose pane
/// title never names the conversation, and that can't be asked), the title isn't needed. None when unsure; prepare_live
/// then asks the agent itself where it can.
pub fn live_conversation(agents: &Registry, tool: &str, pane: &str, any_title: bool) -> Option<String> {
    let agent = agents.get(tool)?;
    let info = tmux::display(pane, "#{pane_tty}\t#{pane_current_path}\t#{pane_title}")?;
    let mut fields = info.splitn(3, '\t');
    let (tty, folder, title) = (fields.next()?, fields.next()?, fields.next().unwrap_or_default());
    let title = agent.title(title);
    // The agent's process on the pane's terminal: its command line, and how long ago it started.
    let output = Command::new("ps")
        .args(["-t", tty.trim_start_matches("/dev/"), "-o", "etime=,comm=,args="])
        .output()
        .ok()?;
    let processes = String::from_utf8_lossy(&output.stdout).into_owned();
    // ps cuts comm to 16 characters, so a long install path loses the tool's name: the command line after it has it whole.
    let is_tool = |word: &str| word.rsplit('/').next() == Some(tool);
    let process = processes.lines().find(|line| line.split_whitespace().skip(1).take(2).any(is_tool))?;
    // The id on the command line is only where it began: /clear, or a /resume inside the agent, moves it on to another
    // conversation. So it stands with the rest, and the pane's title says which one is open now.
    let list = recent_past(agents);
    let (mut resumed, mut resumed_folder) = (String::new(), String::new());
    for id in resume_ids(process, tool) {
        if !agent.has_conversation(&id) {
            continue;
        }
        // Resume can use another folder; /clear can start the next conversation back in the pane's folder.
        resumed_folder = list
            .iter()
            .find(|c| c.tool == tool && c.id == id)
            .map(|c| c.folder.clone())
            .unwrap_or_default();
        resumed = id;
    }
    // A second's slack: ps counts its age and the file system its writes in whole seconds, so a write in the second it
    // started can look older than it.
    let since = tmux::now() - elapsed(process.split_whitespace().next()?) - 1;
    // A folder it was moved to, if any: never an empty one, which any conversation without a folder would match.
    let moved = detect::moved_folder(pane).filter(|moved| !moved.is_empty());
    let mut folders = vec![folder];
    folders.extend(moved.as_deref());
    if !resumed_folder.is_empty() {
        folders.push(&resumed_folder);
    }
    let mut found = the_one(&list, tool, &folders, &title, since, &resumed, any_title);
    // Moved with a /cd we didn't send, tmux may not know the folder; a title of its own still says which one it is.
    if found.is_none() && !any_title && !title.is_empty() {
        let everywhere: Vec<&str> = list.iter().map(|c| c.folder.as_str()).collect();
        found = the_one(&list, tool, &everywhere, &title, since, &resumed, false);
    }
    // No title to go by (not named yet, or named apart from its record): the command line's id, but not for Codex, whose
    // /clear keeps its title until its next turn.
    found.or_else(|| (!resumed.is_empty() && tool != "codex").then_some(resumed))
}

/// The command that continues conversation `id` of `source` in `target`: a resume, or for another agent a new
/// conversation told to read the transcript.
pub fn conversation_command(agents: &Registry, source: &str, id: &str, target: &str, path: Option<&PathBuf>) -> Result<String, String> {
    let (Some(from), Some(to)) = (agents.get(source), agents.get(target)) else {
        let missing = if agents.get(source).is_none() { source } else { target };
        return Err(format!("Agent is not tracked: {missing}"));
    };
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("Invalid conversation id".into());
    }
    if !agents.installed().iter().any(|a| a == target) {
        return Err(format!("Agent is not installed: {target}"));
    }
    if source == target {
        return from.resume(id).ok_or_else(|| format!("Cannot resume {source}"));
    }
    if !to.hands_off() {
        return Err(format!("Cannot transfer to {target}"));
    }
    let path = path.cloned().or_else(|| from.transcript(id)).filter(|p| std::fs::File::open(p).is_ok());
    let Some(path) = path else {
        return Err(format!("Cannot read the saved {source} conversation: {id}"));
    };
    let quoted = tmux::shell_quote(&path.to_string_lossy());
    let prompt = crate::config::get()
        .handoff
        .prompt
        .replace("{agent}", source)
        .replace("{id}", id)
        .replace("{transcript}", &quoted);
    Ok(to.prompt(&prompt))
}

/// One conversation to open once the board closes.
#[derive(Clone, Debug)]
pub struct Opening {
    pub tool: String,
    pub id: String,
    pub folder: String,
    pub target: String,
    pub path: Option<PathBuf>,
}

/// Resumes conversations in one new window, giving its first pane. Every handoff is resolved before opening any panes: a missing transcript
/// must not start an empty agent.
pub fn resume(agents: &Registry, origin: &str, list: &[Opening]) -> Result<Option<String>, String> {
    let mut commands = Vec::new();
    for open in list {
        if open.target != open.tool && !std::path::Path::new(&open.folder).is_dir() {
            return Err(format!("Conversation folder no longer exists: {}", open.folder));
        }
        commands.push((
            open.folder.as_str(),
            conversation_command(agents, &open.tool, &open.id, &open.target, open.path.as_ref())?,
        ));
    }
    let session = tmux::origin_session(origin).ok_or("The board's session is gone")?;
    let window = sessions::next_window_name(&session).ok_or("Could not read the session's windows")?;
    if let Some((folder, _)) = commands.iter().find(|(folder, _)| !std::path::Path::new(folder).is_dir()) {
        return Err(format!("Folder no longer exists: {folder}. Nothing opened"));
    }
    let failed = || "Could not open the conversation".to_string();
    let (mut first, mut last) = (String::new(), String::new());
    for (folder, command) in &commands {
        let pane = if first.is_empty() {
            tmux::out(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                &format!("{session}:"),
                "-n",
                &tmux::name(&window),
                "-c",
                &tmux::name(folder),
            ])
        } else {
            tmux::out(&[
                "split-window",
                "-d",
                crate::config::get().board.split.flag(),
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                &last,
                "-c",
                &tmux::name(folder),
            ])
        };
        let pane = pane.ok_or_else(failed)?;
        if first.is_empty() {
            first = pane.clone();
        }
        if !tmux::run_in(&pane, command) {
            return Err(failed());
        }
        sessions::spread(&first);
        last = pane;
    }
    if first.is_empty() {
        return Ok(None);
    }
    tmux::go(&first);
    Ok(Some(first))
}

/// A live agent handing its conversation over, checked and ready to open once the board closes.
#[derive(Clone, Debug)]
pub struct LiveHandoff {
    pub pane: String,
    pub opening: Opening,
}

/// Finds a live pane's saved transcript: without typing into the agent when possible, else the agent writes it out.
pub fn prepare_live(agents: &Registry, pane: &str, target: &str) -> Result<LiveHandoff, String> {
    let blocked = |reason: String| format!("Cannot transfer: {reason}");
    if let Some(reason) = detect::live_blocker(pane, agents) {
        return Err(blocked(reason));
    }
    let source = tmux::pane_command(pane).ok_or("The agent is no longer available")?;
    let from = agents.get(&source).ok_or("The agent is no longer available")?;
    if !from.hands_off() {
        return Err(format!("Cannot transfer from {source}"));
    }
    if !agents.get(target).is_some_and(|a| a.hands_off()) {
        return Err(format!("Cannot transfer to {target}"));
    }
    if !agents.installed().iter().any(|a| a == target) {
        return Err(format!("Agent is not installed: {target}"));
    }
    let folder = tmux::display(pane, "#{pane_current_path}").ok_or("The agent is no longer available")?;
    if !std::path::Path::new(&folder).is_dir() {
        return Err("Conversation folder no longer exists".into());
    }
    let dir = PathBuf::from(format!("{}/agents-handoff-{}-{}", tmux::temp_dir(), std::process::id(), tmux::now()));
    tmux::private_dir(&dir).map_err(|e| e.to_string())?;
    let found = (|| {
        let saved = |any_title| {
            let id = live_conversation(agents, &source, pane, any_title)?;
            let path = from.transcript(&id).filter(|p| p.is_file())?;
            Some((id, path))
        };
        if let Some(found) = saved(false) {
            return Ok(found);
        }
        if from.exports() {
            return from.export(pane, &dir);
        }
        // Never typed into: the one conversation in its folder written to since it started.
        saved(true).ok_or_else(|| format!("Could not tell which {} conversation this is; open it from History", agents.name(&source)))
    })();
    let checked = found.and_then(|(id, path)| {
        conversation_command(agents, &source, &id, target, Some(&path))?;
        match detect::live_blocker(pane, agents) {
            Some(reason) => Err(blocked(reason)),
            None => Ok((id, path)),
        }
    });
    // An export written here is read by the new agent later.
    if !checked.as_ref().is_ok_and(|(_, path)| path.starts_with(&dir)) {
        let _ = std::fs::remove_dir_all(&dir);
    }
    let (id, path) = checked?;
    Ok(LiveHandoff {
        pane: pane.to_string(),
        opening: Opening {
            tool: source,
            id,
            folder,
            target: target.to_string(),
            path: Some(path),
        },
    })
}

/// Starts the new CLI next to the agent, in its folder or `folder`, then closes the agent: its conversation stays in History.
/// Only once the new one is running, so a failed start leaves the agent as it was. Gives the new pane.
pub fn replace(agents: &Registry, handoff: &LiveHandoff, folder: Option<&str>) -> Result<String, String> {
    let (pane, open) = (&handoff.pane, &handoff.opening);
    if let Some(reason) = detect::live_blocker(pane, agents) {
        return Err(format!("Cannot transfer: {reason}"));
    }
    if tmux::pane_command(pane).as_deref() != Some(open.tool.as_str()) {
        return Err("Cannot transfer: the source agent changed".into());
    }
    let command = conversation_command(agents, &open.tool, &open.id, &open.target, open.path.as_ref())?;
    let cwd = folder.unwrap_or(&open.folder);
    let new = tmux::out(&["split-window", "-d", "-P", "-F", "#{pane_id}", "-t", pane, "-c", &tmux::name(cwd)]).ok_or("Could not open a pane")?;
    tmux::run_in(&new, &command);
    // Ten seconds by the clock, however long each look takes on a busy machine.
    let deadline = Instant::now() + Duration::from_secs(10);
    let started = loop {
        if tmux::pane_command(&new).as_deref() == Some(open.target.as_str()) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if !started {
        tmux::ok(&["kill-pane", "-t", &new]);
        return Err(format!("{} didn't start; original still open", agents.name(&open.target)));
    }
    tmux::run(&["kill-pane", "-t", pane])?;
    Ok(new)
}

/// Live Claude handoffs leave their export for the new agent to read; a day later it has long been read.
/// One of `tmux::write_atomic`'s halves: its file's name with ".new", a process id and a write number ("4242-7") for
/// an extension (older builds: the process id alone).
fn half_written(name: &str) -> bool {
    name.rsplit_once(".new").is_some_and(|(_, tag)| {
        let numbers: Vec<&str> = tag.split('-').collect();
        numbers.len() <= 2 && numbers.iter().all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

pub fn clean_old_handoffs() {
    let older = |entry: &std::fs::DirEntry, than: u64| {
        let modified = entry.metadata().and_then(|m| m.modified()).ok();
        modified.and_then(|t| t.elapsed().ok()).is_some_and(|age| age > Duration::from_secs(than))
    };
    for entry in std::fs::read_dir(tmux::temp_dir()).into_iter().flatten().flatten() {
        if older(&entry, 86_400) && entry.file_name().to_string_lossy().starts_with("agents-handoff-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    // A write cut off between its two steps leaves its half ("history.new4242"); after an hour, no write is still going.
    for entry in std::fs::read_dir(tmux::history_dir()).into_iter().flatten().flatten() {
        if half_written(&entry.file_name().to_string_lossy()) && older(&entry, 3_600) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_halves_of_atomic_writes_are_cleaned_up() {
        assert!(half_written("claude.new13384"));
        assert!(half_written("history.new7"));
        assert!(half_written("claude.new4242-7"));
        assert!(!half_written("claude.new4242-"));
        assert!(!half_written("history.tsv"));
        assert!(!half_written("notes.newsletter"));
        assert!(!half_written("layout.new"));
    }

    fn conversation(id: &str, time: i64, folder: &str, title: &str) -> Conversation {
        Conversation {
            tool: "claude".into(),
            id: id.into(),
            time,
            folder: folder.into(),
            title: title.into(),
        }
    }

    #[test]
    fn etime_formats() {
        assert_eq!(elapsed("01:02"), 62);
        assert_eq!(elapsed("01:00:02"), 3602);
        assert_eq!(elapsed("2-00:00:01"), 172_801);
    }

    #[test]
    fn a_handed_off_agents_prompt_id_is_not_taken_for_its_own() {
        assert_eq!(resume_ids("claude --resume abcd1234-ef", "claude"), ["abcd1234-ef"]);
        assert_eq!(resume_ids("codex resume abcd1234", "codex"), ["abcd1234"]);
        assert_eq!(resume_ids("opencode --session=ses_12345678", "opencode"), ["ses_12345678"]);
        assert!(resume_ids("codex Continue from claude conversation abcd1234", "codex").is_empty());
        assert!(resume_ids("codex -s workspace-write", "codex").is_empty(), "Codex's -s is its sandbox");
    }

    #[test]
    fn an_old_conversation_with_the_same_title_is_not_taken_for_the_live_one() {
        let list = [conversation("old", 50, "/w", "Fix"), conversation("new", 150, "/w", "Fix")];
        assert_eq!(the_one(&list, "claude", &["/w"], "Fix", 100, "", false).as_deref(), Some("new"));
    }

    #[test]
    fn ambiguity_finds_nothing() {
        let list = [conversation("a", 150, "/w", "Fix"), conversation("b", 160, "/w", "Fix")];
        assert_eq!(the_one(&list, "claude", &["/w"], "Fix", 100, "", false), None);
        assert_eq!(the_one(&list, "claude", &["/other"], "Fix", 100, "", false), None);
    }

    #[test]
    fn any_title_needs_one_conversation_in_the_folder() {
        let list = [conversation("a", 150, "/w", "Whatever")];
        assert_eq!(the_one(&list, "claude", &["/w"], "", 100, "", true).as_deref(), Some("a"));
    }

    #[test]
    fn conversation_lines_round_trip_with_an_empty_title() {
        let c = conversation("a", 1, "/w", "");
        assert_eq!(Conversation::parse(&c.line()), Some(c));
    }
}
