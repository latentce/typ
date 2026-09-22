//! Session performance figures available the moment a session ends.
//!
//! These describe the submitted text and the elapsed time of a session as
//! typed, corrections included; they are what a user sees on finishing. The
//! motor estimate and pattern statistics are a separate analysis over the
//! event log.

use crate::session::{Outcome, SessionState};

/// Characters in the submitted text: everything typed for each word, extras
/// included, plus the space that submitted each word. A completed session's
/// final word carries no space whether it ended on its last character or on
/// a space.
pub fn final_characters(state: &SessionState) -> usize {
    let typed: usize = (0..state.word_count())
        .map(|word| state.typed(word).len())
        .sum();
    let spaces = match state.outcome() {
        Some(Outcome::Completed) => state.word_count() - 1,
        _ => state.current_word(),
    };
    typed + spaces
}

/// Microseconds from the first printable keystroke to the final event.
/// `None` before the first printable keystroke.
pub fn elapsed_micros(state: &SessionState) -> Option<u64> {
    let started = state.started_at_micros()?;
    let last = state.events().last()?.at_micros;
    Some(last.saturating_sub(started))
}

/// Gross words per minute: final characters over five over elapsed minutes.
/// `None` when no time has elapsed.
pub fn gross_wpm(state: &SessionState) -> Option<f64> {
    let elapsed = elapsed_micros(state).filter(|&e| e > 0)?;
    let minutes = elapsed as f64 / 60_000_000.0;
    Some(final_characters(state) as f64 / 5.0 / minutes)
}

/// Correctness of the submitted text after corrections: target characters
/// typed correctly, over target characters plus extra characters. Untyped
/// target characters and extras both count against it.
pub fn final_accuracy(state: &SessionState) -> f64 {
    let mut correct = 0usize;
    let mut denominator = 0usize;
    for word in 0..state.word_count() {
        let target = state.prompt().word(word);
        let typed = state.typed(word);
        let target_len = target.chars().count();
        correct += target
            .chars()
            .zip(typed)
            .filter(|(expected, actual)| expected == *actual)
            .count();
        denominator += target_len + typed.len().saturating_sub(target_len);
    }
    if denominator == 0 {
        0.0
    } else {
        correct as f64 / denominator as f64
    }
}
