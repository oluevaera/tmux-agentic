//! Claude Code.
use super::{Agent, Registry, Who, pane_transcript, read_ends, sleep};
use crate::history::Conversation;
use crate::{detect, tmux};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

pub struct Claude;

fn root() -> PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR").map_or_else(|_| PathBuf::from(tmux::home()).join(".claude"), PathBuf::from)
}

/// Every value of `"key":"…"`, unescaped. Text inside a message has its quotes escaped, so it never matches.
fn values(text: &str, key: &str) -> Vec<String> {
    let pattern = format!("\"{key}\":\"");
    let mut found = Vec::new();
    for (at, _) in text.match_indices(&pattern) {
        let start = at + pattern.len() - 1;
        let bytes = text.as_bytes();
        let mut i = start + 1;
        while i < bytes.len() && bytes[i] != b'"' {
            i += if bytes[i] == b'\\' { 2 } else { 1 };
        }
        if let Some(Ok(value)) = text.get(start..=i).map(serde_json::from_str::<String>) {
            found.push(value);
        }
    }
    found
}

/// Folder and title of a transcript: the `/rename` name, else Claude's own title.
/// What a transcript says of itself so far: read on from where it was last read, as a live one grows all the time.
#[derive(Clone, Default)]
struct Summary {
    upto: u64,
    cwd: Option<String>,
    custom: Option<String>,
    ai: Option<String>,
}

fn read_conversation(path: &Path) -> (String, String) {
    static KNOWN: LazyLock<Mutex<HashMap<PathBuf, Summary>>> = LazyLock::new(Default::default);
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    let known = KNOWN.lock().unwrap().get(path).cloned();
    let mut summary = known.filter(|s| s.upto <= size).unwrap_or_default();
    if let Some((text, upto)) = super::new_lines(path, summary.upto) {
        // /cd moves the transcript too, and it resumes from there: its folder is the one it moved to last, not where it
        // began.
        match text.rfind("\"type\":\"relocated\"") {
            Some(moved) => summary.cwd = values(&text[moved..], "cwd").into_iter().next(),
            None if summary.cwd.is_none() => summary.cwd = values(&text, "cwd").into_iter().next(),
            None => {}
        }
        summary.custom = values(&text, "customTitle").pop().or(summary.custom);
        summary.ai = values(&text, "aiTitle").pop().or(summary.ai);
        summary.upto = upto;
    }
    let (cwd, title) = (summary.cwd.clone(), summary.custom.clone().or_else(|| summary.ai.clone()));
    let mut known = KNOWN.lock().unwrap();
    if known.len() > 4096 {
        known.clear();
    }
    known.insert(path.to_path_buf(), summary);
    (cwd.unwrap_or_default(), title.unwrap_or_default().replace(['\n', '\t'], " "))
}

/// Its working line: a glyph, a word ending "…", then the time it has taken: "✻ Thinking… (3s". A finished turn reads
/// "Worked for 4s", with no "…".
fn spinner(line: &str) -> bool {
    let mut words = line.trim_start().splitn(3, ' ');
    let (Some(glyph), Some(word), Some(rest)) = (words.next(), words.next(), words.next()) else {
        return false;
    };
    !glyph.is_ascii()
        && glyph.chars().count() == 1
        && word.ends_with('…')
        && rest.starts_with('(')
        && rest[1..].starts_with(|c: char| c.is_ascii_digit())
}

fn context_percent(tail: &str) -> Option<String> {
    let mut model = String::new();
    let mut used = 0;
    for line in tail.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let message = &entry["message"];
        let tokens = |key: &str| message["usage"][key].as_u64().unwrap_or(0);
        let total = tokens("input_tokens") + tokens("cache_creation_input_tokens") + tokens("cache_read_input_tokens");
        if total > 0 {
            used = total;
            model = message["model"].as_str().unwrap_or_default().to_string();
        }
    }
    if used == 0 {
        return None;
    }
    // shortcut: the window isn't recorded anywhere, so it follows the model: Claude 5 models (Opus, Sonnet, Fable) have
    // 1M, Haiku and older ones 200K (unless a conversation is already past that); update when a model differs.
    // The major version is the id's first number: claude-3-5-sonnet is 3, claude-sonnet-4-5 is 4, claude-opus-5-4 is 5.
    let major = model.split('-').find_map(|part| part.parse::<u32>().ok());
    let older = model.contains("haiku") || major.is_some_and(|major| major < 5);
    let window = if older && used <= 200_000 { 200_000 } else { 1_000_000 };
    Some(format!("{}%", used * 100 / window))
}

