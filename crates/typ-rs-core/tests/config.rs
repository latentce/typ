use typ_rs_core::model::SchedulerConfig;

#[test]
fn the_default_config_round_trips_through_json() {
    let config = SchedulerConfig::default();
    let json = config.to_json();
    assert!(json.starts_with('{') && json.ends_with('}'), "{json}");
    assert_eq!(SchedulerConfig::from_json(&json).unwrap(), config);
}

#[test]
fn the_defaults_are_the_documented_values() {
    let config = SchedulerConfig::default();
    assert_eq!(config.pattern_half_life_days, 45.0);
    assert_eq!(config.baseline_half_life_days, 7.0);
    assert_eq!(config.kappa, 10.0);
    assert_eq!(
        (config.root_prior_errors, config.root_prior_correct),
        (1.0, 19.0)
    );
    assert_eq!(config.interrupted_min_clean_intervals, 20);
    assert_eq!(config.context_refit_sessions, 5);
    assert_eq!(config.context_ridge_lambda, 1.0);
    assert_eq!(
        (config.accuracy_gate_zero, config.accuracy_gate_full),
        (0.90, 0.98)
    );
    assert_eq!(
        (
            config.weight_error,
            config.weight_speed,
            config.weight_inconsistency,
            config.weight_hesitation
        ),
        (0.50, 0.25, 0.10, 0.15)
    );
    assert_eq!(config.slowness_share, 0.3);
    assert_eq!(config.log_ratio_variance_cap, 1.0);
    assert_eq!(config.importance_floor, 0.005);
    assert_eq!(config.min_pattern_words, 5);
    assert_eq!((config.candidates, config.max_targets), (8, 5));
    assert_eq!(
        (config.deferral_probability, config.deferral_window),
        (0.25, 3)
    );
    assert_eq!(config.dose, 6);
    assert_eq!(
        (
            config.plateau_min_sessions,
            config.plateau_min_dose,
            config.plateau_factor,
            config.plateau_recovery_sessions
        ),
        (4, 20, 0.5, 10)
    );
    assert_eq!(
        (
            config.ramp_start_share,
            config.ramp_full_share,
            config.ramp_sessions
        ),
        (0.30, 0.80, 4)
    );
    assert_eq!(config.pool_sample, 200);
    assert_eq!(config.coverage_scale, 20.0);
    assert_eq!(config.temperature, 0.25);
    assert_eq!(
        (config.recent_word_sessions, config.recent_word_penalty),
        (5, 1.0)
    );
    assert_eq!((config.overload_targets, config.overload_penalty), (3, 1.0));
    assert_eq!((config.long_word_length, config.length_penalty), (10, 0.2));
    assert_eq!(config.min_exposure_gap, 2);
    assert_eq!(config.contamination_sessions, 10);
    assert_eq!(config.recent_half_life_sessions, 5.0);
}

#[test]
fn a_fractional_or_negative_count_is_an_error() {
    for bad in [
        r#"{"interrupted_min_clean_intervals": 2.5}"#,
        r#"{"interrupted_min_clean_intervals": -1}"#,
        r#"{"context_refit_sessions": 0.5}"#,
    ] {
        assert!(SchedulerConfig::from_json(bad).is_err(), "{bad}");
    }
}

#[test]
fn every_tunable_appears_by_name_in_the_json() {
    let json = SchedulerConfig::default().to_json();
    for key in [
        "pattern_half_life_days",
        "baseline_half_life_days",
        "kappa",
        "root_prior_errors",
        "root_prior_correct",
        "latency_variance_prior",
        "offset_regulariser",
        "interrupted_min_clean_intervals",
        "context_refit_sessions",
        "context_ridge_lambda",
        "accuracy_gate_zero",
        "accuracy_gate_full",
        "weight_error",
        "weight_speed",
        "weight_inconsistency",
        "weight_hesitation",
        "slowness_share",
        "log_ratio_variance_cap",
        "importance_floor",
        "min_pattern_words",
        "candidates",
        "max_targets",
        "deferral_probability",
        "deferral_window",
        "dose",
        "plateau_min_sessions",
        "plateau_min_dose",
        "plateau_factor",
        "plateau_recovery_sessions",
        "ramp_start_share",
        "ramp_full_share",
        "ramp_sessions",
        "pool_sample",
        "coverage_scale",
        "temperature",
        "recent_word_sessions",
        "recent_word_penalty",
        "overload_targets",
        "overload_penalty",
        "long_word_length",
        "length_penalty",
        "min_exposure_gap",
        "contamination_sessions",
        "recent_half_life_sessions",
    ] {
        assert!(
            json.contains(&format!("\"{key}\":")),
            "{key} missing: {json}"
        );
    }
}

#[test]
fn a_changed_tunable_reads_back_changed() {
    let config = SchedulerConfig {
        kappa: 2.5,
        interrupted_min_clean_intervals: 7,
        ..SchedulerConfig::default()
    };
    let read = SchedulerConfig::from_json(&config.to_json()).unwrap();
    assert_eq!(read.kappa, 2.5);
    assert_eq!(read.interrupted_min_clean_intervals, 7);
    assert_eq!(read, config);
}

#[test]
fn a_missing_tunable_takes_its_default_and_an_unknown_one_is_ignored() {
    let read = SchedulerConfig::from_json(r#"{"kappa": 4, "humidity": 0.7}"#).unwrap();
    assert_eq!(read.kappa, 4.0);
    assert_eq!(
        read.pattern_half_life_days,
        SchedulerConfig::default().pattern_half_life_days
    );
}

#[test]
fn a_tunable_can_be_set_by_name() {
    let mut config = SchedulerConfig::default();
    config.set("kappa", 4.0).unwrap();
    config.set("dose", 8.0).unwrap();
    assert_eq!(config.kappa, 4.0);
    assert_eq!(config.dose, 8);
    assert!(config.set("dose", 2.5).is_err());
    assert!(config.set("dose", -1.0).is_err());
    assert!(config.set("humidity", 0.7).is_err());
    assert_eq!(config.dose, 8);
}

#[test]
fn the_listed_tunables_are_the_json_members_with_their_values() {
    let config = SchedulerConfig {
        kappa: 3.0,
        ..SchedulerConfig::default()
    };
    let listed: Vec<(&str, f64)> = config.tunables().collect();
    assert!(listed.contains(&("kappa", 3.0)));
    assert!(listed.contains(&("dose", 6.0)));
    let json = config.to_json();
    for (name, _) in &listed {
        assert!(json.contains(&format!("\"{name}\":")), "{name}");
    }
    assert_eq!(listed.len(), json.matches(':').count());
}

#[test]
fn malformed_json_is_an_error() {
    for bad in ["", "{", "kappa: 4", r#"{"kappa": "ten"}"#, r#"{"kappa" 4}"#] {
        assert!(SchedulerConfig::from_json(bad).is_err(), "{bad:?}");
    }
}
