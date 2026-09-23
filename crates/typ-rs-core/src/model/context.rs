//! The context model: the part of a slot's latency explained by where the
//! slot sits and what the fingers must do to reach it, rather than by the
//! user's weakness on the pattern.
//!
//! Every slot has a feature vector ([`slot_features`]): its position in its
//! word, the word's length and frequency, whether its bigram crosses a word
//! boundary, and the geometry of the transition from the previous key on the
//! profile's layout. A linear model over those features ([`Coefficients`])
//! is fitted by ridge regression over the bigram-level aggregates the
//! pattern statistics keep: each bigram's mean residual against its mean
//! features, weighted by its evidence. Until the first fit the model has no
//! coefficients and every effect is zero.

use crate::corpus::Corpus;
use crate::layout::Layout;
use crate::prompt::{Prompt, Slot};

/// One context feature of a slot, in the order features are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    /// The slot is its word's first character.
    FirstOfWord,
    /// The slot is its word's last character (not the space after it).
    LastOfWord,
    /// The length of the slot's word in characters; for the space after a
    /// word, the word it closes.
    WordLength,
    /// The natural log of the word's share of corpus occurrences; a word
    /// outside the corpus counts as its rarest word.
    LogWordFrequency,
    /// The slot's bigram includes the word-separating space: the slot is a
    /// word's first character or the space after one.
    Boundary,
    /// The previous key and this one are struck by the same finger.
    SameFinger,
    /// The previous key and this one are struck by the same hand.
    SameHand,
    /// Rows between the previous key and this one.
    RowChange,
    /// Straight-line distance from the previous key to this one, in key
    /// widths.
    KeyDistance,
}

impl Feature {
    pub const ALL: [Feature; FEATURE_COUNT] = [
        Feature::FirstOfWord,
        Feature::LastOfWord,
        Feature::WordLength,
        Feature::LogWordFrequency,
        Feature::Boundary,
        Feature::SameFinger,
        Feature::SameHand,
        Feature::RowChange,
        Feature::KeyDistance,
    ];

    /// The feature's position in a [`Features`] vector.
    pub fn index(self) -> usize {
        self as usize
    }

    /// The feature's name as stored and shown.
    pub fn name(self) -> &'static str {
        match self {
            Feature::FirstOfWord => "first_of_word",
            Feature::LastOfWord => "last_of_word",
            Feature::WordLength => "word_length",
            Feature::LogWordFrequency => "log_word_frequency",
            Feature::Boundary => "boundary",
            Feature::SameFinger => "same_finger",
            Feature::SameHand => "same_hand",
            Feature::RowChange => "row_change",
            Feature::KeyDistance => "key_distance",
        }
    }
}

/// How many features a slot has.
pub const FEATURE_COUNT: usize = 9;

/// A slot's feature values, or sums of them, indexed by [`Feature::index`].
pub type Features = [f64; FEATURE_COUNT];

/// The features of one slot of a prompt, with the transition geometry read
/// from `layout`. The slot's previous key is the character before it in the
/// prompt read as space-padded text, so a word's first character follows
/// the space bar and so does the very first slot of the prompt. A character
/// the layout has no key for leaves the geometry features at zero.
pub fn slot_features(prompt: &Prompt, slot: Slot, layout: Layout, corpus: &Corpus) -> Features {
    let word = prompt.word(slot.word);
    let length = word.chars().count();
    let pattern = prompt.pattern_ending_at(slot);
    let mut keys = pattern.chars().rev();
    let current = keys.next().and_then(|c| layout.key(c));
    let previous = keys.next().and_then(|c| layout.key(c));

    let mut f = [0.0; FEATURE_COUNT];
    f[Feature::FirstOfWord.index()] = f64::from(slot.position == 0);
    f[Feature::LastOfWord.index()] = f64::from(slot.position + 1 == length);
    f[Feature::WordLength.index()] = length as f64;
    f[Feature::LogWordFrequency.index()] = log_word_frequency(corpus, word);
    f[Feature::Boundary.index()] = f64::from(slot.position == 0 || slot.position == length);
    if let (Some(from), Some(to)) = (previous, current) {
        f[Feature::SameFinger.index()] =
            f64::from(from.hand == to.hand && from.hand.is_some() && from.finger == to.finger);
        f[Feature::SameHand.index()] = f64::from(from.hand == to.hand && from.hand.is_some());
        f[Feature::RowChange.index()] = f64::from(from.row.abs_diff(to.row));
        f[Feature::KeyDistance.index()] = from.distance_to(&to);
    }
    f
}

/// The natural log of a word's share of corpus occurrences. A word the
/// corpus does not have is taken to be as rare as its rarest word.
pub fn log_word_frequency(corpus: &Corpus, word: &str) -> f64 {
    let word = corpus
        .word_by_text(word)
        .or_else(|| corpus.words().last())
        .expect("a corpus has at least one word");
    2.0 * word.frequency_weight.ln()
}

