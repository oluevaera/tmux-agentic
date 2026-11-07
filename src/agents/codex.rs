//! Codex.
use super::{Agent, Registry, Who, conversations, on_path, pane_transcript, read_ends, sleep, sqlite};
use crate::history::Conversation;
use crate::{detect, tmux};
use regex_lite::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

pub struct Codex;

const UUID: &str = "[[:xdigit:]]{8}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{12}";

fn root() -> PathBuf {
    std::env::var("CODEX_HOME").map_or_else(|_| PathBuf::from(tmux::home()).join(".codex"), PathBuf::from)
}

/// Codex names these state_<n>.sqlite.
fn database() -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(root())
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "sqlite") && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("state_")))
        .collect();
    found.sort();
    found.pop()
}

/// Every file below `dir` called rollout-*.jsonl.
fn rollouts(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rollouts(&path, found);
        } else if path.extension().is_some_and(|e| e == "jsonl") && path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("rollout-")) {
            found.push(path);
        }
    }
}

/// Seconds since the epoch of an ISO time, from the days of its civil date.
fn epoch(iso: &str) -> Option<i64> {
    let number = |range: std::ops::Range<usize>| iso.get(range)?.parse::<i64>().ok();
    let (mut y, mut m) = (number(0..4)?, number(5..7)?);
    if m <= 2 {
        y -= 1;
        m += 12;
    }
    let days = 365 * y + y / 4 - y / 100 + y / 400 + (153 * (m - 3) + 2) / 5 + number(8..10)? - 719_469;
    Some(days * 86_400 + number(11..13)? * 3600 + number(14..16)? * 60 + number(17..19)?)
}

/// Without sqlite3 (Codex bundles SQLite as a library): its plain session_index.jsonl (id, name, time; appended to, so
/// the last line per id is current) plus each conversation file's first line for the folder, cached as it never changes.
fn from_index() -> Vec<Conversation> {
    static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!("({UUID})$")).unwrap());
    let Ok(index) = std::fs::read_to_string(root().join("session_index.jsonl")) else {
        return Vec::new();
    };
    let cache = tmux::history_dir().join("codex-folders.tsv");
    let text = std::fs::read_to_string(&cache).unwrap_or_default();
    let mut folders: HashMap<String, String> = text
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(id, folder)| (id.to_string(), folder.to_string()))
        .collect();
    let mut files = Vec::new();
    rollouts(&root().join("sessions"), &mut files);
    for file in files {
        let stem = file.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let Some(id) = ID.find(&stem).map(|m| m.as_str().to_string()) else {
            continue;
        };
        // Kept once known; one read before Codex wrote its first line is empty, and read again next time.
        if folders.get(&id).is_some_and(|folder| !folder.is_empty()) {
            continue;
        }
        let head = read_ends(&file, 64_000, 0);
        let first = head.lines().next().unwrap_or_default();
        let start = first.find("\"cwd\":\"").map(|at| at + 7);
        let folder = start.and_then(|s| first[s..].find('"').map(|end| first[s..s + end].to_string()));
        folders.insert(id, folder.unwrap_or_default());
    }
    let lines: Vec<String> = folders.iter().map(|(id, folder)| format!("{id}\t{folder}")).collect();
    let _ = tmux::write_atomic(&cache, &lines.join("\n"));
    let mut latest: HashMap<String, Conversation> = HashMap::new();
    for line in index.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let Some(id) = entry["id"].as_str() else { continue };
        let time = entry["updated_at"].as_str().and_then(epoch).unwrap_or(0);
        let title = entry["thread_name"].as_str().unwrap_or_default().to_string();
        let folder = folders.get(id).cloned().unwrap_or_default();
        latest.insert(
            id.to_string(),
            Conversation {
                tool: "codex".into(),
                id: id.to_string(),
                time,
                folder,
                title,
            },
        );
    }
    latest.into_values().collect()
}

/// The thread id from a /status card printed between two captures: there must be more "Session:" lines than before,
/// and the last one is new. An older card in the scrollback never counts.
fn fresh_status(before: &str, after: &str) -> Option<String> {
    static SESSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"Session:\s*({UUID})")).unwrap());
    let count = |text: &str| text.lines().filter(|line| SESSION.is_match(line)).count();
    if count(after) <= count(before) {
        return None;
    }
    after.lines().rev().find_map(|line| SESSION.captures(line).map(|c| c[1].to_string()))
}

