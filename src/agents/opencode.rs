//! OpenCode.
use super::{Agent, Registry, Turn, Who, conversations, on_path, pane_conversation, sqlite};
use crate::history::{self, Conversation};
use crate::tmux;
use regex_lite::Regex;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

pub struct OpenCode;

fn database() -> PathBuf {
    tmux::xdg("XDG_DATA_HOME", ".local/share").join("opencode/opencode.db")
}

/// Where OpenCode's background service listens (`opencode serve --service`: every OpenCode shares it) and the
/// Authorization its password makes, from the state file it writes. Loopback only: the password goes nowhere else.
fn service() -> Option<(String, String)> {
    let state = tmux::xdg("XDG_STATE_HOME", ".local/state").join("opencode/service.json");
    let file: Value = serde_json::from_str(&std::fs::read_to_string(state).ok()?).ok()?;
    let address = file["url"].as_str()?.strip_prefix("http://")?.trim_end_matches('/').to_string();
    if !["127.0.0.1:", "localhost:", "[::1]:"].iter().any(|host| address.starts_with(host)) {
        return None;
    }
    let password = file["password"].as_str()?;
    Some((address, format!("Basic {}", base64(format!("opencode:{password}").as_bytes()))))
}

/// The JSON the service answers a GET of `path` with. Plain HTTP/1.0, one request on loopback: no client library needed.
fn get(path: &str) -> Option<Value> {
    let (address, auth) = service()?;
    let to = address.to_socket_addrs().ok()?.next()?;
    let mut stream = TcpStream::connect_timeout(&to, Duration::from_millis(300)).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    write!(stream, "GET {path} HTTP/1.0\r\nHost: {address}\r\nAuthorization: {auth}\r\n\r\n").ok()?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).ok()?;
    let (head, body) = answer.split_once("\r\n\r\n")?;
    head.split_whitespace().nth(1).filter(|code| *code == "200")?;
    serde_json::from_str(body).ok()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

fn api_update(id: &str, data: &str) -> bool {
    let param = format!("sessionID={id}");
    let args = ["api", "session.update", "--param", &param, "--data", data];
    Command::new("opencode").args(args).output().is_ok_and(|o| o.status.success())
}

impl Agent for OpenCode {
    fn command(&self) -> &str {
        "opencode"
    }

    fn name(&self) -> String {
        "OpenCode".into()
    }

    /// Pane title "OC | <session title>"; "OpenCode" until it has one.
    fn title(&self, pane_title: &str) -> String {
        match pane_title.strip_prefix("OC | ") {
            Some(title) => title.to_string(),
            None if pane_title == "OpenCode" => String::new(),
            None => pane_title.to_string(),
        }
    }

    /// Its sessions are in a SQLite database: top-level ones only (sub-agent sessions have a parent); one without a
    /// title yet shows its slug ("happy-forest"). Times are in milliseconds.
    fn history(&self) -> Vec<Conversation> {
        let sql = format!(
            "select 'opencode', id, time_updated / 1000, directory, replace(replace(coalesce(nullif(title, ''), slug), char(10), ' '), char(9), ' ')
            from session_v2 where parent_id is null and time_archived is null order by time_updated desc limit {}",
            crate::config::get().history.limit
        );
        let db = database();
        if !db.is_file() {
            Vec::new()
        } else {
            sqlite(&db, &sql).map_or_else(Vec::new, |rows| conversations(&rows))
        }
    }

    /// The left of its footer: "~/repos/ProjectC:main   shift+tab agents  ctrl+p commands", the branch in a git repository.
    /// Too long for the pane it's shortened with "…", which says nothing about where it is.
    fn folder(&self, screen: &str) -> Option<String> {
        let bottom = screen.lines().rev().filter(|line| !line.trim().is_empty()).take(6);
        let footer = bottom.into_iter().find(|line| line.contains("ctrl+p commands"))?;
        let shown = footer.trim().split("  ").next()?.trim();
        if !shown.starts_with(['~', '/']) || shown.contains('…') {
            return None;
        }
        let bare = shown.rsplit_once(':').map(|(path, _branch)| path);
        [Some(shown), bare]
            .into_iter()
            .flatten()
            .map(tmux::untilde)
            .find(|folder| Path::new(folder).is_dir())
    }

    fn cd(&self, folder: &str) -> Option<String> {
        Some(format!("/cd {folder}"))
    }

    fn resume(&self, id: &str) -> Option<String> {
        Some(format!("opencode --session {id}"))
    }

    /// Its first argument is a folder, so the message goes to --prompt.
    fn prompt(&self, message: &str) -> String {
        format!("opencode --prompt {}", tmux::shell_quote(message))
    }

    fn hands_off(&self) -> bool {
        true
    }

    /// The session as JSON, written out by opencode itself, for the agent it is handed to. Refreshed each time, so a
    /// live session hands over its latest messages.
    fn transcript(&self, id: &str) -> Option<PathBuf> {
        let output = Command::new("opencode").args(["session", "export", id]).output().ok()?;
        if !output.status.success() || output.stdout.is_empty() {
            return None;
        }
        let file = tmux::history_dir().join(format!("opencode-{id}.json"));
        tmux::write_atomic(&file, &String::from_utf8_lossy(&output.stdout)).ok()?;
        Some(file)
    }

    fn logs_turns(&self) -> bool {
        true
    }

