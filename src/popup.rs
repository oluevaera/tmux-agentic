//! Opening the board in a classic tmux popup. The popup is modal, so a second press of the root key reaches the board
//! (which closes on it) rather than tmux.
use crate::{config, tmux};

/// Popup size for a window, as "width height" of the popup's outer box (border included, which is what display-popup
/// takes): the shares config.toml gives, the height adjusted by one line so it splits evenly above and below.
fn geometry(width: i32, height: i32, wide: u16, high: u16) -> (i32, i32) {
    let tall = height * i32::from(high) / 100;
    (width * i32::from(wide) / 100, tall - (height - tall) % 2)
}

/// Binds the keys that open the board and go back as config.toml says, unbinding the ones bound before first: a key
/// changed or set to "none" leaves nothing behind. Notes which version of the file they come from, so a change to it is
/// noticed by whoever looks next: the status watch, or the board as it opens.
pub fn bind() {
    let keys = config::get().keys.clone();
    let words = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<String>>();
    let mut unbinds = Vec::new();
    for bound in tmux::option("@agentic-bound").unwrap_or_default().split_whitespace() {
        if let Some((table, key)) = bound.split_once(':') {
            unbinds.push(words(&["unbind", "-q", "-T", table, key]));
        }
    }
    let me = std::env::current_exe().map_or_else(|_| "agentic".to_string(), |p| p.to_string_lossy().into_owned());
    let mut binds = Vec::new();
    for (table, key, command) in [
        ("prefix", &keys.open, "toggle"),
        ("root", &keys.open_root, "toggle"),
        ("prefix", &keys.back, "back"),
    ] {
        if let Some(key) = key {
            let run = format!("{} {command}", tmux::shell_quote(&me));
            binds.push((format!("{table}:{key}"), words(&["bind", "-T", table, key, "run-shell", "-b", &run])));
        }
    }
    let notes = |bound: &[String]| {
        [
            words(&["set", "-g", "@agentic-bound", &bound.join(" ")]),
            words(&["set", "-g", "@agentic-config", &config::stamp()]),
        ]
    };
    // One call, quick enough for the board to bind as it opens; if tmux turns a key down, one at a time, so the rest bind.
    let all: Vec<String> = binds.iter().map(|(name, _)| name.clone()).collect();
    let together: Vec<Vec<String>> = unbinds
        .iter()
        .cloned()
        .chain(binds.iter().map(|(_, bind)| bind.clone()))
        .chain(notes(&all))
        .collect();
    if tmux::chain(&together).is_err() {
        let _ = tmux::chain(&unbinds);
        let bound: Vec<String> = binds
            .into_iter()
            .filter(|(_, bind)| tmux::chain(std::slice::from_ref(bind)).is_ok())
            .map(|(name, _)| name)
            .collect();
        let _ = tmux::chain(&notes(&bound));
    }
}

/// Binds the keys again if config.toml changed since they were bound, and says what's wrong with it.
pub fn follow_config() -> bool {
    follow(&tmux::option("@agentic-config").unwrap_or_default())
}

/// The same, told which version of the file the keys were bound from.
fn follow(bound_from: &str) -> bool {
    if bound_from == config::stamp() {
        return false;
    }
    config::reload();
    bind();
    if let Some(first) = config::warnings().first() {
        tmux::display_message(&format!("tmux-agentic: {first}"));
    }
    true
}

