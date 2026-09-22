use rand_chacha::ChaCha8Rng;
use rand_core::{Rng, SeedableRng};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{
    EndCondition, EventKind, Input, Key, MAX_EXTRAS, Outcome, SEMANTICS_VERSION, SessionState,
};

/// One keystroke every 100 ms unless a test says otherwise.
const STEP: u64 = 100_000;

fn session(prompt: &str) -> SessionState {
    SessionState::new(
        Prompt::new(prompt.split(' ')),
        EndCondition::AfterWords(usize::MAX),
    )
}

/// `⌫` is backspace, `⎋` is an interrupt, `↵` is Enter, `⇥` is Tab, `⟲` is a
/// resize, `⚙` is some other non-character key; everything else is typed.
fn key(symbol: char) -> Key {
    match symbol {
        '⌫' => Key::Backspace,
        '⎋' => Key::Interrupt,
        '↵' => Key::Char('\n'),
        '⇥' => Key::Char('\t'),
        '⟲' => Key::Resize,
        '⚙' => Key::Other,
        c => Key::Char(c),
    }
}

fn typed_at(state: &mut SessionState, script: &str, start: u64) -> u64 {
    let mut at = start;
    for symbol in script.chars() {
        state.apply_event(Input::new(at, key(symbol)));
        at += STEP;
    }
    at
}

fn typed(state: &mut SessionState, script: &str) {
    let start = state.events().last().map_or(0, |e| e.at_micros + STEP);
    typed_at(state, script, start);
}

fn run(prompt: &str, script: &str) -> SessionState {
    let mut state = session(prompt);
    typed(&mut state, script);
    state
}

fn word_text(state: &SessionState, word: usize) -> String {
    state.typed(word).iter().collect()
}

fn caret(state: &SessionState) -> (usize, usize) {
    (state.current_word(), state.position())
}

// --- Editing rules ---------------------------------------------------------

#[test]
fn printable_characters_advance_the_caret_whether_correct_or_not() {
    for (script, expected_typed, expected_caret) in [
        ("c", "c", (0, 1)),
        ("ca", "ca", (0, 2)),
        ("cx", "cx", (0, 2)),
        ("Ca", "Ca", (0, 2)),
        ("cé", "cé", (0, 2)),
        ("cat", "cat", (0, 3)),
    ] {
        let state = run("cat dog", script);
        assert_eq!(word_text(&state, 0), expected_typed, "{script}");
        assert_eq!(caret(&state), expected_caret, "{script}");
        assert_eq!(state.outcome(), None, "{script}");
    }
}

#[test]
fn extra_characters_past_the_word_end_are_kept_up_to_a_cap_and_then_ignored() {
    let state = run("cat dog", "catxyz");
    assert_eq!(word_text(&state, 0), "catxyz");
    assert_eq!(caret(&state), (0, 6));

    let script = format!("cat{}", "x".repeat(MAX_EXTRAS + 3));
    let state = run("cat dog", &script);
    assert_eq!(
        word_text(&state, 0),
        format!("cat{}", "x".repeat(MAX_EXTRAS))
    );
    assert_eq!(caret(&state), (0, 3 + MAX_EXTRAS));
    assert_eq!(
        state.events().len(),
        script.chars().count(),
        "keystrokes beyond the cap are still recorded"
    );
}

#[test]
fn backspace_removes_the_last_typed_character_including_extras() {
    for (script, expected_typed) in [
        ("ca⌫", "c"),
        ("cat⌫⌫", "c"),
        ("catx⌫", "cat"),
        ("catxy⌫⌫⌫", "ca"),
        ("c⌫⌫⌫", ""),
    ] {
        let state = run("cat dog", script);
        assert_eq!(word_text(&state, 0), expected_typed, "{script}");
        assert_eq!(
            caret(&state),
            (0, expected_typed.chars().count()),
            "{script}"
        );
    }
}

