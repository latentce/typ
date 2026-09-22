//! The session pipeline: from a finished session to what it says about the
//! user's typing.
//!
//! [`analyze`] is the one entry point. It takes the finished
//! [`SessionState`] (the prompt and the ordered event log with the editing
//! rules applied) and reconstructs how every word was typed, aligns each
//! word's first attempt to its target to attribute errors to patterns,
//! classifies every keystroke's incoming latency as clean motor evidence or
//! not, and computes the session's metrics. The terminal prints from it,
//! `typ replay` shows it, and the pattern statistics are built from it.
//! Nothing here depends on a terminal or a database.

mod alignment;
mod attempt;
mod intervals;

use std::fmt;

use crate::metrics;
use crate::prompt::{Prompt, Slot};
use crate::session::{EventKind, SessionState};

/// Everything the analysis derives from one session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionAnalysis {
    /// One entry per word of the session, in prompt order.
    pub words: Vec<WordAnalysis>,
    /// One entry per keystroke from the first printable character on, in
    /// event order.
    pub intervals: Vec<Interval>,
    pub metrics: SessionMetrics,
}

/// How one word was typed and where its first attempt went wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct WordAnalysis {
    pub target: Box<str>,
    /// Every character typed for the word in order, including those later
    /// removed by backspace.
    pub attempt_history: Vec<char>,
    /// The characters typed before any backspace plus those typed at
    /// positions never attempted before, frozen at the word's first
    /// submission. Extras past the word's end are included.
    pub first_attempt: String,
    /// Whether the word was submitted: left with a space, or the word the
    /// session completed on. Only submitted words enter the accuracy
    /// figures.
    pub submitted: bool,
    /// The errors in the first attempt, each attributed to a pattern.
    pub errors: Vec<ErrorAttribution>,
    /// Characters wrong, missing, or extra in the text as first submitted,
    /// counted by position. A word fixed after re-entry still counts them:
    /// they stood when the word was submitted. Zero for an unsubmitted
    /// word.
    pub uncorrected_errors: usize,
    /// The position of the first uncorrected error; slots after it are
    /// `following_error`. `None` for an unsubmitted word.
    pub first_uncorrected_error: Option<usize>,
}

impl WordAnalysis {
    /// Errors in the first attempt, weights summed: what the word's target
    /// characters are reduced by for raw accuracy.
    pub fn error_count(&self) -> f64 {
        self.errors.iter().map(|e| e.weight).sum()
    }

    /// Whether anything was typed in the word.
    pub fn reached(&self) -> bool {
        self.submitted || !self.attempt_history.is_empty()
    }
}

/// One error in a word's first attempt, attributed to a pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct ErrorAttribution {
    pub edit: Edit,
    /// The pattern the error counts against: the text of the prompt ending
    /// at the edit's slot, up to a trigram; for a transposition, the bigram
    /// it spans.
    pub pattern: Box<str>,
    /// One, unless the alignment was ambiguous and the error is shared
    /// between equally good alignments.
    pub weight: f64,
}

/// One operation of the alignment from a word's first attempt to its
/// target. Slots are positions within the word; the position just past the
/// end is the space that follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// A different character was typed at the slot.
    Substitution { slot: usize, actual: char },
    /// The characters at `slot - 1` and `slot` were typed in the other
    /// order; one error against two characters.
    Transposition { slot: usize },
    /// The slot's character was never typed.
    Omission { slot: usize },
    /// A character was typed that belongs to no slot, before the character
    /// at `slot`; at the word's length it is a trailing extra.
    Insertion { slot: usize, actual: char },
}

impl Edit {
    /// The slot the error is attributed to.
    pub fn slot(self) -> usize {
        match self {
            Edit::Substitution { slot, .. }
            | Edit::Transposition { slot }
            | Edit::Omission { slot }
            | Edit::Insertion { slot, .. } => slot,
        }
    }
}

impl fmt::Display for Edit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Edit::Substitution { slot, actual } => write!(f, "substitution {actual} at {slot}"),
            Edit::Transposition { slot } => write!(f, "transposition at {}-{slot}", slot - 1),
            Edit::Omission { slot } => write!(f, "omission at {slot}"),
            Edit::Insertion { slot, actual } => write!(f, "insertion {actual} before {slot}"),
        }
    }
}

