//! Decoded terminal inputs and the events recorded from them.

/// A decoded terminal input as the session receives it, before it has been
/// resolved against the prompt. The caller supplies only what it alone knows:
/// when the input arrived, which key it was, and whether it came inside a
/// bracketed paste or later in a batch of pending inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Input {
    /// Monotonic time in microseconds from raw-mode entry.
    pub at_micros: u64,
    pub key: Key,
    pub in_paste: bool,
    pub burst: bool,
}

impl Input {
    pub fn new(at_micros: u64, key: Key) -> Input {
        Input {
            at_micros,
            key,
            in_paste: false,
            burst: false,
        }
    }

    pub fn in_paste(self) -> Input {
        Input {
            in_paste: true,
            ..self
        }
    }

    pub fn burst(self) -> Input {
        Input {
            burst: true,
            ..self
        }
    }
}

/// The key a terminal input decoded to. Space arrives as `Char(' ')`;
/// control characters such as Enter and Tab arrive as `Char` too and are
/// ignored, except `Esc` and `Ctrl-C`, which count as `Interrupt`. `Other` is
/// any non-character key (arrows, function keys) and is likewise ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Backspace,
    Resize,
    /// `Ctrl-C` or `Esc`, already decoded by the terminal.
    Interrupt,
    Other,
}

/// What kind of input an event records. Open: modes added later may record
/// further kinds, so matches outside this crate must have a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EventKind {
    Char,
    Backspace,
    Space,
    Resize,
    Interrupt,
}

impl EventKind {
    /// Whether the event is a key the user pressed to edit, as opposed to a
    /// resize or interrupt.
    pub fn is_keystroke(self) -> bool {
        matches!(
            self,
            EventKind::Char | EventKind::Backspace | EventKind::Space
        )
    }

    /// The kind's stable name, as stored with the event. Never renamed: a
    /// stored session must read back under any later version.
    pub fn name(self) -> &'static str {
        match self {
            EventKind::Char => "char",
            EventKind::Backspace => "backspace",
            EventKind::Space => "space",
            EventKind::Resize => "resize",
            EventKind::Interrupt => "interrupt",
        }
    }

    /// The kind with the given stored name, if this version knows it.
    pub fn from_name(name: &str) -> Option<EventKind> {
        match name {
            "char" => Some(EventKind::Char),
            "backspace" => Some(EventKind::Backspace),
            "space" => Some(EventKind::Space),
            "resize" => Some(EventKind::Resize),
            "interrupt" => Some(EventKind::Interrupt),
            _ => None,
        }
    }
}

/// Quality flags recorded with an event so that the analysis can decide
/// which intervals carry motor evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventFlags {
    /// The keystroke that starts the session: the first printable character
    /// not pasted. Keystrokes before it are recorded but carry no timing
    /// flags.
    pub first_of_session: bool,
    /// The first keystroke after a terminal resize.
    pub after_resize: bool,
    /// Arrived inside a bracketed paste; recorded but never applied.
    pub in_paste: bool,
    /// Arrived after the first input of a batch read together.
    pub burst: bool,
    /// At least [`LONG_PAUSE_MICROS`] since the previous keystroke. A coarse
    /// capture-time marker; the analysis derives hesitations from timestamps.
    pub long_pause: bool,
}

/// The gap from the previous keystroke at which an event is flagged
/// `long_pause`: the floor of the hesitation threshold.
pub const LONG_PAUSE_MICROS: u64 = 1_500_000;

/// One recorded input, resolved against the prompt at the moment it arrived:
/// `word_index` and `position` are the caret before the event was applied,
/// `expected` the character the prompt wanted there (the following space once
/// the word is fully typed), `actual` the character the key produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputEvent {
    pub seq: u32,
    pub at_micros: u64,
    pub kind: EventKind,
    /// `None` only for events that are not keystrokes.
    pub expected: Option<char>,
    /// `None` for backspace and for events that are not keystrokes.
    pub actual: Option<char>,
    pub word_index: usize,
    pub position: usize,
    pub flags: EventFlags,
}

impl InputEvent {
    /// The input that produced this event, so that a stored event log can be
    /// applied again. The resolved fields and timing flags are dropped: the
    /// session derives them afresh.
    pub fn input(&self) -> Input {
        let key = match self.kind {
            EventKind::Char => Key::Char(self.actual.expect("a char event carries its character")),
            EventKind::Space => Key::Char(' '),
            EventKind::Backspace => Key::Backspace,
            EventKind::Resize => Key::Resize,
            EventKind::Interrupt => Key::Interrupt,
        };
        Input {
            at_micros: self.at_micros,
            key,
            in_paste: self.flags.in_paste,
            burst: self.flags.burst,
        }
    }
}