#[test]
fn backspace_at_word_start_re_enters_the_previous_word_only_if_it_has_an_uncorrected_error() {
    // Submitted correct: backspace does nothing.
    let state = run("cat dog", "cat ⌫");
    assert_eq!(caret(&state), (1, 0));

    // Submitted with an incorrect character: re-entered after its last typed character.
    let state = run("cat dog", "cxt ⌫");
    assert_eq!(caret(&state), (0, 3));
    assert_eq!(word_text(&state, 0), "cxt");

    // Submitted with an omission.
    let state = run("cat dog", "ca ⌫");
    assert_eq!(caret(&state), (0, 2));

    // Submitted with extras: caret lands after the extras.
    let state = run("cat dog", "catxx ⌫");
    assert_eq!(caret(&state), (0, 5));

    // Only if the current word is empty.
    let state = run("cat dog", "cxt d⌫⌫");
    assert_eq!(caret(&state), (0, 3));
    let state = run("cat dog", "cxt d⌫");
    assert_eq!(caret(&state), (1, 0));

    // Never before the first word.
    let state = run("cat dog", "⌫");
    assert_eq!(caret(&state), (0, 0));

    // Consecutive errored words can each be re-entered in turn.
    let state = run("cat dog fox", "cxt dxg ⌫⌫⌫⌫⌫⌫");
    assert_eq!(caret(&state), (0, 2));
    assert_eq!(word_text(&state, 1), "");
}

#[test]
fn a_re_entered_word_can_be_fixed_and_resubmitted() {
    let state = run("cat dog", "cxt ⌫⌫⌫at d");
    assert_eq!(word_text(&state, 0), "cat");
    assert_eq!(word_text(&state, 1), "d");
    assert_eq!(caret(&state), (1, 1));

    // Once fixed, it can no longer be re-entered.
    let state = run("cat dog", "cxt ⌫⌫⌫at ⌫");
    assert_eq!(caret(&state), (1, 0));
}

#[test]
fn space_submits_the_word_and_at_word_start_does_nothing() {
    let state = run("cat dog", "cat ");
    assert_eq!(caret(&state), (1, 0));
    assert_eq!(state.words_completed(), 1);

    let state = run("cat dog", "cat  ");
    assert_eq!(caret(&state), (1, 0));

    let state = run("cat dog", " ");
    assert_eq!(caret(&state), (0, 0));
    assert_eq!(state.words_completed(), 0);

    // Submitting early leaves omissions and moves on.
    let state = run("cat dog", "c d");
    assert_eq!(word_text(&state, 0), "c");
    assert_eq!(caret(&state), (1, 1));
}

// --- Session end ------------------------------------------------------------

#[test]
fn session_completes_when_the_final_word_is_typed_correctly() {
    let state = run("cat dog", "cat dog");
    assert_eq!(state.outcome(), Some(Outcome::Completed));
    assert_eq!(state.words_completed(), 2);

    // Not while the final word still has an uncorrected error.
    let state = run("cat dog", "cat dxg");
    assert_eq!(state.outcome(), None);
    let state = run("cat dog", "cat doog");
    assert_eq!(state.outcome(), None);

    // Fixing the error and typing the final character correctly ends it.
    let state = run("cat dog", "cat dxg⌫⌫og");
    assert_eq!(state.outcome(), Some(Outcome::Completed));
}

#[test]
fn session_completes_when_space_is_pressed_after_the_final_word() {
    for script in ["cat dxg ", "cat d ", "cat dogg "] {
        let state = run("cat dog", script);
        assert_eq!(state.outcome(), Some(Outcome::Completed), "{script}");
        assert_eq!(state.words_completed(), 2, "{script}");
    }
    let state = run("cat dog", "cat  ");
    assert_eq!(state.outcome(), None);
}

#[test]
fn the_end_condition_may_stop_before_the_prompt_runs_out() {
    let mut state = SessionState::new(
        Prompt::new(["cat", "dog", "fox"]),
        EndCondition::AfterWords(2),
    );
    assert_eq!(state.word_count(), 2);
    typed(&mut state, "cat dog");
    assert_eq!(state.outcome(), Some(Outcome::Completed));
    assert_eq!(state.words_completed(), 2);

    let state = SessionState::new(Prompt::new(["cat"]), EndCondition::AfterWords(0));
    assert_eq!(state.word_count(), 1);
}

