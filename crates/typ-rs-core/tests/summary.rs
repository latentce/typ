use std::collections::{BTreeMap, BTreeSet};

use typ_rs_core::analysis::analyze;
use typ_rs_core::compose::{ComposedWord, Contamination, WordRole};
use typ_rs_core::corpus::Corpus;
use typ_rs_core::metrics::{
    PatternTransfer, ProbeMetrics, RecentSeries, Sustained, WordPerformance, accumulate_transfer,
    probe_trend, summarize, word_initiation_median_micros, word_performances,
};
use typ_rs_core::model::{ModelState, SchedulerConfig};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{EndCondition, Input, Key, Outcome, SessionState};

fn config() -> SchedulerConfig {
    SchedulerConfig::default()
}

/// Types `script` against `prompt`, each keystroke `step` microseconds
/// after the last. `⌫` is backspace, `⎋` an interrupt, `…` a one-second
/// pause before the next key.
fn typed_at(prompt: &str, script: &str, step: u64) -> SessionState {
    let mut state = SessionState::new(
        Prompt::new(prompt.split(' ')),
        EndCondition::AfterWords(usize::MAX),
    );
    let mut at = 0;
    for symbol in script.chars() {
        let key = match symbol {
            '⌫' => Key::Backspace,
            '⎋' => Key::Interrupt,
            '…' => {
                at += 1_000_000;
                continue;
            }
            c => Key::Char(c),
        };
        state.apply_event(Input::new(at, key));
        at += step;
    }
    state
}

fn typed(prompt: &str, script: &str) -> SessionState {
    typed_at(prompt, script, 100_000)
}

fn probe() -> ComposedWord {
    ComposedWord {
        role: WordRole::Probe,
        exposed_targets: Vec::new(),
        selection_score: None,
        contamination: None,
    }
}

fn contaminated_probe() -> ComposedWord {
    ComposedWord {
        contamination: Some(Contamination {
            recently_targeted_word: false,
            recently_targeted_patterns: vec!["at".into()],
        }),
        ..probe()
    }
}

fn targeted() -> ComposedWord {
    ComposedWord {
        role: WordRole::Targeted,
        exposed_targets: vec!["at".into()],
        selection_score: Some(0.0),
        contamination: None,
    }
}

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

// --- Recent series -------------------------------------------------------------

fn series(value: f64) -> RecentSeries {
    RecentSeries {
        wpm: Some(value),
        adjusted_ratio: Some(value),
        reference_wpm: Some(value),
    }
}

#[test]
fn the_first_value_of_a_recent_series_is_taken_as_it_is() {
    let first = RecentSeries::default().advanced(&series(80.0), 5.0);
    assert_eq!(first, series(80.0));
}

#[test]
fn a_recent_series_moves_halfway_to_a_new_level_over_one_half_life() {
    let mut recent = series(100.0);
    for _ in 0..5 {
        recent = recent.advanced(&series(200.0), 5.0);
    }
    assert!(close(recent.wpm.unwrap(), 150.0), "{recent:?}");
    assert!(close(recent.reference_wpm.unwrap(), 150.0));
    // One step is the half-life's share of the gap.
    let one = series(100.0).advanced(&series(200.0), 5.0);
    let alpha = 1.0 - 0.5f64.powf(0.2);
    assert!(close(one.wpm.unwrap(), 100.0 + 100.0 * alpha), "{one:?}");
}

#[test]
fn a_figure_a_session_lacks_leaves_its_average_as_it_was() {
    let missing = RecentSeries {
        wpm: Some(200.0),
        adjusted_ratio: None,
        reference_wpm: None,
    };
    let recent = series(100.0).advanced(&missing, 5.0);
    assert!(recent.wpm.unwrap() > 100.0);
    assert_eq!(recent.adjusted_ratio, Some(100.0));
    assert_eq!(recent.reference_wpm, Some(100.0));
}

// --- Session summary -----------------------------------------------------------

#[test]
fn an_interrupted_session_has_no_summary() {
    let state = typed("cat dog fox", "cat do⎋");
    let update = ModelState::new().apply_session(&state, 1_000, Corpus::bundled(), &config());
    assert_eq!(
        summarize(&state, &update, &vec![probe(); 3], None, &config()),
        None
    );
}

