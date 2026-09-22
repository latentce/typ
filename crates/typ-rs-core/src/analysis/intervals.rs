//! Decides which incoming latencies carry motor evidence.
//!
//! Every keystroke from the first printable character on gets an interval.
//! An interval is clean unless something about the keystroke, the one
//! before it, or the word it lands in says the latency is not the user
//! simply typing the next character; an otherwise clean interval longer
//! than the hesitation threshold is a hesitation instead.

use super::attempt::{Keystroke, Role};
use super::{Exclusion, Interval, IntervalClass, median};
use crate::prompt::Slot;
use crate::session::{LONG_PAUSE_MICROS, SessionState};

/// The hesitation threshold is this multiple of the running median clean
/// latency, and never below [`LONG_PAUSE_MICROS`].
const HESITATION_MULTIPLE: u64 = 4;

/// Classifies every keystroke's interval. `first_uncorrected_error[w]` is the
/// position in word `w` after which slots follow an uncorrected error.
pub(super) fn classify(
    state: &SessionState,
    keystrokes: &[Keystroke],
    first_uncorrected_error: &[Option<usize>],
) -> Vec<Interval> {
    let events = state.events();
    let prompt = state.prompt();
    let mut intervals = Vec::new();
    let mut previous: Option<(u64, Role)> = None;
    let mut clean = RunningMedian::default();
    let mut started = false;

    for keystroke in keystrokes {
        let event = &events[keystroke.event];
        started |= event.flags.first_of_session;
        if !started {
            continue;
        }

        let latency = previous.map(|(at, _)| event.at_micros.saturating_sub(at));
        let mut reasons = Vec::new();
        if event.flags.first_of_session {
            reasons.push(Exclusion::FirstOfSession);
        }
        match keystroke.role {
            Role::Backspace => reasons.push(Exclusion::Backspace),
            Role::Replacement => reasons.push(Exclusion::Replacement),
            Role::FirstAttempt | Role::Ignored => {}
        }
        if matches!(previous, Some((_, Role::Backspace | Role::Replacement))) {
            reasons.push(Exclusion::AfterCorrection);
        }
        if first_uncorrected_error[event.word_index].is_some_and(|e| event.position > e) {
            reasons.push(Exclusion::FollowingError);
        }
        if event.flags.after_resize {
            reasons.push(Exclusion::AfterResize);
        }
        if event.flags.in_paste {
            reasons.push(Exclusion::InPaste);
        }
        if event.flags.burst {
            reasons.push(Exclusion::Burst);
        }
        if keystroke.role == Role::Ignored && !event.flags.in_paste {
            reasons.push(Exclusion::Ignored);
        }

        let class = if !reasons.is_empty() {
            IntervalClass::Excluded(reasons)
        } else {
            let latency = latency.expect("only the first keystroke has no latency");
            let threshold = clean.hesitation_threshold();
            if latency > threshold {
                IntervalClass::Hesitation {
                    threshold_micros: threshold,
                }
            } else {
                clean.push(latency);
                IntervalClass::Clean
            }
        };

        let target_len = prompt.word(event.word_index).chars().count();
        let slot = Slot {
            word: event.word_index,
            position: event.position.min(target_len),
        };
        intervals.push(Interval {
            seq: event.seq,
            at_micros: event.at_micros,
            kind: event.kind,
            slot,
            pattern: prompt.pattern_ending_at(slot).into(),
            actual: event.actual,
            latency_micros: latency,
            class,
        });
        if !event.flags.in_paste {
            previous = Some((event.at_micros, keystroke.role));
        }
    }
    intervals
}

/// The clean latencies so far, kept sorted so the median is at hand.
#[derive(Default)]
struct RunningMedian {
    sorted: Vec<u64>,
}

impl RunningMedian {
    fn push(&mut self, latency: u64) {
        let at = self.sorted.partition_point(|&l| l < latency);
        self.sorted.insert(at, latency);
    }

    /// `max(1.5 s, 4 × median)`; the floor alone until a clean latency exists.
    fn hesitation_threshold(&self) -> u64 {
        median(&self.sorted).map_or(LONG_PAUSE_MICROS, |m| {
            (m * HESITATION_MULTIPLE).max(LONG_PAUSE_MICROS)
        })
    }
}
