//! Session performance figures available the moment a session ends.
//!
//! These describe the submitted text and the elapsed time of a session as
//! typed, corrections included; they are what a user sees on finishing. The
//! motor estimate and pattern statistics are a separate analysis over the
//! event log. A completed session's figures are gathered into a
//! [`SessionSummary`], the record cached for it: its own performance, how
//! it compares with the model's prediction and with the user's recent
//! sessions, and how its probes went. The per-word figures behind the
//! probe and transfer views ([`WordPerformance`]) are here too.

use std::collections::{BTreeMap, BTreeSet};

use crate::analysis::{IntervalClass, SessionAnalysis};
use crate::compose::{ComposedWord, WordRole};
use crate::model::{SchedulerConfig, SessionUpdate};
use crate::prompt::{Prompt, Slot};
use crate::session::{EventKind, Outcome, SessionState};

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

/// The user's recent level on each speed figure: an exponentially weighted
/// average over completed sessions, so that one session's figure has
/// something to be compared with that neither a single previous session
/// nor a lifetime average would give. Also the figures of one session,
/// when it is what a series is advanced by.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RecentSeries {
    pub wpm: Option<f64>,
    pub adjusted_ratio: Option<f64>,
    pub reference_wpm: Option<f64>,
}

impl RecentSeries {
    /// The series after one more completed session with the figures
    /// `latest`. Each figure moves toward the session's by the share that
    /// puts it halfway there after `half_life_sessions` such sessions; a
    /// first figure is taken as it is, and a figure the session lacks
    /// leaves the average as it was.
    pub fn advanced(&self, latest: &RecentSeries, half_life_sessions: f64) -> RecentSeries {
        let alpha = if half_life_sessions > 0.0 {
            1.0 - 0.5f64.powf(1.0 / half_life_sessions)
        } else {
            1.0
        };
        let advance = |previous: Option<f64>, latest: Option<f64>| match (previous, latest) {
            (Some(p), Some(l)) => Some(p + alpha * (l - p)),
            (None, Some(l)) => Some(l),
            (previous, None) => previous,
        };
        RecentSeries {
            wpm: advance(self.wpm, latest.wpm),
            adjusted_ratio: advance(self.adjusted_ratio, latest.adjusted_ratio),
            reference_wpm: advance(self.reference_wpm, latest.reference_wpm),
        }
    }
}

/// How one word of a session was typed, with why it was in the prompt: what
/// the probe and word-initiation views are built from.
#[derive(Debug, Clone, PartialEq)]
pub struct WordPerformance {
    /// The word's index in the prompt.
    pub index: usize,
    pub role: WordRole,
    /// A probe that overlapped recent targeted practice; never true of a
    /// targeted word.
    pub contaminated: bool,
    pub submitted: bool,
    /// Seconds from the keystroke before the word's first to its last,
    /// every keystroke typed for the word counted, corrections and extras
    /// included; the first keystroke of the session contributes nothing.
    pub seconds: f64,
    /// The word's target characters plus the space that submitted it, when
    /// one was typed: what the word demanded, whatever was typed for it.
    pub characters: usize,
    pub target_characters: usize,
    /// The word's own raw accuracy; `None` for an unsubmitted word.
    pub raw_accuracy: Option<f64>,
    /// The incoming latency of the word's first character; `None` for the
    /// first keystroke of the session and for a word never started.
    pub initiation_micros: Option<u64>,
}

