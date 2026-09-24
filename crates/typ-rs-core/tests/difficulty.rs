use typ_rs_core::analysis::IntervalClass;
use typ_rs_core::corpus::{Corpus, ReferenceDistribution};
use typ_rs_core::metrics::gross_wpm;
use typ_rs_core::model::{ModelState, SchedulerConfig};
use typ_rs_core::prompt::{Prompt, Slot};
use typ_rs_core::session::{EndCondition, Input, Key, Outcome, SessionState};

fn config() -> SchedulerConfig {
    SchedulerConfig::default()
}

/// Types `script` against `prompt`, each keystroke `step` microseconds
/// after the last. `⌫` is backspace, `⎋` an interrupt, `…` a two-second
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

/// Types the prompt perfectly, every keystroke taking exactly the latency
/// the model predicts for its slot.
fn typed_as_predicted(model: &ModelState, prompt: &Prompt, at: i64) -> SessionState {
    let corpus = Corpus::bundled();
    let config = config();
    let mut state = SessionState::new(
        prompt.clone(),
        EndCondition::AfterWords(prompt.word_count()),
    );
    let mut now = 0u64;
    for slot in prompt.slots() {
        if slot
            != (Slot {
                word: 0,
                position: 0,
            })
        {
            let log_latency = model
                .predicted_log_latency(prompt, slot, at, corpus, &config)
                .unwrap();
            now += (log_latency.exp() * 1_000_000.0).round() as u64;
        }
        state.apply_event(Input::new(now, Key::Char(prompt.expected_at(slot))));
    }
    assert_eq!(state.outcome(), Some(Outcome::Completed));
    state
}

/// A model that has seen `sessions` clean sessions of `prompt` at 200 ms a
/// keystroke, so that it has a baseline and pattern estimates.
fn trained(prompt: &str, sessions: usize) -> ModelState {
    let mut model = ModelState::new();
    for i in 0..sessions {
        model.apply_session(
            &typed_at(prompt, prompt, 200_000),
            1_000 + i as i64 * 86_400,
            Corpus::bundled(),
            &config(),
        );
    }
    model
}

const FORTY_WORDS: &str = "the of and to in is you that it he was for on are as with his \
    they at be this have from or one had by word but not what all were we when \
    your can said there would";

#[test]
fn expected_and_actual_clean_seconds_are_summed_over_the_same_clean_slots() {
    // A hesitation before `d`, a correction in `fox`, and the first
    // keystroke are not clean; only the clean slots enter either sum. On a
    // first session every slot is predicted at the bootstrapped baseline,
    // the session's own median clean log-latency.
    let state = typed_at("cat dog fox", "cat …dog fxo⌫⌫ox", 200_000);
    let mut model = ModelState::new();
    let update = model.apply_session(&state, 1_000, Corpus::bundled(), &config());
    let clean: Vec<u64> = update
        .analysis
        .intervals
        .iter()
        .filter(|i| i.class == IntervalClass::Clean)
        .filter_map(|i| i.latency_micros)
        .collect();
    assert!(clean.len() >= 5, "{clean:?}");
    assert!(clean.len() < update.analysis.intervals.len());

    let difficulty = update.difficulty.unwrap();
    let actual: f64 = clean.iter().map(|&l| l as f64 / 1e6).sum();
    assert!((difficulty.actual_clean_seconds - actual).abs() < 1e-9);
    let baseline = update.applied.unwrap().user_baseline.unwrap();
    let expected = clean.len() as f64 * baseline.exp();
    assert!(
        (difficulty.expected_clean_seconds - expected).abs() < 1e-9,
        "{difficulty:?}"
    );
    assert!((difficulty.adjusted_ratio() - 1.0).abs() < 1e-9);
}

#[test]
fn reference_equivalent_wpm_matches_gross_wpm_on_a_clean_probe_only_first_session() {
    // With nothing known before it, the first session is its own
    // standard: typed cleanly at a steady pace, the speed it translates to
    // on the reference sample is its own speed. Gross WPM counts one more
    // character than there are intervals, so the two agree to within that.
    let state = typed_at(FORTY_WORDS, FORTY_WORDS, 100_000);
    let mut model = ModelState::new();
    let update = model.apply_session(&state, 1_000, Corpus::bundled(), &config());
    let reference = update.difficulty.unwrap().reference_wpm();
    let gross = gross_wpm(&state).unwrap();
    assert!(
        (reference - gross).abs() / gross < 0.01,
        "reference {reference} gross {gross}"
    );
}