#[test]
fn nothing_is_recorded_or_applied_after_the_session_ends() {
    let mut state = run("cat dog", "cat dog");
    let events = state.events().len();
    let effects = state.apply_event(Input::new(10 * STEP, Key::Char('x')));
    assert!(!effects.recorded && !effects.changed && effects.ended.is_none());
    assert_eq!(state.events().len(), events);
    assert_eq!(word_text(&state, 1), "dog");
}

#[test]
fn interrupt_ends_the_session_as_interrupted_and_is_recorded() {
    let mut state = session("cat dog");
    typed(&mut state, "cat do");
    let effects = state.apply_event(Input::new(99 * STEP, Key::Interrupt));

    assert_eq!(effects.ended, Some(Outcome::Interrupted));
    assert!(effects.recorded);
    assert_eq!(state.outcome(), Some(Outcome::Interrupted));
    assert_eq!(state.words_completed(), 1);
    assert_eq!(state.events().last().unwrap().kind, EventKind::Interrupt);
}

// --- Recording --------------------------------------------------------------

#[test]
fn pasted_input_is_recorded_with_the_flag_and_not_applied() {
    let mut state = session("cat dog");
    let effects = state.apply_event(Input::new(0, Key::Char('c')).in_paste());
    assert!(effects.recorded && !effects.changed);
    assert_eq!(caret(&state), (0, 0));

    let event = &state.events()[0];
    assert_eq!(event.kind, EventKind::Char);
    assert_eq!(event.actual, Some('c'));
    assert!(event.flags.in_paste);
    assert!(
        !event.flags.first_of_session,
        "a pasted event is not the first keystroke"
    );

    let effects = state.apply_event(Input::new(STEP, Key::Char('c')));
    assert!(effects.recorded && effects.changed);
    assert!(state.events()[1].flags.first_of_session);
}

#[test]
fn control_keys_are_ignored_and_not_recorded() {
    let state = run("cat dog", "c↵⇥⚙a");
    assert_eq!(word_text(&state, 0), "ca");
    assert_eq!(state.events().len(), 2);
    assert!(state.events().iter().all(|e| e.kind == EventKind::Char));

    let mut state = session("cat dog");
    let effects = state.apply_event(Input::new(0, Key::Char('\u{7}')));
    assert_eq!(effects, Default::default());
    assert!(state.events().is_empty());
}

#[test]
fn recorded_events_carry_the_caret_and_expected_character_at_arrival() {
    type Recorded = (EventKind, Option<char>, Option<char>, usize, usize);
    let state = run("cat dog", "cx⌫atzz dog");
    let recorded: Vec<Recorded> = state
        .events()
        .iter()
        .map(|e| (e.kind, e.expected, e.actual, e.word_index, e.position))
        .collect();
    assert_eq!(
        recorded,
        [
            (EventKind::Char, Some('c'), Some('c'), 0, 0),
            (EventKind::Char, Some('a'), Some('x'), 0, 1),
            (EventKind::Backspace, Some('t'), None, 0, 2),
            (EventKind::Char, Some('a'), Some('a'), 0, 1),
            (EventKind::Char, Some('t'), Some('t'), 0, 2),
            (EventKind::Char, Some(' '), Some('z'), 0, 3),
            (EventKind::Char, Some(' '), Some('z'), 0, 4),
            (EventKind::Space, Some(' '), Some(' '), 0, 5),
            (EventKind::Char, Some('d'), Some('d'), 1, 0),
            (EventKind::Char, Some('o'), Some('o'), 1, 1),
            (EventKind::Char, Some('g'), Some('g'), 1, 2),
        ]
    );
    let sequence: Vec<u32> = state.events().iter().map(|e| e.seq).collect();
    assert_eq!(sequence, (0..11).collect::<Vec<_>>());
}

#[test]
fn early_space_records_the_omitted_character_as_expected() {
    let state = run("cat dog", "c d");
    let space = &state.events()[1];
    assert_eq!(space.kind, EventKind::Space);
    assert_eq!((space.expected, space.actual), (Some('a'), Some(' ')));
}

