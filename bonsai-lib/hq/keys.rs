//! Vim keys shared by every view: counts, two-key prefixes, and motions.
//!
//! Lists move a cursor with these motions; help, dialogs, and the output view
//! scroll their text with the same keys, so navigation works the same
//! everywhere.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Motion {
    /// `j`, ↓, Ctrl-N.
    Down,
    /// `k`, ↑, Ctrl-P.
    Up,
    /// Ctrl-D.
    HalfDown,
    /// Ctrl-U.
    HalfUp,
    /// Ctrl-F, Page Down.
    PageDown,
    /// Ctrl-B, Page Up.
    PageUp,
    /// Ctrl-E: move the view, not the cursor.
    ScrollDown,
    /// Ctrl-Y.
    ScrollUp,
    /// `gg`, Home; with a count, that line.
    First,
    /// `G`, End; with a count, that line.
    Last,
    /// `H`.
    ScreenTop,
    /// `M`.
    ScreenMiddle,
    /// `L`.
    ScreenBottom,
    /// `zt`.
    AlignTop,
    /// `zz`.
    AlignMiddle,
    /// `zb`.
    AlignBottom,
    /// `{`.
    PreviousGroup,
    /// `}`.
    NextGroup,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Input {
    /// A motion and the count typed before it, if any.
    Motion(Motion, Option<usize>),
    /// A doubled key such as `dd` or `yy`.
    Pair(char),
    /// Any other key, and the count typed before it.
    Key(KeyEvent, Option<usize>),
    /// A count digit or the first key of a pair: wait for the next key.
    Pending,
}

/// Count and prefix state between keys.
#[derive(Debug, Default)]
pub(super) struct Keys {
    count: Option<usize>,
    pending: Option<char>,
}

impl Keys {
    /// Whether a count or a prefix is waiting for its next key.
    pub(super) fn waiting(&self) -> bool {
        self.count.is_some() || self.pending.is_some()
    }

    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Interpret a key. `pairs` lists the doubled keys this view accepts
    /// besides `gg` and `zz`/`zt`/`zb`.
    pub(super) fn feed(&mut self, key: KeyEvent, pairs: &[char]) -> Input {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        let code = shifted(&key);
        if let Some(first) = self.pending.take() {
            let count = self.count.take();
            if plain {
                match (first, code) {
                    ('g', KeyCode::Char('g')) => return Input::Motion(Motion::First, count),
                    ('z', KeyCode::Char('z')) => return Input::Motion(Motion::AlignMiddle, count),
                    ('z', KeyCode::Char('t')) => return Input::Motion(Motion::AlignTop, count),
                    ('z', KeyCode::Char('b')) => return Input::Motion(Motion::AlignBottom, count),
                    (first, KeyCode::Char(second)) if first == second && pairs.contains(&first) => {
                        return Input::Pair(first);
                    }
                    _ => {}
                }
            }
            // Anything else cancels the prefix, as in vim, and counts afresh.
        }
        if plain
            && let KeyCode::Char(digit @ '0'..='9') = code
            && (digit != '0' || self.count.is_some())
        {
            let value = digit.to_digit(10).unwrap_or(0) as usize;
            self.count = Some(
                self.count
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(value)
                    .min(99_999),
            );
            return Input::Pending;
        }
        let count = self.count.take();
        let motion = if ctrl {
            match code {
                KeyCode::Char('n' | 'N') => Some(Motion::Down),
                KeyCode::Char('p' | 'P') => Some(Motion::Up),
                KeyCode::Char('d' | 'D') => Some(Motion::HalfDown),
                KeyCode::Char('u' | 'U') => Some(Motion::HalfUp),
                KeyCode::Char('f' | 'F') => Some(Motion::PageDown),
                KeyCode::Char('b' | 'B') => Some(Motion::PageUp),
                KeyCode::Char('e' | 'E') => Some(Motion::ScrollDown),
                KeyCode::Char('y' | 'Y') => Some(Motion::ScrollUp),
                _ => None,
            }
        } else if plain {
            match code {
                KeyCode::Down | KeyCode::Char('j') => Some(Motion::Down),
                KeyCode::Up | KeyCode::Char('k') => Some(Motion::Up),
                KeyCode::PageDown => Some(Motion::PageDown),
                KeyCode::PageUp => Some(Motion::PageUp),
                KeyCode::Home => Some(Motion::First),
                KeyCode::End | KeyCode::Char('G') => Some(Motion::Last),
                KeyCode::Char('H') => Some(Motion::ScreenTop),
                KeyCode::Char('M') => Some(Motion::ScreenMiddle),
                KeyCode::Char('L') => Some(Motion::ScreenBottom),
                KeyCode::Char('{') => Some(Motion::PreviousGroup),
                KeyCode::Char('}') => Some(Motion::NextGroup),
                KeyCode::Char(first @ ('g' | 'z')) => {
                    self.begin(first, count);
                    return Input::Pending;
                }
                KeyCode::Char(first) if pairs.contains(&first) => {
                    self.begin(first, count);
                    return Input::Pending;
                }
                _ => None,
            }
        } else {
            None
        };
        match motion {
            Some(motion) => Input::Motion(motion, count),
            None => Input::Key(key, count),
        }
    }

