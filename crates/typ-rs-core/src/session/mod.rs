//! The live editing state of one session and the rules that drive it.
//!
//! Nothing here knows about a terminal: the session receives decoded
//! [`Input`]s, records the ones that count as [`InputEvent`]s, and updates the
//! typed text. The same prompt and the same inputs always produce the same
//! state, so a stored event log reproduces a session exactly.

mod event;

pub use event::{EventFlags, EventKind, Input, InputEvent, Key, LONG_PAUSE_MICROS};

use crate::prompt::Prompt;

/// Identifies the editing rules in this module. Bump whenever a rule changes
/// so stored sessions can tell which rules their events were captured under.
pub const SEMANTICS_VERSION: u32 = 1;

/// The most extra characters a word accepts; further printable input is
/// recorded but ignored.
pub const MAX_EXTRAS: usize = 8;

/// When a session is over. Only a fixed word count exists; open so a timed
/// variant can be added later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EndCondition {
    /// After this many words of the prompt; clamped to the prompt's length
    /// and to at least one word.
    AfterWords(usize),
}

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The prompt was reached to its end.
    Completed,
    /// The user ended the session early.
    Interrupted,
}

impl Outcome {
    /// The outcome's stable name, as stored with the session. Never renamed.
    pub fn name(self) -> &'static str {
        match self {
            Outcome::Completed => "completed",
            Outcome::Interrupted => "interrupted",
        }
    }

    /// The outcome with the given stored name, if this version knows it.
    pub fn from_name(name: &str) -> Option<Outcome> {
        match name {
            "completed" => Some(Outcome::Completed),
            "interrupted" => Some(Outcome::Interrupted),
            _ => None,
        }
    }
}

/// What one input did, for the renderer. The analysis never reads effects; it
/// works from the finished state and the event log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Effects {
    /// The input was appended to the event log.
    pub recorded: bool,
    /// The typed text or caret moved, so the display must be laid out again.
    pub changed: bool,
    /// This input ended the session.
    pub ended: Option<Outcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct WordState {
    /// Characters currently typed for the word, extras included.
    typed: Vec<char>,
}

/// The state of one session: the prompt, the text typed for each word, the
/// caret, and the events recorded so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    prompt: Prompt,
    word_count: usize,
    words: Vec<WordState>,
    current_word: usize,
    events: Vec<InputEvent>,
    outcome: Option<Outcome>,
    started_at_micros: Option<u64>,
    previous_keystroke_at: Option<u64>,
    resized_since_keystroke: bool,
}

impl SessionState {
    pub fn new(prompt: Prompt, end: EndCondition) -> SessionState {
        let word_count = match end {
            EndCondition::AfterWords(n) => n.clamp(1, prompt.word_count()),
        };
        SessionState {
            words: vec![WordState::default(); word_count],
            word_count,
            prompt,
            current_word: 0,
            events: Vec::new(),
            outcome: None,
            started_at_micros: None,
            previous_keystroke_at: None,
            resized_since_keystroke: false,
        }
    }