#[test]
fn the_first_completed_session_records_its_own_figures_as_the_baseline() {
    let state = typed("cat dog", "cat dg ");
    assert_eq!(state.outcome(), Some(Outcome::Completed));
    let update = ModelState::new().apply_session(&state, 1_000, Corpus::bundled(), &config());
    let summary = summarize(
        &state,
        &update,
        &[probe(), contaminated_probe()],
        None,
        &config(),
    )
    .unwrap();

    let metrics = &update.analysis.metrics;
    assert_eq!(summary.gross_wpm, metrics.gross_wpm);
    assert_eq!(summary.raw_accuracy, metrics.raw_accuracy);
    assert_eq!(summary.final_accuracy, metrics.final_accuracy);
    assert_eq!(summary.consistency, metrics.consistency);
    assert_eq!(summary.corrections, metrics.corrections);
    assert_eq!(
        summary.session_offset,
        update.applied.unwrap().session_offset
    );
    let difficulty = update.difficulty.unwrap();
    assert_eq!(summary.adjusted_ratio, Some(difficulty.adjusted_ratio()));
    assert_eq!(summary.reference_wpm, Some(difficulty.reference_wpm()));
    assert_eq!(
        summary.recent,
        RecentSeries {
            wpm: summary.gross_wpm,
            adjusted_ratio: summary.adjusted_ratio,
            reference_wpm: summary.reference_wpm,
        }
    );
    // "cat" is the one uncontaminated probe: 4 characters with its space
    // over the 0.3 s after its first keystroke is 160 wpm, all correct.
    // "dg" for "dog" is contaminated: the word's 4 characters over 0.3 s,
    // one omission in three.
    assert_eq!(summary.probes.words, 1);
    assert!(
        close(summary.probes.wpm.unwrap(), 160.0),
        "{:?}",
        summary.probes
    );
    assert_eq!(summary.probes.raw_accuracy, Some(1.0));
    assert_eq!(summary.contaminated_probes.words, 1);
    assert!(
        close(summary.contaminated_probes.wpm.unwrap(), 160.0),
        "{:?}",
        summary.contaminated_probes
    );
    assert!(close(
        summary.contaminated_probes.raw_accuracy.unwrap(),
        2.0 / 3.0
    ));
}

#[test]
fn a_later_session_advances_the_recent_series_from_the_previous_one() {
    let state = typed("cat dog", "cat dog");
    let update = ModelState::new().apply_session(&state, 1_000, Corpus::bundled(), &config());
    let previous = RecentSeries {
        wpm: Some(100.0),
        adjusted_ratio: Some(1.0),
        reference_wpm: Some(100.0),
    };
    let summary = summarize(
        &state,
        &update,
        &vec![probe(); 2],
        Some(&previous),
        &config(),
    )
    .unwrap();
    let expected = previous.advanced(
        &RecentSeries {
            wpm: summary.gross_wpm,
            adjusted_ratio: summary.adjusted_ratio,
            reference_wpm: summary.reference_wpm,
        },
        config().recent_half_life_sessions,
    );
    assert_eq!(summary.recent, expected);
    assert!(summary.recent.wpm.unwrap() > 100.0);
}

// --- Word performance ----------------------------------------------------------

#[test]
fn word_performances_carry_role_contamination_time_characters_and_accuracy() {
    // "cat" clean, a one-second pause before "dog" (its initiation), then
    // "fxo⌫⌫ox" for "fox": a substitution corrected.
    let state = typed("cat dog fox", "cat …dog fxo⌫⌫ox");
    let analysis = analyze(&state);
    let words = word_performances(&analysis, &[probe(), contaminated_probe(), targeted()]);
    assert_eq!(words.len(), 3);

    let cat = &words[0];
    assert_eq!(
        (cat.role, cat.contaminated, cat.submitted),
        (WordRole::Probe, false, true)
    );
    // `c` is first of the session; `a`, `t`, and the space each 100 ms.
    assert!(close(cat.seconds, 0.3), "{cat:?}");
    assert_eq!(cat.characters, 4);
    assert_eq!(cat.raw_accuracy, Some(1.0));
    assert_eq!(cat.initiation_micros, None);

    let dog = &words[1];
    assert_eq!((dog.role, dog.contaminated), (WordRole::Probe, true));
    assert_eq!(dog.initiation_micros, Some(1_100_000));
    assert!(close(dog.seconds, 1.4), "{dog:?}");
    assert_eq!(dog.characters, 4);

    let fox = &words[2];
    assert_eq!((fox.role, fox.contaminated), (WordRole::Targeted, false));
    // Seven keystrokes at 100 ms each, no space: the session ended on `x`.
    assert!(close(fox.seconds, 0.7), "{fox:?}");
    assert_eq!(fox.characters, 3);
    assert!(close(fox.raw_accuracy.unwrap(), 2.0 / 3.0));
}

