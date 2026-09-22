use rand_chacha::ChaCha8Rng;
use rand_core::{Rng, SeedableRng};

use super::{Corpus, WordId};

/// The frozen distribution over corpus words from which probes are drawn:
/// probability proportional to frequency weight, independent of the user.
///
/// Sampling is implemented here rather than borrowed from a library so that
/// a seed reproduces the same words as long as ChaCha8 and `rand_core`'s
/// seed expansion are unchanged; only those and this inversion step are
/// involved.
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
    fn sample(&self, rng: &mut impl Rng) -> WordId {
        let u = unit_interval(rng.next_u64()) * self.total();
        let i = self.cumulative.partition_point(|&c| c <= u);
        WordId::from_index(i.min(self.cumulative.len() - 1))
    }

    /// An endless, reproducible stream of words for the given seed.
    pub fn seeded_sampler(&self, seed: u64) -> impl Iterator<Item = WordId> + '_ {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        std::iter::from_fn(move || Some(self.sample(&mut rng)))
    }

    fn total(&self) -> f64 {
        *self.cumulative.last().expect("corpus is not empty")
    }
}

/// Maps 64 random bits onto `[0, 1)` using the top 53 bits.
fn unit_interval(bits: u64) -> f64 {
    (bits >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}
