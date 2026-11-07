//! Calls to tmux, and the paths everything shares.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// Every call to tmux. Unit tests get a server of their own: nothing they do can ever reach your sessions.
pub fn command() -> Command {
    let mut tmux = Command::new("tmux");
    if cfg!(test) {
        tmux.args(["-L", "agentic-unit-tests"]);
    }
    tmux
}

/// A name or folder as tmux keeps it, not read as a format: `#` doubled. For names and start folders only, never for
/// a format meant as one.
pub fn name(text: &str) -> String {
    text.replace('#', "##")
}

/// Arguments as tmux takes them word for word: one ending in ";" would end the command there, so it's escaped. A lone
/// ";" is left, being how commands are joined on purpose.
fn literal(args: &[&str]) -> Vec<String> {
    args.iter()
        .map(|arg| match arg.strip_suffix(';') {
            Some(rest) if !arg.is_empty() && *arg != ";" => format!("{rest}\\;"),
            _ => arg.to_string(),
        })
        .collect()
}

/// What tmux printed, without the last newline, or None when it failed.
pub fn out(args: &[&str]) -> Option<String> {
    let output = command().args(literal(args)).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.ends_with('\n') {
        text.pop();
    }
    Some(text)
}

/// One open connection to tmux (control mode), for the board's polling: a query costs no new process. It is a tmux client
/// of its own, so only queries with explicit targets go through it: a switch-client here would switch this client, not you.
struct Control {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static CONTROL: Mutex<Option<Control>> = Mutex::new(None);

/// no-output: tmux would otherwise stream every pane's output here. ignore-size: it must not affect window sizes.
pub fn connect(session: &str) {
    let spawned = command()
        .args(["-C", "attach", "-f", "no-output,ignore-size", "-t", session])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else { return };
    if let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) {
        *CONTROL.lock().unwrap() = Some(Control {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        });
    }
}

/// Before anything that depends on the current client: tmux could take this one for it.
pub fn disconnect() {
    *CONTROL.lock().unwrap() = None;
}

