//! The user's pattern statistics and how a finished session updates them.
//!
//! Every pattern the user has typed has a [`PatternStats`] row of decaying
//! sums; the empty pattern is the root of the chain and holds the user
//! baseline. A finished session enters the model through
//! [`ModelState::apply_session`], which classifies its intervals against the
//! baseline in force at its start, estimates how fast or slow the session
//! was as a whole, and then adds every clean latency, hesitation, and
//! first-attempt outcome to the chain of the pattern it belongs to.
//! Estimates ([`ModelState::estimate`]) shrink each pattern toward its
//! parent, so they are defined before a pattern has any evidence of its own.
//!
//! Everything here is a cache of the stored sessions: applying the same
//! sessions in the same order to an empty model reproduces it exactly.

mod config;

use std::collections::{BTreeMap, BTreeSet};

pub use config::{ConfigError, SchedulerConfig};

use crate::analysis::{HesitationThreshold, IntervalClass, SessionAnalysis, analyze_with, median};
use crate::prompt::Slot;
use crate::session::{EventKind, Outcome, SessionState};

/// Identifies the analysis and scheduling algorithm. Bump whenever anything
/// that feeds a cache changes; every cache is stamped with it and rebuilt
/// from the stored sessions when it differs.
pub const MODEL_VERSION: u32 = 1;

/// The pattern text of the root of the chain: the user as a whole.
pub const ROOT: &str = "";

const SECONDS_PER_DAY: f64 = 86_400.0;

/// The decaying sums kept for one pattern. Latency sums are over the
/// session-adjusted log-latency residual `x` (for the root, over log-latency
/// itself, so that its mean is the user baseline); outcome sums count
/// first-attempt slots. All decay toward zero with the pattern's half-life,
/// measured from `last_update`.
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
    /// First-attempt slots typed correctly.
    pub c: f64,
    /// First-attempt errors.
    pub e: f64,
    /// Hesitations.
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
            self.c *= w;
            self.e *= w;
            self.h *= w;
            self.last_update = at;
        }
        self
    }

    fn observe_latency(&mut self, x: f64, weight: f64) {
        self.s0 += weight;
        self.s1 += weight * x;
        self.s2 += weight * x * x;
        self.w2 += weight * weight;
    }

    fn observe_outcome(&mut self, correct: f64, error: f64) {
        self.c += correct;
        self.e += error;
    }

    fn observe_hesitation(&mut self) {
        self.h += 1.0;
    }
}

/// What the model believes about one pattern, every quantity shrunk toward
/// the pattern's parent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PatternEstimate {
    /// The posterior mean of the pattern's log-latency residual: how much
    /// slower (positive) or faster than the user baseline its slot is typed.
    pub absolute_slowness: f64,
    /// The posterior variance of the residual around that mean.
    pub variance: f64,
    /// The probability that the slot is wrong on the first attempt.
    pub error_probability: f64,
    /// The share of the slot's eligible intervals that were hesitations.
    pub hesitation_rate: f64,
    pub n_eff: f64,
}

/// Every pattern's statistics for one profile.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelState {
    patterns: BTreeMap<Box<str>, PatternStats>,
    dirty: BTreeSet<Box<str>>,
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
}

/// The quantities a session's observations were measured against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppliedObservations {
    /// The user baseline snapshotted at session start, or the session's own
    /// median clean log-latency when there was none. `None` only when the
    /// session had no clean interval at all.
    pub user_baseline: Option<f64>,
    /// How much faster or slower than the baseline the session was, in
    /// log-latency.
    pub session_offset: f64,
    pub clean_intervals: usize,
}

impl ModelState {
    pub fn new() -> ModelState {
        ModelState::default()
    }

    /// Rebuilds a model from stored rows.
    pub fn from_rows(rows: impl IntoIterator<Item = (Box<str>, PatternStats)>) -> ModelState {
        ModelState {
            patterns: rows.into_iter().collect(),
            dirty: BTreeSet::new(),
        }
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
        PatternEstimate {
            absolute_slowness: mean,
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
            variance: (spread + k * config.latency_variance_prior) / (s.s0 + k),
            error_probability: (s.e + prior_errors) / (s.c + s.e + prior_trials),
            hesitation_rate: (s.h + prior_errors) / (s.s0 + s.h + prior_trials),
            n_eff: s.n_eff(),
        }
    }

    /// Enters a finished session that started at `started_at` (Unix seconds)
    /// into the model. Every pattern touched is decayed to `started_at`
    /// first, so what an observation adds depends on the time elapsed since
    /// the pattern was last seen, not on how many sessions came between.
    pub fn apply_session(
        &mut self,
        state: &SessionState,
        started_at: i64,
        config: &SchedulerConfig,
    ) -> SessionUpdate {
        let snapshot = self.user_baseline();
        let threshold = snapshot.map_or(
            HesitationThreshold::RunningMedian,
            HesitationThreshold::UserBaseline,
        );
        let analysis = analyze_with(state, threshold);

        let clean: Vec<(&str, f64)> = analysis
            .intervals
            .iter()
            .filter(|i| i.class == IntervalClass::Clean)
            .filter_map(|i| {
                let micros = i.latency_micros?;
                Some((i.pattern.as_ref(), (micros as f64 / 1_000_000.0).ln()))
            })
            .collect();
        let completed = state.outcome() == Some(Outcome::Completed);
        if !completed && clean.len() < config.interrupted_min_clean_intervals {
            return SessionUpdate {
                analysis,
                applied: None,
            };
        }

        let baseline = snapshot.or_else(|| {
            let mut sorted: Vec<f64> = clean.iter().map(|&(_, l)| l).collect();
            sorted.sort_by(f64::total_cmp);
            median(&sorted, |a, b| (a + b) / 2.0)
        });
        let session_offset = baseline.map_or(0.0, |b| {
            let mut residuals: Vec<f64> = clean.iter().map(|&(_, l)| l - b).collect();
            residuals.sort_by(f64::total_cmp);
            let n = residuals.len() as f64;
            median(&residuals, |a, b| (a + b) / 2.0).unwrap_or(0.0) * n
                / (n + config.offset_regulariser)
        });

        let mut observer = Observer {
            model: self,
            at: started_at,
            config,
        };
        for &(pattern, log_latency) in &clean {
            let baseline = baseline.expect("a clean interval implies a baseline");
            let x = log_latency - baseline - session_offset;
            observer.chain(pattern, |s| s.observe_latency(x, 1.0));
            observer.root(|s| s.observe_latency(log_latency, 1.0));
        }
        for interval in &analysis.intervals {
            if matches!(interval.class, IntervalClass::Hesitation { .. }) {
                observer.chain(&interval.pattern, PatternStats::observe_hesitation);
                observer.root(PatternStats::observe_hesitation);
            }
        }
        observer.outcomes(state, &analysis);

        let clean_intervals = clean.len();
        SessionUpdate {
            analysis,
            applied: Some(AppliedObservations {
                user_baseline: baseline,
                session_offset,
                clean_intervals,
            }),
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
