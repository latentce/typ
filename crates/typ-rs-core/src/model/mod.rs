//! The user's pattern statistics and how a finished session updates them.
//!
//! Every pattern the user has typed has a [`PatternStats`] row of decaying
//! sums; the empty pattern is the root of the chain and holds the user
//! baseline. A finished session enters the model through
//! [`ModelState::apply_session`], which classifies its intervals against the
//! baseline in force at its start, estimates how fast or slow the session
//! was as a whole, and then adds every clean latency, hesitation, and
//! first-attempt outcome to the chain of the pattern it belongs to. How
//! much a session's latencies count depends on its raw accuracy: speed
//! bought by accepting errors is not speed. Estimates
//! ([`ModelState::estimate`]) shrink each pattern toward its parent, so they
//! are defined before a pattern has any evidence of its own, and separate
//! what the pattern's physical and positional context explains
//! ([`context`]) from what is left as the user's own weakness; the
//! [`Weakness`] of a pattern combines that with its accuracy, consistency,
//! and hesitation shortfalls into one distribution the scheduler samples.
//!
//! Everything here is a cache of the stored sessions: applying the same
//! sessions in the same order to an empty model reproduces it exactly,
//! context model included, because the model is refitted at fixed points
//! in that sequence and nowhere else.

mod config;
pub mod context;
mod weakness;

use std::collections::{BTreeMap, BTreeSet};

pub use config::{ConfigError, SchedulerConfig};
pub use context::ContextModel;
pub use weakness::{Weakness, WeaknessComponents};

use crate::analysis::{HesitationThreshold, IntervalClass, SessionAnalysis, analyze_with, median};
use crate::corpus::{Corpus, ReferenceDistribution};
use crate::layout::Layout;
use crate::prompt::{Prompt, Slot};
use crate::session::{EventKind, Outcome, SessionState};
use context::{Aggregate, Coefficients, Features, slot_features};

/// Identifies the analysis and scheduling algorithm. Bump whenever anything
/// that feeds a cache changes; every cache is stamped with it and rebuilt
/// from the stored sessions when it differs.
pub const MODEL_VERSION: u32 = 5;

/// The pattern text of the root of the chain: the user as a whole.
pub const ROOT: &str = "";

const SECONDS_PER_DAY: f64 = 86_400.0;

/// The decaying sums kept for one pattern. Latency sums are over the
/// session-adjusted log-latency residual `x` (for the root, over log-latency
/// itself, so that its mean is the user baseline); outcome sums count
/// first-attempt slots. Latencies and hesitations are speed evidence and
/// enter at their session's accuracy factor; outcomes enter in full. All
/// decay toward zero with the pattern's half-life, measured from
/// `last_update`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PatternStats {
    /// Total weight of latency observations.
    pub s0: f64,
    /// Weighted sum of `x`.
    pub s1: f64,
    /// Weighted sum of `x²`.
    pub s2: f64,
    /// Sum of squared weights, for the effective sample size.
    pub w2: f64,
    /// Weighted sums of each context feature over the latency observations,
    /// so that `features / s0` is the pattern's mean context. For the root,
    /// the user's typical context.
    pub features: Features,
    /// First-attempt slots typed correctly.
    pub c: f64,
    /// First-attempt errors.
    pub e: f64,
    /// Total weight of hesitations.
    pub h: f64,
    /// When the sums were last decayed to, Unix seconds.
    pub last_update: i64,
}

impl PatternStats {
    /// The effective number of equally weighted observations behind the
    /// latency sums: `S0² / W2`.
    pub fn n_eff(&self) -> f64 {
        if self.w2 > 0.0 {
            self.s0 * self.s0 / self.w2
        } else {
            0.0
        }
    }

    /// The sums as they stand at `at`: every observation weighted by
    /// `exp(−ln 2 · Δt / half_life)`. Time never runs backwards here: an
    /// earlier `at` leaves the sums as they are.
    pub fn decayed_to(mut self, at: i64, half_life_days: f64) -> PatternStats {
        if at > self.last_update {
            let elapsed_days = (at - self.last_update) as f64 / SECONDS_PER_DAY;
            let w = (-std::f64::consts::LN_2 * elapsed_days / half_life_days).exp();
            self.s0 *= w;
            self.s1 *= w;
            self.s2 *= w;
            self.w2 *= w * w;
            for f in &mut self.features {
                *f *= w;
            }
            self.c *= w;
            self.e *= w;
            self.h *= w;
            self.last_update = at;
        }
        self
    }