#[test]
fn timestamps_flags_and_timer_start_are_recorded() {
    let mut state = session("cat dog");
    state.apply_event(Input::new(1_000, Key::Resize));
    state.apply_event(Input::new(5_000, Key::Char('c')));
    state.apply_event(Input::new(5_000 + STEP, Key::Char('a')).burst());
    state.apply_event(Input::new(5_000 + STEP + 2_000_000, Key::Char('t')));
    state.apply_event(Input::new(9_000_000, Key::Resize));
    state.apply_event(Input::new(9_100_000, Key::Char('x')).in_paste());
    state.apply_event(Input::new(9_200_000, Key::Char(' ')));

    let events = state.events();
    let flags = |i: usize| {
        let f = events[i].flags;
        (
            f.first_of_session,
            f.after_resize,
            f.in_paste,
            f.burst,
            f.long_pause,
        )
    };
    assert_eq!(events[0].kind, EventKind::Resize);
    assert_eq!((events[0].expected, events[0].actual), (None, None));
    assert_eq!(events[0].at_micros, 1_000);
    assert_eq!(flags(0), (false, false, false, false, false));
    assert_eq!(flags(1), (true, true, false, false, false));
    assert_eq!(flags(2), (false, false, false, true, false));
    assert_eq!(flags(3), (false, false, false, false, true));
    assert_eq!(flags(4), (false, false, false, false, false));
    assert_eq!(
        flags(5),
        (false, false, true, false, false),
        "a pasted event neither consumes the resize flag nor counts as a keystroke"
    );
    assert_eq!(flags(6), (false, true, false, false, true));
    assert_eq!(events[6].kind, EventKind::Space);

    assert_eq!(state.started_at_micros(), Some(5_000));
    assert_eq!(session("cat").started_at_micros(), None);
}

#[test]
fn keystrokes_before_the_first_printable_carry_no_timing_flags() {
    let mut state = session("cat dog");
    state.apply_event(Input::new(0, Key::Char(' ')));
    state.apply_event(Input::new(STEP, Key::Backspace));
    state.apply_event(Input::new(5_000_000, Key::Char('c')));
    state.apply_event(Input::new(5_000_000 + STEP, Key::Char('a')));

    let flags: Vec<(bool, bool)> = state
        .events()
        .iter()
        .map(|e| (e.flags.first_of_session, e.flags.long_pause))
        .collect();
    assert_eq!(
        flags,
        [
            (false, false),
            (false, false),
            (true, false),
            (false, false)
        ]
    );
    assert_eq!(state.started_at_micros(), Some(5_000_000));
}

#[test]
fn escape_and_ctrl_c_arriving_as_characters_are_interrupts() {
    for c in ['\u{1b}', '\u{3}'] {
        let mut state = session("cat dog");
        typed(&mut state, "ca");
        let effects = state.apply_event(Input::new(99 * STEP, Key::Char(c)));
        assert_eq!(effects.ended, Some(Outcome::Interrupted), "{c:?}");
        assert_eq!(state.events().last().unwrap().kind, EventKind::Interrupt);
        assert_eq!(state.events().last().unwrap().actual, None);
    }
}

#[test]
fn effects_report_recording_change_and_end() {
    let mut state = session("cat dog");
    let e = state.apply_event(Input::new(0, Key::Char(' ')));
    assert!(e.recorded && !e.changed && e.ended.is_none());
    let e = state.apply_event(Input::new(STEP, Key::Char('c')));
    assert!(e.recorded && e.changed && e.ended.is_none());
    let e = state.apply_event(Input::new(2 * STEP, Key::Backspace));
    assert!(e.recorded && e.changed);
    let e = state.apply_event(Input::new(3 * STEP, Key::Backspace));
    assert!(e.recorded && !e.changed);
    let e = state.apply_event(Input::new(4 * STEP, Key::Resize));
    assert!(e.recorded && !e.changed);
    let e = state.apply_event(Input::new(5 * STEP, Key::Char('\n')));
    assert!(!e.recorded && !e.changed);
    typed(&mut state, "cat do");
    let e = state.apply_event(Input::new(99 * STEP, Key::Char('g')));
    assert!(e.recorded && e.changed && e.ended == Some(Outcome::Completed));
}

// --- Determinism and robustness ---------------------------------------------

const RANDOM_PROMPT: &str = "the quick brown fox jumps over the lazy dog a i";

