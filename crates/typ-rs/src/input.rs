//! Reads terminal events and decodes them into the keys the session
//! understands.
//!
//! Timing matters more than anything else here: an event is stamped the
//! moment it is read, before it is decoded, applied, or rendered.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use typ_rs_core::session::Key;

/// A terminal event the session cares about, decoded from the terminal's
/// own representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    Key(Key),
    /// Text from a bracketed paste, to be recorded but never applied.
    Paste(String),
    Resize {
        columns: u16,
        rows: u16,
    },
    /// `Shift-Tab`: discard the attempt and begin another on a fresh
    /// prompt. Never a session key, so the state machine and the event log
    /// never see it.
    Restart,
}

/// A decoded event and when it was read, in microseconds from raw-mode entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampedEvent {
    pub at_micros: u64,
    pub event: TerminalEvent,
}

/// Blocks until at least one event is available, then drains everything
/// already pending, stamping each on read. Never returns an empty batch.
pub fn read_batch(now_micros: impl Fn() -> u64) -> io::Result<Vec<StampedEvent>> {
    let mut batch = Vec::new();
    loop {
        let raw = event::read()?;
        let at_micros = now_micros();
        if let Some(event) = decode(raw) {
            batch.push(StampedEvent { at_micros, event });
        }
        if !event::poll(Duration::ZERO)? {
            if batch.is_empty() {
                continue;
            }
            return Ok(batch);
        }
    }
}

/// Maps a terminal event to what the session should see, or `None` for
/// events that carry no meaning here (key releases, focus, mouse).
///
/// `Esc` and `Ctrl-C` interrupt. `Shift-Tab` restarts: every terminal
/// delivers it as `BackTab`, whether through the legacy `ESC [ Z` sequence
/// or the kitty keyboard protocol, so that one code with no modifier but
/// Shift is the whole key; plain `Tab` and `Enter` stay inert. `Ctrl-H` is
/// treated as backspace because some terminals send it for the Backspace
/// key. Any other character with a modifier held (control, alt, super) is
/// a command in some other program, not typed text, and is ignored.
pub fn decode(event: Event) -> Option<TerminalEvent> {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => {
            let modifiers = key.modifiers.difference(KeyModifiers::SHIFT);
            if key.code == KeyCode::BackTab && modifiers.is_empty() {
                return Some(TerminalEvent::Restart);
            }
            let key = match key.code {
                KeyCode::Esc => Key::Interrupt,
                KeyCode::Char('c') if modifiers == KeyModifiers::CONTROL => Key::Interrupt,
                KeyCode::Char('h') if modifiers == KeyModifiers::CONTROL => Key::Backspace,
                KeyCode::Char(_) if !modifiers.is_empty() => Key::Other,
                KeyCode::Char(c) => Key::Char(c),
                KeyCode::Backspace => Key::Backspace,
                _ => Key::Other,
            };
            Some(TerminalEvent::Key(key))
        }
        Event::Key(_) => None,
        Event::Paste(text) => Some(TerminalEvent::Paste(text)),
        Event::Resize(columns, rows) => Some(TerminalEvent::Resize { columns, rows }),
        Event::FocusGained | Event::FocusLost | Event::Mouse(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyEventState};

    fn press(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn key(event: Event) -> Key {
        match decode(event) {
            Some(TerminalEvent::Key(key)) => key,
            other => panic!("expected a key, got {other:?}"),
        }
    }

    #[test]
    fn printable_characters_are_typed_whether_or_not_shift_is_held() {
        assert_eq!(
            key(press(KeyCode::Char('a'), KeyModifiers::NONE)),
            Key::Char('a')
        );
        assert_eq!(
            key(press(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            Key::Char('A')
        );
        assert_eq!(
            key(press(KeyCode::Char(' '), KeyModifiers::NONE)),
            Key::Char(' ')
        );
    }

    #[test]
    fn esc_and_ctrl_c_interrupt() {
        assert_eq!(key(press(KeyCode::Esc, KeyModifiers::NONE)), Key::Interrupt);
        assert_eq!(
            key(press(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Key::Interrupt
        );
    }

    #[test]
    fn backspace_and_ctrl_h_both_erase() {
        assert_eq!(
            key(press(KeyCode::Backspace, KeyModifiers::NONE)),
            Key::Backspace
        );
        assert_eq!(
            key(press(KeyCode::Char('h'), KeyModifiers::CONTROL)),
            Key::Backspace
        );
    }

    #[test]
    fn other_modified_characters_and_non_character_keys_are_ignored() {
        assert_eq!(
            key(press(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Key::Other
        );
        assert_eq!(
            key(press(KeyCode::Char('c'), KeyModifiers::ALT)),
            Key::Other
        );
        assert_eq!(
            key(press(KeyCode::Char('x'), KeyModifiers::SUPER)),
            Key::Other
        );
        assert_eq!(key(press(KeyCode::Enter, KeyModifiers::NONE)), Key::Other);
        assert_eq!(key(press(KeyCode::Tab, KeyModifiers::NONE)), Key::Other);
        assert_eq!(key(press(KeyCode::Left, KeyModifiers::NONE)), Key::Other);
        assert_eq!(key(press(KeyCode::F(1), KeyModifiers::NONE)), Key::Other);
    }

    #[test]
    fn shift_tab_restarts_and_no_other_tab_or_enter_does() {
        assert_eq!(
            decode(press(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(TerminalEvent::Restart)
        );
        assert_eq!(
            decode(press(KeyCode::BackTab, KeyModifiers::NONE)),
            Some(TerminalEvent::Restart)
        );
        assert_eq!(key(press(KeyCode::Tab, KeyModifiers::NONE)), Key::Other);
        assert_eq!(key(press(KeyCode::Tab, KeyModifiers::SHIFT)), Key::Other);
        assert_eq!(key(press(KeyCode::Enter, KeyModifiers::NONE)), Key::Other);
        assert_eq!(
            key(press(
                KeyCode::BackTab,
                KeyModifiers::SHIFT | KeyModifiers::CONTROL
            )),
            Key::Other
        );
        assert_eq!(
            key(press(
                KeyCode::BackTab,
                KeyModifiers::SHIFT | KeyModifiers::ALT
            )),
            Key::Other
        );
    }

    #[test]
    fn a_tab_inside_a_paste_is_pasted_text_not_a_restart() {
        assert_eq!(
            decode(Event::Paste("a\tb".into())),
            Some(TerminalEvent::Paste("a\tb".into()))
        );
    }

    #[test]
    fn key_releases_focus_changes_and_mouse_events_carry_nothing() {
        let release = Event::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        });
        assert_eq!(decode(release), None);
        assert_eq!(decode(Event::FocusGained), None);
        assert_eq!(decode(Event::FocusLost), None);
    }

    #[test]
    fn pastes_and_resizes_pass_through() {
        assert_eq!(
            decode(Event::Paste("hello".into())),
            Some(TerminalEvent::Paste("hello".into()))
        );
        assert_eq!(
            decode(Event::Resize(80, 24)),
            Some(TerminalEvent::Resize {
                columns: 80,
                rows: 24
            })
        );
    }
}