/// Opens the board. Every tmux call before the popup shows is time you wait, so there are two: one reads all it needs,
/// the other sets the session up and opens the popup.
pub fn toggle() -> i32 {
    // The session's own detach-on-destroy, unset if it has none: put back as it was once the board closes.
    let Some(info) = tmux::out(&[
        "display",
        "-p",
        "#{pane_id} #{session_id} #{window_width} #{window_height} #{client_height} #{window_id} #{status-position} #{@agentic-open} #{@agentic-config} #{prefix}",
        ";",
        "show",
        "-qv",
        "detach-on-destroy",
    ]) else {
        return 1;
    };
    let mut lines = info.lines();
    let (Some(first), kept) = (lines.next(), lines.next().unwrap_or_default().to_string()) else {
        return 1;
    };
    let f: Vec<&str> = first.split(' ').collect();
    let [
        origin,
        session,
        width,
        height,
        client_height,
        window,
        status_at,
        open_now,
        bound_from,
        prefix,
    ] = f[..]
    else {
        return 1;
    };
    follow(bound_from);
    let number = |s: &str| s.parse::<i32>().unwrap_or(0);
    let (width, height, client_height) = (number(width), number(height), number(client_height));
    let config = config::get();
    let (popup_width, popup_height) = geometry(width, height, config.board.width, config.board.height);
    // The status bar's lines, counted only when they sit above the window: below it, they push nothing down.
    let top = if status_at == "top" { client_height - height } else { 0 };
    // Centered on the window, not on the pane it was opened from (tmux's default). -x is the left column, -y one line below
    // the bottom border, both on the whole screen.
    let x = (width - popup_width) / 2;
    let y = top + (height - popup_height) / 2 + popup_height;
    let me = std::env::current_exe().map_or_else(|_| "agentic".to_string(), |p| p.to_string_lossy().into_owned());
    let env = [
        format!("AGENTS_ORIGIN={origin}"),
        format!("AGENTS_ORIGIN_SESSION={session}"),
        format!("AGENTS_ORIGIN_WINDOW={window}"),
        format!("AGENTS_CLOSE_KEY={}", config.keys.open_root.as_deref().unwrap_or("none")),
        // The prefix then the open key closes it too: the popup has the keys, so tmux never sees that press.
        format!("AGENTS_PREFIX={prefix}"),
        format!("AGENTS_OPEN_KEY={}", config.keys.open.as_deref().unwrap_or("none")),
    ];
    // The terminal's own background unless the theme asks for one, so it matches a transparent terminal (tmux 3.7+
    // defaults to a solid grey). Not "default": that inherits popup-style, which is that grey. Only on this popup:
    // setting popup-style would restyle everyone else's too.
    let background = format!("bg={}", config.background.map_or_else(|| "terminal".to_string(), crate::theme::tmux));
    let border = format!("{background},fg={}", crate::theme::tmux(config.theme.border));
    let (width, height, x, y) = (popup_width.to_string(), popup_height.to_string(), x.to_string(), y.to_string());
    // How many boards are open on this session, this one included: another may be open from a second client. The first
    // keeps the session's own detach-on-destroy for the last to put back; theirs is only the board's.
    let open_now = open_now.parse::<u32>().unwrap_or(0);
    let count = (open_now + 1).to_string();
    let keep = if kept.is_empty() { "unset" } else { kept.as_str() };
    let mut args = vec![
        // While the board is open you're looking at it, not at the pane under it: an agent finishing there is Done.
        "set",
        "-q",
        "-t",
        session,
        "@agentic-open",
        &count,
        ";",
    ];
    if open_now == 0 {
        args.extend(["set", "-q", "-t", session, "@agentic-kept", keep, ";"]);
    }
    args.extend([
        // A change in the board can close this session; you'd be thrown out of tmux instead of landing in another one.
        "set",
        "-q",
        "-t",
        session,
        "detach-on-destroy",
        "off",
        ";",
        "display-popup",
        "-E",
        "-t",
        origin,
        "-w",
        &width,
        "-h",
        &height,
        "-x",
        &x,
        "-y",
        &y,
        "-b",
        "rounded",
        "-s",
        &background,
        "-S",
        &border,
    ]);
    for pair in &env {
        args.extend(["-e", pair]);
    }
    let command = format!("{} inside", tmux::shell_quote(&me));
    args.push(&command);
    let status = tmux::command().args(&args).status().ok().and_then(|s| s.code()).unwrap_or(1);
    // The last board to close puts the session's own setting back; one closing before another only counts itself out.
    let still = tmux::display(session, "#{@agentic-open}")
        .and_then(|n| n.parse::<u32>().ok())
        .unwrap_or(1);
    if still > 1 {
        tmux::ok(&["set", "-q", "-t", session, "@agentic-open", &(still - 1).to_string()]);
    } else {
        let kept = tmux::display(session, "#{@agentic-kept}").unwrap_or_else(|| "unset".into());
        let put_back = match kept.as_str() {
            "unset" => vec!["set", "-qu", "-t", session, "detach-on-destroy"],
            value => vec!["set", "-q", "-t", session, "detach-on-destroy", value],
        };
        let forget = [
            "set",
            "-qu",
            "-t",
            session,
            "@agentic-open",
            ";",
            "set",
            "-qu",
            "-t",
            session,
            "@agentic-kept",
            ";",
        ];
        tmux::ok(&[&forget[..], &put_back].concat());
    }
    // The board says what went wrong itself and ends well; closing the terminal hangs it up (129), which is cancelling
    // it. Anything else is it stopping unexpectedly, or tmux not opening the popup: said here, as a status would have
    // run-shell put the pane in view-mode.
    if status != 0 && status != 129 {
        tmux::display_message(&format!("tmux-agentic: the board stopped unexpectedly (exit {status})"));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_splits_evenly_above_and_below() {
        assert_eq!(geometry(150, 50, 90, 80), (135, 40));
        assert_eq!(geometry(151, 49, 90, 80), (135, 39));
        let (_, tall) = geometry(100, 47, 90, 80);
        assert_eq!((47 - tall) % 2, 0);
        assert_eq!(geometry(200, 50, 50, 100), (100, 50));
    }
}
