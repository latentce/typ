use std::collections::BTreeSet;

use typ_rs_core::corpus::{CORPUS_VERSION, Corpus, PatternFrequency};

fn corpus(rows: &[(&str, u64)]) -> Corpus {
    let mut csv = String::from("ngram,freq,cumshare\n");
    for (word, count) in rows {
        csv.push_str(&format!("{word},{count},0.0\n"));
    }
    Corpus::from_csv(&csv).unwrap()
}

fn texts(corpus: &Corpus) -> Vec<&str> {
    corpus.words().iter().map(|w| &*w.text).collect()
}

fn assert_frequencies(level: &[PatternFrequency], expected: &[(&str, f64)]) {
    let actual: Vec<(&str, f64)> = level.iter().map(|p| (&*p.pattern, p.frequency)).collect();
    assert_eq!(actual.len(), expected.len(), "{actual:?} vs {expected:?}");
    for ((ap, af), (ep, ef)) in actual.iter().zip(expected) {
        assert_eq!(ap, ep, "{actual:?} vs {expected:?}");
        assert!((af - ef).abs() < 1e-12, "{ap}: {af} vs {ef}");
    }
}

#[test]
fn filter_keeps_lowercase_ascii_words_and_only_a_and_i_as_single_letters() {
    let corpus = corpus(&[
        ("the", 100),
        ("I", 90),
        ("a", 80),
        ("God", 70),
        ("i", 60),
        ("b", 50),
        ("don't", 40),
        ("café", 30),
        ("of", 20),
        ("x2", 10),
    ]);
    assert_eq!(texts(&corpus), ["the", "a", "i", "of"]);
}

#[test]
fn filter_keeps_the_ten_thousand_most_frequent_remaining_words_in_rank_order() {
    let total = 10_050;
    let mut rows: Vec<(String, u64)> = (0..total)
        .map(|i| {
            let text: String = (0..4)
                .rev()
                .map(|k| (b'a' + ((i / 26usize.pow(k)) % 26) as u8) as char)
                .collect();
            (text, (total - i) as u64)
        })
        .collect();
    rows.reverse();
    let rows: Vec<(&str, u64)> = rows.iter().map(|(t, c)| (t.as_str(), *c)).collect();

    let corpus = corpus(&rows);

    assert_eq!(corpus.words().len(), 10_000);
    assert_eq!(&*corpus.words()[0].text, "aaaa");
    assert_eq!(&*corpus.words()[9_999].text, rows[50].0);
}

#[test]
fn frequency_weight_is_square_root_of_frequency_normalized_over_retained_words() {
    let corpus = corpus(&[("I", 4), ("to", 3), ("a", 1)]);

    let to = &corpus.words()[0];
    let a = &corpus.words()[1];
    assert_eq!((&*to.text, to.length), ("to", 2));
    assert_eq!((&*a.text, a.length), ("a", 1));
    assert!((to.frequency_weight - 0.75f64.sqrt()).abs() < 1e-12);
    assert!((a.frequency_weight - 0.5).abs() < 1e-12);
}

// Running text is modeled as words drawn by frequency, each followed by a
// space: "to to a to ..." Here 75% of words are "to" (3 slots) and 25% are
// "a" (2 slots), so a word contributes 2.75 slots on average. A pattern's
// frequency is the probability that a random slot ends it.
#[test]
fn pattern_frequencies_are_slot_probabilities_with_space_as_an_ordinary_character() {
    let corpus = corpus(&[("to", 3), ("a", 1)]);

    assert_frequencies(
        corpus.characters(),
        &[
            (" ", 4.0 / 11.0),
            ("a", 1.0 / 11.0),
            ("o", 3.0 / 11.0),
            ("t", 3.0 / 11.0),
        ],
    );
    assert_frequencies(
        corpus.bigrams(),
        &[
            (" a", 1.0 / 11.0),
            (" t", 3.0 / 11.0),
            ("a ", 1.0 / 11.0),
            ("o ", 3.0 / 11.0),
            ("to", 3.0 / 11.0),
        ],
    );
    // "o t" spans two words: 75% of words end in "o" and 75% start with "t".
    assert_frequencies(
        corpus.trigrams(),
        &[
            (" a ", 1.0 / 11.0),
            (" to", 3.0 / 11.0),
            ("a a", 1.0 / 44.0),
            ("a t", 3.0 / 44.0),
            ("o a", 3.0 / 44.0),
            ("o t", 9.0 / 44.0),
            ("to ", 3.0 / 11.0),
        ],
    );
}

