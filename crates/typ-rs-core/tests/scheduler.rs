use std::collections::{BTreeMap, BTreeSet};

use typ_rs_core::corpus::Corpus;
use typ_rs_core::model::{ModelState, SchedulerConfig};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::random::Rng;
use typ_rs_core::scheduler::{
    self, SelectedTarget, TargetRole, TrainingEvent, TrainingHistory, achieved_doses,
    eligible_patterns, same_chain, select_targets, word_exposures,
};
use typ_rs_core::session::{EndCondition, Input, Key, SessionState};

fn config() -> SchedulerConfig {
    SchedulerConfig::default()
}

/// Types `script` against `prompt` at 200 ms per keystroke; `⎋` interrupts.
fn typed(prompt: &str, script: &str) -> SessionState {
    let mut state = SessionState::new(
        Prompt::new(prompt.split(' ')),
        EndCondition::AfterWords(usize::MAX),
    );
    for (i, c) in script.chars().enumerate() {
        let key = match c {
            '⎋' => Key::Interrupt,
            c => Key::Char(c),
        };
        state.apply_event(Input::new(i as u64 * 200_000, key));
    }
    state
}

/// A model that has seen the 120 most common words cleanly twice, then
/// three sessions with `x` for `a` in `cat` and a pause before the `g` of
/// `catalog`.
fn trained_model() -> ModelState {
    let corpus = Corpus::bundled();
    let mut model = ModelState::new();
    let common: Vec<&str> = corpus
        .words()
        .iter()
        .take(120)
        .map(|w| w.text.as_ref())
        .collect();
    let words = common.join(" ");
    model.apply_session(&typed(&words, &words), 1_000, corpus, &config());
    model.apply_session(&typed(&words, &words), 1_500, corpus, &config());
    for i in 0..3 {
        let mut state = SessionState::new(
            Prompt::new(["that", "cat", "the", "catalog"]),
            EndCondition::AfterWords(4),
        );
        let mut at = 0;
        for c in "that cxt the catalog".chars() {
            if c == 'g' {
                at += 2_200_000;
            }
            state.apply_event(Input::new(at, Key::Char(c)));
            at += 200_000;
        }
        state.apply_event(Input::new(at, Key::Char(' ')));
        model.apply_session(&state, 2_000 + i, corpus, &config());
    }
    model
}

fn select(model: &ModelState, history: &TrainingHistory, seed: u64) -> Vec<SelectedTarget> {
    select_targets(
        model,
        Corpus::bundled(),
        &config(),
        history,
        3_000,
        &mut Rng::seeded(seed),
    )
}

fn with_role(targets: &[SelectedTarget], role: TargetRole) -> Vec<&str> {
    targets
        .iter()
        .filter(|t| t.role == role)
        .map(|t| t.pattern.as_ref())
        .collect()
}

fn target(pattern: &str, role: TargetRole, weakness_mean: f64) -> SelectedTarget {
    SelectedTarget {
        pattern: pattern.into(),
        role,
        weakness_mean,
        weakness_sd: 0.1,
        priority: 1.0,
        planned_dose: if role == TargetRole::Deferred { 0 } else { 6 },
    }
}

fn event(pattern: &str, role: TargetRole, achieved_dose: usize) -> TrainingEvent {
    TrainingEvent {
        target: target(pattern, role, 0.5),
        achieved_dose,
    }
}

// --- Eligibility -----------------------------------------------------------------

#[test]
fn eligible_patterns_are_bigrams_and_trigrams_in_enough_words_above_the_importance_floor() {
    let corpus = Corpus::bundled();
    let config = config();
    let eligible = eligible_patterns(corpus, &config);
    assert!(eligible.len() > 1_000, "{}", eligible.len());
    for e in &eligible {
        let len = e.pattern.chars().count();
        assert!(len == 2 || len == 3, "{:?}", e.pattern);
        assert!(
            corpus.words_containing(&e.pattern).len() >= config.min_pattern_words,
            "{:?}",
            e.pattern
        );
        assert!(e.importance >= config.importance_floor, "{:?}", e.pattern);
        assert_eq!(e.importance, corpus.pattern_frequency(&e.pattern).sqrt());
    }
    let patterns: BTreeSet<&str> = eligible.iter().map(|e| e.pattern.as_ref()).collect();
    assert!(patterns.contains(" th"), "word-initial trigram");
    assert!(patterns.contains("e "), "word-final bigram");
    assert!(
        !patterns.contains("e t"),
        "a trigram straddling two words has no word"
    );

    // A rare trigram is excluded by the word count or the floor, never by
    // being a trigram; raising the floor removes more.
    let strict = SchedulerConfig {
        importance_floor: 0.05,
        ..config
    };
    assert!(eligible_patterns(corpus, &strict).len() < eligible.len() / 4);
}

