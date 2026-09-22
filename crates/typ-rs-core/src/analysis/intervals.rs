//! Decides which incoming latencies carry motor evidence.
//!
//! Every keystroke from the first printable character on gets an interval.
//! An interval is clean unless something about the keystroke, the one
//! before it, or the word it lands in says the latency is not the user
//! simply typing the next character; an otherwise clean interval longer
//! than the hesitation threshold is a hesitation instead.

use super::attempt::{Keystroke, Role};
use super::{Exclusion, HesitationThreshold, Interval, IntervalClass, median};
use crate::prompt::Slot;
use crate::session::{LONG_PAUSE_MICROS, SessionState};

/// The hesitation threshold is this multiple of a typical clean latency,
/// and never below [`LONG_PAUSE_MICROS`].
const HESITATION_MULTIPLE: f64 = 4.0;

/// Classifies every keystroke's interval. `first_uncorrected_error[w]` is the
/// position in word `w` after which slots follow an uncorrected error.
pub(super) fn classify(
    state: &SessionState,
    keystrokes: &[Keystroke],
    first_uncorrected_error: &[Option<usize>],
    threshold: HesitationThreshold,
) -> Vec<Interval> {
    let events = state.events();
    let prompt = state.prompt();
    let mut intervals = Vec::new();
    let mut previous: Option<(u64, Role)> = None;
    let mut hesitation = Threshold::new(threshold);
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
            let threshold_micros = hesitation.micros();
            if latency > threshold_micros {
                IntervalClass::Hesitation { threshold_micros }
            } else {
                hesitation.observe_clean(latency);
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

/// The hesitation threshold as the session proceeds: fixed from the user
/// baseline, or following the clean latencies seen so far.
enum Threshold {
    Fixed(u64),
    Running {
        /// The clean latencies so far, kept sorted so the median is at hand.
        sorted: Vec<u64>,
    },
}

impl Threshold {
    fn new(threshold: HesitationThreshold) -> Threshold {
        match threshold {
            HesitationThreshold::RunningMedian => Threshold::Running { sorted: Vec::new() },
            HesitationThreshold::UserBaseline(log_seconds) => {
                Threshold::Fixed(over_typical(log_seconds.exp() * 1_000_000.0))
            }
        }
    }

    fn observe_clean(&mut self, latency: u64) {
        if let Threshold::Running { sorted } = self {
            let at = sorted.partition_point(|&l| l < latency);
            sorted.insert(at, latency);
        }
    }

    fn micros(&self) -> u64 {
        match self {
            Threshold::Fixed(micros) => *micros,
            Threshold::Running { sorted } => median(sorted, |a, b| (a + b) / 2)
                .map_or(LONG_PAUSE_MICROS, |m| over_typical(m as f64)),
        }
    }
}

/// `max(1.5 s, 4 × typical)` in microseconds.
fn over_typical(typical_micros: f64) -> u64 {
    ((typical_micros * HESITATION_MULTIPLE) as u64).max(LONG_PAUSE_MICROS)
}
