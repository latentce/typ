use typ_rs_core::metrics::{elapsed_micros, final_accuracy, final_characters, gross_wpm};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{EndCondition, Input, Key, Outcome, SessionState};

/// One keystroke every 100 ms.
const STEP: u64 = 100_000;

/// Types `script` (`⌫` is backspace) starting at `start` microseconds.
fn run_from(prompt: &str, script: &str, start: u64) -> SessionState {
    let mut state = SessionState::new(
        Prompt::new(prompt.split(' ')),
        EndCondition::AfterWords(usize::MAX),
    );
    for (i, symbol) in script.chars().enumerate() {
        let key = match symbol {
            '⌫' => Key::Backspace,
            c => Key::Char(c),
        };
        state.apply_event(Input::new(start + i as u64 * STEP, key));
    }
    state
}

fn run(prompt: &str, script: &str) -> SessionState {
    run_from(prompt, script, 0)
}

#[test]
fn final_characters_count_the_submitted_text_with_extras_and_separating_spaces() {
    let state = run("cat dog", "catx dg ");
    assert_eq!(state.outcome(), Some(Outcome::Completed));
    // "catx" + " " + "dg"; the submitting space is not part of the text.
    assert_eq!(final_characters(&state), 7);
}

#[test]
fn final_characters_of_an_untouched_session_are_zero() {
    assert_eq!(final_characters(&run("cat dog", "")), 0);
}

#[test]
fn final_characters_of_an_interrupted_session_count_a_space_per_submitted_word() {
    let mut state = run("cat dog fox", "cat dog fo");
    state.apply_event(Input::new(10 * STEP, Key::Interrupt));
    assert_eq!(state.outcome(), Some(Outcome::Interrupted));
    // "cat" + " " + "dog" + " " + "fo"
    assert_eq!(final_characters(&state), 10);
}

#[test]
fn elapsed_runs_from_the_first_printable_keystroke_to_the_final_event() {
    // Reading time before the first keystroke is not counted: the script
    // starts at t = 5 s and the final event lands 6 steps later.
    let state = run_from("cat dog", "cat dog", 5_000_000);
    assert_eq!(elapsed_micros(&state), Some(6 * STEP));
}

#[test]
fn elapsed_is_none_before_the_first_printable_keystroke() {
    assert_eq!(elapsed_micros(&run("cat dog", "")), None);
    assert_eq!(elapsed_micros(&run("cat dog", "⌫⌫")), None);
}

#[test]
fn gross_wpm_is_final_characters_over_five_per_elapsed_minute() {
    // 7 final characters over 0.6 s: 7 / 5 / 0.01 min = 140 wpm.
    let state = run("cat dog", "cat dog");
    let wpm = gross_wpm(&state).unwrap();
    assert!((wpm - 140.0).abs() < 1e-9, "{wpm}");
}

#[test]
fn gross_wpm_is_none_without_an_elapsed_interval() {
    assert_eq!(gross_wpm(&run("cat dog", "")), None);
    // A single keystroke that also ends the session has no duration.
    assert_eq!(gross_wpm(&run("a", "a")), None);
}

#[test]
fn final_accuracy_is_correct_slots_over_target_characters_plus_extras() {
    // "cat" typed "cat": 3 of 3; "fox" typed "foxxx": 3 of 3 plus 2 extras;
    // "dog" typed "dg": d correct, g wrong at o's slot, g never typed → 1 of 3.
    let state = run("cat fox dog", "cat foxxx dg ");
    assert_eq!(state.outcome(), Some(Outcome::Completed));
    let acc = final_accuracy(&state);
    assert!((acc - 7.0 / 11.0).abs() < 1e-9, "{acc}");
}

#[test]
fn final_accuracy_reflects_corrections_not_first_attempts() {
    let state = run("cat", "cxt⌫⌫at");
    assert_eq!(final_accuracy(&state), 1.0);
}

#[test]
fn final_accuracy_of_an_untouched_session_is_zero() {
    assert_eq!(final_accuracy(&run("cat dog", "")), 0.0);
}
