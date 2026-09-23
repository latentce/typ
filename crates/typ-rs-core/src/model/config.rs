//! Every tunable of the analysis and scheduling, in one place.

use std::fmt;

/// The tunables of the model. One structure holds them all so that a
/// session can record what produced it and a simulator can vary them
/// without code changes. The defaults are starting points, not tuned values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SchedulerConfig {
    /// Half-life in days of a pattern's statistics.
    pub pattern_half_life_days: f64,
    /// Half-life in days of the user baseline.
    pub baseline_half_life_days: f64,
    /// How many observations a parent's estimate is worth when shrinking a
    /// pattern toward it.
    pub kappa: f64,
    /// The Beta prior on the user-wide error probability and hesitation
    /// rate: pseudo-errors and pseudo-correct slots.
    pub root_prior_errors: f64,
    pub root_prior_correct: f64,
    /// The variance of log-latency the user-wide variance shrinks toward
    /// before there is evidence.
    pub latency_variance_prior: f64,
    /// Clean intervals' worth of pull toward zero on the session offset: the
    /// offset is the median residual scaled by `n / (n + regulariser)`.
    pub offset_regulariser: f64,
    /// An interrupted session's observations count only with at least this
    /// many clean intervals.
    pub interrupted_min_clean_intervals: usize,
    /// The context model is refitted after every this many completed
    /// sessions; zero never refits.
    pub context_refit_sessions: usize,
    /// The ridge penalty on the context model's weights, in units of
    /// latency weight.
    pub context_ridge_lambda: f64,
    /// The session raw accuracy at or below which a session's speed
    /// evidence carries no weight, and the one from which it carries full
    /// weight; the accuracy factor rises linearly between them.
    pub accuracy_gate_zero: f64,
    pub accuracy_gate_full: f64,
    /// The share of a pattern's weakness carried by each component: error
    /// excess, speed excess, inconsistency, and hesitation excess.
    pub weight_error: f64,
    pub weight_speed: f64,
    pub weight_inconsistency: f64,
    pub weight_hesitation: f64,
    /// How much of a pattern's absolute slowness counts toward its training
    /// value on top of its weakness.
    pub slowness_share: f64,
    /// The most variance the log of an error or hesitation rate ratio can
    /// carry into the weakness uncertainty; one means a pattern with no
    /// evidence is taken to lie within a factor of `e` of its parent, one
    /// standard deviation.
    pub log_ratio_variance_cap: f64,
    /// A bigram or trigram is eligible for targeting only above this
    /// importance and only if it occurs in at least this many corpus words.
    pub importance_floor: f64,
    pub min_pattern_words: usize,
    /// How many top-ranked patterns become candidates, and how many of the
    /// undeferred candidates become targets.
    pub candidates: usize,
    pub max_targets: usize,
    /// The chance that a candidate is withheld as a control, and for how
    /// many sessions it then stays out of candidacy.
    pub deferral_probability: f64,
    pub deferral_window: usize,
    /// Exposures a target should receive in a session.
    pub dose: usize,
    /// A target is plateaued once practised in at least this many sessions
    /// with more than this cumulative achieved dose and no change in its
    /// weakness beyond its uncertainty; its priority is then scaled by the
    /// plateau factor, recovering to one over this many untargeted sessions.
    pub plateau_min_sessions: usize,
    pub plateau_min_dose: usize,
    pub plateau_factor: f64,
    pub plateau_recovery_sessions: usize,
    /// The targeted share of a prompt: zero before the first completed
    /// session, then rising linearly from the start share to the full share
    /// over this many completed sessions.
    pub ramp_start_share: f64,
    pub ramp_full_share: f64,
    pub ramp_sessions: usize,
}