    /// The pattern's mean context over its latency observations; `None`
    /// without any.
    pub fn mean_features(&self) -> Option<Features> {
        (self.s0 > 0.0).then(|| std::array::from_fn(|i| self.features[i] / self.s0))
    }

    fn observe_latency(&mut self, x: f64, weight: f64, features: &Features) {
        self.s0 += weight;
        self.s1 += weight * x;
        self.s2 += weight * x * x;
        self.w2 += weight * weight;
        for (sum, f) in self.features.iter_mut().zip(features) {
            *sum += weight * f;
        }
    }

    fn observe_outcome(&mut self, correct: f64, error: f64) {
        self.c += correct;
        self.e += error;
    }

    fn observe_hesitation(&mut self, weight: f64) {
        self.h += weight;
    }
}

/// What the model believes about one pattern, every quantity shrunk toward
/// the pattern's parent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PatternEstimate {
    /// The posterior mean of the pattern's log-latency residual: how much
    /// slower (positive) or faster than the user baseline its slot is typed.
    /// What the pattern costs the user, context and all.
    pub absolute_slowness: f64,
    /// The part of that explained by the pattern's typical context: where
    /// it sits in its words and what the fingers must do to reach it. Zero
    /// until the context model has been fitted; a pattern with no latency
    /// evidence of its own takes its parent's.
    pub context_effect: f64,
    /// What is left of the slowness once the context effect is removed: the
    /// speed component of the user's weakness on the pattern.
    pub pattern_effect: f64,
    /// The posterior variance of the residual around that mean.
    pub variance: f64,
    /// The probability that the slot is wrong on the first attempt.
    pub error_probability: f64,
    /// The share of the slot's eligible intervals (clean or hesitation, by
    /// weight) that were hesitations.
    pub hesitation_rate: f64,
    pub n_eff: f64,
}

/// Every pattern's statistics for one profile, with the layout its context
/// features are read from and the context model fitted to them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelState {
    layout: Layout,
    patterns: BTreeMap<Box<str>, PatternStats>,
    dirty: BTreeSet<Box<str>>,
    context: ContextModel,
}

/// What applying a session did.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionUpdate {
    /// The session as interpreted, with intervals classified against the
    /// baseline the observations used.
    pub analysis: SessionAnalysis,
    /// `None` when the session's observations were not applied: an
    /// interrupted session with too few clean intervals.
    pub applied: Option<AppliedObservations>,
    /// How the session's clean typing compared with what the model at its
    /// start predicted. Only for a completed session with at least one
    /// clean interval: an interrupted session reports no speed.
    pub difficulty: Option<DifficultyAdjustment>,
}

/// The quantities a session's observations were measured against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppliedObservations {
    /// The user baseline snapshotted at session start, or the session's own
    /// median clean log-latency when there was none. `None` only when the
    /// session had no clean interval at all.
    pub user_baseline: Option<f64>,
    /// How much faster or slower than the baseline the session was, in
    /// log-latency, once the context of its slots is accounted for.
    pub session_offset: f64,
    /// The weight every latency and hesitation of the session entered
    /// with: zero at or below the accuracy gate, one from its top.
    pub accuracy_factor: f64,
    pub clean_intervals: usize,
}

/// How a completed session's clean typing compared with the model's
/// prediction for the very same slots, made from the model as it stood
/// at session start with the session offset at zero: what the user would
/// do on a typical day. Both sums run over the session's clean slots and
/// no others, so the slots excluded from the motor estimate do not bias
/// the comparison either way. The prompt's difficulty is in both sums, so
/// the ratio is free of it; applied to the model's prediction for the
/// fixed reference sample, it gives the speed the session translates to
/// on standard text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DifficultyAdjustment {
    /// Seconds the model predicted for the session's clean slots.
    pub expected_clean_seconds: f64,
    /// Seconds those slots actually took.
    pub actual_clean_seconds: f64,
    /// Words per minute the model predicted for the reference sample.
    pub predicted_reference_wpm: f64,
}

impl DifficultyAdjustment {
    /// How much faster (above one) or slower than predicted the session
    /// was typed.
    pub fn adjusted_ratio(&self) -> f64 {
        self.expected_clean_seconds / self.actual_clean_seconds
    }