/// How each word of a session was typed. `words` gives each word's role and
/// contamination as composed; a word beyond the end of it is taken for an
/// uncontaminated probe.
pub fn word_performances(
    analysis: &SessionAnalysis,
    words: &[ComposedWord],
) -> Vec<WordPerformance> {
    analysis
        .words
        .iter()
        .enumerate()
        .map(|(index, word)| {
            let target_characters = word.target.chars().count();
            let intervals = analysis.intervals.iter().filter(|i| i.slot.word == index);
            let seconds = intervals
                .clone()
                .filter_map(|i| i.latency_micros)
                .map(|l| l as f64 / 1_000_000.0)
                .sum();
            let initiation_micros = intervals
                .clone()
                .find(|i| i.kind == EventKind::Char && i.slot.position == 0)
                .and_then(|i| i.latency_micros);
            let composed = words.get(index);
            WordPerformance {
                index,
                role: composed.map_or(WordRole::Probe, |w| w.role),
                contaminated: composed.is_some_and(|w| {
                    w.role == WordRole::Probe
                        && w.contamination.as_ref().is_some_and(|c| {
                            c.recently_targeted_word || !c.recently_targeted_patterns.is_empty()
                        })
                }),
                submitted: word.submitted,
                seconds,
                characters: target_characters + usize::from(space_typed(analysis, index)),
                target_characters,
                raw_accuracy: word.raw_accuracy(),
                initiation_micros,
            }
        })
        .collect()
}

/// Whether a word was left with a space: every submitted word but the last,
/// and the last when the session ended on a space rather than on its final
/// character.
pub fn space_typed(analysis: &SessionAnalysis, index: usize) -> bool {
    analysis.words[index].submitted
        && (index + 1 < analysis.words.len()
            || analysis
                .intervals
                .last()
                .is_some_and(|i| i.kind == EventKind::Space))
}

impl WordPerformance {
    /// A submitted probe with a typing time, contaminated or not.
    fn is_measured_probe(&self) -> bool {
        self.role == WordRole::Probe && self.submitted && self.seconds > 0.0
    }
}

/// Speed and raw accuracy over a set of probe words.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ProbeMetrics {
    pub words: usize,
    /// The words' characters over five over the minutes they took, all
    /// together; `None` without a word.
    pub wpm: Option<f64>,
    /// First-attempt correct characters over target characters, all
    /// together; `None` without a word.
    pub raw_accuracy: Option<f64>,
}

impl ProbeMetrics {
    /// The metrics over the given words; unsubmitted words and words that
    /// took no time are left out.
    pub fn over<'a>(words: impl Iterator<Item = &'a WordPerformance>) -> ProbeMetrics {
        let mut count = 0usize;
        let mut seconds = 0.0;
        let mut characters = 0usize;
        let mut target_characters = 0usize;
        let mut correct = 0.0;
        for w in words.filter(|w| w.submitted && w.seconds > 0.0) {
            count += 1;
            seconds += w.seconds;
            characters += w.characters;
            target_characters += w.target_characters;
            correct += w.raw_accuracy.unwrap_or(0.0) * w.target_characters as f64;
        }
        ProbeMetrics {
            words: count,
            wpm: (seconds > 0.0).then(|| characters as f64 / 5.0 / (seconds / 60.0)),
            raw_accuracy: (target_characters > 0).then(|| correct / target_characters as f64),
        }
    }
}

/// The figures of a completed session as cached and shown.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub gross_wpm: Option<f64>,
    pub raw_accuracy: f64,
    pub final_accuracy: f64,
    pub consistency: Option<f64>,
    pub corrections: usize,
    pub session_offset: f64,
    /// `None` without a clean interval to compare.
    pub adjusted_ratio: Option<f64>,
    pub reference_wpm: Option<f64>,
    /// The recent series with this session included.
    pub recent: RecentSeries,
    /// Over the session's probes clear of recent targeted practice.
    pub probes: ProbeMetrics,
    /// Over the probes that overlapped it.
    pub contaminated_probes: ProbeMetrics,
}