#[test]
fn a_harder_prompt_typed_as_predicted_shows_no_drop_in_reference_equivalent_wpm() {
    let corpus = Corpus::bundled();
    let config = config();
    let at = 100 * 86_400;
    // Slow on `th`: one session of 40 words, then one where every `h`
    // after `t` takes a second, so the pattern's estimate rises.
    let mut model = trained(FORTY_WORDS, 3);
    let slow = {
        let prompt = Prompt::new(FORTY_WORDS.split(' '));
        let count = prompt.word_count();
        assert_eq!(count, 40);
        let mut state = SessionState::new(prompt.clone(), EndCondition::AfterWords(count));
        let mut now = 0;
        let mut previous = ' ';
        for word in 0..count {
            let text = prompt.word(word);
            for key in text.chars().chain((word + 1 < count).then_some(' ')) {
                now += if previous == 't' && key == 'h' {
                    1_000_000
                } else {
                    200_000
                };
                state.apply_event(Input::new(now, Key::Char(key)));
                previous = key;
            }
        }
        assert_eq!(state.outcome(), Some(Outcome::Completed));
        state
    };
    model.apply_session(&slow, at - 86_400, corpus, &config);
    let th = model.estimate("th", at, &config);
    let ca = model.estimate("ca", at, &config);
    assert!(
        th.absolute_slowness > ca.absolute_slowness + 0.2,
        "{th:?} {ca:?}"
    );

    let easy = Prompt::new("cat dog fox owl cat dog fox owl cat dog fox owl".split(' '));
    let hard =
        Prompt::new("the that this they then them there these the that this they".split(' '));
    let easy_state = typed_as_predicted(&model, &easy, at);
    let hard_state = typed_as_predicted(&model, &hard, at);
    let easy_gross = gross_wpm(&easy_state).unwrap();
    let hard_gross = gross_wpm(&hard_state).unwrap();
    assert!(
        hard_gross < 0.95 * easy_gross,
        "hard {hard_gross} easy {easy_gross}"
    );

    let easy_update = model
        .clone()
        .apply_session(&easy_state, at, corpus, &config);
    let hard_update = model
        .clone()
        .apply_session(&hard_state, at, corpus, &config);
    let easy_difficulty = easy_update.difficulty.unwrap();
    let hard_difficulty = hard_update.difficulty.unwrap();
    // Latencies are rounded to the microsecond, so the ratios are one to
    // within that.
    assert!((easy_difficulty.adjusted_ratio() - 1.0).abs() < 1e-4);
    assert!((hard_difficulty.adjusted_ratio() - 1.0).abs() < 1e-4);
    let predicted = model
        .predicted_wpm(
            &ReferenceDistribution::new(corpus).fixed_sample(corpus),
            at,
            corpus,
            &config,
        )
        .unwrap();
    assert!((easy_difficulty.reference_wpm() - predicted).abs() < 1e-2 * predicted);
    assert!((hard_difficulty.reference_wpm() - predicted).abs() < 1e-2 * predicted);
}

#[test]
fn typing_faster_than_predicted_raises_the_ratio_and_the_reference_equivalent_wpm() {
    let corpus = Corpus::bundled();
    let config = config();
    let at = 10 * 86_400;
    let model = trained(FORTY_WORDS, 2);
    let predicted = model
        .predicted_wpm(
            &ReferenceDistribution::new(corpus).fixed_sample(corpus),
            at,
            corpus,
            &config,
        )
        .unwrap();
    // Every keystroke at 100 ms, half the 200 ms the model has seen.
    let fast = typed_at(FORTY_WORDS, FORTY_WORDS, 100_000);
    let update = model.clone().apply_session(&fast, at, corpus, &config);
    let difficulty = update.difficulty.unwrap();
    assert!(difficulty.adjusted_ratio() > 1.8, "{difficulty:?}");
    assert!(difficulty.reference_wpm() > 1.8 * predicted);
}

#[test]
fn no_difficulty_adjustment_without_a_completed_session_or_a_clean_interval() {
    let corpus = Corpus::bundled();
    let config = config();
    let long = FORTY_WORDS
        .split(' ')
        .take(20)
        .collect::<Vec<_>>()
        .join(" ");
    let mut script = long.clone();
    script.push('⎋');
    let interrupted = typed_at(FORTY_WORDS, &script, 200_000);
    let update = ModelState::new().apply_session(&interrupted, 1_000, corpus, &config);
    assert!(update.applied.is_some(), "enough clean intervals to count");
    assert_eq!(update.difficulty, None);

    // A completed session whose only interval is the first: nothing clean.
    let update =
        ModelState::new().apply_session(&typed_at("ab", "ab", 200_000), 1_000, corpus, &config);
    assert_eq!(update.analysis.metrics.clean_intervals, 1);
    assert!(update.difficulty.is_some());
    let update =
        ModelState::new().apply_session(&typed_at("a", "a", 200_000), 1_000, corpus, &config);
    assert_eq!(update.analysis.metrics.clean_intervals, 0);
    assert_eq!(update.difficulty, None);
}

#[test]
fn predictions_need_a_baseline() {
    let corpus = Corpus::bundled();
    let prompt = Prompt::new(["cat"]);
    let slot = Slot {
        word: 0,
        position: 1,
    };
    assert_eq!(
        ModelState::new().predicted_log_latency(&prompt, slot, 1_000, corpus, &config()),
        None
    );
    assert_eq!(
        ModelState::new().predicted_wpm(&prompt, 1_000, corpus, &config()),
        None
    );
    let model = trained("cat dog", 1);
    let predicted = model
        .predicted_log_latency(&prompt, slot, 1_000, corpus, &config())
        .unwrap();
    // Every keystroke took 200 ms.
    assert!((predicted.exp() - 0.2).abs() < 1e-6, "{predicted}");
    let wpm = model
        .predicted_wpm(&prompt, 1_000, corpus, &config())
        .unwrap();
    // Three characters, two of them predicted at 200 ms: 3 / 5 / (0.4 / 60).
    assert!((wpm - 90.0).abs() < 1e-3, "{wpm}");
}