impl Default for SchedulerConfig {
    fn default() -> SchedulerConfig {
        SchedulerConfig {
            pattern_half_life_days: 45.0,
            baseline_half_life_days: 7.0,
            kappa: 10.0,
            root_prior_errors: 1.0,
            root_prior_correct: 19.0,
            latency_variance_prior: 0.1,
            offset_regulariser: 20.0,
            interrupted_min_clean_intervals: 20,
            context_refit_sessions: 5,
            context_ridge_lambda: 1.0,
            accuracy_gate_zero: 0.90,
            accuracy_gate_full: 0.98,
            weight_error: 0.50,
            weight_speed: 0.25,
            weight_inconsistency: 0.10,
            weight_hesitation: 0.15,
            slowness_share: 0.3,
            log_ratio_variance_cap: 1.0,
            importance_floor: 0.005,
            min_pattern_words: 5,
            candidates: 8,
            max_targets: 5,
            deferral_probability: 0.25,
            deferral_window: 3,
            dose: 6,
            plateau_min_sessions: 4,
            plateau_min_dose: 20,
            plateau_factor: 0.5,
            plateau_recovery_sessions: 10,
            ramp_start_share: 0.30,
            ramp_full_share: 0.80,
            ramp_sessions: 4,
        }
    }
}

/// One tunable as JSON sees it: its member name and how to read and write
/// it as a number. Adding a tunable means adding a row here.
struct Tunable {
    name: &'static str,
    get: fn(&SchedulerConfig) -> f64,
    set: fn(&mut SchedulerConfig, f64),
    /// A count: only a non-negative whole number is accepted.
    whole_number: bool,
}

const TUNABLES: &[Tunable] = &[
    Tunable {
        name: "pattern_half_life_days",
        get: |c| c.pattern_half_life_days,
        set: |c, v| c.pattern_half_life_days = v,
        whole_number: false,
    },
    Tunable {
        name: "baseline_half_life_days",
        get: |c| c.baseline_half_life_days,
        set: |c, v| c.baseline_half_life_days = v,
        whole_number: false,
    },
    Tunable {
        name: "kappa",
        get: |c| c.kappa,
        set: |c, v| c.kappa = v,
        whole_number: false,
    },
    Tunable {
        name: "root_prior_errors",
        get: |c| c.root_prior_errors,
        set: |c, v| c.root_prior_errors = v,
        whole_number: false,
    },
    Tunable {
        name: "root_prior_correct",
        get: |c| c.root_prior_correct,
        set: |c, v| c.root_prior_correct = v,
        whole_number: false,
    },
    Tunable {
        name: "latency_variance_prior",
        get: |c| c.latency_variance_prior,
        set: |c, v| c.latency_variance_prior = v,
        whole_number: false,
    },
    Tunable {
        name: "offset_regulariser",
        get: |c| c.offset_regulariser,
        set: |c, v| c.offset_regulariser = v,
        whole_number: false,
    },
    Tunable {
        name: "interrupted_min_clean_intervals",
        get: |c| c.interrupted_min_clean_intervals as f64,
        set: |c, v| c.interrupted_min_clean_intervals = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "context_refit_sessions",
        get: |c| c.context_refit_sessions as f64,
        set: |c, v| c.context_refit_sessions = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "context_ridge_lambda",
        get: |c| c.context_ridge_lambda,
        set: |c, v| c.context_ridge_lambda = v,
        whole_number: false,
    },
    Tunable {
        name: "accuracy_gate_zero",
        get: |c| c.accuracy_gate_zero,
        set: |c, v| c.accuracy_gate_zero = v,
        whole_number: false,
    },
    Tunable {
        name: "accuracy_gate_full",
        get: |c| c.accuracy_gate_full,
        set: |c, v| c.accuracy_gate_full = v,
        whole_number: false,
    },
    Tunable {
        name: "weight_error",
        get: |c| c.weight_error,
        set: |c, v| c.weight_error = v,
        whole_number: false,
    },
    Tunable {
        name: "weight_speed",
        get: |c| c.weight_speed,
        set: |c, v| c.weight_speed = v,
        whole_number: false,
    },
    Tunable {
        name: "weight_inconsistency",
        get: |c| c.weight_inconsistency,
        set: |c, v| c.weight_inconsistency = v,
        whole_number: false,
    },
    Tunable {
        name: "weight_hesitation",
        get: |c| c.weight_hesitation,
        set: |c, v| c.weight_hesitation = v,
        whole_number: false,
    },
    Tunable {
        name: "slowness_share",
        get: |c| c.slowness_share,
        set: |c, v| c.slowness_share = v,
        whole_number: false,
    },
    Tunable {
        name: "log_ratio_variance_cap",
        get: |c| c.log_ratio_variance_cap,
        set: |c, v| c.log_ratio_variance_cap = v,
        whole_number: false,
    },
    Tunable {
        name: "importance_floor",
        get: |c| c.importance_floor,
        set: |c, v| c.importance_floor = v,
        whole_number: false,
    },
    Tunable {
        name: "min_pattern_words",
        get: |c| c.min_pattern_words as f64,
        set: |c, v| c.min_pattern_words = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "candidates",
        get: |c| c.candidates as f64,
        set: |c, v| c.candidates = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "max_targets",
        get: |c| c.max_targets as f64,
        set: |c, v| c.max_targets = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "deferral_probability",
        get: |c| c.deferral_probability,
        set: |c, v| c.deferral_probability = v,
        whole_number: false,
    },
    Tunable {
        name: "deferral_window",
        get: |c| c.deferral_window as f64,
        set: |c, v| c.deferral_window = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "dose",
        get: |c| c.dose as f64,
        set: |c, v| c.dose = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "plateau_min_sessions",
        get: |c| c.plateau_min_sessions as f64,
        set: |c, v| c.plateau_min_sessions = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "plateau_min_dose",
        get: |c| c.plateau_min_dose as f64,
        set: |c, v| c.plateau_min_dose = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "plateau_factor",
        get: |c| c.plateau_factor,
        set: |c, v| c.plateau_factor = v,
        whole_number: false,
    },
    Tunable {
        name: "plateau_recovery_sessions",
        get: |c| c.plateau_recovery_sessions as f64,
        set: |c, v| c.plateau_recovery_sessions = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "ramp_start_share",
        get: |c| c.ramp_start_share,
        set: |c, v| c.ramp_start_share = v,
        whole_number: false,
    },
    Tunable {
        name: "ramp_full_share",
        get: |c| c.ramp_full_share,
        set: |c, v| c.ramp_full_share = v,
        whole_number: false,
    },
    Tunable {
        name: "ramp_sessions",
        get: |c| c.ramp_sessions as f64,
        set: |c, v| c.ramp_sessions = v as usize,
        whole_number: true,
    },
];

