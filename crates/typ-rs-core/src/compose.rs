//! Composes the prompt for a session.
//!
//! Only frequency-weighted composition exists: every word is drawn from the
//! reference distribution, so the whole prompt consists of probes. Targeted
//! composition is built on top of this once there are pattern statistics to
//! target.

use crate::corpus::{Corpus, ReferenceDistribution};
use crate::prompt::Prompt;

/// Draws `word_count` words from the reference distribution. The same seed
/// composes the same prompt for a given corpus version.
pub fn frequency_weighted(corpus: &Corpus, word_count: usize, seed: u64) -> Prompt {
    Prompt::new(
        ReferenceDistribution::new(corpus)
            .seeded_sampler(seed)
            .take(word_count)
            .map(|id| corpus.text(id)),
    )
}