/// A completed session's summary: its performance figures, its difficulty
/// adjustment, the recent series advanced from `previous` (the series as
/// of the last completed session, `None` for the first), and its probe
/// metrics. `None` for an interrupted session, which reports no figures.
pub fn summarize(
    state: &SessionState,
    update: &SessionUpdate,
    words: &[ComposedWord],
    previous: Option<&RecentSeries>,
    config: &SchedulerConfig,
) -> Option<SessionSummary> {
    if state.outcome() != Some(Outcome::Completed) {
        return None;
    }
    let metrics = &update.analysis.metrics;
    let adjusted_ratio = update.difficulty.map(|d| d.adjusted_ratio());
    let reference_wpm = update.difficulty.map(|d| d.reference_wpm());
    let latest = RecentSeries {
        wpm: metrics.gross_wpm,
        adjusted_ratio,
        reference_wpm,
    };
    let performances = word_performances(&update.analysis, words);
    let probes = |contaminated: bool| {
        ProbeMetrics::over(
            performances
                .iter()
                .filter(|w| w.is_measured_probe() && w.contaminated == contaminated),
        )
    };
    Some(SessionSummary {
        gross_wpm: metrics.gross_wpm,
        raw_accuracy: metrics.raw_accuracy,
        final_accuracy: metrics.final_accuracy,
        consistency: metrics.consistency,
        corrections: metrics.corrections,
        session_offset: update.applied.map_or(0.0, |a| a.session_offset),
        adjusted_ratio,
        reference_wpm,
        recent: previous
            .copied()
            .unwrap_or_default()
            .advanced(&latest, config.recent_half_life_sessions),
        probes: probes(false),
        contaminated_probes: probes(true),
    })
}

/// A change in probe performance the evidence supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sustained {
    Improvement,
    Decline,
}

/// Probe performance over a rolling window of the most recent probe words,
/// against the window before it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbeTrend {
    /// Over the most recent uncontaminated probes, up to the window.
    pub current: ProbeMetrics,
    /// Over the contaminated probes among them.
    pub contaminated: ProbeMetrics,
    /// Over the full window of uncontaminated probes before those; `None`
    /// when there are not enough for a full one.
    pub previous: Option<ProbeMetrics>,
    /// Set only when both windows are full and the 95% confidence interval
    /// of the current window's pace excludes the previous window's.
    pub sustained: Option<Sustained>,
}

/// The probe trend over `words`, most recent first, with windows of
/// `window` uncontaminated probe words. Contaminated probes are set aside
/// as they are met, until the current window is full.
pub fn probe_trend<'a>(
    words: impl Iterator<Item = &'a WordPerformance>,
    window: usize,
) -> ProbeTrend {
    let mut current: Vec<&WordPerformance> = Vec::new();
    let mut contaminated: Vec<&WordPerformance> = Vec::new();
    let mut previous: Vec<&WordPerformance> = Vec::new();
    for w in words.filter(|w| w.is_measured_probe()) {
        if current.len() < window {
            if w.contaminated {
                contaminated.push(w);
            } else {
                current.push(w);
            }
        } else if !w.contaminated {
            previous.push(w);
            if previous.len() == window {
                break;
            }
        }
    }
    let both_full = window > 0 && current.len() == window && previous.len() == window;
    let sustained = both_full.then(|| {
        let (mean, se) = pace(&current);
        let (before, _) = pace(&previous);
        let z = 1.96;
        if before < mean - z * se {
            Some(Sustained::Decline)
        } else if before > mean + z * se {
            Some(Sustained::Improvement)
        } else {
            None
        }
    });
    ProbeTrend {
        current: ProbeMetrics::over(current.iter().copied()),
        contaminated: ProbeMetrics::over(contaminated.iter().copied()),
        previous: both_full.then(|| ProbeMetrics::over(previous.iter().copied())),
        sustained: sustained.flatten(),
    }
}

/// The words' seconds per character all together, with its standard error
/// taking each word as one observation weighted by its characters.
fn pace(words: &[&WordPerformance]) -> (f64, f64) {
    let characters: f64 = words.iter().map(|w| w.characters as f64).sum();
    let seconds: f64 = words.iter().map(|w| w.seconds).sum();
    let mean = seconds / characters;
    let variance: f64 = words
        .iter()
        .map(|w| {
            let weight = w.characters as f64;
            let deviation = w.seconds / weight - mean;
            weight * weight * deviation * deviation
        })
        .sum::<f64>()
        / (characters * characters);
    (mean, variance.sqrt())
}

