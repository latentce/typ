use std::collections::HashSet;

use typ_rs_core::compose::{self, ComposedPrompt, ComposedWord, Contamination, WordRole};
use typ_rs_core::corpus::Corpus;
use typ_rs_core::model::{ModelState, SchedulerConfig};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::random::Rng;
use typ_rs_core::scheduler::{
    SelectedTarget, TargetRole, TrainingEvent, TrainingHistory, achieved_doses, word_exposures,
};
use typ_rs_core::session::{EndCondition, Input, Key, SessionState};

fn config() -> SchedulerConfig {
    SchedulerConfig::default()
}

fn typed(prompt: &str, script: &str) -> SessionState {
    let mut state = SessionState::new(
        Prompt::new(prompt.split(' ')),
        EndCondition::AfterWords(usize::MAX),
    );
    for (i, c) in script.chars().enumerate() {
        state.apply_event(Input::new(i as u64 * 200_000, Key::Char(c)));
    }
    state
}

/// A model with `completed` clean completed sessions over common words,
/// then one with `x` for `a` in `cat`.
fn model_after(completed: u32) -> ModelState {
    let corpus = Corpus::bundled();
    let mut model = ModelState::new();
    let common: Vec<&str> = corpus
        .words()
        .iter()
        .take(100)
        .map(|w| w.text.as_ref())
        .collect();
    let words = common.join(" ");
    for i in 0..completed.saturating_sub(1) {
        model.apply_session(
            &typed(&words, &words),
            1_000 + i64::from(i),
            corpus,
            &config(),
        );
    }
    if completed > 0 {
        model.apply_session(
            &typed("cat cat cat", "cxt cxt cxt "),
            2_000,
            corpus,
            &config(),
        );
    }
    assert_eq!(model.context_model().completed_sessions, completed);
    model
}

fn compose_after(completed: u32, word_count: usize, seed: u64) -> ComposedPrompt {
    compose::next_prompt(
        &model_after(completed),
        Corpus::bundled(),
        &config(),
        &TrainingHistory::new(),
        3_000,
        word_count,
        seed,
    )
}

fn targeted_count(composed: &ComposedPrompt) -> usize {
    composed
        .words
        .iter()
        .filter(|w| w.role == WordRole::Targeted)
        .count()
}

// --- Frequency-weighted -------------------------------------------------------------

#[test]
fn a_frequency_weighted_prompt_has_the_requested_number_of_corpus_words_all_probes() {
    let corpus = Corpus::bundled();
    let known: HashSet<&str> = corpus.words().iter().map(|w| w.text.as_ref()).collect();

    for words in [1, 10, 50, 200] {
        let composed = compose::frequency_weighted(corpus, words, 3);
        assert_eq!(composed.prompt.word_count(), words);
        assert_eq!(composed.words.len(), words);
        assert!(
            composed
                .prompt
                .words()
                .iter()
                .all(|w| known.contains(w.as_ref()))
        );
        assert!(composed.words.iter().all(|w| w.role == WordRole::Probe));
        assert!(composed.targets.is_empty());
    }
}

#[test]
fn the_same_seed_composes_the_same_prompt_and_a_different_seed_does_not() {
    let corpus = Corpus::bundled();
    assert_eq!(
        compose::frequency_weighted(corpus, 50, 7),
        compose::frequency_weighted(corpus, 50, 7)
    );
    assert_ne!(
        compose::frequency_weighted(corpus, 50, 7),
        compose::frequency_weighted(corpus, 50, 8)
    );
}

// --- Ramp ------------------------------------------------------------------------------

#[test]
fn the_targeted_share_ramps_from_nothing_through_the_first_completed_sessions() {
    let config = config();
    let share = |completed| compose::targeted_share(completed, &config);
    assert_eq!(share(0), 0.0);
    assert!((share(1) - 0.30).abs() < 1e-12, "{}", share(1));
    assert!(
        (share(2) - (0.30 + 0.50 / 3.0)).abs() < 1e-12,
        "{}",
        share(2)
    );
    assert!(
        (share(3) - (0.30 + 1.00 / 3.0)).abs() < 1e-12,
        "{}",
        share(3)
    );
    assert!((share(4) - 0.80).abs() < 1e-12, "{}", share(4));
    assert_eq!(share(5), 0.80);
    assert_eq!(share(100), 0.80);
}

