//! What a run says about the learner once it is over: whether practice on
//! the targets showed up where it was not drilled, and what a gain
//! estimate would make of it.

use std::collections::{BTreeMap, BTreeSet};

use typ_rs_core::analysis::IntervalClass;
use typ_rs_core::compose::WordRole;
use typ_rs_core::corpus::Corpus;
use typ_rs_core::metrics::space_typed;
use typ_rs_core::model::SchedulerConfig;
use typ_rs_core::model::context::slot_features;
use typ_rs_core::prompt::Slot;
use typ_rs_core::scheduler::{SelectedTarget, TargetRole};

use crate::run::{Run, SessionRecord};

/// A mean over some observations.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Estimate {
    pub mean: f64,
    pub count: usize,
}

impl Estimate {
    fn over(values: impl IntoIterator<Item = f64>) -> Estimate {
        let mut sum = 0.0;
        let mut count = 0;
        for v in values {
            sum += v;
            count += 1;
        }
        Estimate {
            mean: if count == 0 { 0.0 } else { sum / count as f64 },
            count,
        }
    }
}

/// What a learning-gain estimate reads on a run.
///
/// The naive figure is the posterior weakness of each target at selection
/// less its posterior a deferral window later: what a scheduler rewarded on
/// before-and-after gain would see. It is biased upward however the learner
/// learns: patterns are selected because they looked weak, so on
/// re-measurement they look better with no learning at all. Drift is the
/// same change over every eligible pattern; it stays near zero because
/// weakness is measured against the user baseline, which moves with any
/// global change.
///
/// The corrected figure is a randomized comparison instead. Every
/// candidate was deferred or not by a coin toss, so the newly deferred
/// candidates of a session and its targets differ only by that toss; the
/// two arms are compared on fresh observations alone: what the learner
/// typed at their slots in the sessions after selection, as
/// session-adjusted, context-corrected log-latency residuals and
/// first-attempt errors. Each selection counts once, whatever it was
/// typed in: a target that keeps being targeted gives many observations
/// and a deferred candidate few, and pooling them would weight the arms
/// by how weak their patterns kept looking. Neither arm's selection-time
/// estimate enters the outcome, so its bias cancels; both arms are
/// measured over the same sessions against the same baseline, so drift
/// cancels. On a learner with no practice-dependent improvement it must
/// read about zero, within the standard error the errors' scarcity leaves
/// it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GainCheck {
    /// Posterior weakness drop over targets, selection to a deferral window
    /// later.
    pub naive: Estimate,
    /// The same over every eligible pattern, over the same periods.
    pub drift: Estimate,
    /// Fresh outcomes of the deferred arm against the targeted arm.
    pub corrected: Comparison,
}

/// The targeted arm against the deferred arm on fresh, delayed
/// observations, over every session that had both.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Comparison {
    /// Sessions contributing.
    pub sessions: usize,
    pub targets: Arm,
    pub deferred: Arm,
}

impl Comparison {
    /// The deferred arm's score less the targeted arm's: positive when the
    /// targets ended up better than the controls, in the weakness's own
    /// units for its speed and error components.
    pub fn gain(&self, config: &SchedulerConfig) -> Option<f64> {
        Some(self.deferred.score(config)? - self.targets.score(config)?)
    }

    /// The standard error of the gain, from the spread of each arm's
    /// per-selection outcomes.
    pub fn standard_error(&self, config: &SchedulerConfig) -> Option<f64> {
        let variance =
            self.targets.score_variance(config)? + self.deferred.score_variance(config)?;
        Some(variance.sqrt())
    }
}

/// One arm of the comparison: each selection's fresh outcome, one value
/// per selection that had the observations for it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Arm {
    pub selections: usize,
    /// Each selection's mean residual over its clean intervals.
    pub residuals: Vec<Outcome>,
    /// Each selection's error mass over its first-attempt trials.
    pub error_rates: Vec<Outcome>,
}

/// One selection's outcome, tagged with its pattern because the same
/// pattern is selected again and again and its outcomes share a truth.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub pattern: Box<str>,
    pub value: f64,
}