fn random_input(rng: &mut ChaCha8Rng, at: u64) -> Input {
    let key = match rng.next_u32() % 100 {
        0..50 => Key::Char((b'a' + (rng.next_u32() % 26) as u8) as char),
        50..60 => Key::Char(' '),
        60..65 => Key::Char(
            ['é', '漢', '\u{301}', 'Z', '\n', '\t', '\u{1b}'][rng.next_u32() as usize % 7],
        ),
        65..90 => Key::Backspace,
        90..96 => Key::Resize,
        96..99 => Key::Other,
        _ => Key::Interrupt,
    };
    let mut input = Input::new(at, key);
    if rng.next_u32() % 20 == 0 {
        input = input.in_paste();
    }
    if rng.next_u32() % 10 == 0 {
        input = input.burst();
    }
    input
}

fn random_log(seed: u64) -> Vec<Input> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut at = 0;
    (0..300)
        .map(|_| {
            at += u64::from(rng.next_u32() % 3_000_000);
            random_input(&mut rng, at)
        })
        .collect()
}

fn replay(log: &[Input]) -> SessionState {
    let mut state = session(RANDOM_PROMPT);
    for input in log {
        state.apply_event(*input);
    }
    state
}

#[test]
fn the_same_event_log_yields_an_identical_state() {
    for seed in 0..50 {
        let log = random_log(seed);
        assert_eq!(replay(&log), replay(&log), "seed {seed}");
    }
    let a = run("cat dog", "cxt ⌫⌫⌫at dog");
    let b = run("cat dog", "cxt ⌫⌫⌫at dog");
    assert_eq!(a, b);
    assert_eq!(a.events(), b.events());
}

#[test]
fn arbitrary_event_logs_never_panic_and_keep_the_caret_inside_the_current_word() {
    for seed in 0..500 {
        let mut state = session(RANDOM_PROMPT);
        for (i, input) in random_log(seed).into_iter().enumerate() {
            let before = state.events().len();
            let effects = state.apply_event(input);
            let context = format!("seed {seed}, input {i}: {input:?}");

            let word = state.current_word();
            assert!(word < state.word_count(), "{context}");
            let target_len = state.prompt().word(word).chars().count();
            assert_eq!(state.position(), state.typed(word).len(), "{context}");
            assert!(state.position() <= target_len + MAX_EXTRAS, "{context}");
            assert!(
                (word + 1..state.word_count()).all(|w| state.typed(w).is_empty()),
                "{context}"
            );
            assert_eq!(
                state.events().len(),
                before + usize::from(effects.recorded),
                "{context}"
            );
            assert!(
                state.events().windows(2).all(|w| w[0].seq + 1 == w[1].seq),
                "{context}"
            );
            if let Some(outcome) = effects.ended {
                assert_eq!(state.outcome(), Some(outcome), "{context}");
            }
        }
    }
}

#[test]
fn a_stored_event_log_replays_to_the_identical_state() {
    for seed in 0..50 {
        let original = replay(&random_log(seed));
        let replayed = SessionState::replay(
            original.prompt().clone(),
            EndCondition::AfterWords(original.word_count()),
            original.events(),
        );
        assert_eq!(replayed, original, "seed {seed}");
    }
}

#[test]
fn event_kinds_have_stable_names_that_round_trip() {
    for (kind, name) in [
        (EventKind::Char, "char"),
        (EventKind::Backspace, "backspace"),
        (EventKind::Space, "space"),
        (EventKind::Resize, "resize"),
        (EventKind::Interrupt, "interrupt"),
    ] {
        assert_eq!(kind.name(), name);
        assert_eq!(EventKind::from_name(name), Some(kind));
    }
    assert_eq!(EventKind::from_name("keystroke"), None);
}

#[test]
fn outcomes_have_stable_names_that_round_trip() {
    for (outcome, name) in [
        (Outcome::Completed, "completed"),
        (Outcome::Interrupted, "interrupted"),
    ] {
        assert_eq!(outcome.name(), name);
        assert_eq!(Outcome::from_name(name), Some(outcome));
    }
    assert_eq!(Outcome::from_name("abandoned"), None);
}

#[test]
fn semantics_version_is_declared() {
    const { assert!(SEMANTICS_VERSION >= 1) };
}
