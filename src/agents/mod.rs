//! What each coding agent does differently. A tool only has to implement what it supports: an empty implementation
//! still gets listing, state, previews, moving panes and `/rename`.
mod claude;
mod codex;
mod opencode;

use crate::history::{self, Conversation};
use crate::text;
use crate::tmux;
use ratatui::style::Color;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

pub trait Agent: Send + Sync {
    fn command(&self) -> &str;
    /// How its name is written.
    fn name(&self) -> String {
        text::capitalize(self.command())
    }
    /// The conversation name in its pane title: without a leading spinner glyph (Claude: "✳ Topic") and a trailing
    /// " | folder" (Codex: "Topic | ~").
    fn title(&self, pane_title: &str) -> String {
        default_title(pane_title)
    }
    /// Past conversations.
    fn history(&self) -> Vec<Conversation> {
        Vec::new()
    }
    /// The command that continues conversation `id`.
    fn resume(&self, _id: &str) -> Option<String> {
        None
    }
    /// The command that starts a new one.
    fn start(&self) -> String {
        self.command().to_string()
    }
    /// How it starts with a first message.
    fn prompt(&self, message: &str) -> String {
        format!("{} {}", self.command(), tmux::shell_quote(message))
    }
    fn hands_off(&self) -> bool {
        false
    }
    /// The saved transcript of conversation `id`, for handoffs.
    fn transcript(&self, _id: &str) -> Option<PathBuf> {
        None
    }
    /// Whether conversation `id` is one it has. By its transcript, unless finding that costs more than asking.
    fn has_conversation(&self, id: &str) -> bool {
        self.transcript(id).is_some_and(|path| path.is_file())
    }
    fn exports(&self) -> bool {
        false
    }
    /// Asks a live pane which conversation it holds: (id, transcript path). A file written in `dir` is kept.
    fn export(&self, _pane: &str, _dir: &Path) -> Result<(String, PathBuf), String> {
        Err("Cannot export".into())
    }
    /// The command that moves a running session to another folder, keeping its conversation.
    fn cd(&self, _folder: &str) -> Option<String> {
        None
    }
    /// The folder it says it's in, when its screen shows one: its own /cd doesn't change what tmux reports.
    fn folder(&self, _screen: &str) -> Option<String> {
        None
    }
    /// Whether its /cd moves the process itself, so tmux reports the folder and whatever we sent is only a request.
    fn moves_itself(&self) -> bool {
        false
    }
    /// Finishes an empty `/rename`, e.g. accepting a suggestion dialog. Prints the outcome.
    fn finish_rename(&self, _pane: &str) -> Option<Result<String, String>> {
        None
    }
    fn retitles(&self) -> bool {
        false
    }
    /// Renames without typing into the agent. Prints the outcome.
    fn retitle(&self, _pane: &str, _name: &str, _agents: &Registry) -> Result<String, String> {
        Err("Not renamed: this agent can't be renamed that way".into())
    }
    /// How full its context window is, e.g. `11%`.
    fn context(&self, _pane: &str, _title: &str, _agents: &Registry) -> Option<String> {
        None
    }
    /// For agents whose busy phrase doesn't always show.
    fn busy(&self, _screen: &str) -> bool {
        false
    }
    /// Whether it keeps a record of its turns `turn` reads.
    fn logs_turns(&self) -> bool {
        false
    }
    /// Whether that record shows all its work, so a record saying it's idle outweighs a screen showing it busy.
    fn records_all_work(&self) -> bool {
        true
    }
    /// Where its conversation is, by the agent's own record rather than its screen; None when it keeps none, or this
    /// pane's can't be found. `title` is the conversation's (`title`).
    fn turn(&self, _pane: &str, _title: &str, _agents: &Registry) -> Option<Turn> {
        None
    }
    /// Your first message (if earlier than the last), your last, and the answer to it.
    fn messages(&self, _id: &str) -> Vec<(Who, String)> {
        Vec::new()
    }
    fn removes(&self) -> bool {
        false
    }
    /// Removes a conversation recoverably. Prints the outcome.
    fn remove(&self, _id: &str) -> Result<String, String> {
        Err("Not removed: this agent can't remove conversations".into())
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Who {
    First,
    You,
    Agent,
}

/// What History's preview shows from a conversation: your first message when it isn't also your last.
fn exchange(first: String, you: String, answer: String) -> Vec<(Who, String)> {
    let mut messages = Vec::new();
    if first != you {
        messages.push((Who::First, first));
    }
    messages.push((Who::You, you));
    messages.push((Who::Agent, answer));
    messages
}

/// An agent with no module of its own, tracked through `AGENTS`.
struct Plain(String);

impl Agent for Plain {
    fn command(&self) -> &str {
        &self.0
    }
}

fn builtin() -> Vec<Box<dyn Agent>> {
    vec![Box::new(claude::Claude), Box::new(codex::Codex), Box::new(opencode::OpenCode)]
}

pub fn on_path(command: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .any(|dir| std::fs::metadata(Path::new(dir).join(command)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// The agents being tracked, alphabetically: every built-in one whose command is installed, or the ones config.toml names
/// (`AGENTS`, when set, names them instead).
pub struct Registry {
    agents: Vec<Box<dyn Agent>>,
    installed: Vec<String>,
}

impl Registry {
    pub fn load() -> Self {
        // AGENTS, else config.toml's list (enabled = [] meaning none), else every one installed.
        let chosen = std::env::var("AGENTS")
            .ok()
            .filter(|names| !names.trim().is_empty())
            .map(|names| names.split_whitespace().map(str::to_string).collect())
            .or_else(|| crate::config::get().agents.clone());
        let mut names: Vec<&str> = chosen.iter().flatten().map(String::as_str).collect();
        let mut agents: Vec<Box<dyn Agent>> = if chosen.is_none() {
            builtin().into_iter().filter(|a| on_path(a.command())).collect()
        } else {
            names.sort();
            names.dedup();
            let mut known = builtin();
            names
                .iter()
                .map(|name| match known.iter().position(|a| a.command() == *name) {
                    Some(i) => known.remove(i),
                    None => Box::new(Plain(name.to_string())) as Box<dyn Agent>,
                })
                .collect()
        };
        agents.sort_by(|a, b| a.command().cmp(b.command()));
        let installed = agents.iter().map(|a| a.command().to_string()).filter(|a| on_path(a)).collect();
        Registry { agents, installed }
    }

    /// Every built-in agent, each as if installed: unit tests see the same board on any machine, CI's included.
    #[cfg(test)]
    pub fn every_builtin() -> Self {
        let mut agents = builtin();
        agents.sort_by(|a, b| a.command().cmp(b.command()));
        let installed = agents.iter().map(|a| a.command().to_string()).collect();
        Registry { agents, installed }
    }

    pub fn all(&self) -> impl Iterator<Item = &dyn Agent> {
        self.agents.iter().map(|a| a.as_ref())
    }

    pub fn get(&self, command: &str) -> Option<&dyn Agent> {
        self.all().find(|a| a.command() == command)
    }

    pub fn tracks(&self, command: &str) -> bool {
        self.get(command).is_some()
    }

    /// The tracked agents that are installed: what "n" offers.
    pub fn installed(&self) -> &[String] {
        &self.installed
    }

    /// The installed agent after `current`, wrapping around; None with only one to choose from.
    pub fn next_installed(&self, current: &str) -> Option<String> {
        if self.installed.len() < 2 {
            return None;
        }
        let at = self.installed.iter().position(|a| a == current);
        let next = at.map_or(0, |i| (i + 1) % self.installed.len());
        Some(self.installed[next].clone())
    }

    pub fn name(&self, command: &str) -> String {
        self.get(command).map_or_else(|| text::capitalize(command), |a| a.name())
    }

    /// The command that starts a new conversation.
    pub fn start(&self, command: &str) -> String {
        self.get(command).map_or_else(|| command.to_string(), |a| a.start())
    }

    /// Its color in the Kind column, so you can tell the CLIs apart at a glance: the theme's.
    pub fn tint(&self, command: &str) -> Color {
        crate::text::palette().agent(command).unwrap_or(Color::Reset)
    }

    /// Whether the agent can hand a conversation to another installed one.
    pub fn switchable(&self, tool: &str) -> bool {
        self.get(tool).is_some_and(|a| a.hands_off()) && self.installed.iter().any(|a| a == tool) && self.installed.len() > 1
    }
}

pub fn default_title(title: &str) -> String {
    // A leading run of non-alphanumerics up to its last space, as in "✳ Topic".
    let run = title.find(char::is_alphanumeric).unwrap_or(title.len());
    let start = title[..run].rfind(' ').map_or(0, |space| space + 1);
    let name = &title[start..];
    match name.rfind(" | ") {
        Some(at) if !name[at + 3..].contains('|') => name[..at].to_string(),
        _ => name.to_string(),
    }
}

/// What's known of the conversation open in a pane.
#[derive(Clone)]
struct PaneConversation {
    title: String,
    at: Instant,
    id: Option<String>,
    /// Its saved transcript, once something asks for it.
    path: Option<Option<PathBuf>>,
}

static PANES: LazyLock<Mutex<HashMap<String, PaneConversation>>> = LazyLock::new(Default::default);

/// The conversation open in agent pane `pane`, cached for a minute: finding it reads ps and the conversation list, too
/// slow to repeat every second for every pane. A changed title invalidates it immediately.
fn pane_entry(tool: &dyn Agent, pane: &str, title: &str, agents: &Registry) -> PaneConversation {
    if let Some(cached) = PANES.lock().unwrap().get(pane)
        && cached.title == title
        && cached.at.elapsed() < Duration::from_secs(60)
    {
        return cached.clone();
    }
    // Panes come and go over a long status watch: one not looked at for ten minutes is forgotten.
    PANES.lock().unwrap().retain(|_, cached| cached.at.elapsed() < Duration::from_secs(600));
    let entry = PaneConversation {
        title: title.to_string(),
        at: Instant::now(),
        id: history::live_conversation(agents, tool.command(), pane, false),
        path: None,
    };
    PANES.lock().unwrap().insert(pane.to_string(), entry.clone());
    entry
}

pub fn pane_conversation(tool: &dyn Agent, pane: &str, title: &str, agents: &Registry) -> Option<String> {
    pane_entry(tool, pane, title, agents).id
}

/// Its saved transcript, cached the same way.
pub fn pane_transcript(tool: &dyn Agent, pane: &str, title: &str, agents: &Registry) -> Option<PathBuf> {
    let entry = pane_entry(tool, pane, title, agents);
    if let Some(path) = entry.path {
        return path;
    }
    let path = entry.id.as_deref().and_then(|id| tool.transcript(id));
    if let Some(cached) = PANES.lock().unwrap().get_mut(pane) {
        cached.path = Some(path.clone());
    }
    path
}

/// Moves `items` to the Trash, where the system's file manager can put them back: ~/.Trash on macOS, for Finder; the
/// freedesktop trash elsewhere, $XDG_DATA_HOME/Trash, with the record of where each came from that restoring needs. Says
/// who can put them back.
pub fn trash(items: &[PathBuf]) -> Result<&'static str, String> {
    let items: Vec<&PathBuf> = items.iter().filter(|item| item.exists()).collect();
    if cfg!(target_os = "macos") {
        // Into ~/.Trash; without Full Disk Access macOS refuses that, and its own trash command does it instead (always
        // the real user's Trash, whatever $HOME says, so it comes second).
        to_mac_trash(&PathBuf::from(tmux::home()).join(".Trash"), &items)
            .map(|()| "Finder")
            .or_else(|why| {
                let left: Vec<&&PathBuf> = items.iter().filter(|item| item.exists()).collect();
                let done =
                    Path::new("/usr/bin/trash").is_file() && Command::new("/usr/bin/trash").args(left).output().is_ok_and(|o| o.status.success());
                if done { Ok("Finder") } else { Err(why) }
            })
    } else {
        let data = tmux::xdg("XDG_DATA_HOME", ".local/share");
        let now = Command::new("date").arg("+%Y-%m-%dT%H:%M:%S").output().ok();
        let date = now.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        to_freedesktop_trash(&data.join("Trash"), &items, &date).map(|()| "your file manager")
    }
}

fn to_mac_trash(trash: &Path, items: &[&PathBuf]) -> Result<(), String> {
    std::fs::create_dir_all(trash).map_err(|_| "no Trash folder".to_string())?;
    for item in items {
        let name = item.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let mut place = trash.join(&name);
        if place.exists() {
            place = trash.join(format!("{name} {}", tmux::now()));
        }
        std::fs::rename(item, &place).map_err(|_| "could not move it to the Trash".to_string())?;
    }
    Ok(())
}

/// The freedesktop trash: each item in files/, and in info/ a NAME.trashinfo saying where it was and when. The record is
/// written first, and only if new, which keeps the name for it.
fn to_freedesktop_trash(trash: &Path, items: &[&PathBuf], date: &str) -> Result<(), String> {
    use std::io::Write;
    let (files, info) = (trash.join("files"), trash.join("info"));
    for dir in [&files, &info] {
        std::fs::create_dir_all(dir).map_err(|_| "no Trash folder".to_string())?;
    }
    for item in items {
        let name = item.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let whole = std::path::absolute(item).map_err(|_| "could not move it to the Trash".to_string())?;
        let failed = || "could not move it to the Trash".to_string();
        // The first name free in both files/ and info/; a record that can't be made for any other reason ends it, or a
        // trash we can't write in would be tried forever.
        let mut n = 1;
        let (place, record) = loop {
            let free = if n == 1 { name.clone() } else { format!("{name}.{n}") };
            n += 1;
            if files.join(&free).exists() {
                continue;
            }
            let record = info.join(format!("{free}.trashinfo"));
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&record) {
                Ok(mut file) => {
                    if write!(file, "[Trash Info]\nPath={}\nDeletionDate={date}\n", url_path(&whole)).is_err() {
                        let _ = std::fs::remove_file(&record);
                        return Err(failed());
                    }
                    break (files.join(free), record);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(failed()),
            }
        };
        if std::fs::rename(item, &place).is_err() {
            let _ = std::fs::remove_file(record);
            return Err("could not move it to the Trash".into());
        }
    }
    Ok(())
}

/// A path as a .trashinfo writes it: URL-encoded, slashes kept.
fn url_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|&b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A log's Context, read again only once it has grown: it's asked for every second for each pane.
pub fn log_context(path: &Path, read: fn(&str) -> Option<String>) -> Option<String> {
    type Read = HashMap<PathBuf, (u64, Option<String>)>;
    static READ: LazyLock<Mutex<Read>> = LazyLock::new(Default::default);
    let size = std::fs::metadata(path).ok()?.len();
    if let Some((seen, context)) = READ.lock().unwrap().get(path)
        && *seen == size
    {
        return context.clone();
    }
    let context = read(&read_ends(path, 0, 300_000));
    let mut known = READ.lock().unwrap();
    if known.len() > 512 {
        known.clear();
    }
    known.insert(path.to_path_buf(), (size, context.clone()));
    context
}

/// Where an agent's own record says its conversation is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Turn {
    Idle,
    Running,
    /// Waiting on you: a permission or a question. Only OpenCode says; Codex's and Claude's logs never record it.
    Asking,
}

impl Turn {
    pub fn open(open: bool) -> Turn {
        if open { Turn::Running } else { Turn::Idle }
    }
}

/// Whether a log's latest turn is still open, by the last line `event` has a word on: true a turn starting or under way,
/// false one over. Read again only once the file has grown.
pub fn last_turn(path: &Path, event: fn(&[u8]) -> Option<bool>) -> Option<bool> {
    /// How far each log has been read (its complete lines), and what it said by then.
    type Seen = HashMap<PathBuf, (u64, Option<bool>)>;
    static SEEN: LazyLock<Mutex<Seen>> = LazyLock::new(Default::default);
    let size = std::fs::metadata(path).ok()?.len();
    let known = SEEN.lock().unwrap().get(path).copied();
    let (upto, open) = match known {
        Some((upto, open)) if upto == size => return open,
        // Grown: only what's new, its newest word on turns taking over (a long turn's output isn't read again).
        Some((upto, open)) if upto < size => {
            let (text, upto) = new_lines(path, upto)?;
            (upto, text.lines().rev().find_map(|line| event(line.as_bytes())).or(open))
        }
        // First look, or written anew: from the end back to its last word.
        _ => (complete_end(path, size), read_last_turn(path, size, event)),
    };
    let mut seen = SEEN.lock().unwrap();
    if seen.len() > 512 {
        seen.clear(); // logs of conversations long over
    }
    seen.insert(path.to_path_buf(), (upto, open));
    open
}

/// The complete lines a log has had added since `from`, and where they end: a line still being written is left for the
/// next read.
pub(crate) fn new_lines(path: &Path, from: u64) -> Option<(String, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |at| at + 1);
    bytes.truncate(end);
    Some((String::from_utf8_lossy(&bytes).into_owned(), from + end as u64))
}

/// Where a log's complete lines end: after its last newline, looked for in its last 64 KB (else its end).
fn complete_end(path: &Path, size: u64) -> u64 {
    let tail = read_ends(path, 0, 65_536);
    let start = size.saturating_sub(tail.len() as u64);
    tail.rfind('\n').map_or(size, |at| start + at as u64 + 1)
}

/// The same, read from the end a chunk at a time: a long turn's tool output can put its start megabytes back.
pub(crate) fn read_last_turn(path: &Path, size: u64, event: fn(&[u8]) -> Option<bool>) -> Option<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let (mut end, mut carry) = (size, Vec::new());
    while end > 0 {
        let start = end.saturating_sub(65_536);
        let mut chunk = vec![0; (end - start) as usize];
        file.seek(SeekFrom::Start(start)).ok()?;
        file.read_exact(&mut chunk).ok()?;
        chunk.extend_from_slice(&carry);
        // The chunk's first line may start in the one before: it's read with that.
        let whole = if start == 0 {
            0
        } else {
            chunk.iter().position(|&b| b == b'\n').map_or(chunk.len(), |i| i + 1)
        };
        if let Some(open) = chunk[whole..].split(|&b| b == b'\n').rev().find_map(event) {
            return Some(open);
        }
        carry = chunk[..whole].to_vec();
        end = start;
    }
    None
}

/// The start and the end of a transcript: files can be megabytes, and the first and last messages are in these.
pub fn read_ends(path: &Path, head: u64, tail: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return String::new() };
    let size = file.metadata().map_or(0, |m| m.len());
    let mut bytes = Vec::new();
    if head > 0 {
        let _ = (&mut file).take(head).read_to_end(&mut bytes);
        bytes.push(b'\n');
    }
    if tail > 0 && file.seek(SeekFrom::Start(size.saturating_sub(tail))).is_ok() {
        let _ = file.read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

pub fn sqlite(database: &Path, sql: &str) -> Option<String> {
    if !on_path("sqlite3") {
        return None;
    }
    let run = |flags: &[&str], database: &std::ffi::OsStr| {
        let output = Command::new("sqlite3")
            .args(flags)
            .args(["-separator", "\t"])
            .arg(database)
            .arg(sql)
            .output()
            .ok()?;
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    // Read-only can't open a WAL database whose -shm file isn't there (it may not make one), as Codex's is once it has
    // closed it: then as an unchanging snapshot, which needs none. Read-only first, so a write still in the WAL shows.
    run(&["-readonly"], database.as_os_str()).or_else(|| {
        let snapshot = format!("file:{}?immutable=1", url_path(database));
        run(&[], std::ffi::OsStr::new(&snapshot))
    })
}

/// Conversations from sqlite3's rows: tool, id, time, folder, title.
pub fn conversations(rows: &str) -> Vec<Conversation> {
    rows.lines().filter_map(Conversation::parse).collect()
}

pub fn sleep(seconds: f64) {
    std::thread::sleep(Duration::from_secs_f64(seconds));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_lose_the_spinner_and_folder() {
        assert_eq!(default_title("✳ Fix the bug"), "Fix the bug");
        assert_eq!(default_title("Fix the bug | ~/work"), "Fix the bug");
        assert_eq!(default_title("⠋ Fix a | b | ~"), "Fix a | b");
        assert_eq!(default_title("Über alles"), "Über alles");
    }

    #[test]
    fn opencode_title_is_its_session() {
        let agent = opencode::OpenCode;
        assert_eq!(agent.title("OC | Refactor"), "Refactor");
        assert_eq!(agent.title("OpenCode"), "");
    }

    #[test]
    fn next_installed_wraps_and_needs_a_choice() {
        let registry = |names: &[&str]| Registry {
            agents: Vec::new(),
            installed: names.iter().map(|n| n.to_string()).collect(),
        };
        let two = registry(&["claude", "codex"]);
        assert_eq!(two.next_installed("claude").as_deref(), Some("codex"));
        assert_eq!(two.next_installed("codex").as_deref(), Some("claude"));
        assert_eq!(two.next_installed("gone").as_deref(), Some("claude"));
        assert_eq!(registry(&["claude"]).next_installed("claude"), None);
    }

    #[test]
    fn a_growing_log_is_read_on_from_where_it_was_line_by_whole_line() {
        let event = |line: &[u8]| match std::str::from_utf8(line).ok()? {
            "start" => Some(true),
            "end" => Some(false),
            _ => None,
        };
        let path = std::env::temp_dir().join(format!("agentic-grow-{}.log", std::process::id()));
        let add = |text: &str| {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        };
        let _ = std::fs::remove_file(&path);
        add("start\nnoise\n");
        assert_eq!(last_turn(&path, event), Some(true));
        add("noise\nen");
        assert_eq!(last_turn(&path, event), Some(true), "a line still being written says nothing yet");
        add("d\n");
        assert_eq!(last_turn(&path, event), Some(false), "once whole, it does");
        add("noise\n");
        assert_eq!(last_turn(&path, event), Some(false), "and stands while nothing newer decides");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_trash_that_cant_be_written_says_so_instead_of_trying_forever() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("agentic-trash-locked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (file, trash) = (root.join("abc.jsonl"), root.join("Trash"));
        std::fs::create_dir_all(trash.join("info")).unwrap();
        std::fs::create_dir_all(trash.join("files")).unwrap();
        std::fs::write(&file, "x").unwrap();
        std::fs::set_permissions(trash.join("info"), std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(to_freedesktop_trash(&trash, &[&file], "2026-11-06T10:00:00").is_err());
        assert!(file.exists(), "left where it was");
        std::fs::set_permissions(trash.join("info"), std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_wal_database_with_no_shm_file_is_still_read() {
        if !on_path("sqlite3") {
            return;
        }
        let dir = std::env::temp_dir().join(format!("agentic-wal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.sqlite");
        let made = Command::new("sqlite3")
            .arg(&db)
            .arg("pragma journal_mode=wal; create table t(x); insert into t values (7); pragma wal_checkpoint(truncate);")
            .output()
            .unwrap();
        assert!(made.status.success());
        // Everything in the main file, its -wal and -shm gone: as Codex leaves its own once it has closed it.
        for side in ["state.sqlite-wal", "state.sqlite-shm"] {
            let _ = std::fs::remove_file(dir.join(side));
        }
        let readonly = Command::new("sqlite3").arg("-readonly").arg(&db).arg("select x from t").output().unwrap();
        eprintln!("read-only alone succeeds here: {}", readonly.status.success());
        assert_eq!(sqlite(&db, "select x from t").as_deref(), Some("7\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_freedesktop_trash_keeps_where_each_came_from_and_never_overwrites() {
        let root = std::env::temp_dir().join(format!("agentic-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (from, trash) = (root.join("my project"), root.join("Trash"));
        std::fs::create_dir_all(from.join("abc")).unwrap();
        let (file, folder) = (from.join("abc.jsonl"), from.join("abc"));
        std::fs::write(&file, "one").unwrap();
        to_freedesktop_trash(&trash, &[&file, &folder], "2026-11-05T10:00:00").unwrap();
        assert!(!file.exists() && trash.join("files/abc.jsonl").is_file() && trash.join("files/abc").is_dir());
        let record = std::fs::read_to_string(trash.join("info/abc.jsonl.trashinfo")).unwrap();
        assert_eq!(
            record,
            format!("[Trash Info]\nPath={}\nDeletionDate=2026-11-05T10:00:00\n", url_path(&file))
        );
        assert!(record.contains("my%20project/abc.jsonl"));
        std::fs::write(&file, "two").unwrap();
        to_freedesktop_trash(&trash, &[&file], "2026-11-05T10:01:00").unwrap();
        assert_eq!(std::fs::read_to_string(trash.join("files/abc.jsonl")).unwrap(), "one", "the first stays");
        assert_eq!(std::fs::read_to_string(trash.join("files/abc.jsonl.2")).unwrap(), "two");
        assert!(trash.join("info/abc.jsonl.2.trashinfo").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
}