impl Arm {
    pub fn mean_residual(&self) -> Option<f64> {
        mean(&self.residuals)
    }

    pub fn mean_error_rate(&self) -> Option<f64> {
        mean(&self.error_rates)
    }

    /// The weakness's speed and error components over the arm: the speed
    /// weight times the mean residual plus the error weight times the log
    /// of the mean error rate, the rate given the root prior's pseudo-count
    /// so an arm with no error still has one.
    pub fn score(&self, config: &SchedulerConfig) -> Option<f64> {
        let residual = self.mean_residual()?;
        let rate = self.prior_shrunk_error_rate(config)?;
        Some(config.weight_speed * residual + config.weight_error * rate.ln())
    }

    fn prior_shrunk_error_rate(&self, config: &SchedulerConfig) -> Option<f64> {
        let rate = self.mean_error_rate()?;
        let n = self.error_rates.len() as f64;
        let prior =
            config.root_prior_errors / (config.root_prior_errors + config.root_prior_correct);
        Some((rate * n + prior) / (n + 1.0))
    }

    /// The variance of the score from the spread of the per-selection
    /// outcomes, selections of one pattern taken together, the log error
    /// rate linearized at the rate.
    fn score_variance(&self, config: &SchedulerConfig) -> Option<f64> {
        let residual = variance_of_mean(&self.residuals)?;
        let rate = self.prior_shrunk_error_rate(config)?;
        let rate_variance = variance_of_mean(&self.error_rates)?;
        Some(
            config.weight_speed.powi(2) * residual
                + config.weight_error.powi(2) * rate_variance / (rate * rate),
        )
    }

    fn add(&mut self, pattern: &str, observed: &Observations) {
        self.selections += 1;
        let outcome = |value: f64| Outcome {
            pattern: pattern.into(),
            value,
        };
        if observed.intervals > 0 {
            self.residuals
                .push(outcome(observed.residual_sum / observed.intervals as f64));
        }
        if observed.trials > 0 {
            self.error_rates
                .push(outcome(observed.errors / observed.trials as f64));
        }
    }
}

fn mean(outcomes: &[Outcome]) -> Option<f64> {
    (!outcomes.is_empty())
        .then(|| outcomes.iter().map(|o| o.value).sum::<f64>() / outcomes.len() as f64)
}

/// The variance of the mean of the outcomes, with those of one pattern
/// treated as one cluster: the squared sums of each cluster's deviations
/// over the squared count, which allows for outcomes of the same pattern
/// moving together.
fn variance_of_mean(outcomes: &[Outcome]) -> Option<f64> {
    let n = outcomes.len();
    if n < 2 {
        return None;
    }
    let m = mean(outcomes)?;
    let mut clusters: BTreeMap<&str, f64> = BTreeMap::new();
    for o in outcomes {
        *clusters.entry(&o.pattern).or_default() += o.value - m;
    }
    if clusters.len() < 2 {
        return None;
    }
    Some(clusters.values().map(|d| d * d).sum::<f64>() / (n * n) as f64)
}

/// What the learner typed at one pattern's slots over some sessions.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Observations {
    intervals: usize,
    residual_sum: f64,
    trials: usize,
    errors: f64,
}

