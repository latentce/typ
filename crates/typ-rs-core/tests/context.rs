use typ_rs_core::corpus::Corpus;
use typ_rs_core::layout::Layout;
use typ_rs_core::model::context::{
    Aggregate, Coefficients, ContextModel, FEATURE_COUNT, Feature, Features, log_word_frequency,
    slot_features,
};
use typ_rs_core::prompt::{Prompt, Slot};

fn corpus(rows: &[(&str, u64)]) -> Corpus {
    let mut csv = String::from("ngram,freq,cumshare\n");
    for (word, count) in rows {
        csv.push_str(&format!("{word},{count},0.0\n"));
    }
    Corpus::from_csv(&csv).unwrap()
}

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

fn get(features: &Features, feature: Feature) -> f64 {
    features[feature.index()]
}

// --- Feature extraction --------------------------------------------------------

#[test]
fn features_are_named_and_indexed_in_one_fixed_order() {
    assert_eq!(Feature::ALL.len(), FEATURE_COUNT);
    for (i, feature) in Feature::ALL.iter().enumerate() {
        assert_eq!(feature.index(), i);
    }
    let names: Vec<&str> = Feature::ALL.iter().map(|f| f.name()).collect();
    assert_eq!(
        names,
        [
            "first_of_word",
            "last_of_word",
            "word_length",
            "log_word_frequency",
            "boundary",
            "same_finger",
            "same_hand",
            "row_change",
            "key_distance",
        ]
    );
}

#[test]
fn a_word_initial_slot_is_first_and_boundary_and_follows_the_space_bar() {
    let corpus = corpus(&[("the", 75), ("cat", 25)]);
    let prompt = Prompt::new(["the", "cat"]);
    let t = slot_features(
        &prompt,
        Slot {
            word: 0,
            position: 0,
        },
        Layout::QWERTY,
        &corpus,
    );
    assert_eq!(get(&t, Feature::FirstOfWord), 1.0);
    assert_eq!(get(&t, Feature::LastOfWord), 0.0);
    assert_eq!(get(&t, Feature::WordLength), 3.0);
    assert!(close(get(&t, Feature::LogWordFrequency), 0.75f64.ln()));
    assert_eq!(get(&t, Feature::Boundary), 1.0);
    // Space bar to `t`: a thumb then an index finger, no hand in common,
    // three rows apart, from (4.5, 3) to (3.75, 0).
    assert_eq!(get(&t, Feature::SameFinger), 0.0);
    assert_eq!(get(&t, Feature::SameHand), 0.0);
    assert_eq!(get(&t, Feature::RowChange), 3.0);
    assert!(close(get(&t, Feature::KeyDistance), 9.5625f64.sqrt()));

    // The second word's first letter reads the same way, with its own
    // frequency.
    let c = slot_features(
        &prompt,
        Slot {
            word: 1,
            position: 0,
        },
        Layout::QWERTY,
        &corpus,
    );
    assert_eq!(get(&c, Feature::FirstOfWord), 1.0);
    assert_eq!(get(&c, Feature::Boundary), 1.0);
    assert!(close(get(&c, Feature::LogWordFrequency), 0.25f64.ln()));
}

#[test]
fn an_interior_slot_carries_only_its_word_and_the_transition_geometry() {
    let corpus = corpus(&[("the", 1)]);
    let prompt = Prompt::new(["the"]);
    // `t` (left index, top row, 3.75) to `h` (right index, home row, 5).
    let h = slot_features(
        &prompt,
        Slot {
            word: 0,
            position: 1,
        },
        Layout::QWERTY,
        &corpus,
    );
    assert_eq!(get(&h, Feature::FirstOfWord), 0.0);
    assert_eq!(get(&h, Feature::LastOfWord), 0.0);
    assert_eq!(get(&h, Feature::Boundary), 0.0);
    assert_eq!(get(&h, Feature::WordLength), 3.0);
    assert_eq!(get(&h, Feature::SameFinger), 0.0);
    assert_eq!(get(&h, Feature::SameHand), 0.0);
    assert_eq!(get(&h, Feature::RowChange), 1.0);
    assert!(close(get(&h, Feature::KeyDistance), 2.5625f64.sqrt()));
}