    /// The reference-equivalent WPM: the speed the user would have shown on
    /// the reference sample, typing as they did this session.
    pub fn reference_wpm(&self) -> f64 {
        self.predicted_reference_wpm * self.adjusted_ratio()
    }
}

impl ModelState {
    /// An empty model for the default layout.
    pub fn new() -> ModelState {
        ModelState::default()
    }

    /// Rebuilds a model from stored rows.
    pub fn from_rows(
        layout: Layout,
        rows: impl IntoIterator<Item = (Box<str>, PatternStats)>,
        context: ContextModel,
    ) -> ModelState {
        ModelState {
            layout,
            patterns: rows.into_iter().collect(),
            dirty: BTreeSet::new(),
            context,
        }
    }

    /// The layout the model's context features are read from: the one the
    /// profile is bound to.
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// The context model as last fitted, and how many completed sessions
    /// have been applied.
    pub fn context_model(&self) -> &ContextModel {
        &self.context
    }

    /// The stored sums of a pattern, as last updated; `None` for a pattern
    /// never observed.
    pub fn stats(&self, pattern: &str) -> Option<PatternStats> {
        self.patterns.get(pattern).copied()
    }

    /// The root's sums: latency sums over log-latency, outcome sums over
    /// every slot.
    pub fn user_baseline_stats(&self) -> PatternStats {
        self.stats(ROOT).unwrap_or_default()
    }

    /// The user's typical clean log-latency in seconds; `None` until a clean
    /// interval has been observed.
    pub fn user_baseline(&self) -> Option<f64> {
        let root = self.user_baseline_stats();
        (root.s0 > 0.0).then(|| root.s1 / root.s0)
    }

    /// Every pattern with statistics, root included, in text order.
    pub fn patterns(&self) -> impl Iterator<Item = (&str, &PatternStats)> {
        self.patterns.iter().map(|(p, s)| (p.as_ref(), s))
    }

    /// The patterns changed since the model was loaded or last marked clean.
    pub fn dirty(&self) -> impl Iterator<Item = (&str, &PatternStats)> {
        self.dirty.iter().map(|p| (p.as_ref(), &self.patterns[p]))
    }

    pub fn mark_clean(&mut self) {
        self.dirty.clear();
    }

    /// The latest time any pattern was updated to; `None` for an empty model.
    pub fn last_update(&self) -> Option<i64> {
        self.patterns.values().map(|s| s.last_update).max()
    }

    /// What the model believes about a pattern as of `at`, every sum decayed
    /// to that time. Works for a pattern never observed: it inherits its
    /// parent's estimate.
    pub fn estimate(&self, pattern: &str, at: i64, config: &SchedulerConfig) -> PatternEstimate {
        if pattern.is_empty() {
            return self.root_estimate(at, config);
        }
        let parent = self.estimate(parent_of(pattern), at, config);
        let s = self
            .stats(pattern)
            .unwrap_or_default()
            .decayed_to(at, config.pattern_half_life_days);
        let k = config.kappa;
        let mean = (s.s1 + k * parent.absolute_slowness) / (s.s0 + k);
        let spread = s.s2 - 2.0 * mean * s.s1 + mean * mean * s.s0;
        let context_effect = s
            .mean_features()
            .map_or(parent.context_effect, |f| self.context.effect(&f));
        PatternEstimate {
            absolute_slowness: mean,
            context_effect,
            pattern_effect: mean - context_effect,
            variance: (spread + k * parent.variance) / (s.s0 + k),
            error_probability: (s.e + k * parent.error_probability) / (s.c + s.e + k),
            hesitation_rate: (s.h + k * parent.hesitation_rate) / (s.s0 + s.h + k),
            n_eff: s.n_eff(),
        }
    }

    /// The user as a whole: residuals are measured from the baseline, so the
    /// root's slowness is zero by construction; its variance, error
    /// probability, and hesitation rate shrink toward the configured priors.
    fn root_estimate(&self, at: i64, config: &SchedulerConfig) -> PatternEstimate {
        let s = self
            .user_baseline_stats()
            .decayed_to(at, config.baseline_half_life_days);
        let k = config.kappa;
        let spread = if s.s0 > 0.0 {
            s.s2 - s.s1 * s.s1 / s.s0
        } else {
            0.0
        };
        let prior_errors = config.root_prior_errors;
        let prior_trials = config.root_prior_errors + config.root_prior_correct;
        PatternEstimate {
            absolute_slowness: 0.0,
            context_effect: 0.0,
            pattern_effect: 0.0,
            variance: (spread + k * config.latency_variance_prior) / (s.s0 + k),
            error_probability: (s.e + prior_errors) / (s.c + s.e + prior_trials),
            hesitation_rate: (s.h + prior_errors) / (s.s0 + s.h + prior_trials),
            n_eff: s.n_eff(),
        }
    }