/// One keystroke's incoming latency and whether it carries motor evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interval {
    /// The keystroke's event sequence number.
    pub seq: u32,
    /// When the keystroke arrived, microseconds from raw-mode entry.
    pub at_micros: u64,
    pub kind: EventKind,
    /// The slot the keystroke was for; extras past a word's end belong to
    /// the following space.
    pub slot: Slot,
    /// The pattern ending at the slot: where a clean latency or a
    /// hesitation is recorded.
    pub pattern: Box<str>,
    pub actual: Option<char>,
    /// Microseconds since the previous keystroke; `None` for the first of
    /// the session.
    pub latency_micros: Option<u64>,
    pub class: IntervalClass,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntervalClass {
    Clean,
    /// Over the hesitation threshold in force at the time; recorded on the
    /// pattern as a hesitation rather than a latency.
    Hesitation {
        threshold_micros: u64,
    },
    /// Excluded for every listed reason.
    Excluded(Vec<Exclusion>),
}

/// Where the hesitation threshold comes from. It is never below 1.5 s and
/// otherwise four times a typical clean latency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HesitationThreshold {
    /// Four times the running median of the session's own clean latencies
    /// so far; the floor alone until one exists.
    RunningMedian,
    /// Four times the user baseline, given as the typical clean log-latency
    /// in seconds, fixed for the whole session.
    UserBaseline(f64),
}

/// Why an interval carries no motor evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    FirstOfSession,
    Backspace,
    /// A keystroke that retyped an attempted position or followed re-entry.
    Replacement,
    /// The keystroke right after a backspace or replacement.
    AfterCorrection,
    /// After an error left uncorrected in the same word.
    FollowingError,
    AfterResize,
    InPaste,
    Burst,
    /// The editing rules ignored the keystroke: a space at a word start or
    /// an extra past the cap.
    Ignored,
}

impl Exclusion {
    /// The reason's name as shown to a developer.
    pub fn name(self) -> &'static str {
        match self {
            Exclusion::FirstOfSession => "first_of_session",
            Exclusion::Backspace => "backspace",
            Exclusion::Replacement => "replacement",
            Exclusion::AfterCorrection => "after_correction",
            Exclusion::FollowingError => "following_error",
            Exclusion::AfterResize => "after_resize",
            Exclusion::InPaste => "in_paste",
            Exclusion::Burst => "burst",
            Exclusion::Ignored => "ignored",
        }
    }
}

/// The session's performance figures.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionMetrics {
    /// Final characters over five over elapsed minutes; `None` when no time
    /// elapsed.
    pub gross_wpm: Option<f64>,
    /// First-attempt correct characters over target characters of the
    /// submitted words, where a transposition is one error against two
    /// characters and an insertion costs one character.
    pub raw_accuracy: f64,
    /// Correctness of the text as submitted, after corrections.
    pub final_accuracy: f64,
    /// Correction keystrokes: backspaces and replacements.
    pub corrections: usize,
    /// Corrections over target characters of the submitted words.
    pub correction_overhead: f64,
    /// Characters wrong, missing, or extra in the submitted words' text as
    /// first submitted.
    pub uncorrected_errors: usize,
    /// Median time from an incorrect keystroke to the backspace that removed
    /// it; `None` when nothing incorrect was removed.
    pub error_latency_micros: Option<u64>,
    /// One minus the coefficient of variation of the clean latencies,
    /// clamped to 0–1; `None` with fewer than two clean intervals.
    pub consistency: Option<f64>,
    pub clean_intervals: usize,
}

/// Analyses a finished session with the hesitation threshold taken from the
/// running median of its own clean latencies. This is the classification
/// for a first session and for a replay without the baseline of the time.
pub fn analyze(state: &SessionState) -> SessionAnalysis {
    analyze_with(state, HesitationThreshold::RunningMedian)
}