pub fn gain_check(run: &Run) -> GainCheck {
    let config = &run.options.config;
    let corpus = Corpus::bundled();
    let window = config.deferral_window.max(1);
    let mut naive = Vec::new();
    let mut drift = Vec::new();
    let mut corrected = Comparison::default();
    for (index, session) in run.sessions.iter().enumerate() {
        let Some(following) = run.sessions.get(index + 1..index + 1 + window) else {
            break;
        };
        let later = &following[window - 1].weakness_at_composition;
        let change = |pattern: &str, before: f64| later.get(pattern).map(|after| before - after);
        let targets: Vec<&SelectedTarget> = session
            .events
            .iter()
            .filter(|e| e.target.role == TargetRole::Target)
            .map(|e| &e.target)
            .collect();
        if targets.is_empty() {
            continue;
        }
        naive.extend(
            targets
                .iter()
                .filter_map(|t| change(&t.pattern, t.weakness_mean)),
        );
        drift.extend(
            session
                .weakness_at_composition
                .iter()
                .filter_map(|(p, &before)| change(p, before)),
        );
        if session.newly_deferred.is_empty() {
            continue;
        }
        corrected.sessions += 1;
        for t in &targets {
            corrected
                .targets
                .add(&t.pattern, &observed_at(run, following, &t.pattern, corpus));
        }
        for pattern in &session.newly_deferred {
            corrected
                .deferred
                .add(pattern, &observed_at(run, following, pattern, corpus));
        }
    }
    GainCheck {
        naive: Estimate::over(naive),
        drift: Estimate::over(drift),
        corrected,
    }
}

/// What the learner typed at the pattern's slots over `sessions`, with
/// every residual corrected for its slot's context by the run's final
/// context model.
fn observed_at(
    run: &Run,
    sessions: &[SessionRecord],
    pattern: &str,
    corpus: &Corpus,
) -> Observations {
    let context = run.model.context_model();
    let layout = run.model.layout();
    let mut observed = Observations::default();
    for session in sessions {
        let Some(applied) = session.applied else {
            continue;
        };
        let Some(baseline) = applied.user_baseline else {
            continue;
        };
        let prompt = &session.composed.prompt;
        for interval in &session.analysis.intervals {
            if interval.class != IntervalClass::Clean || !interval.pattern.ends_with(pattern) {
                continue;
            }
            let Some(latency) = interval.latency_micros else {
                continue;
            };
            let features = slot_features(prompt, interval.slot, layout, corpus);
            observed.intervals += 1;
            observed.residual_sum += (latency as f64 / 1_000_000.0).ln()
                - baseline
                - applied.session_offset
                - context.effect(&features);
        }
        for (index, word) in session.analysis.words.iter().enumerate() {
            if !word.submitted {
                continue;
            }
            let error_mass = word.error_mass();
            let len = word.target.chars().count();
            for position in 0..=len {
                if position == len && !space_typed(&session.analysis, index) {
                    continue;
                }
                let chain = prompt.pattern_ending_at(Slot {
                    word: index,
                    position,
                });
                if chain.ends_with(pattern) {
                    observed.trials += 1;
                    observed.errors += error_mass.get(&position).copied().unwrap_or(0.0).min(1.0);
                }
            }
        }
    }
    observed
}

/// How some slots were typed: first-attempt trials and the error mass
/// among them, and the clean latencies.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SlotSample {
    pub slots: usize,
    pub errors: f64,
    pub clean_latencies_micros: Vec<u64>,
}

impl SlotSample {
    pub fn error_rate(&self) -> Option<f64> {
        (self.slots > 0).then(|| self.errors / self.slots as f64)
    }

    pub fn median_latency_micros(&self) -> Option<u64> {
        let mut sorted = self.clean_latencies_micros.clone();
        sorted.sort_unstable();
        let n = sorted.len();
        match n {
            0 => None,
            _ if n % 2 == 1 => Some(sorted[n / 2]),
            _ => Some((sorted[n / 2 - 1] + sorted[n / 2]) / 2),
        }
    }
}

/// The same kind of slots early in the run and late in it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EarlyLate {
    pub early: SlotSample,
    pub late: SlotSample,
}

impl EarlyLate {
    /// `ln(early / late)` of the median clean latency: positive when the
    /// slots got faster.
    pub fn speed_up(&self) -> Option<f64> {
        let early = self.early.median_latency_micros()? as f64;
        let late = self.late.median_latency_micros()? as f64;
        Some((early / late).ln())
    }
}

/// Transfer: how the patterns the scheduler invested in (practiced in at
/// least [`TRANSFER_MIN_SESSIONS`] sessions) were typed in words never
/// used for targeted practice, early in the run against late, beside every
/// other slot of the same words over the same sessions. The difference
/// between the two speed-ups is the improvement specific to the practiced
/// patterns; a learner that only memorizes the words it is drilled on, or
/// that gets faster across the board, shows none.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Transfer {
    /// Slots whose pattern chain includes an invested-in pattern.
    pub practiced: EarlyLate,
    /// Every other slot of the same words.
    pub other: EarlyLate,
}

