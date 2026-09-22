use typ_rs_core::model::{ModelState, PatternStats, SchedulerConfig};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{EndCondition, Input, Key, SessionState};

const DAY: i64 = 86_400;

/// Types `script` against `prompt` at one keystroke every `step` microseconds.
/// `⌫` is backspace, `⎋` an interrupt, `…` a two-second pause before the next
/// key.
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
                at += 2_000_000;
                continue;
            }
            c => Key::Char(c),
        };
        state.apply_event(Input::new(at, key));
        at += step;
    }
    state
}

/// One keystroke every 200 ms.
fn typed(prompt: &str, script: &str) -> SessionState {
    typed_at(prompt, script, 200_000)
}

fn config() -> SchedulerConfig {
    SchedulerConfig::default()
}

fn stats(model: &ModelState, pattern: &str) -> PatternStats {
    model
        .stats(pattern)
        .unwrap_or_else(|| panic!("no statistics for {pattern:?}"))
}

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

// --- Chain updates -------------------------------------------------------------

#[test]
fn a_clean_interval_updates_the_trigram_bigram_character_and_user_baseline() {
    let mut model = ModelState::new();
    let update = model.apply_session(&typed("cat", "cat"), 1_000, &config());
    assert!(update.applied.is_some());

    // `c` is first of the session; `a` and `t` are clean, with patterns
    // " ca" and "cat".
    for pattern in ["cat", "at", "t", " ca", "ca", "a"] {
        assert_eq!(stats(&model, pattern).s0, 1.0, "{pattern:?}");
    }
    assert_eq!(model.user_baseline_stats().s0, 2.0);
    assert!(
        model.stats("c").is_some(),
        "the slot of `c` is still a trial"
    );
    assert!(model.stats(" c").is_some());
    assert_eq!(
        stats(&model, " c").s0,
        0.0,
        "no latency for the first keystroke"
    );
}

#[test]
fn every_submitted_slot_is_one_trial_split_between_correct_and_error() {
    let mut model = ModelState::new();
    // `x` for `a`: an error at slot 1 (pattern " ca"); `t` and the space are
    // correct at their slots but follow the error.
    model.apply_session(&typed("cat dog", "cxt dog"), 1_000, &config());

    let ca = stats(&model, " ca");
    assert_eq!((ca.c, ca.e), (0.0, 1.0));
    let a = stats(&model, "a");
    assert_eq!((a.c, a.e), (0.0, 1.0));
    let cat = stats(&model, "cat");
    assert_eq!((cat.c, cat.e), (1.0, 0.0));
    // The space after `cat` and the space-less end of `dog`: 6 target
    // characters plus one space is 7 trials.
    let root = model.user_baseline_stats();
    assert_eq!(root.c + root.e, 7.0);
    assert_eq!(root.e, 1.0);
}

#[test]
fn a_transposition_is_an_error_on_the_spanning_bigram_and_its_character() {
    let mut model = ModelState::new();
    model.apply_session(&typed("their", "thier "), 1_000, &config());
    assert_eq!(stats(&model, "ei").e, 1.0);
    assert_eq!(stats(&model, "i").e, 1.0);
    // The trigram ending at the slot is not blamed and gets no trial.
    assert_eq!(model.stats("hei").map_or(0.0, |s| s.c + s.e), 0.0);
}

#[test]
fn a_hesitation_counts_on_its_pattern_chain_and_not_as_a_latency() {
    let mut model = ModelState::new();
    model.apply_session(&typed("cat", "ca…t"), 1_000, &config());
    for pattern in ["cat", "at", "t"] {
        let s = stats(&model, pattern);
        assert_eq!((s.h, s.s0), (1.0, 0.0), "{pattern:?}");
    }
    assert_eq!(model.user_baseline_stats().h, 1.0);
}

// --- Decay and evidence ----------------------------------------------------------

