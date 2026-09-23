use typ_rs_core::corpus::Corpus;
use typ_rs_core::model::{ModelState, SchedulerConfig};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{EndCondition, Input, Key, SessionState};

/// Types `script` against `prompt`, one keystroke every `step` microseconds;
/// `…` is a two-second pause before the next key.
fn typed_at(prompt: &str, script: &str, step: u64) -> SessionState {
    let mut state = SessionState::new(
        Prompt::new(prompt.split(' ')),
        EndCondition::AfterWords(usize::MAX),
    );
    let mut at = 0;
    for symbol in script.chars() {
        if symbol == '…' {
            at += 2_000_000;
            continue;
        }
        state.apply_event(Input::new(at, Key::Char(symbol)));
        at += step;
    }
    state
}

fn typed(prompt: &str, script: &str) -> SessionState {
    typed_at(prompt, script, 200_000)
}

fn config() -> SchedulerConfig {
    SchedulerConfig::default()
}

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

#[test]
fn a_pattern_indistinguishable_from_the_user_has_zero_weakness() {
    // Nothing observed: every pattern inherits the user-wide reference on
    // every component, so every excess is zero. The uncertainty is not.
    let model = ModelState::new();
    let w = model.weakness("th", 1_000, &config());
    assert_eq!(w.mean, 0.0);
    assert!(w.sd > 0.0);

    // With evidence, a pattern typed exactly like everything else stays at
    // zero on speed; its consistency, error, and hesitation components can
    // only be pulled below the reference by the shrinkage toward it.
    let mut model = ModelState::new();
    model.apply_session(
        &typed("cat dog cat dog", "cat dog cat dog"),
        1_000,
        Corpus::bundled(),
        &config(),
    );
    let components = model.weakness_components("at", 1_000, &config());
    assert!(close(components.speed_excess, 0.0), "{components:?}");
    assert!(components.inconsistency <= 0.0, "{components:?}");
    assert!(components.error_excess < 0.0, "{components:?}");
    assert!(components.hesitation_excess < 0.0, "{components:?}");
}

#[test]
fn each_component_moves_against_the_user_wide_reference() {
    let config = config();
    let mut model = ModelState::new();
    model.apply_session(
        &typed("cat dog cat dog", "cat dog cat dog"),
        1_000,
        Corpus::bundled(),
        &config,
    );
    let at = 2_000;

    // `x` for `a` in `cat`: the error lands on ` ca`, whose error excess
    // becomes positive while `dog`'s patterns stay below the reference.
    let mut errors = model.clone();
    errors.apply_session(
        &typed("cat cat", "cxt cxt "),
        at,
        Corpus::bundled(),
        &config,
    );
    let ca = errors.weakness_components(" ca", at, &config);
    let og = errors.weakness_components("og", at, &config);
    assert!(ca.error_excess > 0.0, "{ca:?}");
    assert!(og.error_excess < 0.0, "{og:?}");
    assert!(errors.weakness(" ca", at, &config).mean > errors.weakness("og", at, &config).mean);

    // `xyz` typed at twice the baseline: its speed excess is positive.
    let mut slow = model.clone();
    slow.apply_session(
        &typed_at("xyz", "xyz", 400_000),
        at,
        Corpus::bundled(),
        &config,
    );
    let yz = slow.weakness_components("yz", at, &config);
    assert!(yz.speed_excess > 0.0, "{yz:?}");
    assert!(close(yz.weakness.mean, weighted(&yz, &config)), "{yz:?}");

    // A pause before `g` in `catalog`: hesitation excess on `log`.
    let mut pauses = model.clone();
    pauses.apply_session(
        &typed("catalog catalog", "catalo…g catalo…g"),
        at,
        Corpus::bundled(),
        &config,
    );
    let log = pauses.weakness_components("log", at, &config);
    assert!(log.hesitation_excess > 0.0, "{log:?}");
}

fn weighted(c: &typ_rs_core::model::WeaknessComponents, config: &SchedulerConfig) -> f64 {
    config.weight_error * c.error_excess
        + config.weight_speed * c.speed_excess
        + config.weight_inconsistency * c.inconsistency
        + config.weight_hesitation * c.hesitation_excess
}

#[test]
fn the_uncertainty_is_wider_for_a_sparse_pattern_than_for_a_well_observed_one() {
    let config = config();
    let mut model = ModelState::new();
    let words = "that that that that that that that that that that";
    model.apply_session(&typed(words, words), 1_000, Corpus::bundled(), &config);
    model.apply_session(&typed("cat", "cat"), 1_000, Corpus::bundled(), &config);
    let at = 1_000;

    let dense = model.weakness("hat", at, &config);
    let sparse = model.weakness("cat", at, &config);
    let unseen = model.weakness("xat", at, &config);
    assert!(dense.sd < sparse.sd, "{dense:?} vs {sparse:?}");
    assert!(sparse.sd < unseen.sd, "{sparse:?} vs {unseen:?}");
}
