//! The real binary in real popups: each test runs its own throwaway tmux servers, with a temporary HOME and TMPDIR and fake
//! agents, so it never reaches your running sessions. An outer server's pane attaches to the inner one, which is what
//! shows the popup and takes the keys.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

const BINARY: &str = env!("CARGO_BIN_EXE_agentic");

struct Live {
    root: PathBuf,
    inner: String,
    outer: String,
}

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok()
}

impl Live {
    /// Agents in folders under the test's root: (session, folder, tool).
    fn start(name: &str, agents: &[(&str, &str, &str)]) -> Live {
        let id = format!("{name}-{}", std::process::id());
        let root = env::temp_dir().join(format!("agentic-live-{id}"));
        let _ = fs::remove_dir_all(&root);
        for dir in ["bin", "home", "tmp", "fake", "work"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        let examples = Path::new(BINARY).parent().unwrap().join("examples/fake_agent");
        for tool in ["claude", "codex", "opencode"] {
            fs::copy(&examples, root.join("bin").join(tool)).unwrap();
        }
        let live = Live {
            inner: format!("ag-in-{id}"),
            outer: format!("ag-out-{id}"),
            root,
        };
        let config = live.root.join("conf");
        let other = live.root.join("other");
        let other = other.display();
        fs::write(
            &config,
            format!(
                // The second command stands for the rest of the bar: it logs each run.
                "set -g default-shell /bin/sh\nset -g status-interval 60\nset -g mouse on\nset -g status-right '#({BINARY} status --watch)#(date >> {other})'\nset -g status-right-length 100\n\
                 bind -T root M-a run-shell -b '{BINARY} toggle'\nbind b run-shell -b '{BINARY} back'\n"
            ),
        )
        .unwrap();
        let conf = config.to_string_lossy().into_owned();
        for (i, (session, folder, tool)) in agents.iter().enumerate() {
            let path = live.folder(folder);
            // exec: the agent is the pane's process whatever sh is (dash, unlike bash, would stay in front of it).
            let command = format!("exec {}", live.root.join("bin").join(tool).display());
            let cwd = path.to_string_lossy().into_owned();
            let at_first = [
                "-f",
                &conf,
                "new-session",
                "-d",
                "-s",
                session,
                "-x",
                "120",
                "-y",
                "45",
                "-c",
                &cwd,
                &command,
            ];
            let next = ["new-session", "-d", "-s", session, "-c", &cwd, &command];
            let exists = i > 0 && live.has_session(session);
            if exists {
                live.inner(&["new-window", "-t", &format!("{session}:"), "-c", &cwd, &command]);
            } else if i == 0 {
                live.inner(&at_first);
            } else {
                live.inner(&next);
            }
        }
        let first = agents[0].0;
        let attach = format!("tmux -L {} attach -t {first}", live.inner);
        live.outer(&["-f", "/dev/null", "new-session", "-d", "-s", "o", "-x", "120", "-y", "45", &attach]);
        for (_, folder, _) in agents {
            live.wait_until("the agent to start", || live.fake(folder, "args").exists());
        }
        // A title of its own, as a real agent sets: tmux's default is the machine's name, which on some (CI's macOS)
        // is long enough to cut the board's messages short.
        for line in live.inner(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]).lines() {
            if let Some((pane, path)) = line.split_once(' ') {
                let folder = Path::new(path).file_name().unwrap_or_default().to_string_lossy().into_owned();
                live.inner(&["select-pane", "-t", pane, "-T", &folder]);
            }
        }
        // The terminal attached, as you'd be: until then windows have their detached size.
        live.wait_until("the terminal to attach", || {
            !live.inner(&["list-clients", "-F", "#{client_name}"]).trim().is_empty()
        });
        live
    }

    fn command(&self, socket: &str, args: &[&str]) -> Output {
        Command::new("tmux")
            .arg("-L")
            .arg(socket)
            .args(args)
            .env("HOME", self.root.join("home"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("USER", "t")
            .env("FAKE_DIR", self.root.join("fake"))
            .env("PATH", format!("{}:{}", self.root.join("bin").display(), env::var("PATH").unwrap()))
            .env("AGENTS_HISTORY_HIDE", "")
            .env_remove("TMUX")
            // tmux takes it as the current pane: run from inside tmux, it would name a pane of this server by chance.
            .env_remove("TMUX_PANE")
            .env_remove("AGENTS")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_STATE_HOME")
            .output()
            .unwrap()
    }

    fn inner(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.command(&self.inner, args).stdout).into_owned()
    }

    fn outer(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.command(&self.outer, args).stdout).into_owned()
    }

    fn has_session(&self, name: &str) -> bool {
        self.command(&self.inner, &["has-session", "-t", &format!("={name}")]).status.success()
    }