    /// From its background service: whether the session runs, and whether it waits on a permission or a question.
    fn turn(&self, pane: &str, title: &str, agents: &Registry) -> Option<Turn> {
        let id = pane_conversation(self, pane, title, agents).filter(|id| id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))?;
        if get("/api/session/active")?["data"].get(&id).is_none() {
            return Some(Turn::Idle);
        }
        let pending =
            |what: &str| get(&format!("/api/session/{id}/{what}")).is_some_and(|answer| answer["data"].as_array().is_some_and(|d| !d.is_empty()));
        Some(if pending("permission") || pending("form") {
            Turn::Asking
        } else {
            Turn::Running
        })
    }

    /// One row of its database, not the export transcript() makes of the whole session.
    fn has_conversation(&self, id: &str) -> bool {
        let db = database();
        id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && sqlite(&db, &format!("select 1 from session_v2 where id = '{id}' limit 1")).is_some_and(|rows| !rows.trim().is_empty())
    }

    fn retitles(&self) -> bool {
        true
    }

    /// Its server sets the session's title, and the open session (and its pane title) shows it at once. It can't pick a
    /// name itself the way Claude does, so the name can't be empty.
    fn retitle(&self, pane: &str, name: &str, agents: &Registry) -> Result<String, String> {
        if name.is_empty() {
            return Err("Not renamed: OpenCode needs a name".into());
        }
        let id = history::live_conversation(agents, "opencode", pane, false).or_else(|| history::live_conversation(agents, "opencode", pane, true));
        let Some(id) = id else {
            return Err("Not renamed: can't tell which OpenCode session this is".into());
        };
        let clean: String = name.chars().filter(|c| !c.is_control()).collect();
        let data = serde_json::json!({ "title": clean }).to_string();
        if api_update(&id, &data) {
            Ok(format!("Renamed to: {name}"))
        } else {
            Err("Not renamed: OpenCode didn't take the name".into())
        }
    }

    /// Its own status bar shows it, e.g. "7.5K (4%)".
    fn context(&self, pane: &str, _title: &str, _agents: &Registry) -> Option<String> {
        let screen = tmux::capture(pane, &[])?;
        context_percent(&screen)
    }

    fn messages(&self, id: &str) -> Vec<(Who, String)> {
        let db = database();
        if !db.is_file() || !on_path("sqlite3") || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Vec::new(); // an id, never SQL
        }
        let last = format!("(select seq from session_message where session_id = '{id}' and type = 'user' order by seq desc limit 1)");
        let oneline = |json: &str| format!("replace(replace(json_extract({json}), char(13), ''), char(10), '\\n')");
        let text = oneline("data, '$.text'");
        let part = oneline("part.value, '$.text'");
        let sql = format!(
            "select 'first', {text} from session_message where session_id = '{id}' and type = 'user' and seq < {last} order by seq limit 1;
            select 'you', {text} from session_message where session_id = '{id}' and type = 'user' order by seq desc limit 1;
            select 'agent', {part} from session_message as m, json_each(json_extract(m.data, '$.content')) as part
            where m.session_id = '{id}' and m.type = 'assistant' and json_extract(part.value, '$.type') = 'text' and m.seq > {last}
            order by m.seq desc, part.key desc limit 1;"
        );
        let rows = sqlite(&db, &sql).unwrap_or_default();
        rows.lines()
            .filter_map(|row| row.split_once('\t'))
            .map(|(who, text)| {
                let who = match who {
                    "first" => Who::First,
                    "you" => Who::You,
                    _ => Who::Agent,
                };
                (who, text.replace("\\n", "\n"))
            })
            .collect()
    }

    fn removes(&self) -> bool {
        true
    }

    fn remove(&self, id: &str) -> Result<String, String> {
        if api_update(id, r#"{"archived":true}"#) {
            Ok("Archived (OpenCode can restore it)".into())
        } else {
            Err("Not removed: OpenCode couldn't archive it".into())
        }
    }
}

fn context_percent(screen: &str) -> Option<String> {
    static USED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9.]+K? \(([0-9]+%)\)").unwrap());
    let lines: Vec<&str> = screen.lines().collect();
    let bottom = lines[lines.len().saturating_sub(3)..].join("\n");
    USED.captures_iter(&bottom).last().map(|c| c[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_as_basic_auth_writes_it() {
        assert_eq!(base64(b"opencode:pw"), "b3BlbmNvZGU6cHc=");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
        assert_eq!(base64(b""), "");
    }

    #[test]
    fn its_footer_says_which_folder_it_is_in() {
        let folder = std::env::temp_dir().to_string_lossy().trim_end_matches('/').to_string();
        let screen = format!("  ┃  Build · Big Pickle\n  {folder}      shift+tab agents  ctrl+p commands    ~\n");
        assert_eq!(OpenCode.folder(&screen), Some(folder.clone()));
        assert_eq!(OpenCode.folder(&format!("  {folder}:main   ctrl+p commands")), Some(folder));
        assert_eq!(OpenCode.folder("  /…/534c69ea-576e-4…/two  shift+tab agents  ctrl+p commands"), None);
        assert_eq!(OpenCode.folder("  /no/such/folder/here  ctrl+p commands"), None);
        assert_eq!(OpenCode.folder("  7.5K (4%)  ctrl+p commands"), None);
        assert_eq!(OpenCode.folder("  /tmp  nothing else"), None);
    }

    #[test]
    fn context_comes_from_its_status_bar() {
        assert_eq!(context_percent("a\nb\n  7.5K (4%)  ctrl+p commands").as_deref(), Some("4%"));
        assert_eq!(context_percent("7.5K (4%)\na\nb\nc"), None);
    }
}
