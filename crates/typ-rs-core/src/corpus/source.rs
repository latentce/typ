//! Derives the corpus from the vendored `ngram,freq,cumshare` source list.

use super::{Corpus, MAX_WORDS, PatternFrequency, Word, WordId};

/// Identifies the source list together with the rules in this file. Bump
/// whenever the list, the filter, or the frequency definitions change, so
/// stored sessions can tell which corpus they used.
pub const CORPUS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusError(pub String);

impl std::fmt::Display for CorpusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CorpusError {}

struct Record<'a> {
    word: &'a str,
    count: u64,
}

pub(super) fn from_csv(csv: &str) -> Result<Corpus, CorpusError> {
    let mut kept: Vec<Record> = parse(csv)?
        .into_iter()
        .filter(|r| passes_filter(r.word))
        .collect();
    kept.sort_by_key(|r| std::cmp::Reverse(r.count));
    kept.truncate(MAX_WORDS);
    if kept.is_empty() {
        return Err(CorpusError("no words survive the filter".into()));
    }

    let total: f64 = kept.iter().map(|r| r.count as f64).sum();
    let words: Vec<Word> = kept
        .iter()
        .map(|r| Word {
            text: r.word.into(),
            frequency_weight: (r.count as f64 / total).sqrt(),
            length: r.word.len() as u8,
        })
        .collect();
    let [characters, bigrams, trigrams] = pattern_levels(&words);

    Ok(Corpus {
        words,
        characters,
        bigrams,
        trigrams,
    })
}

fn parse(csv: &str) -> Result<Vec<Record<'_>>, CorpusError> {
    let mut lines = csv.lines();
    match lines.next() {
        Some("ngram,freq,cumshare") => {}
        other => return Err(CorpusError(format!("unexpected header {other:?}"))),
    }
    lines
        .enumerate()
        .filter(|(_, line)| !line.is_empty())
        .map(|(i, line)| {
            let line_number = i + 2;
            let mut fields = line.split(',');
            let (Some(word), Some(count), Some(_share), None) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                return Err(CorpusError(format!(
                    "line {line_number}: expected three fields"
                )));
            };
            let count = count.parse::<u64>().map_err(|e| {
                CorpusError(format!("line {line_number}: bad count {count:?}: {e}"))
            })?;
            Ok(Record { word, count })
        })
        .collect()
}

/// Lowercase `a–z` only; single letters other than `a` and `i` are dropped.
fn passes_filter(word: &str) -> bool {
    let lowercase_ascii = !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase());
    lowercase_ascii && (word.len() >= 2 || word == "a" || word == "i")
}

/// Accumulates one level of patterns: probability mass and containing words.
///
/// Patterns are at most three symbols from a 27-symbol alphabet (space and
/// `a–z`), so each level is a dense table indexed by the pattern's base-27
/// encoding. Space encodes as 0, so table order is lexicographic order.
struct Level {
    len: usize,
    entries: Vec<Accumulated>,
}

#[derive(Default, Clone)]
struct Accumulated {
    mass: f64,
    words: Vec<WordId>,
}

const ALPHABET: usize = 27;

fn symbol(byte: u8) -> usize {
    match byte {
        b' ' => 0,
        b'a'..=b'z' => usize::from(byte - b'a') + 1,
        other => panic!("{other:?} is not a corpus character"),
    }
}

fn byte(symbol: usize) -> u8 {
    match symbol {
        0 => b' ',
        1..=26 => b'a' + (symbol - 1) as u8,
        other => panic!("{other} is not a symbol"),
    }
}

impl Level {
    fn new(len: usize) -> Self {
        Self {
            len,
            entries: vec![Accumulated::default(); ALPHABET.pow(len as u32)],
        }
    }

    fn add(&mut self, pattern: &[u8], mass: f64) -> &mut Accumulated {
        debug_assert_eq!(pattern.len(), self.len);
        let index = pattern.iter().fold(0, |acc, &b| acc * ALPHABET + symbol(b));
        let entry = &mut self.entries[index];
        entry.mass += mass;
        entry
    }

    fn add_in_word(&mut self, pattern: &[u8], mass: f64, word: WordId) {
        let entry = self.add(pattern, mass);
        if entry.words.last() != Some(&word) {
            entry.words.push(word);
        }
    }

    fn finish(self, slots_per_word: f64) -> Vec<PatternFrequency> {
        let len = self.len;
        self.entries
            .into_iter()
            .enumerate()
            .filter(|(_, entry)| entry.mass > 0.0)
            .map(|(mut index, Accumulated { mass, words })| {
                let mut bytes = vec![0u8; len];
                for slot in bytes.iter_mut().rev() {
                    *slot = byte(index % ALPHABET);
                    index /= ALPHABET;
                }
                PatternFrequency {
                    pattern: String::from_utf8(bytes).expect("ASCII").into(),
                    frequency: mass / slots_per_word,
                    words,
                }
            })
            .collect()
    }
}

/// Running text is modelled as words drawn independently by frequency, each
/// followed by a single space. A pattern's frequency is the probability that a
/// random slot of that text ends it, so every level sums to one. Trigrams that
/// straddle two words (`x y`) get the product of the word-end and word-start
/// probabilities; no single word contains them, so their index stays empty.
fn pattern_levels(words: &[Word]) -> [Vec<PatternFrequency>; 3] {
    let mut characters = Level::new(1);
    let mut bigrams = Level::new(2);
    let mut trigrams = Level::new(3);
    let mut word_end = [0.0f64; ALPHABET];
    let mut word_start = [0.0f64; ALPHABET];
    let mut slots_per_word = 0.0;

    for (index, word) in words.iter().enumerate() {
        let id = WordId::from_index(index);
        let f = word.frequency_weight * word.frequency_weight;
        let padded: Vec<u8> = [b" ", word.text.as_bytes(), b" "].concat();
        slots_per_word += f * (padded.len() - 1) as f64;
        for c in &padded[1..] {
            characters.add_in_word(&[*c], f, id);
        }
        for bigram in padded.windows(2) {
            bigrams.add_in_word(bigram, f, id);
        }
        for trigram in padded.windows(3) {
            trigrams.add_in_word(trigram, f, id);
        }
        word_end[symbol(padded[padded.len() - 2])] += f;
        word_start[symbol(padded[1])] += f;
    }

    for (last, &end_mass) in word_end.iter().enumerate() {
        for (first, &start_mass) in word_start.iter().enumerate() {
            if end_mass > 0.0 && start_mass > 0.0 {
                trigrams.add(&[byte(last), b' ', byte(first)], end_mass * start_mass);
            }
        }
    }

    [
        characters.finish(slots_per_word),
        bigrams.finish(slots_per_word),
        trigrams.finish(slots_per_word),
    ]
}
