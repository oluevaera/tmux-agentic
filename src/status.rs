//! The status bar's counts, the Done state behind them, and going back.
use crate::agents::Registry;
use crate::detect::{self, State};
use crate::theme::Palette;
use crate::{config, popup, tmux};
use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

/// Done: an agent that went from working to idle while its pane wasn't in view stays done until its pane is in view, or it
/// works again.
fn settled(state: State, before: Option<State>, looking: bool) -> State {
    match state {
        State::Done if looking => State::Idle,
        State::Idle if !looking && matches!(before, Some(State::Running | State::Done)) => State::Done,
        other => other,
    }
}

/// The bar's counts of the states config.toml has it show, in its theme's colors.
fn counts(states: &[State], show: &[State], palette: &Palette) -> String {
    let count = |state| states.iter().filter(|s| **s == state).count();
    let mut out = String::new();
    for (state, color, label) in [
        (State::Waiting, palette.needs_you, "needs you"),
        (State::Done, palette.done, "done"),
        (State::Running, palette.working, "working"),
        (State::Idle, palette.idle, "idle"),
    ] {
        if count(state) > 0 && show.contains(&state) {
            out.push_str(&format!("#[fg={}]{} {} {label} ", crate::theme::tmux(color), state.dot(), count(state)));
        }
    }
    out
}

/// Whether the pane is what you're looking at.
const LOOKING: &str = "#{&&:#{pane_active},#{&&:#{window_active},#{&&:#{session_attached},#{?@agentic-open,0,1}}}}";

/// Turns what's on each agent's screen into its state, Done included, and the counts for the status bar.
fn settle(agents: Vec<(String, State, bool)>) -> Option<String> {
    let cache = tmux::status_file();
    let saved = std::fs::read_to_string(&cache).unwrap_or_default();
    let before: HashMap<&str, State> = saved
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter_map(|(id, s)| Some((id, State::parse(s)?)))
        .collect();
    let states: Vec<(String, State)> = agents
        .into_iter()
        .map(|(id, state, looking)| {
            let state = settled(state, before.get(id.as_str()).copied(), looking);
            (id, state)
        })
        .collect();
    let lines: Vec<String> = states.iter().map(|(id, state)| format!("{id}\t{}", state.key())).collect();
    let text = lines.join("\n");
    if text != saved {
        tmux::write_atomic(&cache, &text).ok()?;
    }
    // Off, nothing is counted, but the states above are still saved: they're what makes an agent Done.
    let config = crate::config::get();
    let show: &[State] = if config.status.enabled { &config.status.show } else { &[] };
    Some(counts(
        &states.into_iter().map(|(_, state)| state).collect::<Vec<_>>(),
        show,
        &config.theme,
    ))
}

pub fn run(agents: &Registry) -> Option<()> {
    let format = format!("#{{pane_id}}\t{LOOKING}");
    let snapshot = tmux::out(&["list-panes", "-a", "-F", &format])?;
    // A window linked into several sessions lists its panes once for each: in view if in view in any.
    let mut looking: HashMap<&str, bool> = HashMap::new();
    for (id, on) in snapshot.lines().filter_map(|l| l.split_once('\t')) {
        *looking.entry(id).or_default() |= on == "1";
    }
    let panes = detect::panes(agents, false)?;
    let found = panes.into_iter().map(|p| {
        let seen = looking.get(p.id.as_str()).copied().unwrap_or(false);
        (p.id, p.state, seen)
    });
    print!("{}", settle(found.collect())?);
    Some(())
}

/// How long after a window last printed its screen is still worth reading. tmux counts in whole seconds, so what an agent
/// prints just after a read can carry the same time as what that read saw; simply comparing times would miss it for good.
const FRESH: i64 = 3;
/// Now and then all screens are read anyway: a state that was somehow missed doesn't stay wrong.
const RECHECK: Duration = Duration::from_secs(30);
/// The least time between two lines. tmux redraws for one line in a clock second and sits on the next until status-interval
/// comes round, so they have to land in different seconds, whatever its clock and ours make of the moment.
const GAP: Duration = Duration::from_millis(1100);
/// How often to check while an agent is printing: what it does next is what you're waiting to see.
const QUICK: Duration = Duration::from_millis(250);
/// How often to check when every agent is quiet, which is most of the time.
const CALM: Duration = Duration::from_secs(1);

/// Whether an agent's screen needs reading on this check.
fn due(known: bool, printed_ago: i64, recheck: bool) -> bool {
    !known || printed_ago <= FRESH || recheck
}