#[test]
fn statistics_decay_by_elapsed_time_not_by_sessions_between() {
    let config = SchedulerConfig {
        pattern_half_life_days: 45.0,
        ..config()
    };
    // Three sessions at the same moment: nothing decays.
    let mut same_day = ModelState::new();
    for _ in 0..3 {
        same_day.apply_session(&typed("cat", "cat"), 1_000, &config);
    }
    assert_eq!(stats(&same_day, "cat").s0, 3.0);

    // The same three sessions, the last one 45 days after the first two: the
    // pattern's earlier evidence has halved when it is next touched.
    let mut spread = ModelState::new();
    spread.apply_session(&typed("cat", "cat"), 1_000, &config);
    spread.apply_session(&typed("cat", "cat"), 1_000, &config);
    spread.apply_session(&typed("cat", "cat"), 1_000 + 45 * DAY, &config);
    let s = stats(&spread, "cat");
    assert!(close(s.s0, 2.0), "{}", s.s0);
    assert_eq!(s.last_update, 1_000 + 45 * DAY);

    // The user baseline decays faster: its 7-day half-life halves it in a
    // week.
    let root_before = same_day.user_baseline_stats();
    let root_after = root_before.decayed_to(1_000 + 7 * DAY, config.baseline_half_life_days);
    assert!(
        close(root_after.s0, root_before.s0 / 2.0),
        "{}",
        root_after.s0
    );
}

#[test]
fn a_pattern_not_touched_by_a_session_is_decayed_when_it_is_next_read() {
    let mut model = ModelState::new();
    model.apply_session(&typed("cat", "cat"), 1_000, &config());
    model.apply_session(&typed("dog", "dog"), 1_000 + 45 * DAY, &config());
    // Stored as it was, decayed on reading at the later time.
    assert_eq!(stats(&model, "cat").s0, 1.0);
    assert_eq!(stats(&model, "cat").last_update, 1_000);
    let read = stats(&model, "cat").decayed_to(1_000 + 45 * DAY, 45.0);
    assert!(close(read.s0, 0.5), "{}", read.s0);
    assert_eq!(model.last_update(), Some(1_000 + 45 * DAY));
}

#[test]
fn effective_sample_size_counts_equal_weight_observations_and_discounts_faded_ones() {
    let mut model = ModelState::new();
    for _ in 0..4 {
        model.apply_session(&typed("cat", "cat"), 1_000, &config());
    }
    assert!(close(stats(&model, "cat").n_eff(), 4.0));
    // A pattern never observed has no evidence.
    assert_eq!(PatternStats::default().n_eff(), 0.0);

    // Four observations 45 days ago and one fresh: S0 = 3, W2 = 2, so the
    // five count as 4.5 equally weighted ones.
    model.apply_session(&typed("cat", "cat"), 1_000 + 45 * DAY, &config());
    let s = stats(&model, "cat");
    assert!(close(s.s0, 3.0) && close(s.w2, 2.0), "{s:?}");
    assert!(close(s.n_eff(), 4.5), "{}", s.n_eff());
}

// --- Shrinkage -------------------------------------------------------------------

#[test]
fn a_pattern_without_evidence_inherits_its_parent_estimate_including_variance() {
    let mut model = ModelState::new();
    // `a` is typed at 200 ms in a session whose baseline is the median of
    // its clean latencies; `q` never appears.
    model.apply_session(
        &typed("cat dog cat dog", "cat dog cat dog"),
        1_000,
        &config(),
    );
    let at = 1_000;
    let a = model.estimate("a", at, &config());
    let qa = model.estimate("qa", at, &config());
    assert_eq!(qa.absolute_slowness, a.absolute_slowness);
    assert_eq!(qa.variance, a.variance);
    assert_eq!(qa.error_probability, a.error_probability);
    assert_eq!(qa.n_eff, 0.0);
    assert!(qa.variance > 0.0);

    // An empty model still has an estimate: the priors.
    let empty = ModelState::new().estimate("xyz", at, &config());
    assert_eq!(empty.absolute_slowness, 0.0);
    assert!(close(empty.variance, config().latency_variance_prior));
    assert!(
        close(empty.error_probability, 1.0 / 20.0),
        "{}",
        empty.error_probability
    );
}