/// Analyses a finished session with the given hesitation threshold.
pub fn analyze_with(state: &SessionState, threshold: HesitationThreshold) -> SessionAnalysis {
    let reconstruction = attempt::reconstruct(state);
    let prompt = state.prompt();
    let words: Vec<WordAnalysis> = reconstruction
        .words
        .iter()
        .enumerate()
        .map(|(index, attempt)| {
            let target: Vec<char> = prompt.word(index).chars().collect();
            let errors = if attempt.submitted() {
                alignment::align(&attempt.first_attempt, &target)
                    .into_iter()
                    .map(|(edit, weight)| ErrorAttribution {
                        pattern: attributed_pattern(prompt, index, &target, edit).into(),
                        edit,
                        weight,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let mut uncorrected = attempt
                .submitted_text
                .as_deref()
                .map(|text| differences(text, &target))
                .into_iter()
                .flatten()
                .peekable();
            WordAnalysis {
                target: prompt.word(index).into(),
                attempt_history: attempt.history.clone(),
                first_attempt: attempt.first_attempt.iter().collect(),
                submitted: attempt.submitted(),
                errors,
                first_uncorrected_error: uncorrected.peek().copied(),
                uncorrected_errors: uncorrected.count(),
            }
        })
        .collect();

    let first_uncorrected: Vec<Option<usize>> =
        words.iter().map(|w| w.first_uncorrected_error).collect();
    let intervals = intervals::classify(
        state,
        &reconstruction.keystrokes,
        &first_uncorrected,
        threshold,
    );
    let metrics = session_metrics(state, &words, &intervals, &reconstruction);
    SessionAnalysis {
        words,
        intervals,
        metrics,
    }
}

fn session_metrics(
    state: &SessionState,
    words: &[WordAnalysis],
    intervals: &[Interval],
    reconstruction: &attempt::Reconstruction,
) -> SessionMetrics {
    let submitted = || words.iter().filter(|w| w.submitted);
    let target_chars: usize = submitted().map(|w| w.target.chars().count()).sum();
    let correct: f64 = submitted()
        .map(|w| (w.target.chars().count() as f64 - w.error_count()).max(0.0))
        .sum();
    let per_target_char = |count: f64| {
        if target_chars == 0 {
            0.0
        } else {
            count / target_chars as f64
        }
    };

    let mut error_latencies: Vec<u64> = reconstruction
        .keystrokes
        .iter()
        .filter_map(|k| k.removed_error_after_micros)
        .collect();
    error_latencies.sort_unstable();

    let clean: Vec<f64> = intervals
        .iter()
        .filter(|i| i.class == IntervalClass::Clean)
        .filter_map(|i| i.latency_micros)
        .map(|l| l as f64)
        .collect();

    let corrections = reconstruction.corrections();
    SessionMetrics {
        gross_wpm: metrics::gross_wpm(state),
        raw_accuracy: per_target_char(correct),
        final_accuracy: metrics::final_accuracy(state),
        corrections,
        correction_overhead: per_target_char(corrections as f64),
        uncorrected_errors: submitted().map(|w| w.uncorrected_errors).sum(),
        error_latency_micros: median(&error_latencies, |a, b| (a + b) / 2),
        consistency: consistency(&clean),
        clean_intervals: clean.len(),
    }
}

/// The middle value of a sorted list, or `between` the two middle ones.
pub(crate) fn median<T: Copy>(sorted: &[T], between: impl FnOnce(T, T) -> T) -> Option<T> {
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    Some(if n % 2 == 1 {
        sorted[n / 2]
    } else {
        between(sorted[n / 2 - 1], sorted[n / 2])
    })
}

/// `clamp(1 − sd / mean, 0, 1)` with the population standard deviation;
/// undefined below two values.
fn consistency(latencies: &[f64]) -> Option<f64> {
    if latencies.len() < 2 {
        return None;
    }
    let n = latencies.len() as f64;
    let mean = latencies.iter().sum::<f64>() / n;
    let variance = latencies.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / n;
    Some((1.0 - variance.sqrt() / mean).clamp(0.0, 1.0))
}

/// The pattern an error counts against: for a transposition the bigram it
/// spans, otherwise the prompt text ending at its slot.
fn attributed_pattern(prompt: &Prompt, word: usize, target: &[char], edit: Edit) -> String {
    match edit {
        Edit::Transposition { slot } => target[slot - 1..=slot].iter().collect(),
        _ => prompt.pattern_ending_at(Slot {
            word,
            position: edit.slot(),
        }),
    }
}

/// The positions where `typed` differs from `target`: wrong characters,
/// omitted ones, and extras.
fn differences<'a>(typed: &'a [char], target: &'a [char]) -> impl Iterator<Item = usize> + 'a {
    (0..typed.len().max(target.len())).filter(move |&p| typed.get(p) != target.get(p))
}