#[test]
fn an_unreached_word_is_not_submitted_and_a_missing_role_is_an_uncontaminated_probe() {
    let state = typed("cat dog fox", "cat do⎋");
    let words = word_performances(&analyze(&state), &[]);
    assert_eq!(words.len(), 3);
    assert!(words[0].submitted);
    assert!(!words[1].submitted && words[1].raw_accuracy.is_none());
    assert!(!words[2].submitted && words[2].characters == 3);
    assert!(
        words
            .iter()
            .all(|w| w.role == WordRole::Probe && !w.contaminated)
    );
}

// --- Probe metrics and trend ---------------------------------------------------

fn performance(pace: f64, characters: usize, raw: f64, contaminated: bool) -> WordPerformance {
    WordPerformance {
        index: 0,
        role: WordRole::Probe,
        contaminated,
        submitted: true,
        seconds: pace * characters as f64,
        characters,
        target_characters: characters - 1,
        raw_accuracy: Some(raw),
        initiation_micros: None,
    }
}

#[test]
fn probe_metrics_are_characters_over_time_and_correct_over_target_characters() {
    // 4 characters in 0.4 s and 6 in 1.2 s: 10 characters over 1.6 s is
    // 75 wpm. Accuracy is weighted by target characters, one fewer per
    // word than the characters counted for speed: (3 + 0.8 × 5) / 8.
    let words = [
        performance(0.1, 4, 1.0, false),
        performance(0.2, 6, 0.8, false),
    ];
    let metrics = ProbeMetrics::over(words.iter());
    assert_eq!(metrics.words, 2);
    assert!(close(metrics.wpm.unwrap(), 75.0), "{metrics:?}");
    assert!(close(metrics.raw_accuracy.unwrap(), 0.875), "{metrics:?}");
    assert_eq!(ProbeMetrics::over([].iter()), ProbeMetrics::default());
}

#[test]
fn the_probe_window_takes_the_most_recent_uncontaminated_probes_and_splits_off_the_contaminated() {
    let mut words: Vec<WordPerformance> = Vec::new();
    // Most recent first: 3 uncontaminated at 0.1 s a character, one
    // contaminated at 0.2, then older uncontaminated at 0.5.
    words.extend((0..3).map(|_| performance(0.1, 5, 1.0, false)));
    words.push(performance(0.2, 5, 0.9, true));
    words.extend((0..10).map(|_| performance(0.5, 5, 1.0, false)));
    let trend = probe_trend(words.iter(), 3);
    assert_eq!(trend.current.words, 3);
    assert!(
        close(trend.current.wpm.unwrap(), 120.0),
        "{:?}",
        trend.current
    );
    assert_eq!(trend.contaminated.words, 0, "met after the window was full");
    let trend = probe_trend(words.iter(), 4);
    assert_eq!(trend.current.words, 4);
    assert_eq!(trend.contaminated.words, 1);
    assert!(close(trend.contaminated.wpm.unwrap(), 60.0));
}

#[test]
fn the_sustained_marker_needs_two_full_windows_and_a_confidence_interval_clear_of_the_previous_level()
 {
    let steady = |pace: f64| performance(pace, 5, 1.0, false);
    // A clear improvement: 100 at 0.2 s a character after 100 at 0.3.
    let mut words: Vec<WordPerformance> = (0..100).map(|_| steady(0.2)).collect();
    words.extend((0..100).map(|_| steady(0.3)));
    let trend = probe_trend(words.iter(), 100);
    assert_eq!(trend.previous.map(|p| p.words), Some(100));
    assert_eq!(trend.sustained, Some(Sustained::Improvement));

    // The other way round is a sustained decline.
    let trend = probe_trend(words.iter().rev(), 100);
    assert_eq!(trend.sustained, Some(Sustained::Decline));

    // Noisy current window around the previous level: no marker.
    let mut noisy: Vec<WordPerformance> = (0..100)
        .map(|i| steady(if i % 2 == 0 { 0.1 } else { 0.5 }))
        .collect();
    noisy.extend((0..100).map(|_| steady(0.31)));
    let trend = probe_trend(noisy.iter(), 100);
    assert_eq!(trend.sustained, None, "{:?}", trend.current);

    // Too few earlier probes for a previous level: no marker however clear.
    let trend = probe_trend(words.iter().take(150), 100);
    assert_eq!(trend.previous, None);
    assert_eq!(trend.sustained, None);
    let trend = probe_trend(words.iter().take(50), 100);
    assert_eq!(trend.current.words, 50);
    assert_eq!(trend.sustained, None);
}