#[test]
fn characters_are_never_selected() {
    let model = trained_model();
    for seed in 0..5 {
        let targets = select(&model, &TrainingHistory::new(), seed);
        assert!(!targets.is_empty());
        for t in &targets {
            assert!(t.pattern.chars().count() >= 2, "{:?}", t.pattern);
        }
    }
}

// --- Ranking ----------------------------------------------------------------------

#[test]
fn a_planted_weakness_is_a_candidate_in_nearly_every_draw_and_usually_a_target() {
    let model = trained_model();
    let mut candidate = 0;
    let mut target = 0;
    for seed in 0..20 {
        let targets = select(&model, &TrainingHistory::new(), seed);
        // The error every session lands on ` ca` and, through the chain, on
        // `ca`; the two share a chain, so at most one is a candidate.
        let planted: Vec<&SelectedTarget> = targets
            .iter()
            .filter(|t| [" ca", "ca"].contains(&t.pattern.as_ref()))
            .collect();
        if planted.iter().any(|t| t.role != TargetRole::Explore) {
            candidate += 1;
        }
        if planted.iter().any(|t| t.role == TargetRole::Target) {
            target += 1;
        }
    }
    assert!(candidate >= 18, "candidate in {candidate} of 20 draws");
    assert!(target >= 10, "target in {target} of 20 draws");
}

#[test]
fn a_chain_relates_a_pattern_to_its_suffixes_in_both_directions_and_nothing_else() {
    assert!(
        same_chain("th", "ath"),
        "bigram against the trigram above it"
    );
    assert!(
        same_chain("ath", "th"),
        "trigram against the bigram below it"
    );
    assert!(same_chain("h", "ath"));
    assert!(
        !same_chain("th", "the"),
        "`the` backs off to `he`, not `th`"
    );
    assert!(!same_chain("th", "th"), "a pattern is not its own ancestor");
    assert!(!same_chain(" t", "t "));
}

#[test]
fn no_two_selected_candidates_share_a_chain() {
    let model = trained_model();
    for seed in 0..10 {
        let targets = select(&model, &TrainingHistory::new(), seed);
        let candidates: Vec<&str> = targets
            .iter()
            .filter(|t| t.role != TargetRole::Explore)
            .map(|t| t.pattern.as_ref())
            .collect();
        for (i, a) in candidates.iter().enumerate() {
            for b in &candidates[i + 1..] {
                assert!(
                    !(a.ends_with(b) || b.ends_with(a)),
                    "{a:?} and {b:?} selected together (seed {seed})"
                );
            }
        }
    }
}

#[test]
fn the_targets_and_deferred_candidates_come_from_the_top_of_the_ranking() {
    let config = config();
    let model = trained_model();
    let targets = select(&model, &TrainingHistory::new(), 1);
    let candidates = targets
        .iter()
        .filter(|t| t.role != TargetRole::Explore)
        .count();
    assert!(candidates <= config.candidates);
    assert!(with_role(&targets, TargetRole::Target).len() <= config.max_targets);
    for t in &targets {
        assert!(t.weakness_sd > 0.0);
        if t.role == TargetRole::Target {
            assert!(t.priority > 0.0, "{t:?}");
        }
        match t.role {
            TargetRole::Deferred => assert_eq!(t.planned_dose, 0),
            _ => assert_eq!(t.planned_dose, config.dose),
        }
    }
    // Targets are listed in rank order.
    let priorities: Vec<f64> = targets
        .iter()
        .filter(|t| t.role == TargetRole::Target)
        .map(|t| t.priority)
        .collect();
    assert!(
        priorities.windows(2).all(|w| w[0] >= w[1]),
        "{priorities:?}"
    );
}

#[test]
fn the_same_seed_selects_the_same_targets_and_another_seed_differs() {
    let model = trained_model();
    let history = TrainingHistory::new();
    assert_eq!(select(&model, &history, 7), select(&model, &history, 7));
    assert_ne!(select(&model, &history, 7), select(&model, &history, 8));
}

// --- Deferral ---------------------------------------------------------------------