    /// The log-latency the model predicts for one slot of a prompt on a
    /// typical day: the user baseline, the context effect of the slot's
    /// own features, and the pattern effect of the pattern ending at it.
    /// `None` until the model has a baseline.
    pub fn predicted_log_latency(
        &self,
        prompt: &Prompt,
        slot: Slot,
        at: i64,
        corpus: &Corpus,
        config: &SchedulerConfig,
    ) -> Option<f64> {
        let baseline = self.user_baseline()?;
        let pattern = prompt.pattern_ending_at(slot);
        let features = slot_features(prompt, slot, self.layout, corpus);
        Some(Predictor::new(self, baseline, at, config).log_latency(&pattern, &features))
    }

    /// The words per minute the model predicts for typing a prompt on a
    /// typical day: its characters over the seconds predicted for every
    /// slot but the first, whose incoming latency a session never
    /// measures. `None` until the model has a baseline.
    pub fn predicted_wpm(
        &self,
        prompt: &Prompt,
        at: i64,
        corpus: &Corpus,
        config: &SchedulerConfig,
    ) -> Option<f64> {
        let baseline = self.user_baseline()?;
        Some(Predictor::new(self, baseline, at, config).wpm(prompt, corpus))
    }

    /// Enters a finished session that started at `started_at` (Unix seconds)
    /// into the model. Every pattern touched is decayed to `started_at`
    /// first, so what an observation adds depends on the time elapsed since
    /// the pattern was last seen, not on how many sessions came between.
    /// The corpus supplies the word frequencies of the prompt's slots. A
    /// completed session advances the fit cadence and, on every
    /// `context_refit_sessions`th one, the context model is refitted to the
    /// bigram statistics as they then stand.
    pub fn apply_session(
        &mut self,
        state: &SessionState,
        started_at: i64,
        corpus: &Corpus,
        config: &SchedulerConfig,
    ) -> SessionUpdate {
        let snapshot = self.user_baseline();
        let threshold = snapshot.map_or(
            HesitationThreshold::RunningMedian,
            HesitationThreshold::UserBaseline,
        );
        let analysis = analyze_with(state, threshold);

        let prompt = state.prompt();
        let clean: Vec<CleanInterval> = analysis
            .intervals
            .iter()
            .filter(|i| i.class == IntervalClass::Clean)
            .filter_map(|i| {
                let micros = i.latency_micros?;
                Some(CleanInterval {
                    pattern: &i.pattern,
                    log_latency: (micros as f64 / 1_000_000.0).ln(),
                    features: slot_features(prompt, i.slot, self.layout, corpus),
                })
            })
            .collect();
        let completed = state.outcome() == Some(Outcome::Completed);
        if !completed && clean.len() < config.interrupted_min_clean_intervals {
            return SessionUpdate {
                analysis,
                applied: None,
                difficulty: None,
            };
        }

        let accuracy_factor = accuracy_factor(analysis.metrics.raw_accuracy, config);
        let baseline = snapshot.or_else(|| {
            let mut sorted: Vec<f64> = clean.iter().map(|c| c.log_latency).collect();
            sorted.sort_by(f64::total_cmp);
            median(&sorted, |a, b| (a + b) / 2.0)
        });
        let session_offset = baseline.map_or(0.0, |b| {
            let mut residuals: Vec<f64> = clean
                .iter()
                .map(|c| c.log_latency - b - self.context.effect(&c.features))
                .collect();
            residuals.sort_by(f64::total_cmp);
            let n = residuals.len() as f64;
            median(&residuals, |a, b| (a + b) / 2.0).unwrap_or(0.0) * n
                / (n + config.offset_regulariser)
        });
        let difficulty = match baseline {
            Some(baseline) if completed && !clean.is_empty() => {
                let mut predictor = Predictor::new(self, baseline, started_at, config);
                let expected_clean_seconds = clean
                    .iter()
                    .map(|c| predictor.log_latency(c.pattern, &c.features).exp())
                    .sum();
                let actual_clean_seconds = clean.iter().map(|c| c.log_latency.exp()).sum();
                let sample = ReferenceDistribution::new(corpus).fixed_sample(corpus);
                Some(DifficultyAdjustment {
                    expected_clean_seconds,
                    actual_clean_seconds,
                    predicted_reference_wpm: predictor.wpm(&sample, corpus),
                })
            }
            _ => None,
        };

        let mut observer = Observer {
            model: self,
            at: started_at,
            config,
        };
        for c in &clean {
            let baseline = baseline.expect("a clean interval implies a baseline");
            let x = c.log_latency - baseline - session_offset;
            observer.chain(c.pattern, |s| {
                s.observe_latency(x, accuracy_factor, &c.features)
            });
            observer.root(|s| s.observe_latency(c.log_latency, accuracy_factor, &c.features));
        }
        for interval in &analysis.intervals {
            if matches!(interval.class, IntervalClass::Hesitation { .. }) {
                observer.chain(&interval.pattern, |s| s.observe_hesitation(accuracy_factor));
                observer.root(|s| s.observe_hesitation(accuracy_factor));
            }
        }
        observer.outcomes(state, &analysis);

        if completed {
            self.context.completed_sessions += 1;
            let cadence = config.context_refit_sessions;
            if cadence > 0 && self.context.completed_sessions as usize % cadence == 0 {
                self.refit_context(started_at, config);
            }
        }

        let clean_intervals = clean.len();
        SessionUpdate {
            analysis,
            applied: Some(AppliedObservations {
                user_baseline: baseline,
                session_offset,
                accuracy_factor,
                clean_intervals,
            }),
            difficulty,
        }
    }

