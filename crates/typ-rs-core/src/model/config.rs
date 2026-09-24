//! Every tunable of the analysis and scheduling, in one place.

use std::fmt;

/// The tunables of the model. One structure holds them all so that a
/// session can record what produced it and a simulator can vary them
/// without code changes. The defaults were checked against simulated
/// learners with a known weakness: the shrinkage strength and the variance
/// cap sit on a trade-off, where lowering either makes the scheduler
/// re-target a found weakness more consistently at the price of taking
/// longer to find one whose parent character is typed well; a larger dose
/// or a warmer word draw gives a found weakness more exposures but no
/// longer lets every target reach its dose in a 50-word prompt.
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
    /// offset is the median residual scaled by `n / (n + regularizer)`.
    pub offset_regularizer: f64,
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
    /// A target is plateaued once practiced in at least this many sessions
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
    /// The candidate pool for targeted words is every word containing a
    /// practiced pattern plus this many words drawn from the reference
    /// distribution.
    pub pool_sample: usize,
    /// What one fresh exposure of the highest-priority target adds to a
    /// word's coverage gain before the gain's log is taken. The frequency
    /// term of the word score spans a few nats over the corpus, so a value
    /// of a few tens lets coverage lead while common words win among equals.
    pub coverage_scale: f64,
    /// The softmax temperature over word scores: lower picks the
    /// best-scored word more surely, higher spreads the draw. The pool
    /// runs to hundreds of words, so a temperature near one lets the many
    /// mediocre words outweigh the few good ones between them.
    pub temperature: f64,
    /// The penalty on a word's score for having been shown as targeted in
    /// any of the last this many sessions.
    pub recent_word_sessions: usize,
    pub recent_word_penalty: f64,
    /// The penalty per exposed target above this many in one word.
    pub overload_targets: usize,
    pub overload_penalty: f64,
    /// The penalty per character of a word above this length.
    pub long_word_length: usize,
    pub length_penalty: f64,
    /// Two words exposing the same target are kept at least this many
    /// positions apart in the prompt where possible; one or less imposes
    /// nothing.
    pub min_exposure_gap: usize,
    /// A probe is contaminated by targeted practice in the last this many
    /// sessions.
    pub contamination_sessions: usize,
    /// The half-life, in completed sessions, of the recent series a
    /// session's figures are compared with.
    pub recent_half_life_sessions: f64,
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
            offset_regularizer: 20.0,
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
            pool_sample: 200,
            coverage_scale: 20.0,
            temperature: 0.25,
            recent_word_sessions: 5,
            recent_word_penalty: 1.0,
            overload_targets: 3,
            overload_penalty: 1.0,
            long_word_length: 10,
            length_penalty: 0.2,
            min_exposure_gap: 2,
            contamination_sessions: 10,
            recent_half_life_sessions: 5.0,
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
        name: "offset_regularizer",
        get: |c| c.offset_regularizer,
        set: |c, v| c.offset_regularizer = v,
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
    Tunable {
        name: "pool_sample",
        get: |c| c.pool_sample as f64,
        set: |c, v| c.pool_sample = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "coverage_scale",
        get: |c| c.coverage_scale,
        set: |c, v| c.coverage_scale = v,
        whole_number: false,
    },
    Tunable {
        name: "temperature",
        get: |c| c.temperature,
        set: |c, v| c.temperature = v,
        whole_number: false,
    },
    Tunable {
        name: "recent_word_sessions",
        get: |c| c.recent_word_sessions as f64,
        set: |c, v| c.recent_word_sessions = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "recent_word_penalty",
        get: |c| c.recent_word_penalty,
        set: |c, v| c.recent_word_penalty = v,
        whole_number: false,
    },
    Tunable {
        name: "overload_targets",
        get: |c| c.overload_targets as f64,
        set: |c, v| c.overload_targets = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "overload_penalty",
        get: |c| c.overload_penalty,
        set: |c, v| c.overload_penalty = v,
        whole_number: false,
    },
    Tunable {
        name: "long_word_length",
        get: |c| c.long_word_length as f64,
        set: |c, v| c.long_word_length = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "length_penalty",
        get: |c| c.length_penalty,
        set: |c, v| c.length_penalty = v,
        whole_number: false,
    },
    Tunable {
        name: "min_exposure_gap",
        get: |c| c.min_exposure_gap as f64,
        set: |c, v| c.min_exposure_gap = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "contamination_sessions",
        get: |c| c.contamination_sessions as f64,
        set: |c, v| c.contamination_sessions = v as usize,
        whole_number: true,
    },
    Tunable {
        name: "recent_half_life_sessions",
        get: |c| c.recent_half_life_sessions,
        set: |c, v| c.recent_half_life_sessions = v,
        whole_number: false,
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
            if !TUNABLES.iter().any(|t| t.name == name) {
                continue;
            }
            let number: f64 = value
                .parse()
                .map_err(|_| ConfigError(format!("{name}: {value:?} is not a number")))?;
            config.set(&name, number)?;
        }
        Ok(config)
    }

    /// Every tunable by its JSON member name with its current value, in
    /// member order.
    pub fn tunables(&self) -> impl Iterator<Item = (&'static str, f64)> + '_ {
        TUNABLES.iter().map(|t| (t.name, (t.get)(self)))
    }

    /// Sets one tunable by its JSON member name. A name this version does
    /// not know, or a count given a fractional or negative value, is an
    /// error and changes nothing.
    pub fn set(&mut self, name: &str, value: f64) -> Result<(), ConfigError> {
        let tunable = TUNABLES
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| ConfigError(format!("{name} is not a tunable")))?;
        if tunable.whole_number && (value < 0.0 || value.fract() != 0.0) {
            return Err(ConfigError(format!(
                "{name}: {value} is not a whole number"
            )));
        }
        (tunable.set)(self, value);
        Ok(())
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
