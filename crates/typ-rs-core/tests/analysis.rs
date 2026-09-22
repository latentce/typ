use typ_rs_core::analysis::{
    Edit, Exclusion, HesitationThreshold, IntervalClass, SessionAnalysis, analyze, analyze_with,
};
use typ_rs_core::prompt::{Prompt, Slot};
use typ_rs_core::session::{EndCondition, Input, Key, SessionState};

/// One keystroke every 100 ms unless a test says otherwise.
const STEP: u64 = 100_000;

/// A pause long enough to be a hesitation under the 1.5 s floor.
const PAUSE: u64 = 2_000_000;

/// Types `script` against `prompt`. `⌫` is backspace, `⎋` an interrupt, `⟲` a
/// resize, `…` a two-second pause and `·` a half-second pause before the next
/// key; `¶` marks the next character as pasted and `!` as arriving in a burst.
fn state(prompt: &str, script: &str) -> SessionState {
    let mut state = SessionState::new(
        Prompt::new(prompt.split(' ')),
        EndCondition::AfterWords(usize::MAX),
    );
    let mut at = 0;
    let mut paste = false;
    let mut burst = false;
    for symbol in script.chars() {
        let key = match symbol {
            '⌫' => Key::Backspace,
            '⎋' => Key::Interrupt,
            '⟲' => Key::Resize,
            '…' => {
                at += PAUSE;
                continue;
            }
            '·' => {
                at += 500_000;
                continue;
            }
            '¶' => {
                paste = true;
                continue;
            }
            '!' => {
                burst = true;
                continue;
            }
            c => Key::Char(c),
        };
        let mut input = Input::new(at, key);
        if paste {
            input = input.in_paste();
        }
        if burst {
            input = input.burst();
        }
        state.apply_event(input);
        paste = false;
        burst = false;
        at += STEP;
    }
    state
}

fn run(prompt: &str, script: &str) -> SessionAnalysis {
    analyze(&state(prompt, script))
}

fn first_attempts(analysis: &SessionAnalysis) -> Vec<&str> {
    analysis
        .words
        .iter()
        .map(|w| w.first_attempt.as_str())
        .collect()
}

fn history(analysis: &SessionAnalysis, word: usize) -> String {
    analysis.words[word].attempt_history.iter().collect()
}

/// The errors attributed in a word: edit, pattern, weight.
fn errors(analysis: &SessionAnalysis, word: usize) -> Vec<(Edit, &str, f64)> {
    analysis.words[word]
        .errors
        .iter()
        .map(|e| (e.edit, e.pattern.as_ref(), e.weight))
        .collect()
}

// --- Alignment and attribution -----------------------------------------------

#[test]
fn a_substitution_counts_against_the_pattern_ending_at_its_slot() {
    let analysis = run("cat dog", "cxt dog");
    assert_eq!(
        errors(&analysis, 0),
        [(
            Edit::Substitution {
                slot: 1,
                actual: 'x'
            },
            " ca",
            1.0
        )]
    );
    assert_eq!(errors(&analysis, 1), []);
}

#[test]
fn a_transposition_is_one_error_on_the_bigram_it_spans() {
    let analysis = run("their cat", "thier cat");
    assert_eq!(
        errors(&analysis, 0),
        [(Edit::Transposition { slot: 3 }, "ei", 1.0)]
    );
}

#[test]
fn an_omission_counts_against_the_pattern_ending_at_the_omitted_slot() {
    let analysis = run("cat dog", "ct dog");
    assert_eq!(
        errors(&analysis, 0),
        [(Edit::Omission { slot: 1 }, " ca", 1.0)]
    );
}

#[test]
fn an_insertion_counts_against_the_pattern_ending_at_the_following_slot_and_keeps_the_key() {
    let analysis = run("cat dog", "cxat dog");
    assert_eq!(
        errors(&analysis, 0),
        [(
            Edit::Insertion {
                slot: 1,
                actual: 'x'
            },
            " ca",
            1.0
        )]
    );
}