    /// Refits the context model to every bigram with latency evidence, as
    /// of `at`. A fit with no bigram evidence leaves the coefficients as
    /// they were.
    fn refit_context(&mut self, at: i64, config: &SchedulerConfig) {
        let aggregates = self
            .patterns
            .iter()
            .filter(|(pattern, _)| pattern.chars().count() == 2)
            .filter_map(|(_, s)| {
                let s = s.decayed_to(at, config.pattern_half_life_days);
                Some(Aggregate {
                    weight: s.s0,
                    features: s.mean_features()?,
                    residual: s.s1 / s.s0,
                })
            });
        if let Some(coefficients) = Coefficients::fit(aggregates, config.context_ridge_lambda) {
            self.context.coefficients = Some(coefficients);
        }
    }

    /// Decays one pattern's sums to `at`, creating them if the pattern is
    /// new, applies `f` to them, and marks the pattern dirty.
    fn touch(
        &mut self,
        pattern: &str,
        at: i64,
        half_life_days: f64,
        f: impl FnOnce(&mut PatternStats),
    ) {
        let stats = self.patterns.entry(pattern.into()).or_insert(PatternStats {
            last_update: at,
            ..PatternStats::default()
        });
        *stats = stats.decayed_to(at, half_life_days);
        f(stats);
        self.dirty.insert(pattern.into());
    }
}

/// One clean interval of a session: where its latency is recorded and the
/// context it was typed in.
struct CleanInterval<'a> {
    pattern: &'a str,
    log_latency: f64,
    features: Features,
}

/// Predicts latencies from a model as it stands, with the session offset at
/// zero. A prompt of a thousand words repeats its patterns many times over,
/// so each pattern's effect is worked out once.
struct Predictor<'a> {
    model: &'a ModelState,
    baseline: f64,
    at: i64,
    config: &'a SchedulerConfig,
    pattern_effects: BTreeMap<Box<str>, f64>,
}