#[test]
fn evidence_pulls_an_estimate_away_from_its_parent_by_kappa() {
    let config = config();
    let mut model = ModelState::new();
    // Baseline from a first session at a steady 200 ms.
    model.apply_session(&typed("cat dog cat dog", "cat dog cat dog"), 1_000, &config);
    // Then `xyz` typed once at 400 ms, twice as slow. Only the `y` and `z`
    // slots have clean latencies, so the session offset is the median
    // residual ln 2 shrunk by 2 / (2 + 20), and each residual is what is
    // left: ln 2 × 20 / 22.
    model.apply_session(&typed_at("xyz", "xyz", 400_000), 2_000, &config);
    let x = 2f64.ln() * 20.0 / 22.0;
    let at = 2_000;
    let z = model.estimate("z", at, &config);
    let yz = model.estimate("yz", at, &config);
    let xyz = model.estimate("xyz", at, &config);
    // One observation against κ = 10 of the parent at every level; the
    // parent of a character is the baseline, whose residual is zero.
    assert!(
        close(z.absolute_slowness, x / 11.0),
        "{}",
        z.absolute_slowness
    );
    assert!(close(
        yz.absolute_slowness,
        (x + 10.0 * z.absolute_slowness) / 11.0
    ));
    assert!(close(
        xyz.absolute_slowness,
        (x + 10.0 * yz.absolute_slowness) / 11.0
    ));
    assert!(xyz.absolute_slowness > 0.0 && xyz.absolute_slowness < x);
    assert_eq!(xyz.n_eff, 1.0);
}

#[test]
fn error_probability_shrinks_toward_the_parent_and_the_root_prior() {
    let mut model = ModelState::new();
    // `x` for `a` in `cat`, twice; everything else correct; the final space
    // accepts the error and ends the session.
    model.apply_session(&typed("cat cat", "cxt cxt "), 1_000, &config());
    let at = 1_000;
    let root = model.estimate("", at, &config());
    // 2 errors in 8 trials against the Beta(1, 19) prior.
    assert!(
        close(root.error_probability, 3.0 / 28.0),
        "{}",
        root.error_probability
    );
    let a = model.estimate("a", at, &config());
    assert!(close(
        a.error_probability,
        (2.0 + 10.0 * root.error_probability) / 12.0
    ));
    let ca = model.estimate(" ca", at, &config());
    assert!(ca.error_probability > a.error_probability);
    assert!(ca.error_probability < 1.0);
}

// --- Baseline and offset --------------------------------------------------------

#[test]
fn the_first_session_bootstraps_the_baseline_from_its_own_median_clean_log_latency() {
    let mut model = ModelState::new();
    assert_eq!(model.user_baseline(), None);
    let update = model.apply_session(&typed_at("cat dog", "cat dog", 250_000), 1_000, &config());
    let applied = update.applied.unwrap();
    assert!(close(applied.user_baseline.unwrap(), 0.25f64.ln()));
    assert_eq!(applied.session_offset, 0.0);
    assert!(close(model.user_baseline().unwrap(), 0.25f64.ln()));
}

#[test]
fn the_baseline_used_is_the_one_snapshotted_before_the_session_is_applied() {
    let mut model = ModelState::new();
    model.apply_session(&typed_at("cat dog", "cat dog", 250_000), 1_000, &config());
    let before = model.user_baseline().unwrap();
    let update = model.apply_session(&typed_at("cat dog", "cat dog", 500_000), 2_000, &config());
    let applied = update.applied.unwrap();
    assert_eq!(applied.user_baseline, Some(before));
    // The baseline has since moved toward the slower session.
    assert!(model.user_baseline().unwrap() > before);
}

#[test]
fn the_session_offset_is_the_median_residual_shrunk_toward_zero_for_short_sessions() {
    let mut short = ModelState::new();
    short.apply_session(&typed_at("cat dog", "cat dog", 250_000), 1_000, &config());
    let mut long = short.clone();

    // Both sessions are typed at twice the baseline latency.
    let short_offset = short
        .apply_session(&typed_at("cat dog", "cat dog", 500_000), 2_000, &config())
        .applied
        .unwrap()
        .session_offset;
    let words = "cat dog fox owl cat dog fox owl cat dog fox owl cat dog fox owl";
    let long_offset = long
        .apply_session(&typed_at(words, words, 500_000), 2_000, &config())
        .applied
        .unwrap()
        .session_offset;

    assert!(short_offset > 0.0 && long_offset > 0.0);
    assert!(
        short_offset < long_offset,
        "{short_offset} vs {long_offset}"
    );
    assert!(long_offset < 2f64.ln());
    // 62 clean intervals (63 keystrokes less the first) against a
    // regulariser of 20.
    assert!(close(long_offset, 2f64.ln() * 62.0 / 82.0), "{long_offset}");

    // The residuals stored for the pattern have the offset removed: the
    // long session's `at` residuals are well under ln 2.
    let at = stats(&long, "cat");
    assert!(at.s1 / at.s0 < 2f64.ln() / 2.0, "{}", at.s1 / at.s0);
}

