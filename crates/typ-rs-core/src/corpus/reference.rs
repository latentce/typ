use super::{Corpus, WordId};
use crate::random::Rng;

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

    fn total(&self) -> f64 {
        *self.cumulative.last().expect("corpus is not empty")
    }
}