#[test]
fn a_candidate_is_deferred_with_the_configured_probability_and_never_when_it_is_zero() {
    let model = trained_model();
    let mut deferred = 0;
    let mut candidates = 0;
    for seed in 0..40 {
        let targets = select(&model, &TrainingHistory::new(), seed);
        deferred += with_role(&targets, TargetRole::Deferred).len();
        candidates += targets
            .iter()
            .filter(|t| t.role != TargetRole::Explore)
            .count();
    }
    let share = deferred as f64 / candidates as f64;
    assert!((0.15..0.35).contains(&share), "{deferred} of {candidates}");

    let never = SchedulerConfig {
        deferral_probability: 0.0,
        ..config()
    };
    for seed in 0..5 {
        let targets = select_targets(
            &model,
            Corpus::bundled(),
            &never,
            &TrainingHistory::new(),
            3_000,
            &mut Rng::seeded(seed),
        );
        assert!(with_role(&targets, TargetRole::Deferred).is_empty());
        assert_eq!(
            with_role(&targets, TargetRole::Target).len(),
            never.max_targets
        );
    }
}

#[test]
fn a_deferred_pattern_stays_out_of_candidacy_and_is_logged_for_the_whole_window() {
    let config = config();
    let model = trained_model();
    let mut history = TrainingHistory::new();
    let first = select(&model, &history, 3);
    let deferred: Vec<&str> = with_role(&first, TargetRole::Deferred);
    assert!(!deferred.is_empty(), "seed 3 defers nothing; pick another");
    let pattern = deferred[0].to_string();
    history.record(&achieved_doses(&typed("cat", "cat"), &first), [], &config);
    assert_eq!(history.pattern(&pattern).unwrap().deferral_remaining, 2);

    // Two more sessions: logged as deferred each time, never a target or
    // exploration target, and the window runs down.
    for remaining in [1, 0] {
        let next = select(&model, &history, 4);
        let roles: Vec<TargetRole> = next
            .iter()
            .filter(|t| t.pattern.as_ref() == pattern)
            .map(|t| t.role)
            .collect();
        assert_eq!(roles, vec![TargetRole::Deferred], "{pattern:?}: {roles:?}");
        history.record(&achieved_doses(&typed("cat", "cat"), &next), [], &config);
        assert_eq!(
            history.pattern(&pattern).unwrap().deferral_remaining,
            remaining
        );
    }
    assert!(!history.is_deferred(&pattern));

    // Free again: with deferral switched off it can be a target once more.
    let free = SchedulerConfig {
        deferral_probability: 0.0,
        ..config
    };
    let targets = select_targets(
        &model,
        Corpus::bundled(),
        &free,
        &history,
        3_000,
        &mut Rng::seeded(4),
    );
    assert!(
        targets
            .iter()
            .all(|t| t.pattern.as_ref() != pattern || t.role != TargetRole::Deferred)
    );
}

#[test]
fn having_no_targets_at_all_is_accepted_rather_than_undeferring() {
    let always = SchedulerConfig {
        deferral_probability: 1.0,
        ..config()
    };
    let targets = select_targets(
        &trained_model(),
        Corpus::bundled(),
        &always,
        &TrainingHistory::new(),
        3_000,
        &mut Rng::seeded(1),
    );
    assert!(with_role(&targets, TargetRole::Target).is_empty());
    assert_eq!(
        with_role(&targets, TargetRole::Deferred).len(),
        always.candidates
    );
    assert_eq!(with_role(&targets, TargetRole::Explore).len(), 1);
}

// --- Exploration ------------------------------------------------------------------

#[test]
fn one_exploration_target_is_drawn_from_neither_selected_nor_deferred_patterns() {
    let model = trained_model();
    let mut history = TrainingHistory::new();
    history.record(
        &[
            event(" th", TargetRole::Deferred, 0),
            event("he", TargetRole::Deferred, 0),
        ],
        [],
        &config(),
    );
    for seed in 0..20 {
        let targets = select(&model, &history, seed);
        let explore = with_role(&targets, TargetRole::Explore);
        assert_eq!(explore.len(), 1, "seed {seed}");
        let others: Vec<&str> = targets
            .iter()
            .filter(|t| t.role != TargetRole::Explore)
            .map(|t| t.pattern.as_ref())
            .collect();
        assert!(!others.contains(&explore[0]), "seed {seed}: {targets:?}");
        assert!(!history.is_deferred(explore[0]));
        assert_ne!(explore[0], " th");
        assert_ne!(explore[0], "he");
    }
}

// --- Plateau ----------------------------------------------------------------------

