use std::collections::HashMap;

use typ_rs_core::corpus::{Corpus, REFERENCE_SAMPLE_WORDS, ReferenceDistribution, WordId};

fn draw(seed: u64, n: usize) -> Vec<&'static str> {
    let corpus = Corpus::bundled();
    ReferenceDistribution::new(corpus)
        .seeded_sampler(seed)
        .take(n)
        .map(|id| corpus.text(id))
        .collect()
}

#[test]
fn the_same_seed_reproduces_the_same_words_and_a_different_seed_does_not() {
    assert_eq!(draw(7, 50), draw(7, 50));
    assert_ne!(draw(7, 50), draw(8, 50));
    assert_eq!(draw(7, 50).len(), 50);
}

#[test]
fn probability_is_proportional_to_frequency_weight() {
    let corpus = Corpus::bundled();
    let reference = ReferenceDistribution::new(corpus);
    let total_weight: f64 = corpus.words().iter().map(|w| w.frequency_weight).sum();

    let the = corpus.word_ids().next().unwrap();
    assert_eq!(corpus.text(the), "the");
    let expected = corpus.word(the).frequency_weight / total_weight;
    assert!((reference.probability(the) - expected).abs() < 1e-12);

    let sum: f64 = corpus.word_ids().map(|id| reference.probability(id)).sum();
    assert!((sum - 1.0).abs() < 1e-9);
}

#[test]
fn sampling_follows_the_frequency_weights_not_the_raw_frequencies() {
    let corpus = Corpus::bundled();
    let reference = ReferenceDistribution::new(corpus);
    let n = 200_000;

    let mut counts: HashMap<WordId, usize> = HashMap::new();
    for id in reference.seeded_sampler(42).take(n) {
        *counts.entry(id).or_default() += 1;
    }

    let the = corpus.word_ids().next().unwrap();
    let expected = reference.probability(the) * n as f64;
    let sd = (expected * (1.0 - reference.probability(the))).sqrt();
    let observed = counts[&the] as f64;
    assert!(
        (observed - expected).abs() < 5.0 * sd,
        "'the' drawn {observed} times, expected {expected:.0} ± {sd:.0}"
    );
    // Under raw frequency "the" alone would be over 5% of draws.
    assert!(observed < 0.02 * n as f64);
    assert!(
        counts.len() > 5_000,
        "only {} distinct words drawn",
        counts.len()
    );
}

#[test]
fn the_fixed_reference_sample_is_the_same_thousand_words_every_time() {
    let corpus = Corpus::bundled();
    let reference = ReferenceDistribution::new(corpus);
    let sample = reference.fixed_sample(corpus);
    assert_eq!(sample.word_count(), REFERENCE_SAMPLE_WORDS);
    assert_eq!(sample, reference.fixed_sample(corpus));
    // Drawn from the distribution, not the rank list: common words repeat.
    let the = sample
        .words()
        .iter()
        .filter(|w| w.as_ref() == "the")
        .count();
    assert!(the > 5, "'the' appears {the} times");
}
