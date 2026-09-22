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
}