impl<'a> Predictor<'a> {
    fn new(
        model: &'a ModelState,
        baseline: f64,
        at: i64,
        config: &'a SchedulerConfig,
    ) -> Predictor<'a> {
        Predictor {
            model,
            baseline,
            at,
            config,
            pattern_effects: BTreeMap::new(),
        }
    }

    /// The predicted log-latency of a slot with the given pattern and
    /// features.
    fn log_latency(&mut self, pattern: &str, features: &Features) -> f64 {
        let effect = match self.pattern_effects.get(pattern) {
            Some(&effect) => effect,
            None => {
                let effect = self
                    .model
                    .estimate(pattern, self.at, self.config)
                    .pattern_effect;
                self.pattern_effects.insert(pattern.into(), effect);
                effect
            }
        };
        self.baseline + self.model.context.effect(features) + effect
    }

    /// The predicted words per minute for a prompt: its characters
    /// (separating spaces included) over the seconds predicted for every
    /// slot but the first.
    fn wpm(&mut self, prompt: &Prompt, corpus: &Corpus) -> f64 {
        let mut characters = 0usize;
        let mut seconds = 0.0;
        for slot in prompt.slots() {
            characters += 1;
            if slot
                == (Slot {
                    word: 0,
                    position: 0,
                })
            {
                continue;
            }
            let pattern = prompt.pattern_ending_at(slot);
            let features = slot_features(prompt, slot, self.model.layout, corpus);
            seconds += self.log_latency(&pattern, &features).exp();
        }
        characters as f64 / 5.0 / (seconds / 60.0)
    }
}

/// How much a session's speed evidence counts: `clamp((raw − zero) / (full −
/// zero), 0, 1)`, a step at the gate if it has no width.
fn accuracy_factor(raw_accuracy: f64, config: &SchedulerConfig) -> f64 {
    let width = config.accuracy_gate_full - config.accuracy_gate_zero;
    if width <= 0.0 {
        f64::from(raw_accuracy > config.accuracy_gate_zero)
    } else {
        ((raw_accuracy - config.accuracy_gate_zero) / width).clamp(0.0, 1.0)
    }
}

/// Enters one session's observations into the model, all as of the moment
/// the session started.
struct Observer<'a> {
    model: &'a mut ModelState,
    at: i64,
    config: &'a SchedulerConfig,
}

impl Observer<'_> {
    /// Applies `f` to the pattern and every shorter suffix down to one
    /// character.
    fn chain(&mut self, pattern: &str, f: impl Fn(&mut PatternStats)) {
        let mut level = pattern;
        while !level.is_empty() {
            self.model
                .touch(level, self.at, self.config.pattern_half_life_days, &f);
            level = parent_of(level);
        }
    }

    fn root(&mut self, f: impl FnOnce(&mut PatternStats)) {
        self.model
            .touch(ROOT, self.at, self.config.baseline_half_life_days, f);
    }

    /// Every slot of a submitted word is one trial. Its error mass is the
    /// weight of the errors attributed to it, at most one, apportioned to
    /// the patterns they were attributed to; what remains is a correct
    /// outcome for the pattern ending at the slot. A word's following space
    /// is a slot when the space was typed.
    fn outcomes(&mut self, state: &SessionState, analysis: &SessionAnalysis) {
        let prompt = state.prompt();
        let ended_on_space = state
            .events()
            .last()
            .is_some_and(|e| e.kind == EventKind::Space);
        for (index, word) in analysis.words.iter().enumerate() {
            if !word.submitted {
                continue;
            }
            let len = word.target.chars().count();
            let has_space_slot = index + 1 < state.word_count() || ended_on_space;
            let mut error_mass: BTreeMap<usize, f64> = BTreeMap::new();
            for error in &word.errors {
                *error_mass.entry(error.edit.slot()).or_default() += error.weight;
            }
            let slots =
                (0..len).chain((has_space_slot || error_mass.contains_key(&len)).then_some(len));
            for position in slots {
                let total = error_mass.get(&position).copied().unwrap_or(0.0);
                let error = total.min(1.0);
                let scale = if total > 0.0 { error / total } else { 0.0 };
                for attribution in word.errors.iter().filter(|e| e.edit.slot() == position) {
                    let share = attribution.weight * scale;
                    self.chain(&attribution.pattern, |s| s.observe_outcome(0.0, share));
                }
                let correct = 1.0 - error;
                if correct > 0.0 {
                    let pattern = prompt.pattern_ending_at(Slot {
                        word: index,
                        position,
                    });
                    self.chain(&pattern, |s| s.observe_outcome(correct, 0.0));
                }
                self.root(|s| s.observe_outcome(correct, error));
            }
        }
    }
}

/// The next level up the chain: the pattern without its first character.
fn parent_of(pattern: &str) -> &str {
    pattern
        .char_indices()
        .nth(1)
        .map_or("", |(i, _)| &pattern[i..])
}
