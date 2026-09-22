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
