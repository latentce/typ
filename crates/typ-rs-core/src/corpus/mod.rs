//! The bundled, frequency-ranked list of English words and the pattern
//! statistics derived from it.
//!
//! The source list is compiled in from `corpus/1grams_english.csv` and turned
//! into the corpus the first time it is used. Words are kept in rank order
//! (most frequent first) and every pattern level is sorted by pattern text.

mod reference;
mod source;

use std::sync::LazyLock;

pub use reference::{REFERENCE_SAMPLE_WORDS, ReferenceDistribution};
pub use source::{CORPUS_VERSION, CorpusError};

const MAX_WORDS: usize = 10_000;

static SOURCE: &str = include_str!("../../corpus/1grams_english.csv");

static BUNDLED: LazyLock<Corpus> =
    LazyLock::new(|| Corpus::from_csv(SOURCE).expect("the vendored source list is well formed"));

/// One corpus word with its frequency weight and length in characters.
///
/// The frequency weight is the square root of the word's share of all
/// occurrences among the words that survive the filter, so weights depend
/// only on the retained words and sum of squares to one.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub text: Box<str>,
    pub frequency_weight: f64,
    pub length: u8,
}

/// Corpus-level frequency of one pattern: the probability that a random slot
/// of running text ends with it. Space is an ordinary character, so `" t"`,
/// `"e "`, and `"e t"` are all patterns. A trigram that straddles two words
/// such as `"e t"` has a frequency but no containing word.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternFrequency {
    pub pattern: Box<str>,
    pub frequency: f64,
    words: Vec<WordId>,
}

impl PatternFrequency {
    /// Every word whose space-padded text contains the pattern, in rank order.
    pub fn words(&self) -> &[WordId] {
        &self.words
    }
}

/// A word's rank in the corpus (0 is the most frequent); stable for a corpus version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WordId(u16);

impl WordId {
    pub fn index(self) -> usize {
        usize::from(self.0)
    }

    fn from_index(index: usize) -> WordId {
        WordId(u16::try_from(index).expect("at most 10,000 words"))
    }
}

/// The corpus words together with their pattern frequencies and inverted index.
#[derive(Debug, Clone, PartialEq)]
pub struct Corpus {
    words: Vec<Word>,
    /// Every word id, ordered by word text, for lookup by text.
    by_text: Vec<WordId>,
    characters: Vec<PatternFrequency>,
    bigrams: Vec<PatternFrequency>,
    trigrams: Vec<PatternFrequency>,
}

impl Corpus {
    /// The corpus compiled into the binary, identified by [`CORPUS_VERSION`].
    pub fn bundled() -> &'static Corpus {
        &BUNDLED
    }

    /// Builds a corpus from a source list in the vendored `ngram,freq,cumshare`
    /// format: lowercase `a–z` words only, single letters other than `a` and
    /// `i` dropped, the 10,000 most frequent survivors kept.
    pub fn from_csv(csv: &str) -> Result<Corpus, CorpusError> {
        source::from_csv(csv)
    }

    /// All words, most frequent first.
    pub fn words(&self) -> &[Word] {
        &self.words
    }

    pub fn word(&self, id: WordId) -> &Word {
        &self.words[id.index()]
    }

    pub fn text(&self, id: WordId) -> &str {
        &self.word(id).text
    }

    /// The word with exactly this text, if it is in the corpus.
    pub fn word_by_text(&self, text: &str) -> Option<&Word> {
        self.by_text
            .binary_search_by(|&id| self.text(id).cmp(text))
            .ok()
            .map(|i| self.word(self.by_text[i]))
    }

    pub fn word_ids(&self) -> impl ExactSizeIterator<Item = WordId> + use<> {
        (0..self.words.len()).map(WordId::from_index)
    }

    pub fn characters(&self) -> &[PatternFrequency] {
        &self.characters
    }

    pub fn bigrams(&self) -> &[PatternFrequency] {
        &self.bigrams
    }

    pub fn trigrams(&self) -> &[PatternFrequency] {
        &self.trigrams
    }

    /// Looks a character, bigram, or trigram up by its text (space included).
    pub fn pattern(&self, text: &str) -> Option<&PatternFrequency> {
        let level = match text.chars().count() {
            1 => &self.characters,
            2 => &self.bigrams,
            3 => &self.trigrams,
            _ => return None,
        };
        level
            .binary_search_by(|p| p.pattern.as_ref().cmp(text))
            .ok()
            .map(|i| &level[i])
    }

    /// The pattern's corpus frequency, or zero if it never occurs.
    pub fn pattern_frequency(&self, text: &str) -> f64 {
        self.pattern(text).map_or(0.0, |p| p.frequency)
    }

    /// Every word whose space-padded text contains the pattern, in rank order.
    pub fn words_containing(&self, text: &str) -> &[WordId] {
        self.pattern(text).map_or(&[], PatternFrequency::words)
    }
}
