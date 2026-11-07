//! Messages on the band that takes the footer's place: what they say, how they look, and how long they stay.
use super::*;

/// How long good news stays.
pub(super) const DONE_FOR: Duration = Duration::from_secs(3);

/// How long a refused key is explained: you're looking, you just pressed it.
pub(super) const REFUSED_FOR: Duration = Duration::from_secs(5);

/// How long a failure holds the footer: long enough to look up. Its agent's row keeps it after that.
pub(super) const FAILED_FOR: Duration = Duration::from_secs(8);

/// What a message is, which decides how it looks and how long it stays.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tone {
    /// Something is under way.
    Working,
    /// It went fine.
    Done,
    /// A second press confirms it.
    Ask,
    /// It went wrong.
    Failed,
}

/// Which kind of message this is. Whether it went wrong is known where it happened, never read from its wording.
pub fn tone(message: &str, failed: bool) -> Tone {
    if failed {
        Tone::Failed
    } else if message.starts_with("Step ") || message.ends_with('…') {
        Tone::Working
    } else {
        Tone::Done
    }
}

/// The message on the band: what it says, whether it went wrong, and its clock.
#[derive(Default)]
pub struct Notice {
    /// What it says; empty when there's none.
    pub(super) text: String,
    /// It went wrong.
    alarm: bool,
    /// The message on the clock, and when it first showed.
    pub(super) shown: Option<(String, Instant)>,
    /// A failure that belongs to no agent's row: nothing else would remember it, so it stays until dismissed.
    pinned: String,
}

impl Notice {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Good news, or what's under way.
    pub fn say(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.alarm = false;
    }

    /// Something went wrong.
    pub fn fail(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.alarm = true;
    }

    pub fn clear(&mut self) {
        self.text.clear();
    }

    pub fn tone(&self) -> Tone {
        tone(&self.text, self.alarm)
    }

    /// Changes that stopped: the failure stays until dismissed, unless an agent's row keeps it.
    pub(super) fn stop(&mut self, text: String, on_a_row: bool) {
        self.shown = None;
        self.pinned = if on_a_row { String::new() } else { text.clone() };
        self.fail(text);
    }

    /// A failure nothing else would remember, still showing.
    pub fn is_pinned(&self) -> bool {
        !self.text.is_empty() && self.text == self.pinned
    }

    /// Takes the message down when its time is up, so it never blocks the keys for long. Good news and a refused key are
    /// over with the next key. A failure on a row (`on_a_row`) stays there and only its band goes; one with no row stays.
    pub(super) fn expire(&mut self, pressed: Option<KeyCode>, on_a_row: bool) {
        if self.text.is_empty() {
            return;
        }
        let (by_key, lasts) = match self.tone() {
            Tone::Done => (pressed.is_some(), DONE_FOR),
            // Esc is left to dismiss it, or it would also take the next layer with it.
            Tone::Failed if !on_a_row => (pressed.is_some_and(|key| key != KeyCode::Esc), REFUSED_FOR),
            Tone::Failed => (false, FAILED_FOR),
            _ => return,
        };
        if self.is_pinned() {
            return;
        }
        let at = match &self.shown {
            Some((text, at)) if *text == self.text => *at,
            _ => Instant::now(),
        };
        self.shown = Some((self.text.clone(), at));
        if by_key || at.elapsed() > lasts {
            self.shown = None;
            self.text.clear();
        }
    }
}

impl App {
    pub fn say(&mut self, text: impl Into<String>) {
        self.notice.say(text);
    }

    pub fn fail(&mut self, text: impl Into<String>) {
        self.notice.fail(text);
    }

    pub fn report(&mut self, outcome: Result<String, String>) {
        match outcome {
            Ok(text) => self.say(text),
            Err(text) => self.fail(text),
        }
    }

    pub fn tone(&self) -> Tone {
        self.notice.tone()
    }

    /// Whether a failure outlasts the band: it's on an agent's row, or nothing else would remember it.
    pub fn lasting_failure(&self) -> bool {
        !self.failed.is_empty() || self.notice.is_pinned()
    }

    pub(super) fn expire(&mut self, pressed: Option<KeyCode>) {
        self.notice.expire(pressed, !self.failed.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aged(notice: &mut Notice, seconds: u64) {
        notice.shown = Some((notice.text.clone(), Instant::now() - Duration::from_secs(seconds)));
    }

    #[test]
    fn good_news_goes_with_the_next_key_or_in_a_few_seconds() {
        let mut notice = Notice::default();
        notice.say("Renamed");
        notice.expire(None, false);
        assert!(!notice.is_empty());
        notice.expire(Some(KeyCode::Char('j')), false);
        assert!(notice.is_empty());
        notice.say("Renamed");
        aged(&mut notice, 4);
        notice.expire(None, false);
        assert!(notice.is_empty());
    }

    #[test]
    fn a_refused_key_goes_with_the_next_but_esc_is_left_to_dismiss_it() {
        let mut notice = Notice::default();
        notice.fail("Can't rename: busy");
        notice.expire(Some(KeyCode::Esc), false);
        assert!(!notice.is_empty());
        notice.expire(Some(KeyCode::Char('j')), false);
        assert!(notice.is_empty());
    }

    #[test]
    fn a_failure_on_a_row_loses_its_band_only_with_time_and_one_on_none_stays() {
        let mut notice = Notice::default();
        notice.stop("Stopped at step 1".into(), true);
        notice.expire(Some(KeyCode::Char('j')), true);
        assert!(!notice.is_empty() && !notice.is_pinned(), "keys don't take it");
        aged(&mut notice, 9);
        notice.expire(None, true);
        assert!(notice.is_empty());
        notice.stop("Stopped at step 2".into(), false);
        aged(&mut notice, 60);
        notice.expire(Some(KeyCode::Char('j')), false);
        assert!(notice.is_pinned(), "nothing else would remember it");
    }
}
