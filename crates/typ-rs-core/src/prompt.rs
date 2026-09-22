//! The fixed sequence of words presented for one session.

/// The words of one session, composed before the session begins.
///
/// Words are separated by single spaces when shown; a word is never empty and
/// never contains whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    words: Vec<Box<str>>,
}

impl Prompt {
    /// Panics if there are no words or a word is empty or contains whitespace;
    /// prompts are composed from the corpus, so either is a programming error.
    pub fn new<I>(words: I) -> Prompt
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let words: Vec<Box<str>> = words
            .into_iter()
            .map(|w| {
                let w = w.as_ref();
                assert!(
                    !w.is_empty() && !w.chars().any(char::is_whitespace),
                    "prompt word {w:?} must be non-empty with no whitespace"
                );
                w.into()
            })
            .collect();
        assert!(!words.is_empty(), "a prompt needs at least one word");
        Prompt { words }
    }

    pub fn words(&self) -> &[Box<str>] {
        &self.words
    }

    pub fn word(&self, index: usize) -> &str {
        &self.words[index]
    }

    pub fn word_count(&self) -> usize {
        self.words.len()
    }

    /// The prompt as shown: words joined by single spaces.
    pub fn text(&self) -> String {
        self.words.join(" ")
    }

    /// The character expected at a slot: the word's character at that
    /// position, or the space that follows the word at the position just past
    /// its end.
    pub fn expected_at(&self, slot: Slot) -> char {
        self.word(slot.word)
            .chars()
            .nth(slot.position)
            .unwrap_or(' ')
    }

    /// The pattern ending at a slot: its expected character preceded by up to
    /// two characters of the prompt read as space-padded text, so that a
    /// word's first character follows a space and its last is followed by
    /// one. Only the first character of the first word has a single
    /// preceding character.
    pub fn pattern_ending_at(&self, slot: Slot) -> String {
        let mut chars = vec![self.expected_at(slot)];
        let mut cursor = slot;
        while chars.len() < PATTERN_LEN {
            match self.preceding(cursor) {
                Some(previous) => {
                    chars.push(self.expected_at(previous));
                    cursor = previous;
                }
                None => {
                    chars.push(' ');
                    break;
                }
            }
        }
        chars.iter().rev().collect()
    }

    /// The slot before `slot` as the prompt reads; `None` before the first
    /// word's first character.
    fn preceding(&self, slot: Slot) -> Option<Slot> {
        if slot.position > 0 {
            Some(Slot {
                word: slot.word,
                position: slot.position - 1,
            })
        } else if slot.word > 0 {
            let word = slot.word - 1;
            Some(Slot {
                word,
                position: self.word(word).chars().count(),
            })
        } else {
            None
        }
    }
}

/// The longest pattern: a character with two preceding characters.
const PATTERN_LEN: usize = 3;

/// One position in the prompt where a specific character is expected.
/// `position` runs from 0 to the word's length inclusive; the last is the
/// space that follows the word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Slot {
    pub word: usize,
    pub position: usize,
}