#[test]
fn trailing_extras_count_against_the_pattern_ending_in_the_following_space() {
    let analysis = run("cat dog", "catxy dog");
    assert_eq!(
        errors(&analysis, 0),
        [
            (
                Edit::Insertion {
                    slot: 3,
                    actual: 'x'
                },
                "at ",
                1.0
            ),
            (
                Edit::Insertion {
                    slot: 3,
                    actual: 'y'
                },
                "at ",
                1.0
            ),
        ]
    );
}

#[test]
fn among_equally_short_alignments_errors_are_attributed_latest_in_the_word() {
    // `se` for `see`: either `e` could be the omitted one; the later is.
    let analysis = run("see dog", "se dog");
    assert_eq!(
        errors(&analysis, 0),
        [(Edit::Omission { slot: 2 }, "see", 1.0)]
    );
}

#[test]
fn alignments_still_tied_after_the_latest_rule_share_their_errors_equally() {
    // `ba` for `aab` is two operations either way: substitute `b` for the
    // first `a` and omit the `b`, or omit the first `a` and transpose `ab`.
    // Both put their errors at slots 2 and 0.
    let analysis = run("aab dog", "ba dog");
    let mut found = errors(&analysis, 0);
    found.sort_by(|a, b| {
        a.0.slot()
            .cmp(&b.0.slot())
            .then(format!("{:?}", a.0).cmp(&format!("{:?}", b.0)))
    });
    assert_eq!(
        found,
        [
            (Edit::Omission { slot: 0 }, " a", 0.5),
            (
                Edit::Substitution {
                    slot: 0,
                    actual: 'b'
                },
                " a",
                0.5
            ),
            (Edit::Omission { slot: 2 }, "aab", 0.5),
            (Edit::Transposition { slot: 2 }, "ab", 0.5),
        ]
    );
}

#[test]
fn a_word_typed_correctly_has_no_errors_and_an_unsubmitted_word_is_not_aligned() {
    let analysis = run("cat dog", "cat do⎋");
    assert_eq!(errors(&analysis, 0), []);
    assert_eq!(errors(&analysis, 1), []);
}

// --- Clean intervals -----------------------------------------------------------

/// Each interval's class as a short string: `clean`, `hesitation`, or the
/// list of exclusions.
fn classes(analysis: &SessionAnalysis) -> Vec<String> {
    analysis
        .intervals
        .iter()
        .map(|i| match &i.class {
            IntervalClass::Clean => "clean".to_string(),
            IntervalClass::Hesitation { .. } => "hesitation".to_string(),
            IntervalClass::Excluded(reasons) => format!("{reasons:?}"),
        })
        .collect()
}

#[test]
fn the_first_keystroke_of_the_session_is_excluded_and_the_rest_of_a_clean_run_is_clean() {
    let analysis = run("cat dog", "cat dog");
    assert_eq!(
        classes(&analysis),
        [
            "[FirstOfSession]",
            "clean",
            "clean",
            "clean",
            "clean",
            "clean",
            "clean"
        ]
    );
    assert_eq!(analysis.metrics.clean_intervals, 6);
    assert_eq!(analysis.intervals[0].latency_micros, None);
    assert_eq!(analysis.intervals[1].latency_micros, Some(STEP));
}

#[test]
fn keystrokes_before_the_first_printable_character_have_no_interval() {
    let analysis = run("cat dog", " ⌫cat dog");
    assert_eq!(analysis.intervals.len(), 7);
    assert_eq!(analysis.intervals[0].actual, Some('c'));
}