#[test]
fn the_last_letter_is_last_and_the_space_after_it_is_boundary_but_neither_first_nor_last() {
    let corpus = corpus(&[("the", 1)]);
    let prompt = Prompt::new(["the", "the"]);
    let e = slot_features(
        &prompt,
        Slot {
            word: 0,
            position: 2,
        },
        Layout::QWERTY,
        &corpus,
    );
    assert_eq!(get(&e, Feature::LastOfWord), 1.0);
    assert_eq!(get(&e, Feature::Boundary), 0.0);

    let space = slot_features(
        &prompt,
        Slot {
            word: 0,
            position: 3,
        },
        Layout::QWERTY,
        &corpus,
    );
    assert_eq!(get(&space, Feature::FirstOfWord), 0.0);
    assert_eq!(get(&space, Feature::LastOfWord), 0.0);
    assert_eq!(get(&space, Feature::Boundary), 1.0);
    assert_eq!(get(&space, Feature::WordLength), 3.0);
    // `e` (left middle, top row, 1.75) to the space bar (4.5, 3).
    assert_eq!(get(&space, Feature::RowChange), 3.0);
    assert!(close(
        get(&space, Feature::KeyDistance),
        (2.75f64 * 2.75 + 9.0).sqrt()
    ));
}

#[test]
fn same_finger_and_same_hand_transitions_are_recognized() {
    let corpus = corpus(&[("red", 1), ("as", 1)]);
    // `e` to `d`: both left middle finger, one row apart.
    let d = slot_features(
        &Prompt::new(["red"]),
        Slot {
            word: 0,
            position: 2,
        },
        Layout::QWERTY,
        &corpus,
    );
    assert_eq!(get(&d, Feature::SameFinger), 1.0);
    assert_eq!(get(&d, Feature::SameHand), 1.0);
    assert_eq!(get(&d, Feature::RowChange), 1.0);
    assert!(close(get(&d, Feature::KeyDistance), 1.0625f64.sqrt()));
    // `a` to `s`: same hand, neighboring fingers on the home row.
    let s = slot_features(
        &Prompt::new(["as"]),
        Slot {
            word: 0,
            position: 1,
        },
        Layout::QWERTY,
        &corpus,
    );
    assert_eq!(get(&s, Feature::SameFinger), 0.0);
    assert_eq!(get(&s, Feature::SameHand), 1.0);
    assert_eq!(get(&s, Feature::RowChange), 0.0);
    assert_eq!(get(&s, Feature::KeyDistance), 1.0);
}

#[test]
fn a_word_outside_the_corpus_takes_the_rarest_word_frequency() {
    let corpus = corpus(&[("the", 75), ("of", 20), ("cat", 5)]);
    assert!(close(log_word_frequency(&corpus, "of"), 0.20f64.ln()));
    assert!(close(log_word_frequency(&corpus, "xyz"), 0.05f64.ln()));
    let xyz = slot_features(
        &Prompt::new(["xyz"]),
        Slot {
            word: 0,
            position: 1,
        },
        Layout::QWERTY,
        &corpus,
    );
    assert!(close(get(&xyz, Feature::LogWordFrequency), 0.05f64.ln()));
}

// --- Fitting -------------------------------------------------------------------

/// A varied, deterministic set of feature vectors: indicators from the low
/// bits of the row number, continuous features from small multiples of it.
fn synthetic_features(n: usize) -> Vec<Features> {
    (0..n)
        .map(|i| {
            let mut f = [0.0; FEATURE_COUNT];
            f[Feature::FirstOfWord.index()] = f64::from(i % 2 == 0);
            f[Feature::LastOfWord.index()] = f64::from(i % 3 == 0);
            f[Feature::WordLength.index()] = 2.0 + (i % 7) as f64;
            f[Feature::LogWordFrequency.index()] = -4.0 - ((i * 5) % 11) as f64 * 0.7;
            f[Feature::Boundary.index()] = f64::from(i % 4 == 0);
            f[Feature::SameFinger.index()] = f64::from(i % 5 == 0);
            f[Feature::SameHand.index()] = f64::from(i % 3 == 1);
            f[Feature::RowChange.index()] = ((i * 3) % 4) as f64;
            f[Feature::KeyDistance.index()] = ((i * 7) % 13) as f64 * 0.5;
            f
        })
        .collect()
}

