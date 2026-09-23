use typ_rs_core::model::SchedulerConfig;

#[test]
fn the_default_config_round_trips_through_json() {
    let config = SchedulerConfig::default();
    let json = config.to_json();
    assert!(json.starts_with('{') && json.ends_with('}'), "{json}");
    assert_eq!(SchedulerConfig::from_json(&json).unwrap(), config);
}

#[test]
fn the_defaults_are_the_documented_starting_points() {
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
    let read = SchedulerConfig::from_json(r#"{"kappa": 4, "temperature": 0.7}"#).unwrap();
    assert_eq!(read.kappa, 4.0);
    assert_eq!(
        read.pattern_half_life_days,
        SchedulerConfig::default().pattern_half_life_days
    );
}

#[test]
fn malformed_json_is_an_error() {
    for bad in ["", "{", "kappa: 4", r#"{"kappa": "ten"}"#, r#"{"kappa" 4}"#] {
        assert!(SchedulerConfig::from_json(bad).is_err(), "{bad:?}");
    }
}