#[test]
fn backspaces_replacements_and_the_keystroke_after_the_last_replacement_are_excluded() {
    let analysis = run("cat dog", "cxt⌫⌫at dog");
    assert_eq!(
        classes(&analysis),
        [
            "[FirstOfSession]",
            "clean",
            "clean",
            "[Backspace]",
            "[Backspace, AfterCorrection]",
            "[Replacement, AfterCorrection]",
            "[Replacement, AfterCorrection]",
            "[AfterCorrection]",
            "clean",
            "clean",
            "clean",
        ]
    );
}

#[test]
fn a_keystroke_after_a_backspace_that_changed_nothing_is_still_after_a_correction() {
    let analysis = run("cat dog", "cat ⌫dog");
    assert_eq!(
        classes(&analysis)[4..],
        ["[Backspace]", "[AfterCorrection]", "clean", "clean"]
    );
}

#[test]
fn slots_after_an_uncorrected_error_in_the_same_word_are_excluded() {
    // The erroneous keystroke itself is not excluded; everything after it in
    // the word is, including the submitting space. The next word is clean.
    let analysis = run("cat dog", "cxt dog");
    assert_eq!(
        classes(&analysis),
        [
            "[FirstOfSession]",
            "clean",
            "[FollowingError]",
            "[FollowingError]",
            "clean",
            "clean",
            "clean",
        ]
    );
    assert_eq!(analysis.words[0].first_uncorrected_error, Some(1));
    assert_eq!(analysis.words[1].first_uncorrected_error, None);

    // An omission: the early space is itself the erroneous keystroke at the
    // omitted slot, so it is not after the error.
    let analysis = run("cat dog", "ca dog");
    assert_eq!(classes(&analysis)[2..4], ["clean", "clean"]);
    assert_eq!(analysis.words[0].first_uncorrected_error, Some(2));

    // An extra: the space after it follows the error.
    let analysis = run("cat dog", "catx dog");
    assert_eq!(classes(&analysis)[3..5], ["clean", "[FollowingError]"]);
    assert_eq!(analysis.words[0].first_uncorrected_error, Some(3));
}

#[test]
fn an_error_corrected_before_submission_does_not_flag_the_slots_after_it() {
    let analysis = run("cat dog", "cxt⌫⌫at dog");
    assert_eq!(analysis.words[0].first_uncorrected_error, None);
    assert!(!analysis.intervals.iter().any(|i| matches!(
        &i.class,
        IntervalClass::Excluded(reasons) if reasons.contains(&Exclusion::FollowingError)
    )));
}

#[test]
fn the_keystroke_after_a_resize_is_excluded() {
    let analysis = run("cat dog", "ca⟲t dog");
    assert_eq!(classes(&analysis)[2], "[AfterResize]");
}

#[test]
fn a_pasted_keystroke_is_excluded_and_does_not_start_the_next_interval() {
    let analysis = run("cat dog", "ca¶pt dog");
    assert_eq!(classes(&analysis)[2..4], ["[InPaste]", "clean"]);
    // `t` is measured from `a`, across the paste.
    assert_eq!(analysis.intervals[3].latency_micros, Some(2 * STEP));
}

#[test]
fn a_keystroke_arriving_in_a_burst_is_excluded() {
    let analysis = run("cat dog", "ca!t dog");
    assert_eq!(classes(&analysis)[2], "[Burst]");
}

#[test]
fn a_keystroke_the_editing_rules_ignored_is_excluded() {
    let analysis = run("cat dog", "cat  dog");
    assert_eq!(classes(&analysis)[4], "[Ignored]");
}

#[test]
fn an_interval_over_the_hesitation_threshold_is_a_hesitation_on_its_slot() {
    let analysis = run("cat dog", "ca…t dog");
    assert_eq!(classes(&analysis)[2], "hesitation");
    assert_eq!(
        analysis.intervals[2].class,
        IntervalClass::Hesitation {
            threshold_micros: 1_500_000
        }
    );
    assert_eq!(
        analysis.intervals[2].slot,
        Slot {
            word: 0,
            position: 2
        }
    );
    assert_eq!(analysis.intervals[2].pattern.as_ref(), "cat");
    assert_eq!(analysis.intervals[2].latency_micros, Some(PAUSE + STEP));
}