    fn begin(&mut self, first: char, count: Option<usize>) {
        self.pending = Some(first);
        self.count = count;
    }
}

/// A key with Shift folded into the character, as terminals report either.
pub(super) fn shifted(key: &KeyEvent) -> KeyCode {
    match key.code {
        KeyCode::Char(ch) if key.modifiers.contains(KeyModifiers::SHIFT) => {
            KeyCode::Char(ch.to_ascii_uppercase())
        }
        code => code,
    }
}

/// Scroll position of a text view: help, a dialog, or command output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Pager {
    pub(super) top: usize,
    /// Visible lines and total lines, from the last draw.
    pub(super) height: usize,
    pub(super) len: usize,
    /// Keep the end in view as lines arrive, like `tail -f`.
    pub(super) follow: bool,
}

impl Pager {
    pub(super) fn following() -> Self {
        Self {
            follow: true,
            ..Self::default()
        }
    }

    pub(super) fn max_top(&self) -> usize {
        self.len.saturating_sub(self.height)
    }

    /// Record the drawn size and keep the position inside it.
    pub(super) fn fit(&mut self, height: usize, len: usize) {
        self.height = height;
        self.len = len;
        if self.follow {
            self.top = self.max_top();
        }
        self.top = self.top.min(self.max_top());
    }

    pub(super) fn apply(&mut self, motion: Motion, count: Option<usize>) {
        let times = count.unwrap_or(1);
        let half = (self.height / 2).max(1);
        let page = self.height.max(1);
        let top = match motion {
            Motion::Down | Motion::ScrollDown => self.top.saturating_add(times),
            Motion::Up | Motion::ScrollUp => self.top.saturating_sub(times),
            Motion::HalfDown => self.top.saturating_add(half * times),
            Motion::HalfUp => self.top.saturating_sub(half * times),
            Motion::PageDown => self.top.saturating_add(page * times),
            Motion::PageUp => self.top.saturating_sub(page * times),
            Motion::First => count.map_or(0, |line| line.saturating_sub(1)),
            Motion::Last => count.map_or(self.max_top(), |line| line.saturating_sub(1)),
            _ => self.top,
        };
        self.top = top.min(self.max_top());
        self.follow = self.top >= self.max_top();
    }