#[test]
fn a_target_plateaus_after_enough_practice_without_change_and_recovers_when_untargeted() {
    let config = config();
    let mut history = TrainingHistory::new();
    // Three sessions with six exposures each: not yet enough sessions.
    for _ in 0..3 {
        history.record(&[event("th", TargetRole::Target, 6)], [], &config);
    }
    assert_eq!(history.plateau_factor("th", 0.5, 0.1, &config), 1.0);
    // A fourth: 4 sessions, 24 > 20 exposures, weakness unchanged within sd.
    history.record(&[event("th", TargetRole::Target, 6)], [], &config);
    assert_eq!(history.plateau_factor("th", 0.5, 0.1, &config), 0.5);
    // A change larger than the uncertainty is not a plateau.
    assert_eq!(history.plateau_factor("th", 0.2, 0.1, &config), 1.0);
    // Nor is enough sessions without enough dose.
    let mut light = TrainingHistory::new();
    for _ in 0..4 {
        light.record(&[event("th", TargetRole::Target, 5)], [], &config);
    }
    assert_eq!(light.plateau_factor("th", 0.5, 0.1, &config), 1.0);

    // Untargeted sessions recover the factor linearly toward one.
    for _ in 0..5 {
        history.record(&[], [], &config);
    }
    assert_eq!(history.plateau_factor("th", 0.5, 0.1, &config), 0.75);
    for _ in 0..5 {
        history.record(&[], [], &config);
    }
    assert_eq!(history.plateau_factor("th", 0.5, 0.1, &config), 1.0);
}

/// A pattern's first estimate is its noisiest: shrunk toward its parent
/// before it has evidence of its own. A pattern that is truly weak and
/// never changes moves away from that first estimate as evidence arrives,
/// so the plateau check looks back only as far as the practice window.
#[test]
fn a_plateau_is_judged_against_the_weakness_when_the_practice_window_began() {
    let config = config();
    let mut history = TrainingHistory::new();
    let practiced_at = |history: &mut TrainingHistory, mean: f64| {
        let mut e = event("th", TargetRole::Target, 6);
        e.target.weakness_mean = mean;
        history.record(&[e], [], &config);
    };
    // Shrunk first estimates, then settled ones.
    for mean in [0.3, 0.6, 0.9, 1.0, 1.0] {
        practiced_at(&mut history, mean);
    }
    // The window of four practiced sessions began at 0.6: a change.
    assert_eq!(history.plateau_factor("th", 1.0, 0.1, &config), 1.0);
    practiced_at(&mut history, 1.0);
    // Now it began at 0.9: within an uncertainty of 0.15, not of 0.05.
    assert_eq!(history.plateau_factor("th", 1.0, 0.15, &config), 0.5);
    assert_eq!(history.plateau_factor("th", 1.0, 0.05, &config), 1.0);
}

#[test]
fn a_deferral_window_runs_down_with_every_session_even_one_that_selected_nothing() {
    let config = config();
    let mut history = TrainingHistory::new();
    history.record(&[event("th", TargetRole::Deferred, 0)], [], &config);
    assert_eq!(history.pattern("th").unwrap().deferral_remaining, 2);
    history.record(&[], [], &config);
    assert_eq!(history.pattern("th").unwrap().deferral_remaining, 1);
    history.record(&[], [], &config);
    assert!(!history.is_deferred("th"));
    // Freed, it can be deferred afresh for a full window.
    history.record(&[event("th", TargetRole::Deferred, 0)], [], &config);
    assert_eq!(history.pattern("th").unwrap().deferral_remaining, 2);
}

#[test]
fn an_exploration_session_counts_as_practice_and_a_deferral_does_not() {
    let config = config();
    let mut history = TrainingHistory::new();
    history.record(&[event("th", TargetRole::Explore, 4)], [], &config);
    history.record(&[event("th", TargetRole::Deferred, 3)], [], &config);
    let h = history.pattern("th").unwrap();
    assert_eq!((h.sessions_practiced, h.achieved_dose), (1, 4));
    assert_eq!(h.practiced_means, vec![0.5]);
    assert_eq!(h.last_practiced, Some(1));
    assert_eq!(history.sessions(), 2);
}

#[test]
fn the_history_answers_whether_a_pattern_or_word_was_practiced_within_recent_sessions() {
    let config = config();
    let mut history = TrainingHistory::new();
    assert!(!history.practiced_within("th", 10));
    assert!(!history.targeted_word_within("the", 10));

    history.record(
        &[event("th", TargetRole::Target, 6)],
        ["the", "that"],
        &config,
    );
    history.record(&[event("he", TargetRole::Deferred, 0)], [], &config);
    history.record(&[event("an", TargetRole::Explore, 6)], ["and"], &config);
    // Three sessions recorded: `th` and "the" are from the first, so they
    // are within the last three sessions but not the last two; a deferral
    // is not practice.
    assert!(history.practiced_within("th", 3));
    assert!(!history.practiced_within("th", 2));
    assert!(history.practiced_within("an", 1));
    assert!(!history.practiced_within("he", 3));
    assert!(history.targeted_word_within("the", 3));
    assert!(history.targeted_word_within("that", 3));
    assert!(!history.targeted_word_within("the", 2));
    assert!(history.targeted_word_within("and", 1));
    assert!(!history.targeted_word_within("dog", 3));
}