// --- Word initiation -----------------------------------------------------------

#[test]
fn the_word_initiation_median_is_over_words_with_an_initiation_latency() {
    let state = typed("cat dog fox owl", "cat …dog fox owl");
    let words = word_performances(&analyze(&state), &[]);
    // "cat" has none; "dog" 1.1 s, "fox" and "owl" 100 ms: median 100 ms.
    assert_eq!(word_initiation_median_micros(&words), Some(100_000));
    let none = word_performances(&analyze(&typed("cat", "cat")), &[]);
    assert_eq!(word_initiation_median_micros(&none), None);
}

// --- Transfer ------------------------------------------------------------------

#[test]
fn transfer_splits_a_patterns_slots_between_targeted_and_untargeted_words() {
    // `at` is the pattern; "cat" is targeted in this prompt, "that" was
    // targeted recently, "hat" was not. "hat" is typed "hxt": an error on
    // the `a` slot, so the `t` after it is not clean, and the word has to
    // be submitted with a space.
    let state = typed("cat that hat", "cat that hxt ");
    let analysis = analyze(&state);
    let words = [targeted(), probe(), probe()];
    let patterns: BTreeSet<&str> = ["at"].into_iter().collect();
    let mut transfer: BTreeMap<Box<str>, PatternTransfer> = BTreeMap::new();
    accumulate_transfer(
        &mut transfer,
        state.prompt(),
        &analysis,
        &words,
        &patterns,
        |word| word != "that",
    );

    let at = &transfer["at"];
    // "cat" and "that" each have one `t`-after-`a` slot, both correct and
    // clean at 100 ms.
    assert_eq!(at.targeted.slots, 2);
    assert_eq!(at.targeted.errors, 0.0);
    assert_eq!(at.targeted.median_latency_micros(), Some(100_000));
    assert_eq!(at.targeted.raw_accuracy(), Some(1.0));
    // "hat": the `t` slot is a trial (correct) but follows the error on
    // `a`, so it carries no clean latency.
    assert_eq!(at.untargeted.slots, 1);
    assert_eq!(at.untargeted.errors, 0.0);
    assert_eq!(at.untargeted.median_latency_micros(), None);

    // Accumulating another session adds to the same rows.
    let again = typed("hat", "hat");
    accumulate_transfer(
        &mut transfer,
        again.prompt(),
        &analyze(&again),
        &[probe()],
        &patterns,
        |_| true,
    );
    assert_eq!(transfer["at"].untargeted.slots, 2);
    assert_eq!(
        transfer["at"].untargeted.median_latency_micros(),
        Some(100_000)
    );
    assert_eq!(transfer.len(), 1);
}

#[test]
fn transfer_counts_an_error_against_every_level_of_the_slots_chain_that_is_asked_for() {
    // "cat" typed "cxt" and submitted: the error at slot 1 is attributed
    // to " ca"; the slot's chain has `ca` and ` ca`, so both see one trial
    // and one error.
    let state = typed("cat", "cxt ");
    let patterns: BTreeSet<&str> = ["ca", " ca", "at"].into_iter().collect();
    let mut transfer = BTreeMap::new();
    accumulate_transfer(
        &mut transfer,
        state.prompt(),
        &analyze(&state),
        &[probe()],
        &patterns,
        |_| true,
    );
    assert_eq!(transfer["ca"].untargeted.raw_accuracy(), Some(0.0));
    assert_eq!(transfer[" ca"].untargeted.raw_accuracy(), Some(0.0));
    assert_eq!(transfer["at"].untargeted.raw_accuracy(), Some(1.0));
}
