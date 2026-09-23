use std::collections::HashSet;

use typ_rs_core::compose::{self, ComposedPrompt, WordRole};
use typ_rs_core::corpus::Corpus;
use typ_rs_core::model::{ModelState, SchedulerConfig};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::scheduler::{TargetRole, TrainingHistory};
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

// --- Picker ----------------------------------------------------------------------------

#[test]
fn targeted_words_contain_a_practised_pattern_and_are_not_repeated() {
    let corpus = Corpus::bundled();
    let composed = compose_after(5, 50, 2);
    let practised: Vec<&str> = composed.practised().map(|t| t.pattern.as_ref()).collect();
    assert!(!practised.is_empty());
    let mut seen = HashSet::new();
    for (word, meta) in composed.prompt.words().iter().zip(&composed.words) {
        match meta.role {
            WordRole::Targeted => {
                let padded = format!(" {word} ");
                assert!(
                    meta.exposed_targets
                        .iter()
                        .all(|p| padded.contains(p.as_ref())),
                    "{word:?} does not contain {:?}",
                    meta.exposed_targets
                );
                assert!(!meta.exposed_targets.is_empty(), "{word:?} exposes nothing");
                assert!(
                    meta.exposed_targets
                        .iter()
                        .all(|p| practised.contains(&p.as_ref()))
                );
                assert!(seen.insert(word.clone()), "{word:?} picked twice");
                assert!(corpus.word_by_text(word).is_some());
            }
            WordRole::Probe => assert!(meta.exposed_targets.is_empty()),
        }
    }
    // Every practised pattern that has words gets a share of them.
    for pattern in &practised {
        let exposed = composed
            .words
            .iter()
            .filter(|w| w.exposed_targets.iter().any(|p| p.as_ref() == *pattern))
            .count();
        assert!(exposed >= 1, "{pattern:?} never exposed");
    }
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
