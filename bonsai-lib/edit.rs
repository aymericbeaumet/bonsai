use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// One line of text with a cursor, edited with the readline bindings used by
/// the fuzzy picker: Ctrl-B/F, Ctrl-A/E, Ctrl-W, Ctrl-U, Ctrl-K, Ctrl-D, and
/// the Alt word keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineInput {
    text: String,
    /// Byte index on a char boundary.
    cursor: usize,
}

impl LineInput {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    #[cfg(test)]
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    /// Returns whether the key edited this line. Unhandled keys are left for
    /// the surrounding list.
    pub fn edit(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Left if ctrl => self.backward_word(),
            KeyCode::Left => self.backward_char(),
            KeyCode::Right if ctrl => self.forward_word(),
            KeyCode::Right => self.forward_char(),
            KeyCode::Home => self.beginning(),
            KeyCode::End => self.end(),
            KeyCode::Backspace if alt => self.backward_kill_word(),
            KeyCode::Backspace => self.backward_delete(),
            KeyCode::Delete if ctrl => self.kill_word(),
            KeyCode::Delete => self.delete_char(),
            KeyCode::Char('a') if ctrl => self.beginning(),
            KeyCode::Char('b') if ctrl => self.backward_char(),
            KeyCode::Char('b') if alt => self.backward_word(),
            KeyCode::Char('d') if ctrl => self.delete_char(),
            KeyCode::Char('d') if alt => self.kill_word(),
            KeyCode::Char('e') if ctrl => self.end(),
            KeyCode::Char('f') if ctrl => self.forward_char(),
            KeyCode::Char('f') if alt => self.forward_word(),
            KeyCode::Char('h') if ctrl => self.backward_delete(),
            KeyCode::Char('k') if ctrl => self.kill_end(),
            KeyCode::Char('u') if ctrl => self.kill_beginning(),
            KeyCode::Char('w') if ctrl => self.unix_word_rubout(),
            KeyCode::Char(ch) if !ctrl && !alt => self.insert(ch),
            _ => return false,
        }
        true
    }

    /// `prefix` plus the text. While active, the cursor is a reversed
    /// character, or `_` when it is at the end.
    pub fn titled(&self, prefix: &str, active: bool) -> Line<'static> {
        let mut spans = vec![Span::raw(prefix.to_string())];
        if !active {
            spans.push(Span::raw(self.text.clone()));
            spans.push(Span::raw(" "));
            return Line::from(spans);
        }
        let before = self.text[..self.cursor].to_string();
        let after = &self.text[self.cursor..];
        spans.push(Span::raw(before));
        if let Some(ch) = after.chars().next() {
            spans.push(Span::styled(
                ch.to_string(),
                Style::new().add_modifier(Modifier::REVERSED),
            ));
            spans.push(Span::raw(after[ch.len_utf8()..].to_string()));
        } else {
            spans.push(Span::raw("_"));
        }
        spans.push(Span::raw(" "));
        Line::from(spans)
    }

    fn insert(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    fn beginning(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.text.len();
    }

    fn backward_char(&mut self) {
        if let Some(ch) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= ch.len_utf8();
        }
    }

    fn forward_char(&mut self) {
        if let Some(ch) = self.text[self.cursor..].chars().next() {
            self.cursor += ch.len_utf8();
        }
    }

    fn backward_delete(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.backward_char();
        self.text.remove(self.cursor);
    }

    fn delete_char(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    fn kill_beginning(&mut self) {
        self.text.replace_range(..self.cursor, "");
        self.cursor = 0;
    }

    fn kill_end(&mut self) {
        self.text.truncate(self.cursor);
    }

    fn unix_word_rubout(&mut self) {
        let mut pos = self.char_pos();
        let chars: Vec<char> = self.text.chars().collect();
        while pos > 0 && chars[pos - 1].is_whitespace() {
            pos -= 1;
        }
        while pos > 0 && !chars[pos - 1].is_whitespace() {
            pos -= 1;
        }
        self.replace_chars(pos, self.char_pos());
    }

    fn backward_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut pos = self.char_pos();
        if pos == 0 {
            return;
        }
        pos -= 1;
        while pos > 0 && !chars[pos].is_alphanumeric() {
            pos -= 1;
        }
        while pos > 0 && chars[pos - 1].is_alphanumeric() {
            pos -= 1;
        }
        self.set_char_pos(pos);
    }

    fn forward_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut pos = self.char_pos();
        while pos < chars.len() && chars[pos].is_whitespace() {
            pos += 1;
        }
        while pos < chars.len() && !chars[pos].is_whitespace() {
            pos += 1;
        }
        self.set_char_pos(pos);
    }

    fn backward_kill_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let end = self.char_pos();
        if end == 0 {
            return;
        }
        let mut pos = end;
        while pos > 0 && !chars[pos - 1].is_alphanumeric() {
            pos -= 1;
        }
        while pos > 0 && chars[pos - 1].is_alphanumeric() {
            pos -= 1;
        }
        self.replace_chars(pos, end);
    }

    fn kill_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let start = self.char_pos();
        let mut pos = start;
        while pos < chars.len() && !chars[pos].is_alphanumeric() {
            pos += 1;
        }
        while pos < chars.len() && chars[pos].is_alphanumeric() {
            pos += 1;
        }
        self.replace_chars(start, pos);
    }

    fn char_pos(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    fn set_char_pos(&mut self, pos: usize) {
        self.cursor = self.text.chars().take(pos).map(char::len_utf8).sum();
    }

    fn replace_chars(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let byte_start: usize = self.text.chars().take(start).map(char::len_utf8).sum();
        let byte_end: usize = byte_start
            + self
                .text
                .chars()
                .skip(start)
                .take(end - start)
                .map(char::len_utf8)
                .sum::<usize>();
        self.text.replace_range(byte_start..byte_end, "");
        self.cursor = byte_start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn ctrl(ch: char) -> KeyEvent {
        key(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    fn alt(ch: char) -> KeyEvent {
        key(KeyCode::Char(ch), KeyModifiers::ALT)
    }

    fn type_str(input: &mut LineInput, text: &str) {
        for ch in text.chars() {
            assert!(input.edit(&key(KeyCode::Char(ch), KeyModifiers::NONE)));
        }
    }

    #[test]
    fn readline_moves_and_deletes_around_the_cursor() {
        let mut input = LineInput::default();
        type_str(&mut input, "foo bar");
        assert!(input.edit(&ctrl('w')));
        assert_eq!(input.text(), "foo ");
        assert!(input.edit(&ctrl('a')));
        assert!(input.edit(&ctrl('f')));
        assert!(input.edit(&ctrl('d')));
        assert_eq!(input.text(), "fo ");
        assert!(input.edit(&ctrl('e')));
        assert!(input.edit(&ctrl('k')));
        assert_eq!(input.text(), "fo ");
        assert!(input.edit(&ctrl('u')));
        assert!(input.text().is_empty());
        assert!(!input.edit(&ctrl('c')));
    }

    #[test]
    fn ctrl_f_and_arrows_move_by_character_and_ctrl_u_keeps_the_tail() {
        let mut input = LineInput::default();
        type_str(&mut input, "café");
        assert!(input.edit(&key(KeyCode::Left, KeyModifiers::NONE)));
        assert!(input.edit(&ctrl('h')));
        assert_eq!(input.text(), "caé");
        assert!(input.edit(&ctrl('d')));
        assert_eq!(input.text(), "ca");
        assert!(input.edit(&ctrl('a')));
        assert!(input.edit(&ctrl('f')));
        assert!(input.edit(&ctrl('u')));
        assert_eq!(input.text(), "a");
        assert!(input.edit(&key(KeyCode::End, KeyModifiers::NONE)));
        assert!(input.edit(&ctrl('u')));
        assert!(input.text().is_empty());
    }

    #[test]
    fn word_keys_match_the_picker() {
        let mut input = LineInput::default();
        type_str(&mut input, "foo bar   ");
        assert!(input.edit(&ctrl('w')));
        assert_eq!(input.text(), "foo ");
        type_str(&mut input, "baz...");
        assert!(input.edit(&ctrl('a')));
        assert!(input.edit(&alt('f')));
        assert_eq!(&input.text()[..input.cursor], "foo");
        assert!(input.edit(&alt('d')));
        assert_eq!(input.text(), "foo...");
        assert!(input.edit(&key(KeyCode::Right, KeyModifiers::CONTROL)));
        assert_eq!(input.cursor, input.text().len());
    }
}