/// What a prompt says: its text, or with an image its text parts ("(image)" with none). A tool's result isn't one.
fn prompt_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let parts = content.as_array()?;
    if parts.iter().any(|part| part["type"] == "tool_result") {
        return None;
    }
    let text: Vec<&str> = parts.iter().filter_map(|part| part["text"].as_str()).collect();
    Some(if text.is_empty() { "(image)".into() } else { text.join("\n") })
}

/// Your messages are "user" lines holding text that aren't notes or commands (those start "<").
fn read_exchange(text: &str) -> Vec<(Who, String)> {
    let (mut first, mut you, mut answer) = (String::new(), String::new(), String::new());
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        match entry["type"].as_str() {
            Some("user") if entry["isMeta"] != true => {
                if let Some(content) = prompt_text(&entry["message"]["content"])
                    && !content.is_empty()
                    && !content.starts_with('<')
                {
                    if first.is_empty() {
                        first = content.to_string();
                    }
                    you = content.to_string();
                    answer.clear();
                }
            }
            Some("assistant") => {
                let blocks = entry["message"]["content"].as_array();
                let block = blocks.and_then(|b| b.iter().find(|part| part["type"] == "text"));
                if let Some(text) = block.and_then(|b| b["text"].as_str()).filter(|t| !t.is_empty()) {
                    answer = text.to_string();
                }
            }
            _ => {}
        }
    }
    super::exchange(first, you, answer)
}

impl Agent for Claude {
    fn command(&self) -> &str {
        "claude"
    }