fn planted() -> Coefficients {
    Coefficients {
        intercept: 0.05,
        weights: [0.08, -0.03, 0.01, -0.02, 0.12, 0.15, -0.04, 0.03, 0.02],
    }
}

fn aggregates(planted: &Coefficients, n: usize) -> Vec<Aggregate> {
    synthetic_features(n)
        .into_iter()
        .enumerate()
        .map(|(i, features)| Aggregate {
            weight: 1.0 + (i % 6) as f64,
            residual: planted.effect(&features),
            features,
        })
        .collect()
}

#[test]
fn the_effect_is_the_intercept_plus_the_weighted_features() {
    let c = Coefficients {
        intercept: 0.1,
        weights: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5],
    };
    let mut f = [0.0; FEATURE_COUNT];
    f[Feature::FirstOfWord.index()] = 1.0;
    f[Feature::KeyDistance.index()] = 2.0;
    assert!(close(c.effect(&f), 0.1 + 1.0 + 1.0));
}

#[test]
fn the_fit_recovers_planted_coefficients_from_exact_aggregates() {
    let planted = planted();
    let fitted = Coefficients::fit(aggregates(&planted, 60), 1e-9).unwrap();
    assert!(
        (fitted.intercept - planted.intercept).abs() < 1e-6,
        "{fitted:?}"
    );
    for (i, (f, p)) in fitted.weights.iter().zip(&planted.weights).enumerate() {
        assert!((f - p).abs() < 1e-6, "{:?}: {f} vs {p}", Feature::ALL[i]);
    }
}

#[test]
fn a_heavier_ridge_penalty_shrinks_the_weights_but_not_the_intercept() {
    let planted = planted();
    let light = Coefficients::fit(aggregates(&planted, 60), 1e-9).unwrap();
    let heavy = Coefficients::fit(aggregates(&planted, 60), 1e6).unwrap();
    for (l, h) in light.weights.iter().zip(&heavy.weights) {
        assert!(h.abs() < l.abs() && h.abs() < 1e-3, "{l} → {h}");
    }
    // With the weights nearly gone, the intercept is close to the weighted
    // mean residual.
    let rows = aggregates(&planted, 60);
    let total: f64 = rows.iter().map(|a| a.weight).sum();
    let mean = rows.iter().map(|a| a.weight * a.residual).sum::<f64>() / total;
    assert!(
        (heavy.intercept - mean).abs() < 1e-2,
        "{} vs {mean}",
        heavy.intercept
    );
}

#[test]
fn a_fit_needs_some_weight_and_a_single_row_yields_only_an_intercept() {
    assert_eq!(Coefficients::fit(Vec::new(), 1.0), None);
    let weightless = Aggregate {
        weight: 0.0,
        features: synthetic_features(1)[0],
        residual: 0.3,
    };
    assert_eq!(Coefficients::fit([weightless], 1.0), None);

    let one = Aggregate {
        weight: 3.0,
        features: synthetic_features(1)[0],
        residual: 0.3,
    };
    let fitted = Coefficients::fit([one], 1.0).unwrap();
    assert!(close(fitted.intercept, 0.3));
    assert_eq!(fitted.weights, [0.0; FEATURE_COUNT]);
}

#[test]
fn an_unfitted_context_model_has_no_effect_anywhere() {
    let model = ContextModel::default();
    assert_eq!(model.coefficients, None);
    assert_eq!(model.completed_sessions, 0);
    for features in synthetic_features(5) {
        assert_eq!(model.effect(&features), 0.0);
    }
    let fitted = ContextModel {
        completed_sessions: 5,
        coefficients: Some(planted()),
    };
    let f = synthetic_features(1)[0];
    assert!(close(fitted.effect(&f), planted().effect(&f)));
}
