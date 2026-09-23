//! Seeded randomness for everything the scheduler and composer draw.
//!
//! One generator per composition, seeded from the session's stored seed,
//! makes a prompt reproducible from its session row. Sampling is written
//! here rather than borrowed from a library so that a seed keeps producing
//! the same draws as long as ChaCha8 and the seed expansion are unchanged.

use rand_chacha::ChaCha8Rng;
use rand_core::SeedableRng;

/// A seeded source of uniform, normal, weighted, and shuffled draws.
#[derive(Debug, Clone)]
pub struct Rng(ChaCha8Rng);

impl Rng {
    /// A generator whose every draw is determined by `seed`.
    pub fn seeded(seed: u64) -> Rng {
        Rng(ChaCha8Rng::seed_from_u64(seed))
    }

    /// A draw from `[0, 1)` with 53 bits of precision.
    pub fn unit(&mut self) -> f64 {
        (rand_core::Rng::next_u64(&mut self.0) >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// A draw from the standard normal distribution, by Box–Muller.
    pub fn standard_normal(&mut self) -> f64 {
        let u = 1.0 - self.unit();
        let v = self.unit();
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }

    /// A draw from `Normal(mean, sd)`; an `sd` of zero returns the mean.
    pub fn normal(&mut self, mean: f64, sd: f64) -> f64 {
        mean + sd * self.standard_normal()
    }

    /// Whether an event of the given probability happened.
    pub fn chance(&mut self, probability: f64) -> bool {
        self.unit() < probability
    }

    /// An index drawn with probability proportional to its weight; `None`
    /// when no weight is positive. Non-positive weights are never drawn.
    pub fn weighted_index(&mut self, weights: impl Iterator<Item = f64>) -> Option<usize> {
        let mut cumulative = Vec::new();
        let mut total = 0.0;
        for w in weights {
            if w > 0.0 {
                total += w;
            }
            cumulative.push(total);
        }
        if total <= 0.0 {
            return None;
        }
        let u = self.unit() * total;
        let i = cumulative.partition_point(|&c| c <= u);
        Some(i.min(cumulative.len() - 1))
    }

    /// Reorders the slice uniformly at random (Fisher–Yates).
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.unit() * (i + 1) as f64) as usize;
            items.swap(i, j.min(i));
        }
    }
}