#[test]
fn the_hesitation_threshold_rises_with_the_running_median_of_clean_latencies() {
    // At 600 ms per keystroke the median is 600 ms, so the threshold is
    // 2.4 s and a 2.1 s gap is not a hesitation.
    let analysis = run("catalog dog", "c·a·t·a·l…o·g dog");
    assert_eq!(classes(&analysis)[5], "clean");
    // The same gap at 100 ms per keystroke is over the 1.5 s floor.
    let analysis = run("catalog dog", "catal…og dog");
    assert_eq!(classes(&analysis)[5], "hesitation");
}

#[test]
fn a_structurally_excluded_interval_is_not_a_hesitation_however_long() {
    let analysis = run("cat dog", "cxt⌫…⌫at dog");
    assert_eq!(classes(&analysis)[4], "[Backspace, AfterCorrection]");
}

#[test]
fn with_a_user_baseline_the_threshold_is_four_times_it_and_fixed_for_the_session() {
    // A baseline of 600 ms puts the threshold at 2.4 s for every interval,
    // so a 2.1 s gap is clean however fast the session's own keystrokes are.
    let baseline = 0.6f64.ln();
    let analysis = analyze_with(
        &state("catalog dog", "catal…og dog"),
        HesitationThreshold::UserBaseline(baseline),
    );
    assert_eq!(classes(&analysis)[5], "clean");

    // A baseline of 100 ms would put it at 400 ms; the 1.5 s floor holds.
    let analysis = analyze_with(
        &state("catalog dog", "cat·alog dog"),
        HesitationThreshold::UserBaseline(0.1f64.ln()),
    );
    assert_eq!(classes(&analysis)[3], "clean");
    let analysis = analyze_with(
        &state("catalog dog", "cat…alog dog"),
        HesitationThreshold::UserBaseline(0.1f64.ln()),
    );
    assert_eq!(
        analysis.intervals[3].class,
        IntervalClass::Hesitation {
            threshold_micros: 1_500_000
        }
    );
}

#[test]
fn an_extra_character_belongs_to_the_slot_of_the_following_space() {
    let analysis = run("cat dog", "catx dog");
    let extra = &analysis.intervals[3];
    assert_eq!(
        (extra.slot, extra.actual),
        (
            Slot {
                word: 0,
                position: 3
            },
            Some('x')
        )
    );
    let space = &analysis.intervals[4];
    assert_eq!(
        (space.slot, space.actual),
        (
            Slot {
                word: 0,
                position: 3
            },
            Some(' ')
        )
    );
}

// --- Metrics -------------------------------------------------------------------

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

#[test]
fn raw_accuracy_is_first_attempt_correct_characters_over_target_characters() {
    // One substitution in six characters, corrected or not.
    let m = run("cat dog", "cxt dog").metrics;
    assert!(close(m.raw_accuracy, 5.0 / 6.0), "{}", m.raw_accuracy);
    let m = run("cat dog", "cxt⌫⌫at dog").metrics;
    assert!(close(m.raw_accuracy, 5.0 / 6.0), "{}", m.raw_accuracy);
    // A transposition is one error against two characters.
    let m = run("their cat", "thier cat").metrics;
    assert!(close(m.raw_accuracy, 7.0 / 8.0), "{}", m.raw_accuracy);
    // An insertion costs a character.
    let m = run("cat dog", "catx dog").metrics;
    assert!(close(m.raw_accuracy, 5.0 / 6.0), "{}", m.raw_accuracy);
    // A word cannot score below zero.
    let m = run("cat dog", "xxxxxxxx dog").metrics;
    assert!(close(m.raw_accuracy, 0.5), "{}", m.raw_accuracy);
}