    fn folder(&self, name: &str) -> PathBuf {
        let path = self.root.join("work").join(name);
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn fake(&self, folder: &str, ending: &str) -> PathBuf {
        self.root.join("fake").join(format!("{folder}.{ending}"))
    }

    /// What the agent in `folder` shows.
    fn show(&self, folder: &str, screen: &str) {
        fs::write(self.fake(folder, "screen"), screen).unwrap();
    }

    /// Saves config.toml, as you would.
    fn config(&self, text: &str) {
        let dir = self.root.join("home/.config/tmux-agentic");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), text).unwrap();
    }

    fn typed(&self, folder: &str) -> String {
        fs::read_to_string(self.fake(folder, "typed")).unwrap_or_default()
    }

    fn args(&self, folder: &str) -> String {
        fs::read_to_string(self.fake(folder, "args")).unwrap_or_default()
    }

    /// What the terminal shows, popup and all.
    fn screen(&self) -> String {
        self.outer(&["capture-pane", "-p", "-t", "o"])
    }

    fn keys(&self, keys: &[&str]) {
        for key in keys {
            self.outer(&["send-keys", "-t", "o", key]);
            thread::sleep(Duration::from_millis(60));
        }
    }

    fn wait_until(&self, what: &str, done: impl Fn() -> bool) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(8) {
            if done() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("never happened: {what}\n{}", self.screen());
    }

    fn shows(&self, text: &str) {
        self.wait_until(&format!("shows {text}"), || self.screen().contains(text));
    }

    fn gone(&self, text: &str) {
        self.wait_until(&format!("{text} gone"), || !self.screen().contains(text));
    }

    fn open(&self) {
        self.keys(&["M-a"]);
        self.shows("│  › "); // a row under the cursor: keys pressed before there are rows do nothing
    }

    /// Whether every client is in this session, the board's own connection included.
    fn clients_on(&self, session: &str) -> bool {
        let clients = self.inner(&["list-clients", "-F", "#{session_name}"]);
        !clients.is_empty() && clients.lines().all(|name| name == session)
    }

    fn panes(&self) -> String {
        self.inner(&[
            "list-panes",
            "-a",
            "-F",
            "#{session_name} #{window_index} #{pane_current_command} #{pane_current_path}",
        ])
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.command(&self.outer, &["kill-server"]);
        self.command(&self.inner, &["kill-server"]);
        for socket in [&self.outer, &self.inner] {
            let _ = fs::remove_file(format!(
                "/tmp/tmux-{}/{socket}",
                String::from_utf8_lossy(&Command::new("id").arg("-u").output().unwrap().stdout).trim()
            ));
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

macro_rules! need_tmux {
    () => {
        if !tmux_available() {
            eprintln!("skipped: no tmux");
            return;
        }
    };
}

#[test]
fn lists_agents_with_their_state_and_counts_them_in_the_status_bar() {
    need_tmux!();
    let live = Live::start(
        "list",
        &[("main", "api", "claude"), ("main", "web", "codex"), ("other", "docs", "claude")],
    );
    live.show("api", "✻ Thinking… (3s · esc to interrupt)\n\n❯ ");
    live.show("web", "• Ran tests\n\n  enter to confirm · esc to cancel\n\n› ");
    live.show("docs", "⏺ All done.\n\n❯ ");
    live.shows("◆ 1 needs you ● 1 working");
    live.open();
    live.shows("Needs you           Ran tests");
    live.shows("Working");
    live.shows("All done.");
    live.keys(&["3"]);
    live.gone("Ran tests   ");
    live.keys(&["q"]);
    live.gone("History");
}

#[test]
fn the_counts_change_on_their_own_and_leave_the_rest_of_the_bar_alone() {
    need_tmux!();
    let live = Live::start("watch", &[("main", "api", "claude")]);
    let runs = || fs::read_to_string(live.root.join("other")).unwrap_or_default().lines().count();
    live.show("api", "✻ Thinking… (3s · esc to interrupt)\n\n❯ ");
    live.shows("● 1 working");
    thread::sleep(Duration::from_secs(1)); // the redraw that showed it also started the other command: let it land
    let settled = runs();
    thread::sleep(Duration::from_secs(4));
    assert_eq!(runs(), settled, "nothing changed, so nothing else ran");
    live.show("api", "(3s · esc to interrupt)\n\n  enter to confirm · esc to cancel\n\n❯ ");
    live.shows("◆ 1 needs you");
    assert!(runs() <= settled + 1, "one change, one run: {}", runs());
    live.show("api", "⏺ All done.\n\n❯ ");
    live.gone("◆ 1 needs you");
    // Its last output, then silence: the count still has to follow it, and it isn't in view here only if it's Done.
    live.wait_until("the count to clear", || !live.screen().contains("working"));
}

#[test]
fn keys_follow_the_config_file_as_it_is_saved() {
    need_tmux!();
    let live = Live::start("config", &[("main", "api", "claude")]);
    // The whole table: tmux 3.7c lists nothing for `list-keys -T root <key>`.
    let bound = |key: &str| {
        let table = live.inner(&["list-keys", "-T", "root"]);
        table.lines().any(|line| line.contains(&format!(" {key} ")) && line.contains("toggle"))
    };
    live.config("[keys]\nopen_root = \"M-z\"\n");
    live.wait_until("M-z to be bound", || bound("M-z"));
    live.keys(&["M-z"]);
    live.shows("│  › ");
    live.keys(&["M-z"]); // it closes the board too
    live.gone("│  › ");
    live.config("[keys]\nopen_root = \"none\"\n");
    live.wait_until("M-z to be let go", || !bound("M-z"));
}

#[test]
fn the_popup_takes_the_size_the_config_file_gives() {
    need_tmux!();
    let live = Live::start("configsize", &[("main", "api", "claude")]);
    live.config("[board]\nwidth = \"50%\"\n");
    live.open();
    let top = live
        .screen()
        .lines()
        .find(|line| line.contains('╭'))
        .unwrap_or_default()
        .trim()
        .to_string();
    assert_eq!(top.chars().count(), 60, "{top}");
    // The status line is at the bottom: the window is every line above it, and the popup sits in its middle.
    let screen = live.screen();
    let lines: Vec<&str> = screen.lines().collect();
    let top = lines.iter().position(|l| l.contains('╭')).unwrap();
    let bottom = lines.iter().position(|l| l.contains('╰')).unwrap();
    let window = live
        .inner(&["display", "-p", "-t", "main:", "#{window_height}"])
        .trim()
        .parse::<usize>()
        .unwrap();
    assert_eq!(top, window - 1 - bottom, "as much above as below:\n{screen}");
}

#[test]
fn the_board_reopens_on_its_last_layout_unless_one_is_set_and_always_on_all() {
    need_tmux!();
    let live = Live::start("configremember", &[("main", "api", "claude")]);
    // The tree heads each session with its count of agents; the list has no headings.
    let tree = || live.screen().contains("1 agent");
    live.open();
    live.shows("[All");
    assert!(!tree(), "the list the first time");
    live.keys(&["f"]);
    live.wait_until("the tree", tree);
    live.keys(&["0"]);
    live.shows("[History");
    live.keys(&["q"]);
    live.gone("│  › ");
    live.open();
    live.shows("[All");
    live.wait_until("the tree, remembered", tree);
    live.keys(&["q"]);
    live.gone("│  › ");
    live.config("[board]\nlayout = \"list\"\n");
    live.open();
    live.shows("[All");
    assert!(!tree(), "the file's layout, every time");
}

#[test]
fn the_config_file_chooses_the_agents_and_how_much_history() {
    need_tmux!();
    let live = Live::start("configagents", &[("main", "api", "claude"), ("main", "web", "codex")]);
    for i in 0..3 {
        seed_claude(&live, &format!("abcd1234-000{i}"), &format!("past{i}"), &format!("Old work {i}"));
    }
    live.config("[agents]\nenabled = [\"claude\"]\n[history]\nlimit = 2\n");
    live.open();
    live.shows("api");
    assert!(!live.screen().contains("web"), "codex isn't tracked");
    live.keys(&["0"]);
    live.shows("Old work");
    assert_eq!(live.screen().matches("Claude      past").count(), 2, "{}", live.screen());
}

#[test]
fn a_handoff_says_what_the_config_file_tells_it_to() {
    need_tmux!();
    let live = Live::start("confighandoff", &[("main", "api", "codex")]);
    seed_claude(&live, "abcd1234-5678", "past", "Old work");
    live.config("[handoff]\nprompt = \"Pick up {agent} {id} from {transcript} please\"\n");
    live.open();
    live.keys(&["0"]);
    live.shows("Old work");
    live.keys(&["Tab"]);
    live.shows("→ Codex");
    live.keys(&["Enter"]);
    live.gone("History");
    live.wait_until("the handoff prompt", || live.args("past").contains("Pick up claude abcd1234-5678 from"));
    assert!(live.args("past").contains("abcd1234-5678.jsonl' please"));
}

#[test]
fn the_status_bar_counts_what_the_config_file_says_as_it_is_saved() {
    need_tmux!();
    let live = Live::start("configstatus", &[("main", "api", "claude")]);
    live.show("api", "✻ Thinking… (3s · esc to interrupt)\n\n❯ ");
    live.shows("1 working");
    live.config("[status]\nshow = [\"needs-you\"]\n");
    live.gone("1 working");
    live.show("api", "⏺ Done\n\n❯ ");
    live.config("[status]\nshow = [\"idle\"]\n");
    live.shows("1 idle");
}

#[test]
fn board_keys_follow_the_config_file() {
    need_tmux!();
    let live = Live::start("configkeys", &[("main", "api", "claude"), ("main", "web", "claude")]);
    live.config("[board.keys]\nkill = \"z\"\ndown = [\"C-n\", \"Down\"]\n");
    live.open();
    live.keys(&["x"]); // no longer kill
    thread::sleep(Duration::from_millis(300));
    assert!(!live.screen().contains("Its tmux pane will close"));
    live.keys(&["z"]);
    live.shows("Its tmux pane will close");
    live.keys(&["Escape", "C-n", "z", "z"]);
    live.wait_until("one agent killed", || live.panes().matches(" claude ").count() == 1);
}

#[test]
fn the_popup_keeps_the_terminals_own_background() {
    need_tmux!();
    let live = Live::start("popupbg", &[("main", "api", "claude")]);
    live.inner(&["set", "-g", "popup-style", "bg=colour238"]); // tmux 3.7+ has a grey of its own here
    live.open();
    let colored = live.outer(&["capture-pane", "-p", "-e", "-t", "o"]);
    let top = colored.lines().find(|line| line.contains('╭')).unwrap_or_default();
    assert!(!top.contains("[48;"), "{top:?}");
}

#[test]
fn the_popup_can_have_a_background_of_its_own() {
    need_tmux!();
    let live = Live::start("themebg", &[("main", "api", "claude")]);
    live.config("[theme]\nname = \"catppuccin-mocha\"\nbackground = \"#1e1e2e\"\n");
    live.open();
    let colored = live.outer(&["capture-pane", "-p", "-e", "-t", "o"]);
    let top = colored.lines().find(|line| line.contains('╭')).unwrap_or_default();
    assert!(top.contains("48;2;30;30;46"), "#1e1e2e: {top:?}");
}

#[test]
fn a_theme_saved_while_the_board_is_open_shows_in_it() {
    need_tmux!();
    let live = Live::start("themelive", &[("main", "api", "claude")]);
    live.open();
    live.config("[theme]\nname = \"catppuccin-mocha\"\nbackground = \"#1e1e2e\"\n");
    live.shows("config.toml applied");
    live.wait_until("the background behind the list", || {
        live.outer(&["capture-pane", "-p", "-e", "-t", "o"]).contains("48;2;30;30;46")
    });
}

#[test]
fn a_mistake_in_the_config_file_shows_when_the_board_opens() {
    need_tmux!();
    let live = Live::start("configbad", &[("main", "api", "claude")]);
    live.config("[board]\nlayout = \"tre\"\n");
    live.open();
    live.shows("config.toml line 2: board.layout should be");
}

#[test]
fn enter_goes_to_an_agent_and_back_returns() {
    need_tmux!();
    let live = Live::start("enter", &[("main", "api", "claude"), ("other", "docs", "claude")]);
    live.show("docs", "⏺ Docs agent\n\n❯ ");
    live.open();
    live.shows("Docs agent");
    live.keys(&["/", "D", "o", "c", "s"]);
    live.shows("Search › Docs");
    live.keys(&["Enter"]);
    live.gone("History");
    live.shows("[other]");
    live.keys(&["C-b", "b"]);
    live.shows("[main]");
}

#[test]
fn marked_agents_move_together_as_columns_and_keep_running() {
    need_tmux!();
    let live = Live::start("group", &[("main", "api", "claude"), ("main", "web", "claude")]);
    live.open();
    live.shows("web");
    live.keys(&["Space", "k", "Space"]);
    live.shows("2 marked");
    live.keys(&["Enter"]);
    live.gone("History");
    let windows = || live.inner(&["list-windows", "-t", "main", "-F", "#{window_name} #{window_panes}"]);
    live.wait_until("one window of two panes", || windows().trim() == "Agentic 1 2");
    let lefts = live.inner(&["list-panes", "-t", "main", "-F", "#{pane_left}"]);
    assert_eq!(lefts.lines().collect::<std::collections::HashSet<_>>().len(), 2, "side by side: {lefts}");
    assert_eq!(live.panes().matches(" claude ").count(), 2);
}

#[test]
fn a_failed_move_in_the_list_keeps_the_board_open_and_marks_them_again() {
    need_tmux!();
    let live = Live::start("movefail", &[("main", "api", "claude"), ("main", "web", "claude")]);
    live.open();
    live.shows("web");
    live.keys(&["Space", "k", "Space"]);
    live.shows("2 marked");
    // One of them stops being an agent before Enter: nothing can move.
    let web = live.inner(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let web = web.lines().find(|l| l.ends_with("/web")).unwrap().split(' ').next().unwrap().to_string();
    live.inner(&["respawn-pane", "-k", "-t", &web, "sleep 900"]);
    live.keys(&["Enter"]);
    live.shows("✗ Agent in");
    live.shows("esc dismiss");
    live.shows("›▌  ✗"); // marked again, its row says so
    let colored = live.outer(&["capture-pane", "-p", "-e", "-t", "o"]);
    let row = colored.lines().find(|line| line.contains("✗") && line.contains("api")).unwrap();
    assert!(row.contains("48;2;31;44;64"), "on the marked band: {row:?}");
    assert!(live.screen().contains("History"), "still open: {}", live.screen());
}

#[test]
fn the_board_keeps_its_session_after_the_agent_it_was_opened_from_is_gone() {
    need_tmux!();
    let live = Live::start("origingone", &[("main", "api", "claude"), ("main", "web", "claude")]);
    live.inner(&["select-window", "-t", "main:0"]);
    live.open();
    live.shows("api");
    live.keys(&["x", "x"]); // the agent the board was opened from
    live.wait_until("it to be killed", || live.panes().matches(" claude ").count() == 1);
    live.keys(&["n"]);
    live.shows("↵ start"); // a new agent still has a session to go in
}

#[test]
fn x_needs_a_second_press_and_kills_only_that_agent() {
    need_tmux!();
    let live = Live::start("kill", &[("main", "api", "claude"), ("main", "web", "claude")]);
    live.open();
    live.keys(&["x"]);
    live.shows("Its tmux pane will close");
    assert_eq!(live.panes().lines().count(), 2);
    live.keys(&["Escape"]);
    live.gone("Its tmux pane will close");
    live.keys(&["x", "x"]);
    live.wait_until("the pane to close", || live.panes().lines().count() == 1);
    live.shows("Killed agent in");
}

#[test]
fn names_tmux_would_misread_are_kept_as_typed() {
    need_tmux!();
    let live = Live::start("oddnames", &[("main", "api", "claude")]);
    live.open();
    live.keys(&["f"]);
    live.shows("main  1 agent");
    live.keys(&["g", "r"]);
    live.shows("esc keep old name");
    // A leading - (an option to tmux), a # (a format) and a trailing ; (the end of a command). ";" is escaped for this
    // test's own send-keys.
    live.keys(&["-", "w", "i", "p", " ", "#", "1", " ", "P", "R", "\\;"]);
    live.keys(&["Enter"]);
    live.shows("Renamed session to: -wip #1 PR;");
    live.wait_until("the session named just so", || {
        live.inner(&["list-sessions", "-F", "#{session_name}"])
            .lines()
            .any(|name| name == "-wip #1 PR;")
    });
    live.keys(&["f"]); // the layout is remembered: leave it as it was
}

#[test]
fn headings_rename_straight_in_tmux() {
    need_tmux!();
    let live = Live::start("heading", &[("main", "api", "claude")]);
    live.open();
    live.keys(&["f"]);
    live.shows("main  1 agent");
    live.keys(&["g", "r"]);
    live.shows("esc keep old name"); // renamed in its row, nothing above the list
    assert!(!live.screen().contains("Rename"), "{}", live.screen());
    let colored = live.outer(&["capture-pane", "-p", "-e", "-t", "o"]);
    let row = colored.lines().find(|line| line.contains("1 agent")).unwrap();
    assert!(
        row.contains("48;2;26;31;41mmain"),
        "the old name, and only the name, on the field's dark blue: {row:?}"
    );
    live.keys(&["n", "e", "w"]);
    live.shows("new   1 agent"); // typed in its row
    let at = live.outer(&["display", "-p", "-t", "o", "#{cursor_x} #{cursor_y}"]);
    let screen = live.screen();
    let (y, line) = screen.lines().enumerate().find(|(_, l)| l.contains("new   1 agent")).unwrap();
    let x = line[..line.find("new").unwrap()].chars().count() + 3;
    assert_eq!(at.trim(), format!("{x} {y}"), "the terminal's cursor right after what's typed");
    assert!(live.has_session("main"));
    live.keys(&["Enter"]);
    live.shows("Renamed session to: new");
    assert!(live.has_session("new"));
    live.keys(&["f"]); // the layout is remembered: leave it as it was
}

#[test]
fn renaming_an_agent_types_it_only_once_idle_with_an_empty_box() {
    need_tmux!();
    let live = Live::start("name", &[("main", "api", "claude")]);
    live.show("api", "✻ Thinking… (3s · esc to interrupt)\n\n❯ ");
    live.open();
    live.shows("api           Working");
    live.keys(&["r"]);
    live.shows("Can't rename: the agent is running");
    live.show("api", "⏺ Done\n\n❯ ");
    // Done, if the status refresh gets there first: it finished while the board was open.
    live.wait_until("it to finish", || {
        ["api           Idle", "api           Done"].iter().any(|row| live.screen().contains(row))
    });
    live.keys(&["r"]);
    live.shows("esc keep old name");
    live.keys(&["f", "o", "o", "Enter"]);
    live.shows("Renamed to: foo");
    live.shows("foo                             Claude"); // its row too
    live.wait_until("the agent to get /rename", || live.typed("api").contains("/rename foo"));
    assert_eq!(live.inner(&["display", "-p", "-t", "main:", "#{pane_title}"]).trim(), "foo");
}

#[test]
fn a_rename_the_agent_never_gets_says_so_and_keeps_the_old_name() {
    need_tmux!();
    let live = Live::start("namelost", &[("main", "api", "claude")]);
    live.inner(&["select-pane", "-t", "main:", "-T", "old"]);
    live.inner(&["select-pane", "-t", "main:", "-d"]); // the pane takes no keys: /rename never arrives
    live.open();
    live.keys(&["r"]);
    live.shows("esc keep old name");
    live.keys(&["f", "o", "o", "Enter"]);
    live.shows("Not renamed");
    assert!(!live.screen().contains("Renamed to"));
    assert_eq!(live.inner(&["display", "-p", "-t", "main:", "#{pane_title}"]).trim(), "old");
}

#[test]
fn c_twice_sends_compact_and_refuses_a_draft() {
    need_tmux!();
    let live = Live::start("compact", &[("main", "api", "claude")]);
    live.open();
    live.keys(&["c"]);
    live.shows("c again");
    assert_eq!(live.typed("api"), "");
    live.keys(&["c"]);
    live.wait_until("/compact", || live.typed("api").contains("/compact"));
    live.shows("Asked Claude to compact");
    live.keys(&["q"]);
    live.gone("History");
    live.show("api", "❯ half a thought");
    live.wait_until("the draft on screen", || {
        live.inner(&["capture-pane", "-p", "-t", "main"]).contains("half a thought")
    });
    live.open();
    live.keys(&["c", "c"]);
    live.shows("Can't compact: its input box has text");
}

#[test]
fn n_starts_a_draft_in_the_tabbed_agent_and_typed_folder() {
    need_tmux!();
    let live = Live::start("draft", &[("main", "api", "claude")]);
    let other = live.folder("other");
    live.open();
    live.keys(&["n"]);
    live.shows("New agent");
    live.keys(&["Tab"]);
    live.shows("Codex      api");
    live.keys(&["o", "C-u"]);
    for c in other.to_string_lossy().chars() {
        let text = c.to_string();
        live.outer(&["send-keys", "-t", "o", "-l", &text]);
    }
    live.keys(&["Enter"]);
    live.shows("Codex      other");
    live.keys(&["Enter"]);
    live.gone("History");
    let shown = other.to_string_lossy().into_owned();
    live.wait_until("the agent in its folder", || live.panes().contains(&format!("codex {shown}")));
    assert!(live.inner(&["list-windows", "-t", "main", "-F", "#{window_name}"]).contains("Agentic 1"));
}

#[test]
fn n_on_an_agent_starts_the_draft_in_its_cli() {
    need_tmux!();
    let live = Live::start("draftcli", &[("main", "api", "codex")]);
    fs::create_dir_all(live.root.join("tmp/agents-t-history")).unwrap();
    fs::write(live.root.join("tmp/agents-t-history/last-agent"), "claude").unwrap();
    live.open();
    live.shows("Codex      api");
    live.keys(&["n"]);
    live.shows("New Codex agent");
}

#[test]
fn folder_typing_goes_in_and_up_and_escape_keeps_the_folder() {
    need_tmux!();
    let live = Live::start("folder", &[("main", "api", "claude")]);
    live.folder("api/inner");
    live.open();
    live.keys(&["n", "o"]);
    live.shows("set folder");
    live.shows("/api/ "); // and what's in it is listed from the start
    live.keys(&["Down", "Right"]);
    live.shows("/api/inner/ ");
    live.keys(&["Left", "Escape"]);
    live.gone("set folder");
    live.shows("Claude     api"); // the draft is still there with the folder it had
    live.keys(&["Escape"]);
    live.gone("New agent");
}

/// Each agent in main's window of its own, all but the first marked, the cursor on the first's window.
fn marked_after_the_first(name: &str, count: usize) -> Live {
    marked_after_the_first_with(name, count, "")
}

/// The same, with config.toml saved before the board opens.
fn marked_after_the_first_with(name: &str, count: usize, config: &str) -> Live {
    let folders: Vec<String> = (0..count).map(|i| format!("a{i}")).collect();
    let agents: Vec<(&str, &str, &str)> = folders.iter().map(|folder| ("main", folder.as_str(), "claude")).collect();
    let live = Live::start(name, &agents);
    if !config.is_empty() {
        live.config(config);
    }
    live.open();
    live.keys(&["f", "g", "j", "j", "j"]);
    for _ in 1..count {
        live.keys(&["j", "Space", "j"]);
    }
    live.shows(&format!("{} marked", count - 1));
    live.keys(&["g", "j"]);
    live.shows("↵ move ");
    live
}

#[test]
fn many_agents_fit_in_one_window_and_none_is_left_a_sliver() {
    need_tmux!();
    let live = marked_after_the_first("many", 8);
    live.keys(&["Enter"]);
    live.wait_until("eight panes in one window", || {
        live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"]).trim() == "8"
    });
    let widths = live.inner(&["list-panes", "-t", "main", "-F", "#{pane_width}"]);
    let narrowest = widths.lines().filter_map(|w| w.parse::<u32>().ok()).min();
    assert!(narrowest >= Some(10), "{widths}");
}

/// Two marked agents moved into a window that already has two panes: all four share it evenly, as config.toml's split says.
fn moved_agents_share_the_window(name: &str, split: &str) {
    let live = marked_after_the_first_with(name, 3, &format!("[board]\nsplit = \"{split}\"\n"));
    let window = live.inner(&["list-windows", "-t", "main", "-F", "#{window_id}"]);
    let window = window.lines().next().unwrap();
    live.inner(&["split-window", "-d", split_sizes(split).0, "-t", window, "sleep 9000"]);
    live.keys(&["Enter"]);
    live.shared_evenly(window, 4, split);
}

#[test]
fn agents_moved_side_by_side_share_the_window_evenly() {
    need_tmux!();
    moved_agents_share_the_window("shareh", "horizontal");
}

#[test]
fn agents_moved_stacked_share_the_window_evenly() {
    need_tmux!();
    moved_agents_share_the_window("sharev", "vertical");
}

/// Three agents in one window, as config.toml's split says, then one killed: the two left share its room evenly.
fn a_killed_agents_room_is_shared(name: &str, split: &str) {
    let live = marked_after_the_first_with(name, 3, &format!("[board]\nsplit = \"{split}\"\n"));
    let window = live.inner(&["list-windows", "-t", "main", "-F", "#{window_id}"]);
    let window = window.lines().next().unwrap().to_string();
    live.keys(&["Enter"]);
    live.shared_evenly(&window, 3, split);
    live.open();
    live.keys(&["/", "a", "1"]);
    live.shows("Search › a1");
    live.keys(&["Escape"]);
    live.gone("Search ›");
    live.keys(&["x", "x"]);
    live.shared_evenly(&window, 2, split);
}

#[test]
fn a_killed_agents_room_is_shared_side_by_side() {
    need_tmux!();
    a_killed_agents_room_is_shared("killh", "horizontal");
}

#[test]
fn a_killed_agents_room_is_shared_stacked() {
    need_tmux!();
    a_killed_agents_room_is_shared("killv", "vertical");
}

#[test]
fn grouping_leaves_nothing_behind() {
    need_tmux!();
    let live = Live::start("groupclean", &[("main", "api", "claude"), ("main", "web", "claude")]);
    live.open();
    live.shows("web");
    live.keys(&["Space", "k", "Space", "Enter"]);
    live.wait_until("one window with both", || {
        live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"]).trim() == "2"
    });
    assert!(!live.panes().contains("sleep"), "{}", live.panes());
}

#[test]
fn marked_agents_join_the_window_under_the_cursor() {
    need_tmux!();
    let live = Live::start(
        "towindow",
        &[("main", "api", "claude"), ("main", "web", "claude"), ("main", "docs", "claude")],
    );
    live.open();
    live.keys(&["f", "g", "j", "j", "Space", "j", "j", "Space", "j"]);
    live.shows("2 marked");
    live.shows("↵ move ");
    live.keys(&["Enter"]);
    live.wait_until("three panes in one window", || {
        live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"]).trim() == "3"
    });
    assert!(
        live.inner(&["list-windows", "-t", "main", "-F", "#{window_name}"]).trim() != "Agentic 1",
        "its own window, not a new one"
    );
    assert_eq!(live.panes().matches(" claude ").count(), 3, "{}", live.panes());
}

/// main: keep | api, web | docs; other: dest. Tree rows: main, its windows and agents, then other.
fn four_windows(name: &str) -> Live {
    let live = Live::start(
        name,
        &[
            ("main", "keep", "claude"),
            ("main", "api", "claude"),
            ("main", "docs", "claude"),
            ("other", "dest", "claude"),
        ],
    );
    let (cwd, agent) = (live.folder("web"), format!("exec {}", live.root.join("bin/claude").display()));
    live.inner(&["split-window", "-d", "-t", "main:1", "-c", &cwd.to_string_lossy(), &agent]);
    live.wait_until("the agent to start", || live.fake("web", "args").exists());
    live.inner(&["select-window", "-t", "main:0"]);
    live
}

#[test]
fn marked_agents_sent_to_a_session_go_together_whatever_window_they_came_from() {
    need_tmux!();
    let live = four_windows("tosession");
    live.open();
    live.shows("dest");
    live.keys(&["f", "g", "j", "j", "j", "j", "Space", "j", "Space", "j", "j", "Space", "j"]);
    live.shows("3 marked");
    live.shows("↵ move ");
    live.keys(&["Enter"]);
    let windows = || live.inner(&["list-windows", "-t", "other", "-F", "#{window_panes}"]);
    live.wait_until("one window of three beside its own", || {
        let mut sizes: Vec<String> = windows().lines().map(str::to_string).collect();
        sizes.sort();
        sizes == ["1", "3"]
    });
    assert_eq!(live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"]).trim(), "1");
}

#[test]
fn part_of_a_window_sent_to_a_session_leaves_the_rest_behind() {
    need_tmux!();
    let live = four_windows("partial");
    live.open();
    live.shows("dest");
    live.keys(&["f", "g", "j", "j", "j", "j", "j", "Space", "G", "k", "k"]);
    live.shows("1 marked");
    live.shows("↵ move ");
    live.keys(&["Enter"]);
    let names = || live.inner(&["list-windows", "-t", "other", "-F", "#{window_name}"]);
    live.wait_until("a new window for it", || names().contains("Agentic 1"));
    let sizes = live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"]);
    assert_eq!(sizes.lines().collect::<Vec<_>>(), ["1", "1", "1"], "{sizes}");
}

fn seed_claude(live: &Live, id: &str, folder: &str, title: &str) {
    let dir = live.root.join("home/.claude/projects/project");
    fs::create_dir_all(&dir).unwrap();
    let cwd = live.folder(folder);
    let lines = [
        format!(
            r#"{{"type":"user","cwd":"{}","message":{{"role":"user","content":"please fix it"}}}}"#,
            cwd.display()
        ),
        r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Fixed it for you."}]}}"#.to_string(),
        r#"{"type":"system","subtype":"turn_duration"}"#.to_string(),
        format!(r#"{{"aiTitle":"{title}"}}"#),
    ];
    fs::write(dir.join(format!("{id}.jsonl")), lines.join("\n") + "\n").unwrap();
}

#[test]
fn history_previews_and_resumes_a_conversation_in_its_folder() {
    need_tmux!();
    let live = Live::start("resume", &[("main", "api", "claude")]);
    seed_claude(&live, "abcd1234-5678", "past", "Old work");
    live.open();
    live.keys(&["0"]);
    live.shows("Old work");
    live.shows("please fix it");
    live.shows("Fixed it for you.");
    live.keys(&["Enter"]);
    live.gone("History");
    live.wait_until("the agent resumed", || live.args("past") == "--resume abcd1234-5678");
    assert!(live.panes().contains(&format!("claude {}", live.folder("past").display())));
}

#[test]
fn after_clear_a_resumed_pane_follows_its_new_conversation() {
    need_tmux!();
    let live = Live::start("cleared", &[("main", "api", "claude")]);
    seed_claude(&live, "abcd1234-5678", "past", "Old work");
    live.open();
    live.keys(&["0"]);
    live.shows("Old work");
    live.keys(&["Enter"]);
    live.gone("History");
    live.wait_until("it resumed", || live.args("past") == "--resume abcd1234-5678");
    // /clear: a new conversation in the same folder, a turn under way in it, and the pane named after it.
    let dir = live.root.join("home/.claude/projects/project");
    let cwd = live.folder("past");
    let lines = [
        format!(
            r#"{{"type":"user","promptSource":"typed","cwd":"{}","message":{{"role":"user","content":"next thing"}}}}"#,
            cwd.display()
        ),
        r#"{"aiTitle":"New work"}"#.to_string(),
    ];
    fs::write(dir.join("ffff0000-1111.jsonl"), lines.join("\n") + "\n").unwrap();
    let pane = live.inner(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let pane = pane
        .lines()
        .find(|l| l.ends_with(&*cwd.to_string_lossy()))
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .to_string();
    live.inner(&["select-pane", "-t", &pane, "-T", "New work"]);
    live.open();
    live.wait_until("the pane to show its new conversation's turn", || {
        live.screen().lines().any(|l| l.contains("New work") && l.contains("Working"))
    });
}

#[test]
fn a_marked_agent_and_a_conversation_open_together_in_one_window() {
    need_tmux!();
    let live = Live::start("mixed", &[("main", "api", "claude"), ("main", "web", "claude")]);
    live.inner(&["select-window", "-t", "main:0"]);
    seed_claude(&live, "abcd1234-5678", "past", "Old work");
    live.open();
    live.keys(&["Space"]);
    live.shows("1 marked");
    live.keys(&["0"]);
    live.shows("Old work");
    live.shows("↵ open together");
    live.keys(&["Enter"]);
    live.gone("History");
    live.wait_until("the conversation resumed", || live.args("past") == "--resume abcd1234-5678");
    let same_window = |folder: &str| {
        let path = live.folder(folder);
        live.inner(&["list-panes", "-a", "-F", "#{window_id} #{pane_current_path}"])
            .lines()
            .find(|line| line.ends_with(&*path.to_string_lossy()))
            .map(|line| line.split(' ').next().unwrap().to_string())
    };
    live.wait_until("the agent to join it", || {
        same_window("api").is_some() && same_window("api") == same_window("past")
    });
    assert_ne!(same_window("api"), same_window("web"), "the unmarked agent stays out of it");
    assert_eq!(live.panes().matches(" claude ").count(), 3, "{}", live.panes());
}

#[test]
fn tab_hands_a_conversation_to_another_agent_with_its_transcript() {
    need_tmux!();
    let live = Live::start("handoff", &[("main", "api", "codex")]);
    seed_claude(&live, "abcd1234-5678", "past", "Old work");
    live.open();
    live.keys(&["0"]);
    live.shows("Old work");
    live.keys(&["Tab"]);
    live.shows("→ Codex");
    live.shows("(Claude conversation, continued in Codex)");
    live.keys(&["Enter"]);
    live.gone("History");
    live.wait_until("the handoff prompt", || {
        live.args("past")
            .contains("Continue the user's task from claude conversation abcd1234-5678")
    });
    assert!(live.args("past").contains("abcd1234-5678.jsonl"));
    assert!(live.args("past").contains("Before changing anything"));
}

#[test]
fn a_conversation_whose_folder_is_gone_says_so_and_opens_nothing() {
    need_tmux!();
    let live = Live::start("gone", &[("main", "api", "claude")]);
    seed_claude(&live, "abcd1234-5678", "moved", "Old work");
    fs::remove_dir_all(live.folder("moved")).unwrap();
    live.open();
    live.keys(&["0"]);
    live.shows("Old work");
    live.keys(&["Enter"]);
    live.shows("Folder no longer exists");
    assert_eq!(
        live.inner(&["list-windows", "-a", "-F", "#{window_id}"]).lines().count(),
        1,
        "nothing opened"
    );
    live.shows("History");
}

#[test]
fn many_conversations_open_together_with_room_to_read_them() {
    need_tmux!();
    let live = Live::start("resumemany", &[("main", "api", "claude")]);
    for i in 0..8 {
        seed_claude(&live, &format!("abcd1234-000{i}"), &format!("past{i}"), &format!("Old work {i}"));
    }
    live.open();
    live.keys(&["0"]);
    live.shows("Old work");
    for _ in 0..8 {
        live.keys(&["Space", "j"]);
    }
    live.shows("8 marked");
    live.keys(&["Enter"]);
    live.gone("History");
    let widths = || live.inner(&["list-panes", "-a", "-F", "#{window_panes} #{pane_width}"]);
    live.wait_until("eight panes in one window", || widths().contains("8 "));
    let narrowest = widths().lines().filter_map(|l| l.strip_prefix("8 ")?.parse::<u32>().ok()).min();
    assert!(narrowest >= Some(20), "{}", widths());
}

#[test]
fn x_removes_a_conversation_to_the_trash() {
    need_tmux!();
    let live = Live::start("remove", &[("main", "api", "claude")]);
    seed_claude(&live, "abcd1234-5678", "past", "Old work");
    live.open();
    live.keys(&["0"]);
    live.shows("Old work");
    live.keys(&["x"]);
    live.shows("x again (it can be restored)");
    live.keys(&["x"]);
    live.shows("Moved to the Trash");
    live.gone("Old work");
    let trash = if cfg!(target_os = "macos") {
        "home/.Trash"
    } else {
        "home/.local/share/Trash/files"
    };
    assert!(live.root.join(trash).join("abcd1234-5678.jsonl").is_file());
}

#[test]
fn x_keeps_a_conversation_a_live_agent_may_have_open() {
    need_tmux!();
    let live = Live::start("removelive", &[("main", "past", "claude")]);
    seed_claude(&live, "abcd1234-5678", "past", "Old work");
    live.open();
    live.keys(&["0"]);
    live.shows("Old work");
    live.keys(&["x", "x"]);
    live.shows("Not removed");
    assert!(live.root.join("home/.claude/projects/project/abcd1234-5678.jsonl").is_file());
}

#[test]
fn work_finished_out_of_sight_shows_as_done_until_seen() {
    need_tmux!();
    let live = Live::start("done", &[("main", "api", "claude"), ("main", "web", "claude")]);
    live.inner(&["select-window", "-t", "main:1"]);
    live.show("api", "(3s · esc to interrupt)\n\n❯ ");
    live.shows("● 1 working");
    live.show("api", "⏺ Finished\n\n❯ ");
    live.shows("✓ 1 done");
    live.open();
    live.shows("Done");
    live.keys(&["q"]);
    live.gone("History");
    live.inner(&["select-window", "-t", "main:0"]);
    live.gone("✓ 1 done");
}

#[test]
fn the_open_board_counts_as_looking_away() {
    need_tmux!();
    let live = Live::start("away", &[("main", "api", "claude")]);
    live.config("[keys]\nopen_root = \"M-a\"\n");
    live.show("api", "(3s · esc to interrupt)\n\n❯ ");
    live.shows("● 1 working");
    live.open();
    assert_eq!(live.inner(&["show", "-t", "main", "-v", "@agentic-open"]).trim(), "1");
    live.show("api", "⏺ Finished\n\n❯ ");
    live.shows("✓ 1 done");
    live.keys(&["M-a"]); // a root key, when set, closes it too
    live.gone("History");
    live.wait_until("@agentic-open cleared", || {
        live.inner(&["show", "-t", "main", "-qv", "@agentic-open"]).trim().is_empty()
    });
}

#[test]
fn tabs_take_a_click_and_shift_arrows_reach_the_board() {
    need_tmux!();
    let live = Live::start("mouse", &[("main", "api", "claude"), ("main", "web", "claude")]);
    live.show("api", "  enter to confirm · esc to cancel\n\n❯ ");
    live.open();
    live.shows("api           Needs you");
    let screen = live.screen();
    let (y, line) = screen.lines().enumerate().find(|(_, line)| line.contains("All·")).unwrap();
    let x = line[..line.find("Needs you").unwrap()].chars().count();
    let at = format!("{};{}", x + 2, y + 1);
    live.outer(&["send-keys", "-t", "o", "-l", &format!("\x1b[<0;{at}M\x1b[<0;{at}m")]);
    live.shows("[Needs you·1]");
    live.wait_until("only that agent listed", || !live.screen().contains("web           Idle"));
}

#[test]
fn the_prefix_and_the_open_key_close_the_board_as_they_open_it() {
    need_tmux!();
    let live = Live::start("prefixclose", &[("main", "api", "claude")]);
    live.open();
    live.keys(&["C-b", "a"]);
    live.gone("History");
}

#[test]
fn polling_uses_one_connection_that_leaves_sizes_alone_and_closes_with_the_board() {
    need_tmux!();
    let live = Live::start("control", &[("main", "api", "claude")]);
    let size = "#{window_width}x#{window_height}";
    let before = live.inner(&["display", "-p", "-t", "main:", size]);
    live.open();
    let controls = || live.inner(&["list-clients", "-F", "#{client_control_mode}"]).matches('1').count();
    live.wait_until("the board's connection", || controls() == 1);
    assert_eq!(live.inner(&["display", "-p", "-t", "main:", size]), before);
    live.keys(&["q"]);
    live.gone("History");
    live.wait_until("the connection to close", || controls() == 0);
}

#[test]
fn losing_the_connection_keeps_the_board_working() {
    need_tmux!();
    let live = Live::start("lost", &[("main", "api", "claude")]);
    live.show("api", "first line\n\n❯ ");
    live.open();
    live.shows("first line");
    let clients = live.inner(&["list-clients", "-F", "#{client_control_mode} #{client_name}"]);
    let name = clients.lines().find_map(|line| line.strip_prefix("1 ")).expect("a control client");
    live.inner(&["detach-client", "-t", name]);
    live.show("api", "second line\n\n❯ ");
    live.shows("second line");
}

#[test]
fn tabbing_back_to_its_own_agent_drops_the_switch() {
    need_tmux!();
    let live = Live::start("tabback", &[("main", "api", "claude")]);
    live.inner(&["set-environment", "-g", "AGENTS", "claude codex"]);
    live.open();
    live.shows("↵ go to");
    live.keys(&["Tab"]);
    live.shows("↵ apply 1 change");
    live.shows("→ Codex");
    live.keys(&["Tab"]);
    live.shows("↵ go to");
    live.gone("→ Codex");
    live.keys(&["Tab"]);
    live.shows("→ Codex"); // and the cycle carries on
}

fn three_places(name: &str) -> Live {
    three_places_with(name, "")
}

/// The same, with config.toml saved before the board opens: it reads it as it opens.
fn three_places_with(name: &str, config: &str) -> Live {
    let live = Live::start(name, &[("main", "api", "claude"), ("main", "web", "claude"), ("other", "dest", "claude")]);
    if !config.is_empty() {
        live.config(config);
    }
    live.inner(&["select-window", "-t", "main:0"]);
    live.open();
    live.shows("dest");
    live.keys(&["f"]);
    live.shows("other");
    live
}

impl Live {
    /// The cursor onto the row with `text`, by searching for it.
    fn find(&self, text: &str) {
        self.keys(&["/"]);
        for c in text.chars() {
            self.outer(&["send-keys", "-t", "o", "-l", &c.to_string()]);
        }
        self.shows(&format!("Search › {text}"));
        self.keys(&["Escape"]);
        self.gone("Search ›");
    }
}

#[test]
fn a_draft_made_in_a_window_becomes_a_pane_there() {
    need_tmux!();
    let live = three_places("draftpane");
    live.find("web");
    live.keys(&["n"]);
    live.shows("new pane in main › 1");
    live.keys(&["Enter"]);
    live.wait_until("a second pane in that window", || {
        live.inner(&["list-panes", "-t", "main:1", "-F", "#{pane_id}"]).lines().count() == 2
    });
    assert_eq!(live.inner(&["list-windows", "-t", "main", "-F", "#{window_id}"]).lines().count(), 2);
}

/// A split as config.toml names it: tmux's flag, the size its panes share and the one each keeps whole.
fn split_sizes(split: &str) -> (&'static str, &'static str, &'static str) {
    if split == "vertical" {
        ("-v", "height", "width")
    } else {
        ("-h", "width", "height")
    }
}

impl Live {
    /// Waits for `count` panes in `window`, then checks they sit in line and share its room evenly.
    fn shared_evenly(&self, window: &str, count: usize, split: &str) {
        let (_, size, whole) = split_sizes(split);
        let sizes = |of: &str| -> Vec<i32> {
            let format = format!("#{{pane_{of}}}");
            self.inner(&["list-panes", "-t", window, "-F", &format])
                .lines()
                .filter_map(|n| n.parse().ok())
                .collect()
        };
        self.wait_until(&format!("{count} panes in {window}"), || sizes(size).len() == count);
        let shared = sizes(size);
        let (least, most) = (shared.iter().min().unwrap(), shared.iter().max().unwrap());
        assert!(most - least <= 1, "shared evenly: {shared:?}");
        let kept = sizes(whole);
        assert!(kept.iter().all(|n| *n == kept[0]), "in line: {kept:?}");
    }
}

/// A draft started in a window that already has two panes: the three share it evenly, as config.toml's split says.
fn a_third_pane_takes_a_third(name: &str, split: &str) {
    let live = three_places_with(name, &format!("[board]\nsplit = \"{split}\"\n"));
    live.inner(&["split-window", "-d", split_sizes(split).0, "-t", "main:1", "sleep 60"]);
    live.find("web");
    live.keys(&["n"]);
    live.shows("new pane in main › 1");
    live.keys(&["Enter"]);
    live.shared_evenly("main:1", 3, split);
}

#[test]
fn a_third_pane_side_by_side_takes_a_third() {
    need_tmux!();
    a_third_pane_takes_a_third("thirdh", "horizontal");
}

#[test]
fn a_third_pane_stacked_takes_a_third() {
    need_tmux!();
    a_third_pane_takes_a_third("thirdv", "vertical");
}

#[test]
fn a_draft_made_on_a_session_becomes_a_window() {
    need_tmux!();
    let live = three_places("draftwindow");
    live.keys(&["g", "n"]);
    live.shows("new window in main");
    live.keys(&["Enter"]);
    live.wait_until("a new window", || {
        live.inner(&["list-windows", "-t", "main", "-F", "#{window_name}"]).contains("Agentic 1")
    });
}

#[test]
fn a_draft_outside_every_session_makes_a_tmux_session() {
    need_tmux!();
    let live = three_places("draftsession");
    live.keys(&["n", "BTab", "BTab"]);
    live.shows("open in a new tmux session");
    live.shows("+ new tmux session");
    let colored = live.outer(&["capture-pane", "-p", "-e", "-t", "o"]);
    let row = colored.lines().find(|line| line.contains("+ new tmux session")).unwrap();
    assert!(
        row.contains("[94m") || row.contains(";94m"),
        "in the theme's bright blue, as staged rows are: {row:?}"
    );
    let screen = live.screen();
    let lines: Vec<&str> = screen.lines().collect();
    let at = lines.iter().position(|line| line.contains("+ new tmux session")).unwrap();
    assert!(lines[at + 1].contains("└─ + new window"), "{screen}");
    assert!(lines[at + 2].contains("└─ + New agent"), "the agent in it: {screen}");
    live.keys(&["Enter"]);
    live.wait_until("a third session", || live.inner(&["list-sessions"]).lines().count() == 3);
    let shown = live.folder("api").to_string_lossy().into_owned();
    live.wait_until("the agent running in it", || {
        live.panes().matches(&format!(" claude {shown}")).count() == 2
    });
}

#[test]
fn drafts_for_the_same_new_window_start_together_in_it() {
    need_tmux!();
    let live = three_places("drafttogether");
    live.keys(&["g", "n"]);
    live.shows("new window in main");
    live.keys(&["n", "Tab"]); // another, in Codex
    live.wait_until("two drafts", || live.screen().matches("+ New agent").count() == 2);
    live.shows("Codex");
    live.shows("↵ start all");
    let screen = live.screen();
    let lines: Vec<&str> = screen.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.contains("+ new window"))
        .expect("a new window to hold them");
    assert!(lines[at + 1].contains("├─ + New agent") && lines[at + 1].contains("Claude"), "{screen}");
    assert!(lines[at + 2].contains("└─ + New agent") && lines[at + 2].contains("Codex"), "{screen}");
    live.keys(&["Enter"]);
    live.wait_until("one new window", || {
        live.inner(&["list-windows", "-t", "main", "-F", "#{window_name}"]).contains("Agentic 1")
    });
    live.shared_evenly("main:Agentic 1", 2, "horizontal");
    live.wait_until("both agents running in it", || {
        let running = live.inner(&["list-panes", "-t", "main:Agentic 1", "-F", "#{pane_current_command}"]);
        running.lines().collect::<Vec<_>>() == ["claude", "codex"]
    });
    assert_eq!(live.inner(&["list-windows", "-t", "main"]).lines().count(), 3, "one window, not two");
}

#[test]
fn n_does_nothing_in_history() {
    need_tmux!();
    let live = Live::start("nohistory", &[("main", "api", "claude")]);
    seed_claude(&live, "abcd1234-5678", "past", "Old work");
    live.open();
    live.keys(&["0"]);
    live.shows("Old work");
    assert!(!live.screen().contains("n new"));
    live.keys(&["n"]);
    live.keys(&["1"]);
    live.shows("Idle");
    assert!(!live.screen().contains("New agent"));
}

fn type_folder(live: &Live, folder: &Path) {
    live.keys(&["C-u"]);
    for c in folder.to_string_lossy().chars() {
        live.outer(&["send-keys", "-t", "o", "-l", &c.to_string()]);
    }
}

#[test]
fn o_forwards_cd_to_an_idle_agent_and_keeps_it_running() {
    need_tmux!();
    let live = Live::start("cd", &[("main", "api", "claude")]);
    let other = live.folder("other");
    let pane = live.inner(&["display", "-p", "-t", "main", "#{pane_id}"]);
    live.open();
    live.keys(&["o"]);
    live.shows("choose folder");
    live.shows("/api/ "); // in the agent's own row
    let at = live.outer(&["display", "-p", "-t", "o", "#{cursor_x} #{cursor_y}"]);
    let screen = live.screen();
    let (y, line) = screen.lines().enumerate().find(|(_, l)| l.contains("/api/ ")).unwrap();
    let x = line[..line.find("/api/ ").unwrap()].chars().count() + "/api/".len();
    assert_eq!(
        at.trim(),
        format!("{x} {y}"),
        "the cursor right after the path, not at the end of its column"
    );
    assert!(!live.screen().contains("New agent"), "no row of its own");
    type_folder(&live, &other);
    live.shows("/other ");
    live.keys(&["Enter"]);
    live.shows("→ other");
    live.shows("↵ apply 1 change");
    assert_eq!(live.typed("api"), "", "staged, not sent yet");
    live.keys(&["Enter"]);
    live.wait_until("/cd to reach the agent", || {
        live.typed("api").contains(&format!("/cd {}", other.display()))
    });
    live.shows("Asked Claude to move to");
    live.gone("choose folder");
    live.shows("other         Idle"); // where it went, not where tmux thinks it is
    assert_eq!(live.inner(&["display", "-p", "-t", "main", "#{pane_id}"]), pane, "the same pane");
    assert_eq!(live.args("other"), "", "no new agent");
    let typed = live.typed("api");
    assert_eq!(typed.lines().filter(|line| !line.is_empty()).count(), 1, "only that: {typed}");
}

#[test]
fn saying_no_to_claudes_cd_keeps_the_folder_and_a_cd_of_your_own_shows() {
    need_tmux!();
    let live = Live::start("cdno", &[("main", "api", "claude")]);
    let (other, third) = (live.folder("other"), live.folder("third"));
    fs::write(live.root.join("fake/api.decline"), "").unwrap();
    live.open();
    live.keys(&["o"]);
    live.shows("/api/ ");
    type_folder(&live, &other);
    live.shows("/other ");
    live.keys(&["Enter"]);
    live.shows("→ other");
    live.keys(&["Enter"]);
    live.wait_until("/cd to reach the agent", || live.typed("api").contains("/cd "));
    live.shows("Asked Claude to move to");
    live.shows("api           Idle"); // it said it stays
    assert!(!live.screen().contains("other         Idle"), "{}", live.screen());
    live.keys(&["q"]);
    fs::remove_file(live.root.join("fake/api.decline")).unwrap();
    // Typed into Claude itself: the board never heard of it.
    live.inner(&["send-keys", "-t", "main", "-l", &format!("/cd {}", third.display())]);
    live.inner(&["send-keys", "-t", "main", "Enter"]);
    live.wait_until("the agent to move", || live.typed("api").contains("/third"));
    live.open();
    live.shows("third         Idle");
}

#[test]
fn an_agent_that_doesnt_move_itself_shows_where_we_sent_it() {
    need_tmux!();
    let live = Live::start("cdsent", &[("main", "api", "codex")]);
    let other = live.folder("other");
    live.open();
    live.keys(&["o"]);
    live.shows("/api/ ");
    type_folder(&live, &other);
    live.shows("/other ");
    live.keys(&["Enter"]);
    live.shows("→ other");
    live.keys(&["Enter"]);
    live.wait_until("/cd to reach the agent", || live.typed("api").contains("/cd "));
    live.shows("other         Idle");
}

#[test]
fn closing_doesnt_wait_for_an_agent_to_name_itself_and_it_still_finishes() {
    need_tmux!();
    let live = Live::start("renameq", &[("main", "api", "codex")]);
    live.open();
    live.keys(&["r"]);
    live.shows("esc keep old name");
    live.keys(&["BSpace", "Enter"]); // emptied: the agent picks the name
    live.wait_until("/rename", || live.typed("api").contains("/rename"));
    live.keys(&["q"]);
    live.wait_until("the board to close", || {
        live.inner(&["show", "-t", "main", "-qv", "@agentic-open"]).trim().is_empty()
    });
    live.show("api", "Suggested from this conversation\n\n❯ ");
    live.wait_until("the suggestion to be accepted", || live.typed("api").lines().count() == 2);
}

#[test]
fn a_failed_move_says_so_on_its_row_stays_staged_and_retries() {
    need_tmux!();
    let live = Live::start("cdfail", &[("main", "api", "codex")]);
    let other = live.folder("other");
    live.open();
    live.keys(&["o"]);
    live.shows("/api/ ");
    type_folder(&live, &other);
    live.shows("/other ");
    live.keys(&["Enter"]);
    live.shows("→ other");
    live.show("api", "✻ Thinking… (3s · esc to interrupt)\n\n❯ ");
    live.shows("  Working");
    live.keys(&["Enter"]);
    live.shows("✗ Codex can't move: the agent is");
    live.shows("Working             the agent is");
    live.shows("→ other");
    live.shows("↵ retry");
    live.shows("esc dismiss");
    assert_eq!(live.typed("api"), "", "nothing reached it");
    let screen = live.outer(&["capture-pane", "-p", "-e", "-t", "o"]);
    let band = screen.lines().find(|line| line.contains("esc dismiss")).unwrap();
    assert!(band.contains("[91m") && band.contains("[1;7m"), "the theme's own red, filled: {band:?}");
    live.show("api", "⏺ Finished\n\n❯ ");
    live.wait_until("it to finish", || !live.screen().contains("  Working"));
    live.keys(&["Enter"]);
    live.wait_until("the second try to reach it", || live.typed("api").contains("/cd "));
    live.shows("✓ Asked Codex to move to");
    live.gone("can't move");
}

#[test]
fn a_failures_band_goes_by_itself_and_its_row_keeps_it_until_dismissed() {
    need_tmux!();
    let live = Live::start("cdband", &[("main", "api", "codex")]);
    let other = live.folder("other");
    live.open();
    live.keys(&["o"]);
    live.shows("/api/ ");
    type_folder(&live, &other);
    live.shows("/other ");
    live.keys(&["Enter"]);
    live.shows("→ other");
    live.show("api", "✻ Thinking… (3s · esc to interrupt)\n\n❯ ");
    live.shows("  Working");
    live.keys(&["Enter"]);
    live.shows("✗ Codex can't move");
    let start = Instant::now();
    while live.screen().contains("✗ Codex can't move") {
        assert!(start.elapsed() < Duration::from_secs(14), "the band never went:\n{}", live.screen());
        thread::sleep(Duration::from_millis(100));
    }
    assert!(start.elapsed() > Duration::from_secs(6), "gone too soon");
    live.shows("↵ retry 1 change");
    live.shows("esc dismiss");
    live.shows("Working             the agent is");
    live.keys(&["Escape"]);
    live.gone("the agent is");
    live.shows("↵ apply 1 change");
    live.shows("→ other");
}

#[test]
fn claudes_transcript_decides_working_whatever_its_screen_shows() {
    need_tmux!();
    let live = known_agent("claudelog", &[]);
    live.open();
    live.shows("Old work");
    live.shows("  Idle");
    // A turn under way that the screen doesn't show, as while a tool runs quietly.
    let transcript = live.root.join("home/.claude/projects/project/abcd1234-5678.jsonl");
    let mut lines = fs::read_to_string(&transcript).unwrap();
    lines.push_str("{\"type\":\"user\",\"promptSource\":\"typed\",\"message\":{\"content\":\"more\"}}\n");
    fs::write(&transcript, &lines).unwrap();
    live.shows("  Working");
    lines.push_str("{\"type\":\"system\",\"subtype\":\"turn_duration\"}\n");
    fs::write(&transcript, &lines).unwrap();
    // Done if the status watch saw the turn too: the open board counts as looking away. Its window printed nothing, so
    // whether it looked depends on what the tmux version counts as activity.
    live.wait_until("the turn to be over", || {
        let screen = live.screen();
        screen.contains("  Idle") || screen.contains("  Done")
    });
}

#[test]
fn staged_changes_in_the_list_close_it_once_they_work_and_not_if_they_fail() {
    need_tmux!();
    let live = known_agent("listfail", &[("main", "api", "claude"), ("main", "web", "codex")]);
    live.open();
    live.shows("web");
    live.keys(&["Tab"]);
    live.shows("→ Codex");
    // Working, as Claude shows it and writes it: a prompt with no end yet.
    let transcript = live.root.join("home/.claude/projects/project/abcd1234-5678.jsonl");
    let mut lines = fs::read_to_string(&transcript).unwrap();
    lines.push_str("{\"type\":\"user\",\"promptSource\":\"typed\",\"message\":{\"content\":\"more\"}}\n");
    fs::write(&transcript, lines).unwrap();
    live.show("api", "✻ Thinking… (3s · esc to interrupt)\n\n❯ ");
    live.shows("  Working");
    live.keys(&["Enter"]);
    live.shows("Cannot transfer");
    live.shows("esc dismiss");
    live.keys(&["Escape"]);
    live.gone("Cannot transfer");
    live.shows("→ Codex");
}

#[test]
fn o_refuses_a_busy_agent_and_escape_leaves_an_idle_one_alone() {
    need_tmux!();
    let live = Live::start("restartno", &[("main", "api", "claude")]);
    live.show("api", "(3s · esc to interrupt)\n\n❯ ");
    live.open();
    live.shows("api           Working");
    live.keys(&["o"]);
    live.shows("Cannot move: the agent is running");
    assert!(!live.screen().contains("choose folder"));
    live.show("api", "⏺ Finished\n\n❯ ");
    live.wait_until("it to finish", || {
        ["api           Idle", "api           Done"].iter().any(|row| live.screen().contains(row))
    });
    live.keys(&["o"]);
    live.shows("choose folder");
    live.keys(&["Escape"]);
    live.gone("choose folder");
    live.shows("History"); // still open
    assert_eq!(live.typed("api"), "", "nothing sent");
    assert_eq!(live.panes().matches(" claude ").count(), 1, "{}", live.panes());
}

#[test]
fn escape_drops_a_chosen_folder_without_sending_it() {
    need_tmux!();
    let live = Live::start("cddrop", &[("main", "api", "claude")]);
    let other = live.folder("other");
    live.open();
    live.keys(&["o"]);
    live.shows("choose folder");
    type_folder(&live, &other);
    live.shows("/other ");
    live.keys(&["Enter"]);
    live.shows("→ other");
    live.keys(&["Escape"]);
    live.gone("→ other");
    live.shows("api           Idle");
    live.shows("History"); // still open
    assert_eq!(live.typed("api"), "", "nothing sent");
}

#[test]
fn escape_from_a_search_keeps_staged_changes() {
    need_tmux!();
    let live = Live::start("cdsearch", &[("main", "api", "claude")]);
    let other = live.folder("other");
    live.open();
    live.keys(&["o"]);
    live.shows("choose folder");
    type_folder(&live, &other);
    live.shows("/other ");
    live.keys(&["Enter"]);
    live.shows("→ other");
    live.keys(&["/"]);
    live.shows("Search ›");
    live.shows("esc back");
    live.keys(&["Escape"]);
    live.gone("Search ›");
    live.shows("→ other");
    live.shows("esc drop changes");
}

#[test]
fn closing_the_board_drops_staged_changes() {
    need_tmux!();
    let live = Live::start("qdrop", &[("main", "api", "claude")]);
    let other = live.folder("other");
    live.open();
    live.keys(&["o"]);
    live.shows("choose folder");
    type_folder(&live, &other);
    live.shows("/other ");
    live.keys(&["Enter"]);
    live.shows("→ other");
    live.keys(&["q"]);
    live.gone("History");
    assert_eq!(live.typed("api"), "", "nothing sent");
    live.keys(&["M-a"]);
    live.shows("History");
    assert!(!live.screen().contains("→ other"), "q means quit: {}", live.screen());
}

#[test]
fn several_agents_staged_all_move_on_one_enter() {
    need_tmux!();
    let live = Live::start(
        "staged",
        &[("main", "api", "claude"), ("main", "web", "claude"), ("main", "docs", "claude")],
    );
    let (one, two) = (live.folder("one"), live.folder("two"));
    live.open();
    live.keys(&["g", "o"]);
    live.shows("choose folder");
    type_folder(&live, &one);
    live.shows("/one ");
    live.keys(&["Enter"]);
    live.shows("→ one");
    live.keys(&["j"]); // free to move on, with the row still in the list
    live.keys(&["o"]);
    live.shows("choose folder");
    type_folder(&live, &two);
    live.shows("/two ");
    live.keys(&["Enter"]);
    live.shows("→ two");
    live.shows("→ one");
    live.shows("↵ apply 2 changes");
    live.keys(&["G"]);
    assert_eq!(live.typed("api") + &live.typed("web"), "", "nothing sent yet");
    live.keys(&["Enter"]);
    live.wait_until("both /cd commands", || {
        live.typed("api").contains(&format!("/cd {}", one.display())) && live.typed("web").contains(&format!("/cd {}", two.display()))
    });
    live.shows("Asked 2 agents to move");
    live.gone("→ one");
    assert_eq!(live.typed("docs"), "", "the one left alone");
}

#[test]
fn the_dropdown_leaves_every_row_where_it_was() {
    need_tmux!();
    let live = Live::start(
        "cover",
        &[("main", "api", "claude"), ("main", "web", "claude"), ("main", "docs", "claude")],
    );
    for (window, name) in [("main:0", "Alpha"), ("main:1", "Bravo"), ("main:2", "Charlie")] {
        live.inner(&["select-pane", "-t", window, "-T", name]);
    }
    live.folder("api/one");
    live.folder("api/two");
    live.open();
    live.shows("Charlie");
    live.keys(&["g"]);
    let line = |text: &str| live.screen().lines().position(|line| line.contains(text));
    let (bravo, charlie) = (line("Bravo"), line("Charlie"));
    live.keys(&["o"]);
    live.shows("/api/ ");
    live.shows("/api/two");
    assert_eq!((line("Bravo"), line("Charlie")), (bravo, charlie));
    live.keys(&["Down", "Down", "Right"]);
    live.shows("/api/two/ ");
}

/// An agent whose conversation the board can find: api's, titled Old work.
fn known_agent(name: &str, others: &[(&'static str, &'static str, &'static str)]) -> Live {
    let mut agents = vec![("main", "api", "claude")];
    agents.extend_from_slice(others);
    let live = Live::start(name, &agents);
    live.inner(&["set-environment", "-g", "AGENTS", "claude codex"]);
    live.inner(&["select-pane", "-t", "main:0", "-T", "Old work"]);
    live.inner(&["select-window", "-t", "main:0"]);
    seed_claude(&live, "abcd1234-5678", "api", "Old work");
    live
}

fn runs(live: &Live, tool: &str, folder: &str) -> bool {
    live.panes().contains(&format!(" {tool} {}", live.folder(folder).display()))
}

#[test]
fn a_staged_cli_replaces_the_agent_where_it_is_and_closes_the_original() {
    need_tmux!();
    let live = known_agent("swap", &[("main", "web", "claude")]);
    live.open();
    live.keys(&["Tab", "j"]); // stays staged when the cursor leaves
    live.shows("→ Codex");
    live.shows("↵ apply 1 change");
    assert!(runs(&live, "claude", "api"));
    live.keys(&["Enter"]);
    live.gone("History");
    live.wait_until("the new CLI, reading its conversation", || {
        runs(&live, "codex", "api") && live.args("api").contains("abcd1234-5678.jsonl")
    });
    live.wait_until("the original to close", || !runs(&live, "claude", "api"));
    assert!(runs(&live, "claude", "web"), "the other agent is left alone");
    assert_eq!(
        live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"])
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["1", "1"]
    );
}

#[test]
fn a_draft_keeps_any_agent_from_being_replaced() {
    need_tmux!();
    let live = Live::start("swapdraft", &[("main", "api", "opencode")]);
    live.inner(&["set-environment", "-g", "AGENTS", "opencode codex"]);
    let bar = "\x1b[34m┃\x1b[39m";
    let box_with = |text: &str| {
        [
            format!("  {bar}"),
            format!("  {bar}  {text}"),
            format!("  {bar}"),
            format!("  {bar}  \x1b[37mBuild\x1b[90m · \x1b[39mmodel"),
            "  ╹▀▀▀▀".to_string(),
            "      tab agents  ctrl+p commands".to_string(),
        ]
        .join("\n")
    };
    live.show("api", &box_with("\x1b[39mhalf a thought"));
    live.open();
    live.keys(&["Tab"]);
    live.shows("→ Codex");
    live.keys(&["Enter"]);
    live.shows("Cannot transfer: its input box has text");
    assert!(runs(&live, "opencode", "api"), "the agent and its draft stay");
}

#[test]
fn a_marked_agent_can_change_cli_on_its_way_to_a_window() {
    need_tmux!();
    let live = known_agent("swapmove", &[("main", "web", "claude"), ("main", "docs", "claude")]);
    live.open();
    live.shows("docs");
    live.keys(&["f", "g", "j", "j", "Space", "Tab"]); // marked, then a new CLI
    live.shows("1 change");
    live.shows("→ Codex");
    live.keys(&["j", "j", "j"]); // the window of docs
    live.shows("↵ apply 1 change, move 1 into");
    live.keys(&["Enter"]);
    live.wait_until("it to arrive there as the new CLI", || {
        let windows = live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"]);
        runs(&live, "codex", "api") && !runs(&live, "claude", "api") && windows.split_whitespace().collect::<Vec<_>>() == ["1", "2"]
    });
    live.wait_until("it to read its conversation", || live.args("api").contains("abcd1234-5678.jsonl"));
}

#[test]
fn when_the_new_cli_fails_to_start_the_later_steps_do_not_run() {
    need_tmux!();
    let live = known_agent("swapfail", &[("main", "web", "claude"), ("main", "docs", "claude")]);
    live.open();
    live.shows("docs");
    live.keys(&["f", "g", "j", "j", "Space", "Tab", "j", "j", "j"]);
    live.shows("↵ apply 1 change, move 1 into");
    // Passes the check, then fails to run. A script, not removed: your PATH may hold a real codex.
    fs::write(live.root.join("bin/codex"), "#!/bin/sh\nexit 1\n").unwrap();
    live.keys(&["Enter"]);
    let start = Instant::now();
    while !live.screen().contains("Stopped at step 1 of 2") {
        assert!(
            start.elapsed() < Duration::from_secs(25), // the new CLI gets ten seconds; slow CI machines need the rest
            "no report:
{}",
            live.screen()
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert!(live.screen().contains("Step 2 not"), "{}", live.screen());
    assert!(live.screen().contains("↵ retry"), "still staged: {}", live.screen());
    assert!(runs(&live, "claude", "api"), "the original is still open: {}", live.panes());
    assert_eq!(
        live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"])
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["1", "1", "1"],
        "nothing moved"
    );
}

#[test]
fn a_handoff_that_cannot_go_ahead_changes_nothing_and_stays_staged() {
    need_tmux!();
    let live = Live::start("swapno", &[("main", "api", "claude")]);
    live.inner(&["set-environment", "-g", "AGENTS", "claude codex"]);
    live.open();
    live.keys(&["Tab"]);
    live.shows("→ Codex");
    live.keys(&["Enter"]); // no saved conversation to find, and the fake agent can't export one
    live.shows("Could not export");
    live.shows("→ Codex");
    live.shows("History");
    assert!(runs(&live, "claude", "api"));
    assert_eq!(live.panes().lines().count(), 1, "{}", live.panes());
}

/// main: keep | api, web; tree rows: main, its windows and agents. The cursor ends on api, sharing a window with web.
fn shared_window(name: &str) -> Live {
    let live = Live::start(name, &[("main", "keep", "claude"), ("main", "api", "claude")]);
    let (cwd, agent) = (live.folder("web"), format!("exec {}", live.root.join("bin/claude").display()));
    live.inner(&["split-window", "-d", "-t", "main:1", "-c", &cwd.to_string_lossy(), &agent]);
    live.wait_until("the agent to start", || live.fake("web", "args").exists());
    live.inner(&["select-window", "-t", "main:0"]);
    live.open();
    live.shows("web");
    live.keys(&["f", "g", "j", "j", "j", "j"]);
    live
}

#[test]
fn agents_staged_for_new_windows_each_get_their_own() {
    need_tmux!();
    let live = shared_window("detachapart");
    live.keys(&["BTab"]);
    live.shows("open in a new tmux window");
    live.keys(&["g", "j", "j", "j", "j", "BTab"]); // the other agent of that window, not marked
    live.wait_until("both staged", || live.screen().matches("open in a new tmux window").count() >= 2);
    live.keys(&["Enter"]);
    let panes = || live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"]);
    live.wait_until("a window each", || panes().split_whitespace().collect::<Vec<_>>() == ["1", "1", "1"]);
}

#[test]
fn agents_staged_for_a_new_session_each_get_their_own() {
    need_tmux!();
    let live = shared_window("sessionsapart");
    live.keys(&["BTab", "BTab"]);
    live.shows("open in a new tmux session");
    live.keys(&["g", "j", "j", "j", "j", "BTab"]);
    live.shows("open in a new tmux window"); // first a window of its own, once more for a session
    live.keys(&["BTab"]);
    live.wait_until("both staged", || live.screen().matches("open in a new tmux session").count() >= 2);
    live.keys(&["Enter"]);
    live.wait_until("a session each", || live.has_session("api") && live.has_session("web"));
}

#[test]
fn shift_tab_on_a_marked_agent_takes_the_whole_selection_to_one_new_window() {
    need_tmux!();
    let live = Live::start(
        "marksapart",
        &[("main", "api", "claude"), ("main", "web", "claude"), ("main", "keep", "claude")],
    );
    live.open();
    live.shows("keep");
    live.keys(&["f", "g", "j", "j", "Space", "j", "j", "Space"]);
    live.shows("2 marked");
    live.keys(&["BTab"]);
    live.wait_until("both shown leaving", || live.screen().matches("open in a new tmux window").count() >= 2);
    live.keys(&["Enter"]);
    let sizes = || live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"]);
    live.wait_until("the two together in one new window", || {
        let mut s: Vec<String> = sizes().lines().map(str::to_string).collect();
        s.sort();
        s == ["1", "2"]
    });
}

#[test]
fn x_twice_on_a_marked_agent_kills_the_whole_selection() {
    need_tmux!();
    let live = Live::start(
        "killmarked",
        &[("main", "api", "claude"), ("main", "web", "claude"), ("main", "keep", "claude")],
    );
    live.open();
    live.shows("keep");
    live.keys(&["f", "g", "j", "j", "Space", "j", "j", "Space", "k", "k"]);
    live.shows("2 marked");
    live.keys(&["x"]);
    live.shows("Kill 2 agents?");
    live.keys(&["x"]);
    live.shows("Killed 2 agents");
    assert_eq!(live.panes().matches(" claude ").count(), 1, "{}", live.panes());
}

#[test]
fn shift_tab_stages_a_new_window_then_a_session_then_nothing() {
    need_tmux!();
    let live = shared_window("detachcycle");
    let indent = |level: &str| live.screen().lines().any(|line| line.contains(level));
    live.keys(&["BTab"]);
    live.shows("open in a new tmux window");
    live.wait_until("it to step out to a window's level", || indent("›   ├─ ○"));
    live.keys(&["BTab"]);
    live.shows("open in a new tmux session");
    live.wait_until("it to step out to a session's level", || indent("› ○"));
    live.keys(&["BTab"]);
    live.gone("open in a new");
    live.wait_until("it to step back in", || indent("›      ├─ ○"));
    assert_eq!(live.inner(&["list-sessions"]).lines().count(), 1);
}

#[test]
fn a_staged_new_window_takes_the_agent_out_of_its_window() {
    need_tmux!();
    let live = shared_window("detachwindow");
    live.keys(&["BTab"]);
    live.shows("open in a new tmux window");
    live.keys(&["Enter"]);
    let windows = || live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"]);
    live.wait_until("three windows of one", || {
        windows().split_whitespace().collect::<Vec<_>>() == ["1", "1", "1"]
    });
    assert_eq!(live.panes().matches(" claude ").count(), 3, "{}", live.panes());
}

#[test]
fn a_staged_new_session_takes_the_agent_there_and_leaves_no_placeholder() {
    need_tmux!();
    let live = shared_window("detachsession");
    live.keys(&["BTab", "BTab"]);
    live.shows("open in a new tmux session");
    live.keys(&["Enter"]);
    live.wait_until("a session named after its folder", || live.has_session("api"));
    live.wait_until("just the agent in it", || {
        live.inner(&["list-panes", "-s", "-t", "api", "-F", "#{pane_current_command}"]).trim() == "claude"
    });
    assert_eq!(live.panes().matches(" claude ").count(), 3, "{}", live.panes());
}

#[test]
fn a_whole_window_opens_in_a_new_session_with_everything_in_it() {
    need_tmux!();
    let live = Live::start("detachwhole", &[("main", "keep", "claude"), ("main", "api", "claude")]);
    let (cwd, agent) = (live.folder("web"), format!("exec {}", live.root.join("bin/claude").display()));
    live.inner(&["split-window", "-d", "-t", "main:1", "-c", &cwd.to_string_lossy(), &agent]);
    live.inner(&["split-window", "-d", "-t", "main:1"]); // a plain shell goes with the window
    live.inner(&["rename-window", "-t", "main:1", "Work"]);
    live.wait_until("the agent to start", || live.fake("web", "args").exists());
    live.inner(&["select-window", "-t", "main:0"]);
    live.open();
    live.shows("web");
    live.keys(&["f", "g", "j", "j", "j"]);
    live.shows("1 Work");
    live.keys(&["BTab"]);
    live.shows("open in a new tmux session");
    live.shows("↵ apply 1 change");
    live.keys(&["Enter"]);
    live.wait_until("a session named after its folder", || live.has_session("api"));
    let windows = || live.inner(&["list-windows", "-t", "api", "-F", "#{window_name} #{window_panes}"]);
    live.wait_until("just that window, whole", || windows().trim() == "Work 3");
    assert_eq!(live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"]).trim(), "1");
}

#[test]
fn an_agent_alone_in_its_window_goes_straight_to_a_new_session() {
    need_tmux!();
    let live = Live::start("detachalone", &[("main", "api", "claude")]);
    live.open();
    live.keys(&["f", "g", "j", "j", "BTab"]);
    live.shows("open in a new tmux session");
}

#[test]
fn in_the_tree_changes_happen_with_the_board_open_and_you_stay_put() {
    need_tmux!();
    let live = Live::start(
        "stay",
        &[("main", "api", "claude"), ("main", "web", "claude"), ("main", "docs", "claude")],
    );
    live.inner(&["select-window", "-t", "main:0"]);
    let here = || live.inner(&["list-clients", "-F", "#{session_name} #{window_index}"]);
    let before = here();
    live.open();
    live.shows("docs");
    live.keys(&["f", "g", "j", "j", "Space", "j", "j", "j"]); // api, marked; the cursor on the window of docs
    live.shows("↵ move ");
    live.keys(&["Enter"]);
    live.shows("Done");
    live.gone("1 marked");
    live.wait_until("the cursor to go with the agent", || {
        live.screen().lines().any(|line| line.contains("› ") && line.contains(" api "))
    });
    live.keys(&["j"]);
    live.gone("Done"); // with the next key
    live.wait_until("it to have moved", || {
        live.inner(&["list-windows", "-t", "main", "-F", "#{window_panes}"])
            .split_whitespace()
            .collect::<Vec<_>>()
            == ["1", "2"]
    });
    live.shows("History"); // still open, and still works
    live.keys(&["n"]); // and a new agent starts behind it
    live.shows("+ New agent");
    live.keys(&["Enter"]);
    live.shows("Done");
    live.wait_until("a pane for it", || live.panes().lines().count() == 4);
    live.shows("History");
    assert_eq!(here().trim().split(' ').next(), before.trim().split(' ').next(), "the same session");
    live.keys(&["q"]);
    live.gone("History");
}

#[test]
fn enter_on_an_agent_in_the_tree_still_goes_to_it() {
    need_tmux!();
    let live = Live::start("treego", &[("main", "api", "claude"), ("other", "docs", "claude")]);
    live.open();
    live.shows("docs");
    live.keys(&["f", "G", "Enter"]);
    live.gone("History");
    live.shows("[other]");
}

#[test]
fn agents_can_go_to_a_window_or_session_without_agents() {
    need_tmux!();
    let live = Live::start("spare", &[("main", "api", "claude"), ("main", "web", "claude")]);
    live.inner(&["new-window", "-d", "-t", "main:", "-n", "Plain"]);
    live.inner(&["new-session", "-d", "-s", "blank", "-n", "Spare"]);
    live.inner(&["select-window", "-t", "main:0"]);
    live.open();
    live.keys(&["f"]);
    live.shows("2 Plain");
    live.keys(&["g", "j", "j", "Space", "j", "j", "j"]); // api marked; the cursor on Plain
    live.shows("↵ move ");
    live.keys(&["Enter"]);
    live.shows("Done");
    let panes = |target: &str| live.inner(&["list-panes", "-s", "-t", target, "-F", "#{pane_id}"]).lines().count();
    live.wait_until("it to join the plain window", || {
        live.inner(&["list-panes", "-t", "main:2", "-F", "#{pane_id}"]).lines().count() == 2
    });
    live.shows(" api ");
    live.keys(&["Space", "G", "k"]); // the same agent marked; the cursor on the session
    live.shows("blank  0 agents");
    live.shows("↵ move ");
    live.keys(&["Enter"]);
    live.wait_until("it to arrive in that session", || panes("blank") == 2);
}

#[test]
fn killing_the_only_agent_on_the_server_leaves_tmux_running() {
    need_tmux!();
    let live = Live::start("lastkill", &[("main", "api", "claude")]);
    live.open();
    live.keys(&["x", "x"]);
    live.shows("left a shell");
    assert!(live.has_session("main"), "the server is still there");
    live.wait_until("a shell where the agent was", || {
        !live.panes().contains(" claude ") && live.panes().lines().count() == 1
    });
    live.shows("History");
}

#[test]
fn killing_the_last_agent_of_your_session_takes_you_to_another() {
    need_tmux!();
    let live = Live::start("sessionkill", &[("main", "api", "claude"), ("other", "docs", "claude")]);
    live.open();
    live.shows("docs");
    live.keys(&["/", "a", "p", "i"]);
    live.shows("Search › api");
    live.keys(&["Escape"]);
    live.gone("Search ›");
    live.keys(&["x", "x"]);
    live.wait_until("your session to close", || !live.has_session("main"));
    live.wait_until("you to land in the other one", || live.clients_on("other"));
}

#[test]
fn moving_the_last_window_out_of_your_session_keeps_you_attached() {
    need_tmux!();
    let live = Live::start("lastmove", &[("main", "api", "claude"), ("other", "docs", "claude")]);
    live.open();
    live.shows("docs");
    live.keys(&["f", "g", "j", "j", "Space", "j"]); // api marked; the cursor on the session other
    live.shows("↵ move ");
    live.keys(&["Enter"]);
    live.wait_until("it to arrive", || {
        live.inner(&["list-panes", "-s", "-t", "other", "-F", "#{pane_id}"]).lines().count() == 2
    });
    live.wait_until("your session to close", || !live.has_session("main"));
    live.wait_until("you to land where it went", || live.clients_on("other"));
}

#[test]
fn a_new_session_for_the_only_window_of_your_session_keeps_you_attached() {
    need_tmux!();
    let live = Live::start("lastnew", &[("main", "api", "claude")]);
    live.open();
    live.keys(&["f", "g", "j", "j", "BTab"]);
    live.shows("open in a new tmux session");
    live.keys(&["Enter"]);
    live.wait_until("the new session", || live.has_session("api"));
    live.wait_until("you in it", || live.clients_on("api"));
}

#[test]
fn your_detach_setting_is_back_once_the_board_closes() {
    need_tmux!();
    let live = Live::start("detachopt", &[("main", "api", "claude")]);
    live.inner(&["set", "-t", "main", "detach-on-destroy", "no-detached"]);
    live.open();
    assert_eq!(live.inner(&["show", "-qv", "-t", "main", "detach-on-destroy"]).trim(), "off");
    live.keys(&["q"]);
    live.wait_until("it to be restored", || {
        live.inner(&["show", "-qv", "-t", "main", "detach-on-destroy"]).trim() == "no-detached"
    });
}

#[test]
fn a_long_screen_keeps_up_with_the_newest_output() {
    need_tmux!();
    let live = Live::start("preview", &[("main", "api", "claude")]);
    live.show("api", "first line\n\n❯ ");
    live.open();
    live.shows("first line");
    live.show("api", "second line\n\n❯ ");
    live.shows("second line");
    live.gone("first line");
}