    /// One .jsonl file per conversation, named by its id. Files can be megabytes, so each one's folder and title are
    /// cached until the file changes.
    fn history(&self) -> Vec<Conversation> {
        let cache = tmux::history_dir().join("claude.tsv");
        let old: HashMap<String, Conversation> = std::fs::read_to_string(&cache)
            .unwrap_or_default()
            .lines()
            .filter_map(Conversation::parse)
            .map(|c| (c.id.clone(), c))
            .collect();
        let mut files = Vec::new();
        for folder in std::fs::read_dir(root().join("projects")).into_iter().flatten().flatten() {
            for file in std::fs::read_dir(folder.path()).into_iter().flatten().flatten() {
                let path = file.path();
                if path.extension().is_none_or(|e| e != "jsonl") {
                    continue;
                }
                let modified = file.metadata().and_then(|m| m.modified()).ok();
                let time = modified
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs() as i64);
                files.push((time, path));
            }
        }
        // Newest first: past History's limit, only what's known already is kept, and nothing new is read.
        files.sort_by_key(|(time, _)| std::cmp::Reverse(*time));
        let limit = crate::config::get().history.limit;
        let mut all = Vec::new();
        for (at, (time, path)) in files.into_iter().enumerate() {
            let id = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            // A file changed this second may change again within it (a title right after its prompt): read again.
            all.push(match old.get(&id) {
                Some(known) if known.time == time && time < tmux::now() - 1 => known.clone(),
                _ if at >= limit => continue,
                _ => {
                    let (folder, title) = read_conversation(&path);
                    Conversation {
                        tool: "claude".into(),
                        id,
                        time,
                        folder,
                        title,
                    }
                }
            });
        }
        let lines: Vec<String> = all.iter().map(Conversation::line).collect();
        let _ = tmux::write_atomic(&cache, &lines.join("\n"));
        all.retain(|c| !c.folder.is_empty());
        all
    }

    fn cd(&self, folder: &str) -> Option<String> {
        Some(format!("/cd {folder}"))
    }

    /// It asks first for a folder it doesn't know, and you can say no.
    fn moves_itself(&self) -> bool {
        true
    }

    fn resume(&self, id: &str) -> Option<String> {
        Some(format!("claude --resume {id}"))
    }

    fn hands_off(&self) -> bool {
        true
    }

    /// "esc to interrupt" is in its footer, which a long draft or a narrow pane can push out of view; the spinner line
    /// just above the input box ("✢ Topsy-turvying… (2m 32s · ↓ 4.8k tokens)") is there all the time it works.
    fn busy(&self, screen: &str) -> bool {
        let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
        let Some(prompt) = lines.iter().rposition(|l| l.trim_start().starts_with('❯')) else {
            return false;
        };
        lines[prompt.saturating_sub(3)..prompt].iter().any(|line| spinner(line))
    }

    fn transcript(&self, id: &str) -> Option<PathBuf> {
        let name = format!("{id}.jsonl");
        std::fs::read_dir(root().join("projects"))
            .ok()?
            .flatten()
            .map(|d| d.path().join(&name))
            .find(|p| p.is_file())
    }

    fn exports(&self) -> bool {
        true
    }

    /// `/export` writes the conversation into the handoff folder for the new agent to read.
    fn export(&self, pane: &str, dir: &Path) -> Result<(String, PathBuf), String> {
        let id = format!("live-{}", pane.trim_start_matches('%'));
        let path = dir.join("conversation.txt");
        let command = format!("/export {}", path.display());
        if !detect::send_command(pane, &command) {
            return Err("Could not export the live Claude conversation".into());
        }
        // Matched by the export folder's unique name: Claude prints the path tidied up (no "//", maybe /private/var).
        let unique = format!("{}/conversation.txt", dir.file_name().unwrap_or_default().to_string_lossy());
        for _ in 0..30 {
            sleep(0.1);
            let written = std::fs::metadata(&path).is_ok_and(|m| m.len() > 0);
            let screen = tmux::capture(pane, &["-J", "-S", "-30"]).unwrap_or_default();
            let said = screen.lines().any(|line| {
                let lower = line.to_lowercase();
                line.contains(&unique) && (lower.contains("exported") || lower.contains("saved"))
            });
            if written && said {
                return Ok((id, path));
            }
        }
        detect::clear_local_command(pane, &command);
        Err("Could not export the live Claude conversation".into())
    }

    fn context(&self, pane: &str, title: &str, agents: &Registry) -> Option<String> {
        super::log_context(&pane_transcript(self, pane, title, agents)?, context_percent)
    }

    fn logs_turns(&self) -> bool {
        true
    }

    /// A /compact you type writes nothing until it's done, so a busy screen is believed over a record of the last turn's
    /// end.
    fn records_all_work(&self) -> bool {
        false
    }

    /// From the transcript: Claude writes your prompt, each step and the turn's end as they happen, permission prompts
    /// not at all.
    fn turn(&self, pane: &str, title: &str, agents: &Registry) -> Option<super::Turn> {
        super::last_turn(&pane_transcript(self, pane, title, agents)?, turn_event).map(super::Turn::open)
    }

    fn messages(&self, id: &str) -> Vec<(Who, String)> {
        self.transcript(id)
            .map_or_else(Vec::new, |path| read_exchange(&read_ends(&path, 300_000, 2_000_000)))
    }

    fn removes(&self) -> bool {
        true
    }

    /// Claude has no command to delete one conversation, so its transcript (and the folder of the same name next to
    /// it: sub-agents, tool output) goes to the Trash, where the file manager can put it back.
    fn remove(&self, id: &str) -> Result<String, String> {
        let Some(path) = self.transcript(id) else {
            return Err("Not removed: its transcript is gone".into());
        };
        match super::trash(&[path.clone(), path.with_extension("")]) {
            Ok(who) => Ok(format!("Moved to the Trash ({who} can put it back)")),
            Err(why) => Err(format!("Not removed: {why}")),
        }
    }
}