#[test]
fn inverted_index_lists_each_containing_word_once_and_nothing_for_cross_word_trigrams() {
    let corpus = corpus(&[("tot", 3), ("a", 1), ("at", 1)]);
    let index = |pattern: &str| -> Vec<usize> {
        corpus
            .words_containing(pattern)
            .iter()
            .map(|id| id.index())
            .collect()
    };

    assert_eq!(index("t"), [0, 2]);
    assert_eq!(index(" "), [0, 1, 2]);
    assert_eq!(index(" t"), [0]);
    assert_eq!(index("t "), [0, 2]);
    assert_eq!(index(" a "), [1]);
    assert!(corpus.pattern("t a").is_some());
    assert_eq!(index("t a"), []);
}

#[test]
fn malformed_source_lists_are_rejected() {
    assert!(Corpus::from_csv("word,count\nthe,1,0.1\n").is_err());
    assert!(Corpus::from_csv("ngram,freq,cumshare\nthe,many,0.1\n").is_err());
    assert!(Corpus::from_csv("ngram,freq,cumshare\nthe,1\n").is_err());
    assert!(Corpus::from_csv("ngram,freq,cumshare\nI,1,0.1\n").is_err());
}

#[test]
fn bundled_corpus_has_a_version_and_at_most_ten_thousand_words_in_rank_order() {
    let corpus = Corpus::bundled();
    let words = corpus.words();

    const { assert!(CORPUS_VERSION >= 1) };
    assert!(
        !words.is_empty() && words.len() <= 10_000,
        "{}",
        words.len()
    );
    assert_eq!(&*words[0].text, "the");
    assert!(
        words
            .windows(2)
            .all(|w| w[0].frequency_weight >= w[1].frequency_weight)
    );
    let distinct: BTreeSet<&str> = texts(corpus).into_iter().collect();
    assert_eq!(distinct.len(), words.len());
}

#[test]
fn each_bundled_pattern_level_is_a_probability_distribution_over_slots() {
    let corpus = Corpus::bundled();
    for (name, level) in [
        ("characters", corpus.characters()),
        ("bigrams", corpus.bigrams()),
        ("trigrams", corpus.trigrams()),
    ] {
        let total: f64 = level.iter().map(|p| p.frequency).sum();
        assert!((total - 1.0).abs() < 1e-9, "{name} sums to {total}");
        assert!(level.iter().all(|p| p.frequency > 0.0), "{name}");
    }
    assert_eq!(corpus.characters().len(), 27);
}

#[test]
fn bundled_inverted_index_agrees_with_the_words_themselves() {
    let corpus = Corpus::bundled();
    let padded: Vec<String> = corpus
        .words()
        .iter()
        .map(|w| format!(" {} ", w.text))
        .collect();

    for (n, level) in [
        (1usize, corpus.characters()),
        (2, corpus.bigrams()),
        (3, corpus.trigrams()),
    ] {
        let mut expected: BTreeSet<(&str, usize)> = BTreeSet::new();
        for (index, text) in padded.iter().enumerate() {
            let start = if n == 1 { 1 } else { 0 };
            for i in start..=text.len() - n {
                expected.insert((&text[i..i + n], index));
            }
        }
        let actual: BTreeSet<(&str, usize)> = level
            .iter()
            .flat_map(|p| p.words().iter().map(move |w| (&*p.pattern, w.index())))
            .collect();
        assert_eq!(actual, expected, "index mismatch at n = {n}");
        assert!(
            level
                .iter()
                .all(|p| p.words().windows(2).all(|w| w[0] < w[1]))
        );
    }
}

#[test]
fn patterns_are_looked_up_by_text_and_map_back_to_words() {
    let corpus = Corpus::bundled();

    let th = corpus.pattern("th").expect("'th' is a bigram");
    let the = corpus.pattern(" th").expect("' th' is a trigram");
    assert!(th.frequency > the.frequency);
    assert!(corpus.pattern_frequency("th") > corpus.pattern_frequency("zq"));
    assert_eq!(corpus.pattern_frequency("zq"), 0.0);
    assert_eq!(corpus.pattern("four"), None);

    let words: Vec<&str> = corpus
        .words_containing(" th")
        .iter()
        .map(|&id| corpus.text(id))
        .collect();
    assert!(words.contains(&"the") && words.contains(&"this"));
    assert!(words.iter().all(|w| w.starts_with("th")));
    assert!(corpus.words_containing("e t").is_empty());
}

#[test]
fn a_word_is_found_by_its_text() {
    let corpus = corpus(&[("the", 100), ("of", 50), ("cat", 10)]);
    let of = corpus.word_by_text("of").unwrap();
    assert_eq!(&*of.text, "of");
    assert_eq!(of.frequency_weight, (50.0f64 / 160.0).sqrt());
    assert_eq!(corpus.word_by_text("dog"), None);
    assert_eq!(corpus.word_by_text(""), None);
    assert_eq!(corpus.word_by_text("The"), None);
}