#[test]
fn final_accuracy_and_gross_wpm_describe_the_text_as_submitted() {
    let m = run("cat dog", "cxt⌫⌫at dog").metrics;
    assert_eq!(m.final_accuracy, 1.0);
    // 7 final characters over 1.0 s.
    assert!(close(m.gross_wpm.unwrap(), 84.0), "{:?}", m.gross_wpm);
}

#[test]
fn correction_overhead_counts_backspaces_and_replacements_over_target_characters() {
    let m = run("cat dog", "cxt⌫⌫at dog").metrics;
    assert!(
        close(m.correction_overhead, 4.0 / 6.0),
        "{}",
        m.correction_overhead
    );
    assert_eq!(run("cat dog", "cat dog").metrics.correction_overhead, 0.0);
}

#[test]
fn uncorrected_errors_count_what_was_wrong_missing_or_extra_when_first_submitted() {
    // `x` for `a`; `g` for `o` and the `g` never typed.
    assert_eq!(run("cat dog", "cxt dg ").metrics.uncorrected_errors, 3);
    assert_eq!(run("cat dog", "catxx dog").metrics.uncorrected_errors, 2);
    // Fixed before submission: never uncorrected.
    assert_eq!(run("cat dog", "cxt⌫⌫at dog").metrics.uncorrected_errors, 0);
    // Fixed after re-entry: it stood when the word was submitted.
    let analysis = run("cat dog", "cxt ⌫⌫⌫at dog");
    assert_eq!(analysis.metrics.uncorrected_errors, 1);
    assert_eq!(analysis.metrics.final_accuracy, 1.0);
}

#[test]
fn pasted_input_is_never_a_correction() {
    let analysis = run("cat dog", "ca¶pt dog");
    assert_eq!(analysis.metrics.corrections, 0);
    assert_eq!(history(&analysis, 0), "cat");
}

#[test]
fn error_latency_is_the_median_time_an_incorrect_character_stood_before_its_backspace() {
    // `x` typed at 100 ms, removed by the second backspace at 400 ms.
    let m = run("cat dog", "cxt⌫⌫at dog").metrics;
    assert_eq!(m.error_latency_micros, Some(3 * STEP));
    assert_eq!(run("cat dog", "cat dog").metrics.error_latency_micros, None);
    // Removing a correct character is not an error latency.
    assert_eq!(
        run("cat dog", "cat⌫t dog").metrics.error_latency_micros,
        None
    );
}

#[test]
fn consistency_is_one_minus_the_coefficient_of_variation_of_clean_latencies() {
    let m = run("cat dog", "cat dog").metrics;
    assert_eq!(m.consistency, Some(1.0));
    // Clean latencies alternate 100 ms and 600 ms: mean 350, sd 250.
    let m = run("cat dog", "ca·t ·do·g").metrics;
    assert!(
        close(m.consistency.unwrap(), 1.0 - 250.0 / 350.0),
        "{:?}",
        m.consistency
    );
    // Fewer than two clean intervals: undefined.
    assert_eq!(run("cat dog", "c⎋").metrics.consistency, None);
}

#[test]
fn an_interrupted_session_scores_only_its_submitted_words() {
    let m = run("cat dog fox", "cxt dog fo⎋").metrics;
    assert!(close(m.raw_accuracy, 5.0 / 6.0), "{}", m.raw_accuracy);
    assert_eq!(m.uncorrected_errors, 1);
}

// --- Robustness ------------------------------------------------------------------