/// The reply to the next command: Err when the connection is gone, Ok(None) when the command failed. A block's end carries
/// its time and number, so a screen line that merely starts "%end" can't end it. Blocks with flags 0 aren't ours.
fn read_reply(stream: &mut impl BufRead) -> Result<Option<String>, ()> {
    let mut read_line = || {
        let mut bytes = Vec::new();
        match stream.read_until(b'\n', &mut bytes) {
            Ok(n) if n > 0 => {
                if bytes.last() == Some(&b'\n') {
                    bytes.pop();
                }
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
            _ => Err(()),
        }
    };
    loop {
        let line = read_line()?;
        let Some(begin) = line.strip_prefix("%begin ") else {
            if line.starts_with("%exit") {
                return Err(());
            }
            continue; // a notification
        };
        let mut fields = begin.split(' ');
        let (time, number, flags) = (fields.next().unwrap_or(""), fields.next().unwrap_or(""), fields.next().unwrap_or(""));
        let (end, error) = (format!("%end {time} {number} "), format!("%error {time} {number} "));
        let mut body: Vec<String> = Vec::new();
        loop {
            let line = read_line()?;
            let failed = line.starts_with(&error);
            if failed || line.starts_with(&end) {
                if flags == "1" {
                    return Ok((!failed).then(|| body.join("\n")));
                }
                break;
            }
            body.push(line);
        }
    }
}

/// A query with explicit targets: over the open connection when there is one, else as its own process.
/// shortcut: a lost connection (its session was killed) stays lost until the next open; polling just costs what it used to.
pub fn query(args: &[&str]) -> Option<String> {
    let mut control = CONTROL.lock().unwrap();
    if let Some(open) = control.as_mut().filter(|_| !args.iter().any(|arg| arg.contains('\n'))) {
        let line = args.iter().map(|arg| shell_quote(arg)).collect::<Vec<_>>().join(" ") + "\n";
        let sent = open.stdin.write_all(line.as_bytes()).and_then(|()| open.stdin.flush());
        match sent.map_err(|_| ()).and_then(|()| read_reply(&mut open.stdout)) {
            Ok(reply) => return reply,
            Err(()) => *control = None,
        }
    }
    drop(control);
    out(args)
}

pub fn ok(args: &[&str]) -> bool {
    out(args).is_some()
}

/// Like `ok`, with tmux's own error for the footer.
pub fn run(args: &[&str]) -> Result<(), String> {
    let output = command().args(literal(args)).output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Several commands as one call: quicker than one process each, and tmux stops at the first that fails.
pub fn chain(commands: &[Vec<String>]) -> Result<(), String> {
    if commands.is_empty() {
        return Ok(());
    }
    let args: Vec<&str> = commands.iter().flat_map(|c| c.iter().map(String::as_str).chain([";"])).collect();
    run(&args)
}

/// `format` for `target`, or None when tmux has nothing for it. tmux answers a pane, window or session that's gone with
/// every format empty and no error, so an empty answer is taken as gone: nothing asked here (ids, sizes, folders) is
/// empty for one that's there.
pub fn display(target: &str, format: &str) -> Option<String> {
    query(&["display", "-p", "-t", target, format]).filter(|answer| !answer.trim().is_empty())
}

pub fn pane_command(pane: &str) -> Option<String> {
    display(pane, "#{pane_current_command}")
}

pub fn option(name: &str) -> Option<String> {
    out(&["show", "-gqv", name]).filter(|value| !value.is_empty())
}

pub fn capture(pane: &str, flags: &[&str]) -> Option<String> {
    let mut args = vec!["capture-pane", "-p", "-t", pane];
    args.extend_from_slice(flags);
    query(&args)
}

/// The screens of several panes from one call, each under a line of its own that no screen will hold: printable (tmux
/// 3.4 and 3.5 print a control character as "\001") and made new for each call. tmux drops what follows a pane that's
/// gone, so a pane missing here is for the caller to capture alone.
pub fn captures(panes: &[&str], flags: &[&str]) -> HashMap<String, String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |t| t.as_nanos());
    let marker = |i: usize| format!("agentic-screen-{nonce:x}-{}-{i}", std::process::id());
    let markers: Vec<String> = (0..panes.len()).map(marker).collect();
    let mut args = Vec::new();
    for (pane, marker) in panes.iter().zip(&markers) {
        args.extend(["display", "-p", marker.as_str(), ";", "capture-pane", "-p", "-t", pane]);
        args.extend_from_slice(flags);
        args.push(";");
    }
    let mut screens = HashMap::new();
    let Ok(output) = command().args(literal(&args)).output() else {
        return screens;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut current: Option<(&str, Vec<&str>)> = None;
    for line in text.lines() {
        if let Some(i) = markers.iter().position(|m| m == line) {
            if let Some((pane, lines)) = current.take() {
                screens.insert(pane.to_string(), lines.join("\n"));
            }
            current = Some((panes[i], Vec::new()));
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    // The last screen is whole only if tmux got through every command.
    if let Some((pane, lines)) = current.filter(|_| output.status.success()) {
        screens.insert(pane.to_string(), lines.join("\n"));
    }
    screens
}

pub fn send_text(pane: &str, text: &str) -> bool {
    ok(&["send-keys", "-t", pane, "-l", text])
}

pub fn send_key(pane: &str, key: &str) -> bool {
    ok(&["send-keys", "-t", pane, key])
}

/// Types a command into a pane's shell like you would.
pub fn run_in(pane: &str, command: &str) -> bool {
    send_text(pane, command) && send_key(pane, "Enter")
}

/// How many jobs running now stay where you are: a count, so one finishing doesn't end another's.
static STAY: AtomicUsize = AtomicUsize::new(0);

/// Runs `work`, and with `on`, leaves your client where it is meanwhile: new windows open in the background, and `go`
/// does nothing.
pub fn staying<T>(on: bool, work: impl FnOnce() -> T) -> T {
    if !on {
        return work();
    }
    STAY.fetch_add(1, Ordering::Relaxed);
    let done = work();
    STAY.fetch_sub(1, Ordering::Relaxed);
    done
}

/// Takes your client to a pane.
pub fn go(pane: &str) {
    if STAY.load(Ordering::Relaxed) == 0 {
        ok(&["switch-client", "-t", pane]); // selects the pane too
    }
}

/// A new-window or split-window that gives its pane, in the background while staying.
pub fn open(args: &[&str]) -> Option<String> {
    let mut args = args.to_vec();
    if STAY.load(Ordering::Relaxed) > 0 {
        args.insert(1, "-d");
    }
    out(&args)
}

pub fn display_message(text: &str) {
    // Shown as written: a title or folder with # in it would otherwise be read as tmux formats and styles.
    ok(&["display-message", &text.replace('#', "##")]);
}

/// An exact pane id: an empty or partial tmux target could reach the current pane.
pub fn is_pane(id: &str) -> bool {
    id.strip_prefix('%')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// One shell argument; a path or message never becomes shell instructions.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// An XDG base folder: `$var` when it's set to something, else `home_dir` under your home (".config", ".local/share").
pub fn xdg(var: &str, home_dir: &str) -> PathBuf {
    std::env::var(var)
        .ok()
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| PathBuf::from(home()).join(home_dir), PathBuf::from)
}

/// `~/x` for `/home/me/x`.
pub fn tilde(path: &str) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// A folder's name as the lists show it.
pub fn folder_name(path: &str) -> String {
    if path == home() {
        "~".to_string()
    } else {
        path.rsplit('/').next().unwrap_or_default().to_string()
    }
}

pub fn untilde(path: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", home()),
        _ => path.to_string(),
    }
}

/// Where caches and exports go: $TMPDIR (private to you on macOS), else $XDG_RUNTIME_DIR (private to you on most Linux
/// systems), else /tmp, where `private_dir` keeps them to you. macOS's $TMPDIR ends in a slash; these paths are shown to
/// agents, so no "//".
pub fn temp_dir() -> String {
    let set = |name| std::env::var(name).ok().filter(|dir: &String| !dir.is_empty());
    set("TMPDIR")
        .or_else(|| set("XDG_RUNTIME_DIR"))
        .unwrap_or_else(|| "/tmp".into())
        .trim_end_matches('/')
        .to_string()
}

/// A folder only you can open, made or tightened to 0700: the caches and exports in it hold conversation titles and
/// transcripts, and on Linux they may sit in a shared /tmp. One that's someone else's, or a link, is refused: they made
/// it first to read what goes in.
pub fn private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return Err(error),
        _ => {}
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || uid().is_some_and(|me| meta.uid() != me) {
        let why = format!("{} isn't a folder of yours", dir.display());
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, why));
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Your user id, from `id -u`: std has no call for it.
fn uid() -> Option<u32> {
    static UID: OnceLock<Option<u32>> = OnceLock::new();
    *UID.get_or_init(|| {
        let output = std::process::Command::new("id").arg("-u").output().ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    })
}

pub fn history_dir() -> PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "board".into());
    PathBuf::from(format!("{}/agents-{user}-history", temp_dir()))
}