/// The median word-initiation latency over the words that have one.
pub fn word_initiation_median_micros(words: &[WordPerformance]) -> Option<u64> {
    let mut latencies: Vec<u64> = words.iter().filter_map(|w| w.initiation_micros).collect();
    latencies.sort_unstable();
    crate::analysis::median(&latencies, |a, b| (a + b) / 2)
}

/// A pattern's slots over some words: first-attempt trials with the error
/// mass among them, and the clean latencies typed at them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SlotAggregate {
    pub slots: usize,
    pub errors: f64,
    pub clean_latencies_micros: Vec<u64>,
}

impl SlotAggregate {
    /// One minus the error share of the trials; `None` without a trial.
    pub fn raw_accuracy(&self) -> Option<f64> {
        (self.slots > 0).then(|| 1.0 - self.errors / self.slots as f64)
    }

    pub fn median_latency_micros(&self) -> Option<u64> {
        let mut sorted = self.clean_latencies_micros.clone();
        sorted.sort_unstable();
        crate::analysis::median(&sorted, |a, b| (a + b) / 2)
    }
}

/// A pattern's slots split by whether the word they were in has been used
/// for targeted practice.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PatternTransfer {
    pub targeted: SlotAggregate,
    pub untargeted: SlotAggregate,
}

/// Adds one session's slots to the transfer rows of `patterns`. A slot
/// counts for every pattern in its chain that is asked for. Trials come
/// from submitted words, with each slot's error mass capped at one as the
/// model counts it; clean latencies from every keystroke classed clean. A
/// word goes to the untargeted side only if it was not targeted in this
/// prompt and `untargeted_word` returns true for it.
pub fn accumulate_transfer(
    into: &mut BTreeMap<Box<str>, PatternTransfer>,
    prompt: &Prompt,
    analysis: &SessionAnalysis,
    words: &[ComposedWord],
    patterns: &BTreeSet<&str>,
    untargeted_word: impl Fn(&str) -> bool,
) {
    let is_untargeted = |index: usize| {
        words
            .get(index)
            .is_none_or(|w| w.role != WordRole::Targeted)
            && untargeted_word(prompt.word(index))
    };
    let mut add = |pattern: &str, index: usize, f: &dyn Fn(&mut SlotAggregate)| {
        let row = into.entry(pattern.into()).or_default();
        f(if is_untargeted(index) {
            &mut row.untargeted
        } else {
            &mut row.targeted
        });
    };
    let levels_of = |chain: &str| -> Vec<&str> {
        patterns
            .iter()
            .copied()
            .filter(|p| chain.ends_with(p))
            .collect()
    };

    for (index, word) in analysis.words.iter().enumerate() {
        if !word.submitted {
            continue;
        }
        let len = word.target.chars().count();
        let error_mass = word.error_mass();
        let has_space_slot = space_typed(analysis, index) || error_mass.contains_key(&len);
        let slots = (0..len).chain(has_space_slot.then_some(len));
        for position in slots {
            let error = error_mass.get(&position).copied().unwrap_or(0.0).min(1.0);
            let chain = prompt.pattern_ending_at(Slot {
                word: index,
                position,
            });
            for level in levels_of(&chain) {
                add(level, index, &|a| {
                    a.slots += 1;
                    a.errors += error;
                });
            }
        }
    }
    for interval in &analysis.intervals {
        if interval.class != IntervalClass::Clean {
            continue;
        }
        let Some(latency) = interval.latency_micros else {
            continue;
        };
        for level in levels_of(&interval.pattern) {
            add(level, interval.slot.word, &|a| {
                a.clean_latencies_micros.push(latency);
            });
        }
    }
}