    /// `lines 3–20 of 40`, when the text does not fit.
    pub(super) fn position(&self) -> Option<String> {
        (self.len > self.height).then(|| {
            format!(
                "lines {}–{} of {}",
                self.top + 1,
                (self.top + self.height).min(self.len),
                self.len
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    fn feed(keys: &mut Keys, text: &str, pairs: &[char]) -> Vec<Input> {
        text.chars()
            .map(|ch| keys.feed(press(KeyCode::Char(ch)), pairs))
            .collect()
    }

    #[test]
    fn counts_prefix_motions_and_pairs() {
        let mut keys = Keys::default();
        assert_eq!(
            feed(&mut keys, "12j", &[]).pop(),
            Some(Input::Motion(Motion::Down, Some(12)))
        );
        assert_eq!(
            feed(&mut keys, "3gg", &[]).pop(),
            Some(Input::Motion(Motion::First, Some(3)))
        );
        assert_eq!(
            feed(&mut keys, "G", &[]).pop(),
            Some(Input::Motion(Motion::Last, None))
        );
        assert_eq!(
            feed(&mut keys, "dd", &['d', 'y']).pop(),
            Some(Input::Pair('d'))
        );
        assert_eq!(
            feed(&mut keys, "zt", &[]).pop(),
            Some(Input::Motion(Motion::AlignTop, None))
        );
        // A view without `dd` treats `d` as a plain key.
        assert!(matches!(
            feed(&mut keys, "d", &[]).pop(),
            Some(Input::Key(_, None))
        ));
        // A lone zero is a key, and a broken prefix starts over.
        assert!(matches!(
            feed(&mut keys, "0", &[]).pop(),
            Some(Input::Key(_, None))
        ));
        assert_eq!(
            feed(&mut keys, "gj", &[]).pop(),
            Some(Input::Motion(Motion::Down, None))
        );
        assert!(!keys.waiting());
    }

    #[test]
    fn control_keys_and_arrows_map_to_the_same_motions() {
        let mut keys = Keys::default();
        for (key, motion) in [
            (ctrl('d'), Motion::HalfDown),
            (ctrl('u'), Motion::HalfUp),
            (ctrl('f'), Motion::PageDown),
            (ctrl('b'), Motion::PageUp),
            (ctrl('e'), Motion::ScrollDown),
            (ctrl('y'), Motion::ScrollUp),
            (ctrl('n'), Motion::Down),
            (ctrl('p'), Motion::Up),
            (press(KeyCode::Down), Motion::Down),
            (press(KeyCode::Up), Motion::Up),
            (press(KeyCode::PageDown), Motion::PageDown),
            (press(KeyCode::Home), Motion::First),
            (press(KeyCode::End), Motion::Last),
            (
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::SHIFT),
                Motion::Last,
            ),
        ] {
            assert_eq!(keys.feed(key, &[]), Input::Motion(motion, None), "{key:?}");
        }
        assert!(matches!(keys.feed(ctrl('c'), &[]), Input::Key(_, None)));
    }

    #[test]
    fn a_pager_scrolls_within_its_text_and_follows_the_end() {
        let mut pager = Pager::default();
        pager.fit(10, 35);
        pager.apply(Motion::Down, Some(3));
        assert_eq!(pager.top, 3);
        pager.apply(Motion::HalfDown, None);
        assert_eq!(pager.top, 8);
        pager.apply(Motion::PageDown, Some(5));
        assert_eq!(pager.top, 25);
        assert!(pager.follow);
        assert_eq!(pager.position().as_deref(), Some("lines 26–35 of 35"));
        pager.fit(10, 40);
        assert_eq!(pager.top, 30, "follows new lines at the end");
        pager.apply(Motion::First, None);
        assert_eq!(pager.top, 0);
        assert!(!pager.follow);
        pager.fit(10, 50);
        assert_eq!(pager.top, 0, "stays put away from the end");
        pager.apply(Motion::Last, Some(12));
        assert_eq!(pager.top, 11);
        pager.apply(Motion::ScrollUp, Some(100));
        assert_eq!(pager.top, 0);
        pager.fit(60, 50);
        assert_eq!(pager.position(), None);
    }
}
