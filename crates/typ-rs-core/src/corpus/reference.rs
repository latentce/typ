use super::{Corpus, WordId};
use crate::prompt::Prompt;
use crate::random::Rng;

/// How many words the fixed reference sample has.
pub const REFERENCE_SAMPLE_WORDS: usize = 1_000;

/// The seed the fixed reference sample is drawn with. Never changes: the
/// sample changes only when the corpus does.
const REFERENCE_SAMPLE_SEED: u64 = 0x7265_6665_7265_6e63;

/// The frozen distribution over corpus words from which probes are drawn:
/// probability proportional to frequency weight, independent of the user.
#[derive(Debug, Clone)]
pub struct ReferenceDistribution {
    cumulative: Vec<f64>,
}

impl ReferenceDistribution {
    pub fn new(corpus: &Corpus) -> Self {
        let mut running = 0.0;
        let cumulative = corpus
            .words()
            .iter()
            .map(|w| {
                running += w.frequency_weight;
                running
            })
            .collect();
        Self { cumulative }
    }

    pub fn probability(&self, id: WordId) -> f64 {
        let i = id.index();
        let below = if i == 0 { 0.0 } else { self.cumulative[i - 1] };
        (self.cumulative[i] - below) / self.total()
    }

    /// Draws one word using the caller's generator.
    pub fn sample(&self, rng: &mut Rng) -> WordId {
        let u = rng.unit() * self.total();
        let i = self.cumulative.partition_point(|&c| c <= u);
        WordId::from_index(i.min(self.cumulative.len() - 1))
    }

    /// An endless, reproducible stream of words for the given seed.
    pub fn seeded_sampler(&self, seed: u64) -> impl Iterator<Item = WordId> + '_ {
        let mut rng = Rng::seeded(seed);
        std::iter::from_fn(move || Some(self.sample(&mut rng)))
    }

    /// The fixed reference sample: [`REFERENCE_SAMPLE_WORDS`] words drawn
    /// from the distribution with a fixed seed, so the same words for a
    /// given corpus version. The standard prompt a session's speed is
    /// translated onto so that sessions of different difficulty compare.
    pub fn fixed_sample(&self, corpus: &Corpus) -> Prompt {
        Prompt::new(
            self.seeded_sampler(REFERENCE_SAMPLE_SEED)
                .take(REFERENCE_SAMPLE_WORDS)
                .map(|id| corpus.text(id)),
        )
    }

    fn total(&self) -> f64 {
        *self.cumulative.last().expect("corpus is not empty")
    }
}
