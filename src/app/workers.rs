//! What runs beside the board: the agents every second, the preview, input, and config.toml saved while it's open.
use super::*;

/// The agents, every second: first without Context, whose transcript lookups would hold up the first look.
pub(super) fn refresh(agents: Arc<Registry>, tx: Sender<Msg>, wake: Receiver<()>) {
    if let Some(panes) = detect::panes(&agents, false) {
        let _ = tx.send(Msg::Panes { panes, partial: true });
    }
    let (mut last, mut last_windows) = (None, None);
    loop {
        if let Some(windows) = detect::windows()
            && last_windows.as_ref() != Some(&windows)
        {
            last_windows = Some(windows.clone());
            if tx.send(Msg::Windows(windows)).is_err() {
                return;
            }
        }
        if let Some(panes) = detect::panes(&agents, true)
            && last.as_ref() != Some(&panes)
        {
            last = Some(panes.clone());
            if tx.send(Msg::Panes { panes, partial: false }).is_err() {
                return;
            }
        }
        if matches!(wake.recv_timeout(Duration::from_secs(1)), Err(RecvTimeoutError::Disconnected)) {
            return;
        }
    }
}

/// The selected agent's screen, whenever it changes, and a conversation's messages, once.
pub(super) fn preview(agents: Arc<Registry>, tx: Sender<Msg>, wake: Receiver<Target>) {
    let mut target = Target::Nothing;
    let mut shown = None;
    loop {
        match wake.recv_timeout(Duration::from_millis(500)) {
            Ok(next) => {
                target = next;
                shown = None;
                if let Target::Conversation(tool, id) = &target {
                    let messages = agents.get(tool).map_or_else(Vec::new, |a| a.messages(id));
                    let row = RowId::Conversation(tool.clone(), id.clone());
                    if tx.send(Msg::Messages { row, messages }).is_err() {
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if let Target::Pane(pane) = &target
            && let Some(screen) = tmux::capture(pane, &["-e"])
            && shown.as_ref() != Some(&screen)
        {
            let lines = crate::text::squeeze(&screen);
            shown = Some(screen);
            if tx.send(Msg::Screen { pane: pane.clone(), lines }).is_err() {
                return;
            }
        }
    }
}

pub(super) fn start_workers(agents: &Arc<Registry>, tx: &Sender<Msg>, wake_preview: Receiver<Target>, wake_refresh: Receiver<()>) {
    let input = tx.clone();
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            if input.send(Msg::Event(event)).is_err() {
                return;
            }
        }
    });
    let (agents_copy, sender) = (agents.clone(), tx.clone());
    std::thread::spawn(move || refresh(agents_copy, sender, wake_refresh));
    let (agents_copy, sender) = (agents.clone(), tx.clone());
    std::thread::spawn(move || preview(agents_copy, sender, wake_preview));
    // A config.toml saved while the board is open takes effect in it: a stat twice a second.
    let sender = tx.clone();
    std::thread::spawn(move || {
        let mut seen = crate::config::stamp();
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let now = crate::config::stamp();
            if now != seen {
                seen = now;
                crate::config::reload();
                if sender.send(Msg::Config).is_err() {
                    return;
                }
            }
        }
    });
    let (agents_copy, sender) = (agents.clone(), tx.clone());
    std::thread::spawn(move || {
        history::clean_old_handoffs();
        let _ = sender.send(Msg::History(history::past(&agents_copy))); // can take seconds the first time
    });
}
