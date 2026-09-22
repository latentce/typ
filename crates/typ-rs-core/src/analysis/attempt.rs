//! Rebuilds what the user typed for each word from the event log.
//!
//! Every event records the caret it arrived at, so the caret the next event
//! records says what the editing rules did with it: a character that moved
//! the caret on was applied, a backspace that moved it back removed a
//! character, a keystroke that left it where it was changed nothing. The
//! analysis therefore reads the recorded fields and never needs the rules
//! that were in force when the session was typed.

use crate::session::{EventKind, Outcome, SessionState};

/// What a keystroke did, as far as the first-attempt string is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// Extended the word's first attempt, or submitted it for the first time.
    FirstAttempt,
    /// Retyped a position already attempted, or typed anything after the
    /// word was re-entered.
    Replacement,
    /// Removed a character or re-entered the previous word, or tried to.
    Backspace,
    /// Changed nothing: a space at a word start, an extra past the cap, or
    /// a pasted character.
    Ignored,
}

/// One keystroke event and its role.
#[derive(Debug, Clone, Copy)]
pub(super) struct Keystroke {
    /// Index into the session's event log.
    pub event: usize,
    pub role: Role,
    /// For a backspace that removed an incorrect character: how long that
    /// character had stood.
    pub removed_error_after_micros: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct WordAttempt {
    pub history: Vec<char>,
    pub first_attempt: Vec<char>,
    /// The typed text at the word's first submission; `None` until then.
    pub submitted_text: Option<Vec<char>>,
    /// The characters currently typed, each with when it was typed and
    /// whether it was wrong.
    typed: Vec<(char, u64, bool)>,
}

impl WordAttempt {
    pub fn submitted(&self) -> bool {
        self.submitted_text.is_some()
    }

    fn text(&self) -> Vec<char> {
        self.typed.iter().map(|&(c, _, _)| c).collect()
    }

    /// A keystroke at `position` belongs to the first attempt only while the
    /// word has not been submitted and the position has never been typed;
    /// a character there is appended.
    fn extend_first_attempt(&mut self, position: usize, c: Option<char>) -> Role {
        if self.submitted() || position != self.first_attempt.len() {
            return Role::Replacement;
        }
        self.first_attempt.extend(c);
        Role::FirstAttempt
    }
}

pub(super) struct Reconstruction {
    pub words: Vec<WordAttempt>,
    pub keystrokes: Vec<Keystroke>,
}

impl Reconstruction {
    /// Correction keystrokes: backspaces and every keystroke that is not
    /// part of a first attempt.
    pub fn corrections(&self) -> usize {
        self.keystrokes
            .iter()
            .filter(|k| matches!(k.role, Role::Backspace | Role::Replacement))
            .count()
    }
}

pub(super) fn reconstruct(state: &SessionState) -> Reconstruction {
    let events = state.events();
    let final_caret = (state.current_word(), state.position());
    let mut words = vec![WordAttempt::default(); state.word_count()];
    let mut keystrokes = Vec::new();

    for (index, event) in events.iter().enumerate() {
        if !event.kind.is_keystroke() {
            continue;
        }
        let caret = (event.word_index, event.position);
        let caret_after = events
            .get(index + 1)
            .map_or(final_caret, |next| (next.word_index, next.position));
        let last = index + 1 == events.len();
        // A space that completes the session leaves the caret where it is.
        let applied = !event.flags.in_paste
            && (caret_after != caret
                || (event.kind == EventKind::Space
                    && last
                    && state.outcome() == Some(Outcome::Completed)));

        let attempt = &mut words[event.word_index];
        let mut removed_error_after_micros = None;
        let role = match event.kind {
            EventKind::Backspace if !event.flags.in_paste => {
                if applied && event.position > 0 {
                    let (_, typed_at, incorrect) =
                        attempt.typed.pop().expect("a removed character was typed");
                    if incorrect {
                        removed_error_after_micros = Some(event.at_micros.saturating_sub(typed_at));
                    }
                }
                Role::Backspace
            }
            _ if !applied => Role::Ignored,
            EventKind::Char => {
                let c = event.actual.expect("a char event carries its character");
                attempt.history.push(c);
                attempt
                    .typed
                    .push((c, event.at_micros, Some(c) != event.expected));
                let role = attempt.extend_first_attempt(event.position, Some(c));
                if last && state.outcome() == Some(Outcome::Completed) {
                    attempt.submitted_text = Some(attempt.text());
                }
                role
            }
            EventKind::Space => {
                let role = attempt.extend_first_attempt(event.position, None);
                if !attempt.submitted() {
                    attempt.submitted_text = Some(attempt.text());
                }
                role
            }
            _ => unreachable!("only keystrokes are classified"),
        };
        keystrokes.push(Keystroke {
            event: index,
            role,
            removed_error_after_micros,
        });
    }

    Reconstruction { words, keystrokes }
}
