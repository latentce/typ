use std::collections::HashSet;

use typ_rs_core::compose;
use typ_rs_core::corpus::Corpus;

#[test]
fn a_frequency_weighted_prompt_has_the_requested_number_of_corpus_words() {
    let corpus = Corpus::bundled();
    let known: HashSet<&str> = corpus.words().iter().map(|w| w.text.as_ref()).collect();

    for words in [1, 10, 50, 200] {
        let prompt = compose::frequency_weighted(corpus, words, 3);
        assert_eq!(prompt.word_count(), words);
        assert!(prompt.words().iter().all(|w| known.contains(w.as_ref())));
    }
}

#[test]
fn the_same_seed_composes_the_same_prompt_and_a_different_seed_does_not() {
    let corpus = Corpus::bundled();
    assert_eq!(
        compose::frequency_weighted(corpus, 50, 7),
        compose::frequency_weighted(corpus, 50, 7)
    );
    assert_ne!(
        compose::frequency_weighted(corpus, 50, 7),
        compose::frequency_weighted(corpus, 50, 8)
    );
}