#[test]
fn the_first_prompt_is_all_probes_and_selects_nothing() {
    let composed = compose_after(0, 50, 1);
    assert_eq!(composed.prompt.word_count(), 50);
    assert_eq!(targeted_count(&composed), 0);
    assert!(composed.targets.is_empty());
    // Identical to a plain frequency-weighted draw with the same seed.
    assert_eq!(
        composed.prompt,
        compose::frequency_weighted(Corpus::bundled(), 50, 1).prompt
    );
}

#[test]
fn the_targeted_word_count_follows_the_ramp_rounded_to_the_nearest_word() {
    // 30% of 50 is 15; 80% of 50 is 40; 46.7% of 50 is 23.3, so 23.
    assert_eq!(targeted_count(&compose_after(1, 50, 1)), 15);
    assert_eq!(targeted_count(&compose_after(2, 50, 1)), 23);
    assert_eq!(targeted_count(&compose_after(4, 50, 1)), 40);
    assert_eq!(targeted_count(&compose_after(7, 50, 1)), 40);
    assert_eq!(targeted_count(&compose_after(7, 10, 1)), 8);
}

// --- Word selection ------------------------------------------------------------------

fn target(pattern: &str, role: TargetRole, priority: f64) -> SelectedTarget {
    SelectedTarget {
        pattern: pattern.into(),
        role,
        weakness_mean: 0.5,
        weakness_sd: 0.2,
        priority,
        planned_dose: if role == TargetRole::Deferred { 0 } else { 6 },
    }
}

fn targets(patterns: &[&str]) -> Vec<SelectedTarget> {
    patterns
        .iter()
        .map(|p| target(p, TargetRole::Target, 0.1))
        .collect()
}

fn compose_with(
    config: &SchedulerConfig,
    history: &TrainingHistory,
    targets: Vec<SelectedTarget>,
    targeted_count: usize,
    word_count: usize,
    seed: u64,
) -> ComposedPrompt {
    compose::compose(
        Corpus::bundled(),
        config,
        history,
        targets,
        targeted_count,
        word_count,
        &mut Rng::seeded(seed),
    )
}

/// A config whose softmax all but always picks the best-scored word.
fn greedy() -> SchedulerConfig {
    SchedulerConfig {
        temperature: 0.01,
        ..config()
    }
}

fn words_with_meta(composed: &ComposedPrompt) -> Vec<(&str, &ComposedWord)> {
    composed
        .prompt
        .words()
        .iter()
        .map(|w| w.as_ref())
        .zip(&composed.words)
        .collect()
}

fn the_targeted_word(composed: &ComposedPrompt) -> &str {
    let mut targeted = composed.targeted_words();
    let word = targeted.next().expect("one targeted word");
    assert_eq!(targeted.next(), None);
    word
}

/// Types every word of the prompt correctly, ending on the final space.
fn typed_fully(composed: &ComposedPrompt) -> SessionState {
    let text = composed.prompt.text();
    typed(&text, &format!("{text} "))
}

#[test]
fn targeted_words_are_distinct_corpus_words_that_expose_practised_patterns() {
    let corpus = Corpus::bundled();
    for seed in 0..5 {
        let composed = compose_after(5, 50, seed);
        let practised: Vec<&str> = composed.practised().map(|t| t.pattern.as_ref()).collect();
        assert!(!practised.is_empty());
        let mut seen = HashSet::new();
        let mut exposing = 0;
        for (word, meta) in words_with_meta(&composed) {
            match meta.role {
                WordRole::Targeted => {
                    let padded = format!(" {word} ");
                    assert!(
                        meta.exposed_targets
                            .iter()
                            .all(|p| padded.contains(p.as_ref()) && practised.contains(&p.as_ref())),
                        "{word:?} does not expose {:?}",
                        meta.exposed_targets
                    );
                    exposing += usize::from(!meta.exposed_targets.is_empty());
                    assert!(seen.insert(word), "{word:?} picked twice");
                    assert!(corpus.word_by_text(word).is_some());
                    assert!(meta.selection_score.is_some());
                    assert_eq!(meta.contamination, None);
                }
                WordRole::Probe => {
                    assert!(meta.exposed_targets.is_empty());
                    assert_eq!(meta.selection_score, None);
                    assert!(meta.contamination.is_some());
                }
            }
        }
        // Once every target has its dose the remaining picks go by
        // frequency alone, and the pool also holds a sample of words that
        // expose nothing; most targeted words still expose something.
        assert!(exposing * 4 >= targeted_count(&composed) * 3, "{exposing}");
    }
}