impl Transfer {
    /// The practiced slots' speed-up less the other slots'.
    pub fn difference(&self) -> Option<f64> {
        Some(self.practiced.speed_up()? - self.other.speed_up()?)
    }

    fn sample(&mut self, practiced: bool, early: bool) -> &mut SlotSample {
        let side = if practiced {
            &mut self.practiced
        } else {
            &mut self.other
        };
        if early {
            &mut side.early
        } else {
            &mut side.late
        }
    }
}

/// A pattern counts as invested in once practiced in this many sessions;
/// one selected once or twice on a noisy draw says little about transfer.
pub const TRANSFER_MIN_SESSIONS: usize = 3;

/// The transfer over the sessions from the second on (the first has no
/// targets), the earlier half against the later.
pub fn transfer(run: &Run) -> Transfer {
    let mut sessions_practiced: BTreeMap<&str, usize> = BTreeMap::new();
    for e in run.sessions.iter().flat_map(|s| s.practiced()) {
        *sessions_practiced.entry(&e.target.pattern).or_default() += 1;
    }
    let practiced: BTreeSet<&str> = sessions_practiced
        .into_iter()
        .filter(|&(_, n)| n >= TRANSFER_MIN_SESSIONS)
        .map(|(p, _)| p)
        .collect();
    let targeted_words: BTreeSet<&str> = run
        .sessions
        .iter()
        .flat_map(|s| s.composed.targeted_words())
        .collect();
    let is_practiced = |chain: &str| practiced.iter().any(|p| chain.ends_with(p));

    let mut transfer = Transfer::default();
    let considered = &run.sessions[1.min(run.sessions.len())..];
    let half = considered.len() / 2;
    for (i, session) in considered.iter().enumerate() {
        let prompt = &session.composed.prompt;
        let untargeted = |word: usize| {
            session.composed.words[word].role == WordRole::Probe
                && !targeted_words.contains(prompt.word(word))
        };
        let early = i < half;
        for (index, word) in session.analysis.words.iter().enumerate() {
            if !word.submitted || !untargeted(index) {
                continue;
            }
            let error_mass = word.error_mass();
            for position in 0..word.target.chars().count() {
                let chain = prompt.pattern_ending_at(Slot {
                    word: index,
                    position,
                });
                let sample = transfer.sample(is_practiced(&chain), early);
                sample.slots += 1;
                sample.errors += error_mass.get(&position).copied().unwrap_or(0.0).min(1.0);
            }
        }
        for interval in &session.analysis.intervals {
            if interval.class != IntervalClass::Clean || !untargeted(interval.slot.word) {
                continue;
            }
            if let Some(latency) = interval.latency_micros {
                transfer
                    .sample(is_practiced(&interval.pattern), early)
                    .clean_latencies_micros
                    .push(latency);
            }
        }
    }
    transfer
}

/// Planned against achieved exposures over the run's targets.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Doses {
    pub targets: usize,
    pub planned: usize,
    pub achieved: usize,
}

impl Doses {
    pub fn mean_planned(&self) -> f64 {
        per_target(self.planned, self.targets)
    }

    pub fn mean_achieved(&self) -> f64 {
        per_target(self.achieved, self.targets)
    }
}

fn per_target(exposures: usize, targets: usize) -> f64 {
    if targets == 0 {
        0.0
    } else {
        exposures as f64 / targets as f64
    }
}

pub fn doses(run: &Run) -> Doses {
    let mut doses = Doses::default();
    for e in run
        .sessions
        .iter()
        .flat_map(|s| s.events.iter())
        .filter(|e| e.target.role == TargetRole::Target)
    {
        doses.targets += 1;
        doses.planned += e.target.planned_dose;
        doses.achieved += e.achieved_dose;
    }
    doses
}