#[test]
fn the_hesitation_threshold_comes_from_the_baseline_once_there_is_one() {
    let mut model = ModelState::new();
    // A baseline of 500 ms puts the threshold at 2 s, so a 2.2 s gap after
    // 200 ms keystrokes is a hesitation only in the first session, where the
    // running median (200 ms) leaves the 1.5 s floor in force.
    model.apply_session(&typed_at("cat dog", "cat dog", 500_000), 1_000, &config());
    let update = model.apply_session(&typed("catalog", "catalo…g"), 2_000, &config());
    let g = update.analysis.intervals.last().unwrap();
    assert_eq!(
        g.class,
        typ_rs_core::analysis::IntervalClass::Hesitation {
            threshold_micros: 2_000_000
        }
    );
    let update = model.apply_session(&typed("catalog", "catalo…g"), 3_000, &config());
    // Now the baseline is above 500 ms... the same 2.2 s gap is still over.
    assert!(matches!(
        update.analysis.intervals.last().unwrap().class,
        typ_rs_core::analysis::IntervalClass::Hesitation { .. }
    ));
}

// --- Interrupted sessions ------------------------------------------------------

#[test]
fn an_interrupted_session_counts_only_with_enough_clean_intervals() {
    let config = SchedulerConfig {
        interrupted_min_clean_intervals: 5,
        ..config()
    };
    let mut model = ModelState::new();
    // Four clean intervals (`a`, `t`, space, `d`), then interrupted.
    let update = model.apply_session(&typed("cat dog", "cat d⎋"), 1_000, &config);
    assert_eq!(update.applied, None);
    assert_eq!(model.patterns().count(), 0);
    assert_eq!(model.user_baseline(), None);

    // Five clean intervals: applied.
    let update = model.apply_session(&typed("cat dog", "cat do⎋"), 2_000, &config);
    assert_eq!(update.applied.unwrap().clean_intervals, 5);
    assert!(model.user_baseline().is_some());
    // Only submitted words are trials: `do` is not.
    assert_eq!(model.stats("dog"), None);
}

#[test]
fn a_completed_session_counts_however_few_clean_intervals_it_has() {
    let mut model = ModelState::new();
    let update = model.apply_session(&typed("a", "a"), 1_000, &config());
    let applied = update.applied.unwrap();
    assert_eq!(applied.clean_intervals, 0);
    assert_eq!(applied.user_baseline, None);
    assert_eq!(stats(&model, " a").c, 1.0);
    assert_eq!(model.user_baseline(), None);
}

// --- Dirty tracking --------------------------------------------------------------

#[test]
fn the_patterns_a_session_touched_are_dirty_until_marked_clean() {
    let mut model = ModelState::new();
    model.apply_session(&typed("cat", "cat"), 1_000, &config());
    let dirty: Vec<&str> = model.dirty().map(|(p, _)| p).collect();
    assert!(dirty.contains(&"cat") && dirty.contains(&""), "{dirty:?}");
    assert_eq!(dirty.len(), model.patterns().count());

    model.mark_clean();
    assert_eq!(model.dirty().count(), 0);
    model.apply_session(&typed("dog", "dog"), 2_000, &config());
    let dirty: Vec<&str> = model.dirty().map(|(p, _)| p).collect();
    assert!(
        dirty.contains(&"dog") && !dirty.contains(&"cat"),
        "{dirty:?}"
    );

    // A model rebuilt from rows starts clean and equal.
    let rows = model.patterns().map(|(p, s)| (Box::from(p), *s));
    let loaded = ModelState::from_rows(rows);
    assert_eq!(loaded.dirty().count(), 0);
    assert_eq!(loaded.stats("cat"), model.stats("cat"));
    assert_eq!(loaded.user_baseline(), model.user_baseline());
}