    pub fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    /// Rebuilds the state a stored event log left behind by applying every
    /// event again in order. The same prompt and events always produce the
    /// same state, so the result is identical to the one recorded live.
    pub fn replay<'a>(
        prompt: Prompt,
        end: EndCondition,
        events: impl IntoIterator<Item = &'a InputEvent>,
    ) -> SessionState {
        let mut state = SessionState::new(prompt, end);
        for event in events {
            state.apply_event(event.input());
        }
        state
    }

    /// How many words of the prompt the session covers.
    pub fn word_count(&self) -> usize {
        self.word_count
    }

    /// Index of the word the caret is in.
    pub fn current_word(&self) -> usize {
        self.current_word
    }

    /// The caret's position within the current word: the number of
    /// characters typed there, extras included.
    pub fn position(&self) -> usize {
        self.words[self.current_word].typed.len()
    }

    /// Characters currently typed for a word, extras included.
    pub fn typed(&self, word: usize) -> &[char] {
        &self.words[word].typed
    }

    /// Words the caret has moved past. Every word before the caret has been
    /// submitted; once the session completes, every word counts.
    pub fn words_completed(&self) -> usize {
        match self.outcome {
            Some(Outcome::Completed) => self.word_count,
            _ => self.current_word,
        }
    }

    pub fn events(&self) -> &[InputEvent] {
        &self.events
    }

    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }

    /// When the first printable character was typed: the start of the
    /// session's timer. `None` until then.
    pub fn started_at_micros(&self) -> Option<u64> {
        self.started_at_micros
    }

    /// Whether a word, as currently typed, differs from its target.
    pub fn has_uncorrected_error(&self, word: usize) -> bool {
        let typed = &self.words[word].typed;
        typed.iter().copied().ne(self.prompt.word(word).chars())
    }

    /// The character the prompt expects at the caret: the target character
    /// at the caret's position, or the word's following space once the word
    /// is fully typed.
    pub fn expected(&self) -> char {
        self.prompt
            .word(self.current_word)
            .chars()
            .nth(self.position())
            .unwrap_or(' ')
    }

    /// Applies one input: records it if it is an event, updates the typed
    /// text if it is applied, and reports what happened.
    ///
    /// Every printable character, space, and backspace is recorded whether or
    /// not it changed anything (a space at a word start, a refused extra),
    /// as are resizes and interrupts; only control characters and
    /// non-character keys are dropped. Pasted input is recorded but never
    /// applied. Nothing is recorded once the session is over.
    pub fn apply_event(&mut self, input: Input) -> Effects {
        if self.outcome.is_some() {
            return Effects::default();
        }
        let Some((kind, actual)) = classify(input.key) else {
            return Effects::default();
        };

        self.record(input, kind, actual);
        let mut effects = Effects {
            recorded: true,
            ..Effects::default()
        };
        if input.in_paste {
            return effects;
        }

        match kind {
            EventKind::Char => {
                let c = actual.expect("a char event carries its character");
                effects.changed = self.type_char(c);
                if effects.changed
                    && self.is_final_word()
                    && !self.has_uncorrected_error(self.current_word)
                {
                    effects.ended = self.end(Outcome::Completed);
                }
            }
            EventKind::Space => {
                if self.position() > 0 {
                    effects.changed = true;
                    if self.is_final_word() {
                        effects.ended = self.end(Outcome::Completed);
                    } else {
                        self.current_word += 1;
                    }
                }
            }
            EventKind::Backspace => effects.changed = self.backspace(),
            EventKind::Interrupt => effects.ended = self.end(Outcome::Interrupted),
            EventKind::Resize => self.resized_since_keystroke = true,
        }
        effects
    }

    fn record(&mut self, input: Input, kind: EventKind, actual: Option<char>) {
        let mut flags = EventFlags {
            in_paste: input.in_paste,
            burst: input.burst,
            ..EventFlags::default()
        };
        if kind.is_keystroke() && !input.in_paste {
            if kind == EventKind::Char && self.started_at_micros.is_none() {
                self.started_at_micros = Some(input.at_micros);
                flags.first_of_session = true;
            }
            flags.after_resize = self.resized_since_keystroke;
            flags.long_pause = self
                .previous_keystroke_at
                .is_some_and(|prev| input.at_micros.saturating_sub(prev) >= LONG_PAUSE_MICROS);
            self.resized_since_keystroke = false;
            if self.started_at_micros.is_some() {
                self.previous_keystroke_at = Some(input.at_micros);
            }
        }

        let expected = kind.is_keystroke().then(|| self.expected());
        self.events.push(InputEvent {
            seq: u32::try_from(self.events.len()).expect("fewer than 2^32 events"),
            at_micros: input.at_micros,
            kind,
            expected,
            actual,
            word_index: self.current_word,
            position: self.position(),
            flags,
        });
    }

    /// Appends a printable character to the current word; returns whether it
    /// was accepted (the extra-character cap may refuse it).
    fn type_char(&mut self, c: char) -> bool {
        let target_len = self.prompt.word(self.current_word).chars().count();
        let word = &mut self.words[self.current_word];
        if word.typed.len() >= target_len + MAX_EXTRAS {
            return false;
        }
        word.typed.push(c);
        true
    }

    /// Removes the last typed character, or re-enters the previous word when
    /// at the start of one and that word was left with an uncorrected error.
    fn backspace(&mut self) -> bool {
        if self.position() > 0 {
            self.words[self.current_word].typed.pop();
            return true;
        }
        if self.current_word > 0 && self.has_uncorrected_error(self.current_word - 1) {
            self.current_word -= 1;
            return true;
        }
        false
    }

    fn is_final_word(&self) -> bool {
        self.current_word + 1 == self.word_count
    }

    fn end(&mut self, outcome: Outcome) -> Option<Outcome> {
        self.outcome = Some(outcome);
        Some(outcome)
    }
}

/// Which event a key records, if any, together with the character it
/// produces. `Esc` and `Ctrl-C` arriving as characters are interrupts; other
/// control characters and non-character keys record nothing.
fn classify(key: Key) -> Option<(EventKind, Option<char>)> {
    match key {
        Key::Char(' ') => Some((EventKind::Space, Some(' '))),
        Key::Char(ESCAPE | CTRL_C) | Key::Interrupt => Some((EventKind::Interrupt, None)),
        Key::Char(c) if c.is_control() => None,
        Key::Char(c) => Some((EventKind::Char, Some(c))),
        Key::Backspace => Some((EventKind::Backspace, None)),
        Key::Resize => Some((EventKind::Resize, None)),
        Key::Other => None,
    }
}

const ESCAPE: char = '\u{1b}';
const CTRL_C: char = '\u{3}';