/// What one transcript line says about turns. Under way (true): your prompt (it has a promptSource; slash and shell
/// commands, which don't, start none), a tool's result, or Claude's step calling a tool. Over (false): the turn's
/// timing, written as it ends, an interrupt, or Claude's last reply, which an older Claude ends a turn with alone.
/// Sub-agents' lines say nothing about this one.
// shortcut: a Claude that stopped mid-turn and was resumed looks busy until its next turn ends; its process start time
// would tell, if that ever shows up.
fn turn_event(line: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(line).ok()?;
    if !["\"user\"", "\"assistant\"", "turn_duration"].iter().any(|word| text.contains(word)) {
        return None;
    }
    let entry: Value = serde_json::from_str(text).ok()?;
    if entry["isSidechain"] == true {
        return None;
    }
    match entry["type"].as_str()? {
        "system" => (entry["subtype"] == "turn_duration").then_some(false),
        "user" if entry.get("interruptedMessageId").is_some() => Some(false),
        "user" if entry["isMeta"] == true => None,
        "user" => {
            let content = entry["message"]["content"].as_array();
            let result = content.is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "tool_result"));
            (result || entry.get("promptSource").is_some()).then_some(true)
        }
        // Without a stop_reason (rare) it says nothing: the lines before it decide.
        "assistant" => entry["message"]["stop_reason"].as_str().map(|stop| stop == "tool_use"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_runs_from_your_prompt_until_it_ends_is_interrupted_or_claude_answers() {
        let says = |line: &str| turn_event(line.as_bytes());
        assert_eq!(
            says(r#"{"type":"user","promptSource":"typed","message":{"content":"fix it"}}"#),
            Some(true)
        );
        assert_eq!(
            says(r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#),
            Some(true)
        );
        assert_eq!(says(r#"{"type":"assistant","message":{"stop_reason":"tool_use"}}"#), Some(true));
        assert_eq!(says(r#"{"type":"system","subtype":"turn_duration"}"#), Some(false));
        assert_eq!(
            says(r#"{"type":"user","interruptedMessageId":"x","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#),
            Some(false)
        );
        assert_eq!(says(r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}"#), Some(false));
        // Slash commands, meta lines, sub-agents and the rest say nothing.
        assert_eq!(
            says(r#"{"type":"user","message":{"content":"<command-name>/model</command-name>"}}"#),
            None
        );
        assert_eq!(
            says(r#"{"type":"user","isMeta":true,"promptSource":"x","message":{"content":"x"}}"#),
            None
        );
        assert_eq!(
            says(r#"{"type":"assistant","isSidechain":true,"message":{"stop_reason":"tool_use"}}"#),
            None
        );
        assert_eq!(says(r#"{"type":"system","subtype":"away_summary"}"#), None);
        assert_eq!(says(r#"{"type":"assistant","message":{"content":[]}}"#), None);
    }

    #[test]
    fn claude_5_minors_have_the_1m_window_and_older_models_200k() {
        let at = |model: &str| {
            let line = format!(r#"{{"type":"assistant","message":{{"model":"{model}","usage":{{"input_tokens":100000,"output_tokens":0}}}}}}"#);
            context_percent(&line)
        };
        assert_eq!(at("claude-opus-5-4").as_deref(), Some("10%"));
        assert_eq!(at("claude-sonnet-5-3").as_deref(), Some("10%"));
        assert_eq!(at("claude-sonnet-4-5").as_deref(), Some("50%"));
        assert_eq!(at("claude-3-5-sonnet-20241022").as_deref(), Some("50%"));
        assert_eq!(at("claude-haiku-5-5").as_deref(), Some("50%"));
    }

    #[test]
    fn a_prompt_with_an_image_still_counts_as_yours() {
        let lines = [
            r#"{"type":"user","message":{"content":"first question"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"first answer"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"image","source":{}},{"type":"text","text":"what is this?"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"a screenshot"}]}}"#,
        ];
        let exchange = read_exchange(&lines.join("\n"));
        assert!(exchange.contains(&(Who::You, "what is this?".into())), "{exchange:?}");
        assert!(exchange.contains(&(Who::Agent, "a screenshot".into())));
    }

    #[test]
    fn title_prefers_the_rename_and_ignores_quoted_keys() {
        let text = r#"{"cwd":"/w/a","aiTitle":"Auto"}
{"content":"talking about \"customTitle\":\"nope\""}
{"customTitle":"Mine \"x\""}"#;
        assert_eq!(values(text, "cwd"), ["/w/a"]);
        assert_eq!(values(text, "customTitle"), ["Mine \"x\""]);
    }

    #[test]
    fn its_spinner_line_says_it_works_even_without_its_footer() {
        let busy = |screen: &str| Claude.busy(screen);
        assert!(busy(
            "✢ Topsy-turvying… (2m 32s · ↓ 4.8k tokens)\n\n────\n❯ a long draft that pushed the footer away\n────"
        ));
        assert!(busy("✻ Thinking… (3s)\n❯ "));
        assert!(!busy("✻ Worked for 4s · done 12:40 AM\n❯ "), "finished");
        assert!(!busy("I said ✻ Thinking… (3s) earlier\n❯ "), "quoted in a message");
        assert!(!busy("✻ Thinking… (3s)\none\ntwo\nthree\nfour\n❯ "), "not right above the box");
        assert!(!busy("✻ Thinking… (3s)"), "no input box at all");
    }

    #[test]
    fn a_conversation_belongs_to_the_folder_it_moved_to_last() {
        let dir = std::env::temp_dir().join(format!("agentic-claude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("moved.jsonl");
        let _ = std::fs::remove_file(&file);
        // Appended a whole line at a time, as Claude writes it.
        let add = |text: &str| {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file)
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        };
        add("{\"cwd\":\"/w/a\"}\n");
        assert_eq!(read_conversation(&file).0, "/w/a");
        // Said no to /cd: "Staying in", and nothing relocated.
        add("{\"cwd\":\"/w/a\",\"content\":\"Staying in /w/a\"}\n");
        assert_eq!(read_conversation(&file).0, "/w/a");
        add("{\"type\":\"relocated\",\"relocatedCwd\":\"/w/b\"}\n{\"cwd\":\"/w/b\"}\n{\"cwd\":\"/w/b/sub\"}\n");
        assert_eq!(read_conversation(&file).0, "/w/b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_uses_the_last_turn_and_the_models_window() {
        let line = |model: &str, input: u64, read: u64| {
            format!(
                r#"{{"message":{{"model":"{model}","usage":{{"input_tokens":{input},"cache_creation_input_tokens":0,"cache_read_input_tokens":{read}}}}}}}"#
            )
        };
        let text = format!("{}\n{}", line("claude-opus-5-5", 1, 1), line("claude-opus-5-5", 100_000, 10_000));
        assert_eq!(context_percent(&text).as_deref(), Some("11%"));
        assert_eq!(context_percent(&line("claude-haiku-5-5", 100_000, 0)).as_deref(), Some("50%"));
        assert_eq!(context_percent(&line("claude-sonnet-4-5", 100_000, 0)).as_deref(), Some("50%"));
        assert_eq!(context_percent(&line("claude-sonnet-4-5", 300_000, 0)).as_deref(), Some("30%"));
        assert_eq!(context_percent("not json"), None);
    }

    #[test]
    fn exchange_pairs_the_last_message_with_its_answer() {
        let text = r#"{"type":"user","message":{"role":"user","content":"first"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"old answer"}]}}
{"type":"user","isMeta":true,"message":{"role":"user","content":"meta"}}
{"type":"user","message":{"role":"user","content":"<command>x</command>"}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result"}]}}
{"type":"user","message":{"role":"user","content":"last"}}"#;
        assert_eq!(
            read_exchange(text),
            [
                (Who::First, "first".to_string()),
                (Who::You, "last".to_string()),
                (Who::Agent, String::new())
            ]
        );
    }
}