#[test]
fn arbitrary_event_logs_are_analysed_without_panicking_and_deterministically() {
    use rand_chacha::ChaCha8Rng;
    use rand_core::{Rng, SeedableRng};

    let prompt = "the quick brown fox jumps over the lazy dog a i";
    for seed in 0..300 {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut state = SessionState::new(
            Prompt::new(prompt.split(' ')),
            EndCondition::AfterWords(usize::MAX),
        );
        let mut at = 0;
        for _ in 0..200 {
            at += u64::from(rng.next_u32() % 3_000_000);
            let key = match rng.next_u32() % 100 {
                0..55 => Key::Char((b'a' + (rng.next_u32() % 26) as u8) as char),
                55..65 => Key::Char(' '),
                65..90 => Key::Backspace,
                90..95 => Key::Resize,
                95..99 => Key::Char(['é', 'Z', '\n'][rng.next_u32() as usize % 3]),
                _ => Key::Interrupt,
            };
            let mut input = Input::new(at, key);
            if rng.next_u32() % 20 == 0 {
                input = input.in_paste();
            }
            if rng.next_u32() % 10 == 0 {
                input = input.burst();
            }
            state.apply_event(input);
        }

        let analysis = analyze(&state);
        assert_eq!(analysis, analyze(&state), "seed {seed}");
        for word in analysis.words.iter().filter(|w| w.submitted) {
            let total = word.error_count();
            assert!(
                close(total, total.round()),
                "seed {seed}: error weights of {word:?} do not sum to a whole number"
            );
        }
        assert!(
            analysis.metrics.raw_accuracy >= 0.0 && analysis.metrics.raw_accuracy <= 1.0,
            "seed {seed}"
        );
    }
}

// --- Attempt histories -------------------------------------------------------

#[test]
fn the_attempt_history_keeps_every_character_typed_for_a_word_including_removed_ones() {
    let analysis = run("cat dog", "cxt⌫⌫at dog");
    assert_eq!(history(&analysis, 0), "cxtat");
    assert_eq!(history(&analysis, 1), "dog");
}

#[test]
fn the_first_attempt_is_what_was_typed_before_any_backspace() {
    // Corrected before submission: the first attempt is still the wrong text.
    assert_eq!(first_attempts(&run("cat", "cxt⌫⌫at ")), ["cxt"]);
    // Typed straight through.
    assert_eq!(first_attempts(&run("cat dog", "cat dog")), ["cat", "dog"]);
}

#[test]
fn characters_at_never_attempted_positions_extend_the_first_attempt_after_a_backspace() {
    // `x` was attempted at position 1 and replaced; `t` at position 2 is new,
    // so the first attempt reads `cat` less the correction: `cxt`.
    assert_eq!(first_attempts(&run("cat", "cx⌫at ")), ["cxt"]);
    // Every position attempted, then all retyped: only the originals count.
    assert_eq!(first_attempts(&run("cat", "ca⌫⌫xyz ")), ["caz"]);
}

#[test]
fn the_first_attempt_is_frozen_at_the_first_submission() {
    // `ca` submitted early, re-entered, completed: `t` is a correction.
    let analysis = run("cat dog", "ca ⌫t dog");
    assert_eq!(first_attempts(&analysis), ["ca", "dog"]);
    assert_eq!(history(&analysis, 0), "cat");
    assert!(analysis.words[0].submitted);

    // Re-entered and fully retyped after submission.
    assert_eq!(
        first_attempts(&run("cat dog", "cxt ⌫⌫⌫at dog")),
        ["cxt", "dog"]
    );
}

#[test]
fn the_final_word_is_submitted_when_the_session_ends_on_it() {
    let analysis = run("cat dog", "cat dog");
    assert!(analysis.words.iter().all(|w| w.submitted));

    let analysis = run("cat dog fox", "cat do⎋");
    assert!(analysis.words[0].submitted);
    assert!(!analysis.words[1].submitted);
    assert_eq!(analysis.words[1].first_attempt, "do");
    // An unsubmitted word has no uncorrected errors: nothing was submitted.
    assert_eq!(analysis.words[1].first_uncorrected_error, None);
    assert_eq!(analysis.words[1].uncorrected_errors, 0);
}

#[test]
fn extra_characters_are_part_of_the_first_attempt() {
    assert_eq!(
        first_attempts(&run("cat dog", "catxx dog")),
        ["catxx", "dog"]
    );
}