/// One row of the regression: a bigram's evidence, mean features, and mean
/// residual.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aggregate {
    /// The bigram's total latency weight, `S0`.
    pub weight: f64,
    /// The bigram's mean feature vector over its latency observations.
    pub features: Features,
    /// The bigram's mean session-adjusted log-latency residual.
    pub residual: f64,
}

/// A fitted linear model of the context effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefficients {
    pub intercept: f64,
    pub weights: Features,
}

impl Coefficients {
    /// The context effect of a slot with these features, in log-latency.
    pub fn effect(&self, features: &Features) -> f64 {
        self.intercept
            + self
                .weights
                .iter()
                .zip(features)
                .map(|(w, f)| w * f)
                .sum::<f64>()
    }

    /// Weighted ridge regression of residual on features. The model has an
    /// intercept, so a shift common to every bigram (the mean residual) is
    /// absorbed there rather than forced onto the features; it is not
    /// penalised: features and residuals are centred at their weighted
    /// means, the weights are solved from the centred normal equations with
    /// `lambda` added to the diagonal, and the intercept is what makes the
    /// weighted mean residual come out exactly. `None` when the rows carry
    /// no weight at all.
    pub fn fit(
        aggregates: impl IntoIterator<Item = Aggregate>,
        lambda: f64,
    ) -> Option<Coefficients> {
        let rows: Vec<Aggregate> = aggregates.into_iter().filter(|a| a.weight > 0.0).collect();
        let total: f64 = rows.iter().map(|a| a.weight).sum();
        if total <= 0.0 {
            return None;
        }

        let mut mean_features = [0.0; FEATURE_COUNT];
        let mut mean_residual = 0.0;
        for a in &rows {
            for (m, f) in mean_features.iter_mut().zip(&a.features) {
                *m += a.weight * f / total;
            }
            mean_residual += a.weight * a.residual / total;
        }

        let mut normal = [[0.0; FEATURE_COUNT]; FEATURE_COUNT];
        let mut moment = [0.0; FEATURE_COUNT];
        for a in &rows {
            let centred: Features = std::array::from_fn(|i| a.features[i] - mean_features[i]);
            let residual = a.residual - mean_residual;
            for i in 0..FEATURE_COUNT {
                for j in 0..FEATURE_COUNT {
                    normal[i][j] += a.weight * centred[i] * centred[j];
                }
                moment[i] += a.weight * centred[i] * residual;
            }
        }
        for (i, row) in normal.iter_mut().enumerate() {
            row[i] += lambda;
        }

        let weights = solve(normal, moment);
        let intercept = mean_residual
            - weights
                .iter()
                .zip(&mean_features)
                .map(|(w, m)| w * m)
                .sum::<f64>();
        Some(Coefficients { intercept, weights })
    }
}

/// Solves `a x = b` by Gaussian elimination with partial pivoting. The
/// matrix is a ridge-regularised normal matrix, so it is positive definite
/// and never singular; a pivot that is nonetheless zero (only possible with
/// a zero ridge and degenerate rows) contributes a zero coordinate.
fn solve(mut a: [[f64; FEATURE_COUNT]; FEATURE_COUNT], mut b: Features) -> Features {
    for col in 0..FEATURE_COUNT {
        let pivot = (col..FEATURE_COUNT)
            .max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))
            .expect("a column has rows");
        a.swap(col, pivot);
        b.swap(col, pivot);
        if a[col][col] == 0.0 {
            continue;
        }
        for row in col + 1..FEATURE_COUNT {
            let factor = a[row][col] / a[col][col];
            if factor == 0.0 {
                continue;
            }
            let pivot_row = a[col];
            for (entry, pivot_entry) in a[row][col..].iter_mut().zip(&pivot_row[col..]) {
                *entry -= factor * pivot_entry;
            }
            b[row] -= factor * b[col];
        }
    }
    let mut x = [0.0; FEATURE_COUNT];
    for col in (0..FEATURE_COUNT).rev() {
        if a[col][col] == 0.0 {
            continue;
        }
        let rest: f64 = (col + 1..FEATURE_COUNT).map(|k| a[col][k] * x[k]).sum();
        x[col] = (b[col] - rest) / a[col][col];
    }
    x
}

/// A profile's context model: how many of its sessions have completed, and
/// the coefficients of the last fit, if there has been one.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ContextModel {
    /// Completed sessions applied so far; the fit cadence counts these.
    pub completed_sessions: u32,
    /// `None` until the first fit, when every context effect is zero.
    pub coefficients: Option<Coefficients>,
}

impl ContextModel {
    /// The context effect of a slot with these features; zero before the
    /// first fit.
    pub fn effect(&self, features: &Features) -> f64 {
        self.coefficients.map_or(0.0, |c| c.effect(features))
    }
}