// --- Achieved dose ----------------------------------------------------------------

#[test]
fn achieved_dose_counts_typed_exposures_of_the_deepest_selected_pattern_at_most_twice_per_word() {
    let targets = [
        target("at", TargetRole::Target, 0.5),
        target("hat", TargetRole::Explore, 0.5),
        target("ta", TargetRole::Deferred, 0.0),
    ];
    // The `t` of "that" is a `hat` slot, deeper than `at`, so `at` gets
    // nothing from it; "atatat" has three `at` slots and two `ta` slots,
    // of which two each count; "hat" is never reached.
    let state = typed("that atatat hat", "that atatat ⎋");
    let events = achieved_doses(&state, &targets);
    let doses: Vec<(&str, usize)> = events
        .iter()
        .map(|e| (e.target.pattern.as_ref(), e.achieved_dose))
        .collect();
    assert_eq!(doses, vec![("at", 2), ("hat", 1), ("ta", 2)]);
}

#[test]
fn a_deferred_candidate_in_a_targets_chain_does_not_take_the_targets_exposures() {
    // `hat` is only withheld, so the `t` of "that" still exposes the target
    // `at`, and `hat` records its own incidental exposure of the same slot.
    let targets = [
        target("at", TargetRole::Target, 0.5),
        target("hat", TargetRole::Deferred, 0.0),
    ];
    let events = achieved_doses(&typed("that", "that"), &targets);
    let doses: Vec<(&str, usize)> = events
        .iter()
        .map(|e| (e.target.pattern.as_ref(), e.achieved_dose))
        .collect();
    assert_eq!(doses, vec![("at", 1), ("hat", 1)]);
}

#[test]
fn the_following_space_is_a_slot_when_it_was_typed() {
    let targets = [target("t ", TargetRole::Target, 0.5)];
    let dose = |state: &SessionState| achieved_doses(state, &targets)[0].achieved_dose;
    // Between words the space is always typed; after the last word only
    // when the session ended on it (accepting an error), since a session
    // completed on its final character records nothing more.
    assert_eq!(dose(&typed("cat dog", "cat dog")), 1);
    assert_eq!(dose(&typed("cat", "cat")), 0);
    assert_eq!(dose(&typed("cat", "cxt ")), 1);
    assert_eq!(dose(&typed("cat cat", "cat cat ")), 1);
}

#[test]
fn every_selected_pattern_gets_an_event_even_with_no_exposure() {
    let targets = [
        target("xq", TargetRole::Target, 0.5),
        target("zz", TargetRole::Explore, 0.5),
    ];
    let events = scheduler::achieved_doses(&typed("cat", "cat"), &targets);
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.achieved_dose == 0));
    assert_eq!(events[0].target, targets[0]);
}

// --- Word exposures ---------------------------------------------------------------

#[test]
fn a_words_slot_exposes_only_the_deepest_practiced_pattern_in_its_chain() {
    let targets = [
        target("at", TargetRole::Target, 0.5),
        target("hat", TargetRole::Explore, 0.5),
    ];
    // The `t` of "that" is a `hat` slot, so `at` gets nothing from it; in
    // "cat" the same slot's chain is `cat`, so `at` is the deepest.
    assert_eq!(
        word_exposures("that", &targets),
        BTreeMap::from([("hat", 1)])
    );
    assert_eq!(word_exposures("cat", &targets), BTreeMap::from([("at", 1)]));
}

#[test]
fn at_most_two_slots_of_a_word_count_toward_one_pattern() {
    let targets = [target("at", TargetRole::Target, 0.5)];
    assert_eq!(
        word_exposures("atatat", &targets),
        BTreeMap::from([("at", 2)])
    );
}

#[test]
fn a_word_standing_alone_is_read_with_a_space_either_side() {
    let targets = [
        target(" t", TargetRole::Target, 0.5),
        target("t ", TargetRole::Target, 0.5),
        target("og", TargetRole::Deferred, 0.0),
    ];
    // Word-initial and word-final patterns are exposed; a deferred
    // candidate is not practiced and so exposes nothing here.
    assert_eq!(
        word_exposures("tot", &targets),
        BTreeMap::from([(" t", 1), ("t ", 1)])
    );
    assert!(word_exposures("dog", &targets).is_empty());
}