/// The session the board was opened in, by its id, which outlives the pane it was opened from (killed, swapped, or its
/// agent gone); that pane's only when there's no id to go by.
pub fn origin_session(origin_pane: &str) -> Option<String> {
    std::env::var("AGENTS_ORIGIN_SESSION")
        .ok()
        .filter(|id| !id.is_empty())
        .and_then(|id| display(&id, "#{session_id}"))
        .or_else(|| display(origin_pane, "#{session_id}"))
}

/// This tmux server's id (session ids repeat across servers, e.g. with `tmux -L`).
pub fn server_id() -> String {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let from_env = std::env::var("TMUX").ok().and_then(|t| t.split(',').nth(1).map(str::to_string));
        from_env
            .filter(|id| !id.is_empty())
            .or_else(|| out(&["display", "-p", "#{pid}"]))
            .unwrap_or_default()
    })
    .clone()
}

pub fn status_file() -> PathBuf {
    history_dir().join(format!("status-{}", server_id()))
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Written whole or not at all: other boards and the status bar read these files at any moment.
pub fn write_atomic(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        private_dir(dir)?;
    }
    // By process and by write: two threads of one board writing the same file mustn't share its half.
    static WRITES: AtomicUsize = AtomicUsize::new(0);
    let temp = path.with_extension(format!("new{}-{}", std::process::id(), WRITES.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_job_finishing_leaves_another_staying() {
        let inner = staying(true, || {
            staying(false, || ());
            STAY.load(Ordering::Relaxed)
        });
        assert_eq!(inner, 1, "the outer job still stays");
        assert_eq!(STAY.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn tmux_reads_names_and_text_as_written() {
        assert_eq!(literal(&["rename-window", "wip;", ";", "x"]), ["rename-window", "wip\\;", ";", "x"]);
        assert_eq!(name("PR #S done"), "PR ##S done");
    }

    #[test]
    fn a_private_dir_is_made_or_tightened_to_yours_alone() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("agentic-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mode = |dir: &std::path::Path| std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        let made = root.join("new");
        private_dir(&made).unwrap();
        assert_eq!(mode(&made), 0o700);
        let open = root.join("open");
        std::fs::create_dir_all(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&open).unwrap();
        assert_eq!(mode(&open), 0o700, "an existing one is tightened");
        let link = root.join("link");
        std::os::unix::fs::symlink(&open, &link).unwrap();
        assert!(private_dir(&link).is_err(), "a link is refused");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn several_screens_come_from_one_call_and_a_gone_pane_is_left_out() {
        let Ok(installed) = Command::new("tmux").arg("-V").output() else {
            return;
        };
        // The unit tests' server outlives a run: one another tmux started would answer for this one.
        let installed = String::from_utf8_lossy(&installed.stdout).trim().trim_start_matches("tmux ").to_string();
        if out(&["display", "-p", "#{version}"]).is_some_and(|running| running != installed) {
            let _ = out(&["kill-server"]);
        }
        let session = format!("captures-{}", std::process::id());
        let started = |text: &str| {
            let shell = format!("echo '{text}'; sleep 30");
            out(&[
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-s",
                &session,
                "-x",
                "40",
                "-y",
                "9",
                &shell,
            ])
            .or_else(|| out(&["split-window", "-d", "-P", "-F", "#{pane_id}", "-t", &session, &shell]))
        };
        let (Some(one), Some(two)) = (started("one; # %d"), started("two")) else {
            return;
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        let screens = captures(&[&one, &two], &[]);
        assert_eq!(screens.get(&one).map(|s| s.trim()), Some("one; # %d"));
        assert_eq!(screens.get(&two).map(|s| s.trim()), Some("two"));
        let screens = captures(&[&one, "%99999", &two], &[]);
        assert!(screens.contains_key(&one) && !screens.contains_key("%99999"));
        let _ = out(&["kill-session", "-t", &session]);
    }

    #[test]
    fn unit_tests_cannot_reach_your_tmux() {
        let command = command();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["-L", "agentic-unit-tests"]);
    }

    #[test]
    fn only_exact_pane_ids_count() {
        assert!(is_pane("%12"));
        for bad in ["", "%", "12", "%1a", "%1;kill"] {
            assert!(!is_pane(bad));
        }
    }

    #[test]
    fn replies_are_told_from_notifications_and_lookalike_lines() {
        let mut stream =
            "%begin 1 1 0\n%end 1 1 0\n%session-changed $1 a\n%begin 2 7 1\nscreen\n%end of a story\n%end 3 8 1\n%end 2 7 1\n".as_bytes();
        assert_eq!(read_reply(&mut stream), Ok(Some("screen\n%end of a story\n%end 3 8 1".to_string())));
        let mut stream = "%begin 2 8 1\nno such pane\n%error 2 8 1\n".as_bytes();
        assert_eq!(read_reply(&mut stream), Ok(None));
        let mut stream = "%begin 2 9 1\n%end 2 9 1\n".as_bytes();
        assert_eq!(read_reply(&mut stream), Ok(Some(String::new())));
        let mut stream = "%exit\n".as_bytes();
        assert_eq!(read_reply(&mut stream), Err(()));
        let mut stream = "%begin 2 9 1\ncut off".as_bytes();
        assert_eq!(read_reply(&mut stream), Err(()));
    }

    #[test]
    fn quoting_survives_quotes() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