/// A `config_json` that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "malformed config: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

impl SchedulerConfig {
    /// The config as a flat JSON object of numbers, one member per tunable.
    pub fn to_json(&self) -> String {
        let members: Vec<String> = TUNABLES
            .iter()
            .map(|t| format!("\"{}\":{}", t.name, (t.get)(self)))
            .collect();
        format!("{{{}}}", members.join(","))
    }

    /// Reads a config written by [`to_json`](Self::to_json). A tunable that
    /// is absent takes its default, so a config written by an older version
    /// still reads; a member with an unknown name is ignored, so one written
    /// by a newer version reads too.
    pub fn from_json(json: &str) -> Result<SchedulerConfig, ConfigError> {
        let mut config = SchedulerConfig::default();
        for (name, value) in parse_flat_object(json)? {
            let Some(tunable) = TUNABLES.iter().find(|t| t.name == name) else {
                continue;
            };
            let number: f64 = value
                .parse()
                .map_err(|_| ConfigError(format!("{name}: {value:?} is not a number")))?;
            if tunable.whole_number && (number < 0.0 || number.fract() != 0.0) {
                return Err(ConfigError(format!(
                    "{name}: {value} is not a whole number"
                )));
            }
            (tunable.set)(&mut config, number);
        }
        Ok(config)
    }
}

/// Splits `{"name": value, ...}` into its members, values left as text.
/// Only what [`SchedulerConfig::to_json`] produces is accepted: string keys
/// and bare numeric values, with whitespace allowed around tokens.
fn parse_flat_object(json: &str) -> Result<Vec<(String, String)>, ConfigError> {
    let body = json
        .trim()
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .ok_or_else(|| ConfigError("not a JSON object".to_string()))?;
    if body.trim().is_empty() {
        return Ok(Vec::new());
    }
    body.split(',')
        .map(|member| {
            let (key, value) = member
                .split_once(':')
                .ok_or_else(|| ConfigError(format!("member {member:?} has no colon")))?;
            let key = key
                .trim()
                .strip_prefix('"')
                .and_then(|k| k.strip_suffix('"'))
                .filter(|k| !k.contains('"'))
                .ok_or_else(|| ConfigError(format!("key {key:?} is not a string")))?;
            let value = value.trim();
            if value.is_empty() || value.starts_with('"') {
                return Err(ConfigError(format!("{key}: {value:?} is not a number")));
            }
            Ok((key.to_string(), value.to_string()))
        })
        .collect()
}