/// The counts, for as long as tmux keeps reading: quicker than status-interval, which would also run everything else in
/// the status bar that often. An idle agent prints nothing, so tmux is asked which windows printed, and only those screens
/// are read: once a second while all is quiet, four times a second while an agent prints. tmux reruns the bar's other commands whenever a line arrives, so a line only goes out when the
/// counts change.
pub fn watch() {
    let mut agents = Registry::load();
    // The title last: it's the one that can hold a tab.
    let format = format!("#{{pane_id}}\t#{{pane_current_command}}\t#{{window_activity}}\t{LOOKING}\t#{{pane_title}}");
    let mut screens: HashMap<String, State> = HashMap::new();
    let mut shown = None;
    let mut read_all = Instant::now();
    let mut printed = Instant::now();
    let mut config_seen = String::new();
    while let Some(list) = tmux::out(&["list-panes", "-a", "-F", &format]) {
        // A changed config.toml takes effect as it's saved; checking costs one stat.
        let stamp = config::stamp();
        // Each watch reads it again itself: only the first to notice rebinds the keys, but every client's bar follows it.
        if stamp != config_seen {
            config::reload();
            popup::follow_config();
            agents = Registry::load();
            config_seen = stamp;
        }
        let now = tmux::now();
        let recheck = read_all.elapsed() >= RECHECK;
        if recheck {
            read_all = Instant::now();
            // An agent CLI installed since the watch began is counted too.
            agents = Registry::load();
        }
        let mut found: Vec<(&str, &str, &str, bool)> = Vec::new();
        let mut printing = false;
        let mut reading = Vec::new();
        for line in list.lines() {
            let [id, tool, printed, looking, title] = line.splitn(5, '\t').collect::<Vec<_>>()[..] else {
                continue;
            };
            if !agents.tracks(tool) {
                continue;
            }
            // A window linked into several sessions lists its panes once for each: in view if in view in any.
            if let Some(listed) = found.iter_mut().find(|(seen, ..)| *seen == id) {
                listed.3 |= looking == "1";
                continue;
            }
            let printed_ago = now - printed.parse().unwrap_or(0);
            printing |= printed_ago <= FRESH;
            if due(screens.contains_key(id), printed_ago, recheck) {
                reading.push(id);
            }
            found.push((id, tool, title, looking == "1"));
        }
        // Every screen due is read in one call to tmux.
        let captured = tmux::captures(&reading, detect::SCREEN);
        for &(id, tool, title, _) in found.iter().filter(|(id, ..)| reading.contains(id)) {
            let state = match captured.get(id) {
                Some(screen) => detect::inspect_captured(id, screen, &agents, tool, Some(title)).0,
                None => detect::inspect(id, &agents, tool, Some(title)).0,
            };
            screens.insert(id.to_string(), state);
        }
        let found: Vec<(String, State, bool)> = found
            .into_iter()
            .map(|(id, _, _, looking)| (id.to_string(), screens[id], looking))
            .collect();
        screens.retain(|id, _| found.iter().any(|(pane, _, _)| pane == id));
        let Some(counts) = settle(found) else { return };
        if shown.as_ref() != Some(&counts) && printed.elapsed() >= GAP {
            if writeln!(std::io::stdout(), "{counts}").is_err() {
                return;
            }
            shown = Some(counts);
            printed = Instant::now();
        }
        std::thread::sleep(if printing { QUICK } else { CALM });
    }
}

/// prefix + b: back to the agent you were on before the board last took you somewhere (again: forth).
pub fn back() {
    let target = tmux::option("@agentic-previous").unwrap_or_default();
    if tmux::is_pane(&target) && tmux::display(&target, "#{pane_id}").is_some() {
        if let Some(here) = tmux::out(&["display", "-p", "#{pane_id}"]) {
            tmux::ok(&["set", "-g", "@agentic-previous", &here]);
        }
        tmux::ok(&["switch-client", "-t", &target]);
    } else {
        tmux::display_message("No agent to go back to yet: open one from the board first");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screen_is_read_when_its_window_printed_or_it_has_been_a_while() {
        assert!(due(false, 9000, false), "never read");
        assert!(due(true, 0, false), "printing");
        assert!(due(true, FRESH, false), "the same second as the last read can hide more");
        assert!(!due(true, FRESH + 1, false), "quiet: left alone");
        assert!(due(true, 9000, true), "the safety net");
    }

    #[test]
    fn the_bar_counts_only_the_states_it_is_told_to() {
        let states = [State::Waiting, State::Running, State::Running, State::Idle];
        assert_eq!(
            counts(&states, &[State::Waiting, State::Done, State::Running], &Palette::default()),
            "#[fg=brightyellow]◆ 1 needs you #[fg=green]● 2 working "
        );
        assert_eq!(counts(&states, &[State::Idle], &Palette::default()), "#[fg=brightblack]○ 1 idle ");
        assert_eq!(counts(&states, &[State::Done], &Palette::default()), "", "none done: nothing shown");
    }

    #[test]
    fn a_named_theme_colors_the_bar_in_its_own_hex() {
        let mocha = crate::theme::named("catppuccin-mocha").unwrap();
        assert_eq!(counts(&[State::Waiting], &[State::Waiting], &mocha), "#[fg=#f9e2af]◆ 1 needs you ");
    }

    #[test]
    fn work_finished_out_of_sight_is_done_until_seen() {
        assert_eq!(settled(State::Idle, Some(State::Running), false), State::Done);
        assert_eq!(settled(State::Done, Some(State::Running), false), State::Done);
        assert_eq!(settled(State::Idle, Some(State::Done), false), State::Done);
        assert_eq!(settled(State::Done, Some(State::Done), true), State::Idle);
        assert_eq!(settled(State::Idle, Some(State::Running), true), State::Idle);
        assert_eq!(settled(State::Idle, Some(State::Idle), false), State::Idle);
        assert_eq!(settled(State::Running, Some(State::Done), false), State::Running);
    }
}
