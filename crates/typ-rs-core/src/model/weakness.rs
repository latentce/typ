//! A pattern's weakness: its combined shortfall against the user as a whole
//! across accuracy, speed, consistency, and hesitation, held as a
//! distribution rather than a point.
//!
//! Every component compares the pattern's posterior with the user-wide
//! reference, the root of the chain: how much more often the slot is wrong,
//! how much slower it is once its context is accounted for, how much more
//! its latency varies, and how much more often the user stalls before it.
//! The components are combined with fixed weights, and their posterior
//! variances are carried through the same logs and weights by the delta
//! method, so a pattern with little evidence of its own is weak with wide
//! uncertainty rather than confidently average.

use super::{ModelState, ROOT, SchedulerConfig};

/// A pattern's weakness posterior, summarized by its mean and standard
/// deviation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weakness {
    pub mean: f64,
    pub sd: f64,
}

/// The four components of a pattern's weakness, each measured against the
/// user-wide reference, and their weighted combination.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaknessComponents {
    /// `ln(error_probability[p] / error_probability[user])`.
    pub error_excess: f64,
    /// The pattern effect: slowness with the context effect removed.
    pub speed_excess: f64,
    /// `ln(sd[p] / sd[user])` of the log-latency residual.
    pub inconsistency: f64,
    /// `ln(hesitation_rate[p] / hesitation_rate[user])`.
    pub hesitation_excess: f64,
    pub weakness: Weakness,
}

impl ModelState {
    /// The pattern's weakness posterior as of `at`.
    pub fn weakness(&self, pattern: &str, at: i64, config: &SchedulerConfig) -> Weakness {
        self.weakness_components(pattern, at, config).weakness
    }

    /// The pattern's weakness as of `at`, component by component.
    pub fn weakness_components(
        &self,
        pattern: &str,
        at: i64,
        config: &SchedulerConfig,
    ) -> WeaknessComponents {
        let user = self.estimate(ROOT, at, config);
        let estimate = self.estimate(pattern, at, config);
        let stats = self
            .stats(pattern)
            .unwrap_or_default()
            .decayed_to(at, config.pattern_half_life_days);
        let k = config.kappa;

        // Error probability and hesitation rate are Beta posterior means over
        // their trials plus κ pseudo-trials from the parent.
        let error_trials = stats.c + stats.e + k;
        let error_excess = (estimate.error_probability / user.error_probability).ln();
        let error_var = log_ratio_variance(
            estimate.error_probability,
            user.error_probability,
            error_trials,
            config,
        );

        let speed_excess = estimate.pattern_effect;
        let speed_var = estimate.variance / (stats.s0 + k);

        // The log of a standard deviation estimated from n observations has
        // variance about 1 / 2n.
        let inconsistency = 0.5 * (estimate.variance / user.variance).ln();
        let inconsistency_var = 1.0 / (2.0 * (stats.s0 + k));

        let hesitation_trials = stats.s0 + stats.h + k;
        let hesitation_excess = (estimate.hesitation_rate / user.hesitation_rate).ln();
        let hesitation_var = log_ratio_variance(
            estimate.hesitation_rate,
            user.hesitation_rate,
            hesitation_trials,
            config,
        );

        let mean = config.weight_error * error_excess
            + config.weight_speed * speed_excess
            + config.weight_inconsistency * inconsistency
            + config.weight_hesitation * hesitation_excess;
        let variance = config.weight_error.powi(2) * error_var
            + config.weight_speed.powi(2) * speed_var
            + config.weight_inconsistency.powi(2) * inconsistency_var
            + config.weight_hesitation.powi(2) * hesitation_var;
        WeaknessComponents {
            error_excess,
            speed_excess,
            inconsistency,
            hesitation_excess,
            weakness: Weakness {
                mean,
                sd: variance.max(0.0).sqrt(),
            },
        }
    }

    /// The pattern's training value: importance-weighted weakness plus a
    /// share of what the pattern costs in time.
    pub fn training_value(
        &self,
        pattern: &str,
        importance: f64,
        at: i64,
        config: &SchedulerConfig,
    ) -> f64 {
        let weakness = self.weakness(pattern, at, config);
        let estimate = self.estimate(pattern, at, config);
        importance * (weakness.mean + config.slowness_share * estimate.absolute_slowness)
    }
}

/// The delta-method variance of `ln(p / reference)` for a Beta posterior
/// with mean `p` over `trials` trials: the Beta variance `p(1 − p) /
/// (trials + 1)` divided by the square of the rate the log is linearized
/// at. That rate is the larger of `p` and the reference: linearizing at a
/// `p` driven toward zero by error-free trials would make the best-typed
/// patterns the most uncertain, and ever more so with each clean trial,
/// whereas the question the excess answers is how far the pattern sits
/// from the reference. The result is capped at the configured
/// `log_ratio_variance_cap`: a rare event over few trials has an unbounded
/// log-rate, and a ranking sampled from that is noise.
fn log_ratio_variance(p: f64, reference: f64, trials: f64, config: &SchedulerConfig) -> f64 {
    let at = p.max(reference);
    if at <= 0.0 {
        return 0.0;
    }
    (p * (1.0 - p) / (trials + 1.0) / (at * at)).min(config.log_ratio_variance_cap)
}