/// Context used is 100 minus Codex's rounded remaining percentage, from its token log.
/// shortcut: mirrors Codex 0.165.0's 12K baseline; update if its context calculation changes.
/// What one rollout line says about turns: one starting (true), one over (false), or nothing. A session_meta means Codex
/// started again (it stopped mid-turn), so any turn before it is over.
fn turn_event(line: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(line).ok()?;
    if !["task_started", "task_complete", "turn_aborted", "session_meta"]
        .iter()
        .any(|kind| text.contains(kind))
    {
        return None;
    }
    let entry: Value = serde_json::from_str(text).ok()?;
    if entry["type"] == "session_meta" {
        return Some(false);
    }
    match entry["payload"]["type"].as_str()? {
        "task_started" => Some(true),
        "task_complete" | "turn_aborted" => Some(false),
        _ => None,
    }
}

fn context_percent(tail: &str) -> Option<String> {
    let line = tail.lines().rev().find(|line| line.contains("\"token_count\""))?;
    let number = |text: &str, key: &str| -> Option<f64> {
        let rest = &text[text.find(key)? + key.len()..];
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        rest[..end].parse().ok()
    };
    let usage = &line[line.find("\"last_token_usage\":{")?..];
    let total = number(usage, "\"total_tokens\":")?;
    let size = number(line, "\"model_context_window\":")?;
    if size <= 0.0 {
        return None;
    }
    let window = size - 12_000.0;
    let used = (total - 12_000.0).max(0.0);
    let left = if window > 0.0 {
        ((window - used) / window * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    Some(format!("{}%", 100 - left.round() as i64))
}

/// Codex's own context arrives as "user" text too (<environment_context>, # AGENTS.md…): skipped.
fn read_exchange(text: &str) -> Vec<(Who, String)> {
    let (mut first, mut you, mut answer) = (String::new(), String::new(), String::new());
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let payload = if entry["payload"].is_object() { &entry["payload"] } else { &entry };
        let wanted = match payload["role"].as_str() {
            Some("user") => "input_text",
            Some("assistant") => "output_text",
            _ => continue,
        };
        let blocks = payload["content"].as_array();
        let block = blocks.and_then(|b| b.iter().find(|part| part["type"] == wanted));
        let Some(text) = block.and_then(|b| b["text"].as_str()).filter(|t| !t.is_empty()) else {
            continue;
        };
        if wanted == "output_text" {
            answer = text.to_string();
        } else if !text.starts_with('<') && !text.starts_with("# ") {
            if first.is_empty() {
                first = text.to_string();
            }
            you = text.to_string();
            answer.clear();
        }
    }
    super::exchange(first, you, answer)
}

impl Agent for Codex {
    fn command(&self) -> &str {
        "codex"
    }

    /// Every conversation (thread) from its state database; an untitled one was never used.
    fn history(&self) -> Vec<Conversation> {
        let sql = format!(
            "select 'codex', id, updated_at, cwd, replace(replace(coalesce(nullif(name, ''), title), char(10), ' '), char(9), ' ')
            from threads where archived = 0 and title != '' order by updated_at desc limit {}",
            crate::config::get().history.limit
        );
        match database().filter(|_| on_path("sqlite3")) {
            Some(db) => sqlite(&db, &sql).map_or_else(Vec::new, |rows| conversations(&rows)),
            None => from_index(),
        }
    }

    /// Its status bar: "GPT-6.1-Sol high · ~/repos/ProjectC · Topic".
    fn folder(&self, screen: &str) -> Option<String> {
        let bottom = screen.lines().rev().filter(|line| !line.trim().is_empty()).take(6);
        let shown = bottom
            .filter_map(|line| line.split(" · ").nth(1))
            .find(|field| field.starts_with(['~', '/']))?;
        let folder = tmux::untilde(shown.trim());
        Path::new(&folder).is_dir().then_some(folder)
    }

    fn cd(&self, folder: &str) -> Option<String> {
        Some(format!("/cd {folder}"))
    }

    fn resume(&self, id: &str) -> Option<String> {
        Some(format!("codex resume {id}"))
    }

    fn hands_off(&self) -> bool {
        true
    }

    fn transcript(&self, id: &str) -> Option<PathBuf> {
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return None;
        }
        let sql = format!("select rollout_path from threads where id = '{id}'");
        let saved = database().and_then(|db| sqlite(&db, &sql)).map(|path| PathBuf::from(path.trim()));
        if let Some(path) = saved.filter(|p| p.is_file()) {
            return Some(path);
        }
        let mut files = Vec::new();
        rollouts(&root().join("sessions"), &mut files);
        rollouts(&root().join("archived_sessions"), &mut files);
        let suffix = format!("-{id}.jsonl");
        files.into_iter().find(|f| f.to_string_lossy().ends_with(&suffix))
    }

    fn exports(&self) -> bool {
        true
    }

    /// `/status` prints the conversation id, whose saved transcript the new agent reads.
    fn export(&self, pane: &str, _dir: &Path) -> Result<(String, PathBuf), String> {
        let failed = || "Could not identify the live Codex conversation".to_string();
        let before = tmux::capture(pane, &["-J", "-S", "-"]).ok_or_else(failed)?;
        if !detect::send_command(pane, "/status") {
            return Err(failed());
        }
        for _ in 0..30 {
            sleep(0.1);
            let after = tmux::capture(pane, &["-J", "-S", "-"]).ok_or_else(failed)?;
            if let Some(id) = fresh_status(&before, &after) {
                let path = self.transcript(&id).ok_or_else(failed)?;
                return Ok((id, path));
            }
        }
        detect::clear_local_command(pane, "/status");
        Err(failed())
    }

    /// An empty /rename opens a dialog prefilled with Codex's suggestion: accept it.
    fn finish_rename(&self, pane: &str) -> Option<Result<String, String>> {
        for _ in 0..150 {
            sleep(0.1);
            let screen = tmux::capture(pane, &[]).unwrap_or_default();
            if screen.contains("Suggested from this conversation") {
                tmux::send_key(pane, "Enter");
                return Some(Ok("Accepted Codex's suggestion".into()));
            }
            if screen.contains("Type a name and press Enter") {
                // An empty conversation: nothing to suggest a name from.
                tmux::send_key(pane, "Escape");
                return Some(Err(
                    "Not renamed: Codex can't suggest a name before the conversation starts; type one".into()
                ));
            }
        }
        tmux::send_key(pane, "Escape");
        Some(Err("Not renamed: Codex gave no suggestion".into()))
    }

    fn context(&self, pane: &str, title: &str, agents: &Registry) -> Option<String> {
        super::log_context(&pane_transcript(self, pane, title, agents)?, context_percent)
    }

    fn logs_turns(&self) -> bool {
        true
    }

    /// From the rollout: Codex writes a turn's start and end there as they happen, approvals not at all.
    fn turn(&self, pane: &str, title: &str, agents: &Registry) -> Option<super::Turn> {
        super::last_turn(&pane_transcript(self, pane, title, agents)?, turn_event).map(super::Turn::open)
    }

    /// Codex shows "esc to interrupt" only while it thinks or runs tools, not while its answer streams in. A turn ends
    /// with "Worked for 12s" (or "Conversation interrupted"), so an answer line (•) after your latest message (›) is one
    /// still being written until such a line follows that message. • also marks notices, like /cd's, printed after a turn.
    fn busy(&self, screen: &str) -> bool {
        let (mut user, mut input, mut answer, mut ended) = (0, 0, 0, 0);
        for (i, line) in screen.lines().enumerate() {
            let at = i + 1;
            if line == "›" || line.starts_with("› ") {
                user = input;
                input = at;
            }
            if line.starts_with("• ") {
                answer = at;
            }
            if line.contains("Worked for ") || line.contains("Conversation interrupted") {
                ended = at;
            }
        }
        answer > user && ended < user && answer < input
    }

    fn messages(&self, id: &str) -> Vec<(Who, String)> {
        self.transcript(id)
            .map_or_else(Vec::new, |path| read_exchange(&read_ends(&path, 300_000, 2_000_000)))
    }

    fn removes(&self) -> bool {
        true
    }

    fn remove(&self, id: &str) -> Result<String, String> {
        let archived = Command::new("codex").args(["archive", id]).output().is_ok_and(|o| o.status.success());
        if archived {
            Ok(format!("Archived (codex unarchive {id} restores it)"))
        } else {
            Err("Not removed: Codex couldn't archive it".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "11111111-2222-3333-4444-555555555555";
    const B: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    #[test]
    fn iso_times_become_epoch_seconds() {
        assert_eq!(epoch("1970-01-01T00:00:10Z"), Some(10));
        assert_eq!(epoch("2026-11-01T12:00:00Z"), Some(1_793_534_400));
    }

    #[test]
    fn status_card_must_be_new() {
        let old = format!("Session: {A}\n");
        assert_eq!(fresh_status(&old, &old), None);
        assert_eq!(fresh_status(&old, &format!("{old}Session: {B}\n")).as_deref(), Some(B));
    }

    #[test]
    fn a_turn_is_open_from_its_start_until_it_completes_is_aborted_or_codex_starts_again() {
        let dir = std::env::temp_dir().join(format!("agentic-turn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let event = |kind: &str| format!(r#"{{"type":"event_msg","payload":{{"type":"{kind}"}}}}"#);
        let meta = r#"{"type":"session_meta","payload":{"id":"x"}}"#.to_string();
        // A tool's output quoting an event is a string inside the line, never the line's own type.
        let quoted = r#"{"type":"response_item","payload":{"type":"message","text":"\"task_complete\""}}"#.to_string();
        let big = format!(
            r#"{{"type":"response_item","payload":{{"type":"message","text":"{}"}}}}"#,
            "x".repeat(200_000)
        );
        let read = |name: &str, lines: &[String]| {
            let path = dir.join(name);
            std::fs::write(&path, lines.join("\n") + "\n").unwrap();
            crate::agents::read_last_turn(&path, std::fs::metadata(&path).unwrap().len(), turn_event)
        };
        assert_eq!(read("a", &[meta.clone(), event("task_started"), quoted.clone()]), Some(true));
        assert_eq!(read("b", &[meta.clone(), event("task_started"), event("task_complete")]), Some(false));
        assert_eq!(read("c", &[event("task_started"), event("turn_aborted")]), Some(false));
        assert_eq!(read("d", &[event("task_started"), meta.clone(), quoted]), Some(false), "started again");
        assert_eq!(
            read("e", &[meta.clone(), event("task_started"), big.clone(), big]),
            Some(true),
            "far back"
        );
        assert_eq!(read("f", &[event("token_count")]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_matches_the_clis_baseline_and_rounding() {
        let line = |total: u64, window: u64| {
            format!(r#"{{"type":"token_count","info":{{"last_token_usage":{{"total_tokens":{total}}},"model_context_window":{window}}}}}"#)
        };
        assert_eq!(context_percent(&line(112_000, 212_000)).as_deref(), Some("50%"));
        assert_eq!(context_percent(&line(5_000, 212_000)).as_deref(), Some("0%"));
        assert_eq!(context_percent(&line(999_000, 212_000)).as_deref(), Some("100%"));
        assert_eq!(context_percent("nothing"), None);
    }

    #[test]
    fn working_while_the_answer_streams_in() {
        assert!(Codex.busy("› fix it\n• Half an answer\n\n›"));
        assert!(!Codex.busy("› fix it\n• The answer\n─ Worked for 12s ─\n›"));
        assert!(!Codex.busy("• Old answer\n› new message\n›"));
    }

    #[test]
    fn its_folder_comes_from_its_status_bar() {
        let folder = std::env::temp_dir().canonicalize().unwrap().to_string_lossy().into_owned();
        let screen = format!(
            "• Working directory changed\n\n› Ask Codex to do anything\n  GPT-6.1-Sol high · {folder} · Test briefly\n  ← for agents · ? for shortcuts\n\n"
        );
        assert_eq!(Codex.folder(&screen), Some(folder));
        assert_eq!(Codex.folder("  enter to confirm · esc to cancel"), None);
        assert_eq!(
            Codex.folder("  GPT high · /no/such/folder/here · Topic"),
            None,
            "a line that only looks like it"
        );
    }

    #[test]
    fn a_notice_after_a_finished_turn_is_not_an_answer() {
        let screen = "› just testing\n• Got it—I'm here.\n  Worked for 1s • 23:21\n• Working directory changed to: ~/repos/ProjectC\n\n› Ask Codex to do anything\n  GPT-6.1-Sol high";
        assert!(!Codex.busy(screen));
        assert!(Codex.busy("› one\n• done\n  Worked for 1s\n› two\n• Half an answer\n\n›"));
    }

    #[test]
    fn its_own_context_is_not_your_message() {
        let text = r##"{"payload":{"role":"user","content":[{"type":"input_text","text":"<environment_context>"}]}}
{"payload":{"role":"user","content":[{"type":"input_text","text":"# AGENTS.md"}]}}
{"payload":{"role":"user","content":[{"type":"input_text","text":"hello"}]}}
{"payload":{"role":"assistant","content":[{"type":"output_text","text":"hi"}]}}"##;
        assert_eq!(read_exchange(text), [(Who::You, "hello".to_string()), (Who::Agent, "hi".to_string())]);
    }
}