#[test]
fn every_practised_pattern_reaches_its_dose_when_the_corpus_allows() {
    let config = config();
    for seed in 0..10 {
        let mut selected = targets(&["th", "an", "in", "er", "on"]);
        selected.push(target("ou", TargetRole::Explore, 0.0));
        let composed = compose_with(&config, &TrainingHistory::new(), selected, 40, 50, seed);
        assert_eq!(targeted_count(&composed), 40);
        let events = achieved_doses(&typed_fully(&composed), &composed.targets);
        for e in &events {
            assert!(
                e.achieved_dose >= config.dose,
                "seed {seed}: {:?} got {} of {}",
                e.target.pattern,
                e.achieved_dose,
                config.dose
            );
        }
    }
}

#[test]
fn targets_the_scheduler_selects_reach_their_dose_or_exhaust_the_corpus() {
    let corpus = Corpus::bundled();
    let config = config();
    for seed in 0..5 {
        let composed = compose_after(7, 50, seed);
        assert_eq!(targeted_count(&composed), 40);
        let events = achieved_doses(&typed_fully(&composed), &composed.targets);
        for e in events.iter().filter(|e| e.target.role.is_practised()) {
            // A pattern in few corpus words cannot be exposed more often
            // than once per distinct word.
            let possible = config
                .dose
                .min(corpus.words_containing(&e.target.pattern).len());
            assert!(
                e.achieved_dose >= possible,
                "seed {seed}: {:?} got {} of {possible}",
                e.target.pattern,
                e.achieved_dose
            );
        }
    }
}

#[test]
fn the_exploration_target_is_practised_however_low_its_priority() {
    let config = config();
    let selected = vec![
        target("th", TargetRole::Target, 0.9),
        target("ou", TargetRole::Explore, 0.0),
    ];
    let composed = compose_with(&config, &TrainingHistory::new(), selected, 20, 25, 1);
    let events = achieved_doses(&typed_fully(&composed), &composed.targets);
    assert!(
        events.iter().all(|e| e.achieved_dose >= config.dose),
        "{events:?}"
    );
}

#[test]
fn a_word_shown_as_targeted_within_the_recent_window_is_penalised() {
    let config = greedy();
    let selected = || targets(&["th"]);
    // The best word for `th` is the most common one containing it.
    let fresh = compose_with(&config, &TrainingHistory::new(), selected(), 1, 1, 1);
    assert_eq!(the_targeted_word(&fresh), "the");

    let mut history = TrainingHistory::new();
    history.record(&[], ["the"], &config);
    for _ in 1..config.recent_word_sessions {
        history.record(&[], [], &config);
    }
    assert_eq!(history.sessions(), config.recent_word_sessions);
    let within = compose_with(&config, &history, selected(), 1, 1, 1);
    assert_ne!(the_targeted_word(&within), "the");

    history.record(&[], [], &config);
    let beyond = compose_with(&config, &history, selected(), 1, 1, 1);
    assert_eq!(the_targeted_word(&beyond), "the");
}

#[test]
fn a_word_stacking_more_targets_than_the_overload_limit_is_penalised() {
    // "the" exposes all four: word-initial `t`, `th`, `he`, and `e` before
    // the space.
    let selected = || targets(&[" t", "th", "he", "e "]);
    let unpenalised = SchedulerConfig {
        overload_penalty: 0.0,
        ..greedy()
    };
    let composed = compose_with(&unpenalised, &TrainingHistory::new(), selected(), 1, 1, 1);
    assert_eq!(the_targeted_word(&composed), "the");
    assert_eq!(composed.words[0].exposed_targets.len(), 4);

    let penalised = SchedulerConfig {
        overload_penalty: 10.0,
        ..greedy()
    };
    let composed = compose_with(&penalised, &TrainingHistory::new(), selected(), 1, 1, 1);
    assert!(
        composed.words[0].exposed_targets.len() <= penalised.overload_targets,
        "{:?}",
        composed.words[0]
    );
}

#[test]
fn a_word_longer_than_the_limit_is_penalised() {
    let corpus = Corpus::bundled();
    let selected = || targets(&["ati"]);
    let unpenalised = SchedulerConfig {
        length_penalty: 0.0,
        ..greedy()
    };
    let composed = compose_with(&unpenalised, &TrainingHistory::new(), selected(), 5, 5, 1);
    let lengths = |c: &ComposedPrompt| -> Vec<usize> {
        c.targeted_words()
            .map(|w| corpus.word_by_text(w).unwrap().length as usize)
            .collect()
    };
    assert!(
        lengths(&composed)
            .iter()
            .any(|&l| l > unpenalised.long_word_length),
        "{:?}",
        composed.prompt
    );

    let penalised = SchedulerConfig {
        length_penalty: 10.0,
        ..greedy()
    };
    let composed = compose_with(&penalised, &TrainingHistory::new(), selected(), 5, 5, 1);
    assert!(
        lengths(&composed)
            .iter()
            .all(|&l| l <= penalised.long_word_length),
        "{:?}",
        composed.prompt
    );
}

#[test]
fn a_higher_temperature_spreads_the_draw_over_more_words() {
    let distinct = |temperature: f64| {
        let config = SchedulerConfig {
            temperature,
            ..config()
        };
        (0..30)
            .map(|seed| {
                the_targeted_word(&compose_with(
                    &config,
                    &TrainingHistory::new(),
                    targets(&["th"]),
                    1,
                    1,
                    seed,
                ))
                .to_string()
            })
            .collect::<HashSet<_>>()
            .len()
    };
    assert_eq!(distinct(0.01), 1);
    assert!(distinct(1.0) > 1);
    assert!(distinct(5.0) > distinct(1.0));
}

// --- Probes ----------------------------------------------------------------------------

#[test]
fn probes_are_not_filtered_so_a_targeted_word_can_also_be_a_probe() {
    let config = config();
    let mut repeated_as_probe = false;
    for seed in 0..40 {
        let composed = compose_with(
            &config,
            &TrainingHistory::new(),
            targets(&["th"]),
            40,
            50,
            seed,
        );
        let targeted: HashSet<&str> = composed.targeted_words().collect();
        let probes: Vec<&str> = words_with_meta(&composed)
            .into_iter()
            .filter(|(_, m)| m.role == WordRole::Probe)
            .map(|(w, _)| w)
            .collect();
        assert_eq!(probes.len(), 10);
        repeated_as_probe |= probes.iter().any(|p| targeted.contains(p));
    }
    assert!(repeated_as_probe);
}

#[test]
fn probes_may_repeat_within_a_prompt() {
    let composed = compose_with(
        &config(),
        &TrainingHistory::new(),
        targets(&["th"]),
        1,
        400,
        1,
    );
    let probes: Vec<&str> = words_with_meta(&composed)
        .into_iter()
        .filter(|(_, m)| m.role == WordRole::Probe)
        .map(|(w, _)| w)
        .collect();
    assert!(probes.iter().collect::<HashSet<_>>().len() < probes.len());
}

#[test]
fn a_probe_records_its_overlap_with_this_prompts_and_recent_practice() {
    let config = config();
    let mut history = TrainingHistory::new();
    history.record(
        &[TrainingEvent {
            target: target("th", TargetRole::Target, 0.1),
            achieved_dose: 6,
        }],
        ["the"],
        &config,
    );
    let mut saw_the = false;
    let mut saw_current = false;
    let mut saw_clean = false;
    for seed in 0..60 {
        let composed = compose_with(&config, &history, targets(&["an"]), 40, 50, seed);
        let targeted: HashSet<&str> = composed.targeted_words().collect();
        for (word, meta) in words_with_meta(&composed) {
            let Some(c) = &meta.contamination else {
                continue;
            };
            let padded = format!(" {word} ");
            if word == "the" {
                saw_the = true;
                assert!(c.recently_targeted_word);
                assert!(c.recently_targeted_patterns.contains(&"th".into()), "{c:?}");
            }
            if padded.contains("an") {
                saw_current = true;
                assert!(c.recently_targeted_patterns.contains(&"an".into()), "{c:?}");
            }
            if targeted.contains(word) {
                assert!(c.recently_targeted_word, "{word:?} is targeted here");
            }
            if !targeted.contains(word)
                && word != "the"
                && !padded.contains("an")
                && !padded.contains("th")
            {
                saw_clean = true;
                assert_eq!(*c, Contamination::default(), "{word:?}");
            }
            for p in &c.recently_targeted_patterns {
                assert!(padded.contains(p.as_ref()), "{word:?} lacks {p:?}");
            }
        }
    }
    assert!(saw_the && saw_current && saw_clean);
}

#[test]
fn practice_older_than_the_contamination_window_does_not_count() {
    let config = SchedulerConfig {
        contamination_sessions: 2,
        ..config()
    };
    let mut history = TrainingHistory::new();
    history.record(
        &[TrainingEvent {
            target: target("th", TargetRole::Target, 0.1),
            achieved_dose: 6,
        }],
        ["the"],
        &config,
    );
    history.record(&[], [], &config);
    let probe_the = |history: &TrainingHistory| {
        (0..60).find_map(|seed| {
            let composed = compose_with(&config, history, targets(&["an"]), 40, 50, seed);
            words_with_meta(&composed)
                .into_iter()
                .find(|(w, m)| *w == "the" && m.role == WordRole::Probe)
                .and_then(|(_, m)| m.contamination.clone())
        })
    };
    let within = probe_the(&history).expect("a probe \"the\"");
    assert!(within.recently_targeted_word);
    assert_eq!(within.recently_targeted_patterns, vec!["th".into()]);

    history.record(&[], [], &config);
    let beyond = probe_the(&history).expect("a probe \"the\"");
    assert_eq!(beyond, Contamination::default());
}

#[test]
fn an_all_probe_prompt_still_assesses_contamination() {
    let config = config();
    let corpus = Corpus::bundled();
    let common: Vec<&str> = corpus
        .words()
        .iter()
        .take(100)
        .map(|w| w.text.as_ref())
        .collect();
    let mut history = TrainingHistory::new();
    history.record(&[], common.iter().copied(), &config);
    let composed = compose_with(&config, &history, Vec::new(), 0, 50, 1);
    assert_eq!(targeted_count(&composed), 0);
    let mut flagged = 0;
    for (word, meta) in words_with_meta(&composed) {
        let c = meta.contamination.as_ref().expect("assessed");
        assert_eq!(c.recently_targeted_word, common.contains(&word), "{word:?}");
        assert!(c.recently_targeted_patterns.is_empty());
        flagged += usize::from(c.recently_targeted_word);
    }
    assert!(flagged > 0);
}

// --- Assembly --------------------------------------------------------------------------

#[test]
fn exposures_of_one_target_are_kept_apart_by_the_minimum_gap() {
    let config = config();
    for seed in 0..50 {
        let composed = compose_with(
            &config,
            &TrainingHistory::new(),
            targets(&["th", "an", "in", "er", "on"]),
            40,
            50,
            seed,
        );
        // A probe records no exposures, but the slots it happens to expose
        // a target with are kept apart all the same.
        let exposed: Vec<Vec<&str>> = words_with_meta(&composed)
            .into_iter()
            .map(|(word, meta)| match meta.role {
                WordRole::Targeted => meta.exposed_targets.iter().map(AsRef::as_ref).collect(),
                WordRole::Probe => word_exposures(word, &composed.targets)
                    .into_keys()
                    .collect(),
            })
            .collect();
        for i in 0..exposed.len() {
            for j in i + 1..(i + config.min_exposure_gap).min(exposed.len()) {
                let shared: Vec<&str> = exposed[i]
                    .iter()
                    .copied()
                    .filter(|p| exposed[j].contains(p))
                    .collect();
                assert!(
                    shared.is_empty(),
                    "seed {seed}: words {i} and {j} both expose {shared:?} in {:?}",
                    composed.prompt
                );
            }
        }
    }
}

#[test]
fn targeted_words_and_probes_are_mixed_rather_than_grouped() {
    let composed = compose_with(
        &config(),
        &TrainingHistory::new(),
        targets(&["th"]),
        25,
        50,
        3,
    );
    let first_half_probes = composed.words[..25]
        .iter()
        .filter(|w| w.role == WordRole::Probe)
        .count();
    assert!((5..=20).contains(&first_half_probes), "{first_half_probes}");
}

#[test]
fn the_prompt_records_every_selected_target_with_its_dose() {
    let config = config();
    let composed = compose_after(5, 50, 2);
    assert!(
        composed
            .targets
            .iter()
            .any(|t| t.role == TargetRole::Target)
    );
    assert_eq!(
        composed
            .targets
            .iter()
            .filter(|t| t.role == TargetRole::Explore)
            .count(),
        1
    );
    for t in &composed.targets {
        assert!(t.weakness_sd > 0.0);
        match t.role {
            TargetRole::Deferred => assert_eq!(t.planned_dose, 0),
            _ => assert_eq!(t.planned_dose, config.dose),
        }
    }
}

#[test]
fn composition_is_deterministic_in_the_seed() {
    assert_eq!(compose_after(5, 50, 9), compose_after(5, 50, 9));
    assert_ne!(
        compose_after(5, 50, 9).prompt,
        compose_after(5, 50, 10).prompt
    );
}
