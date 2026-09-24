//! The plain-text output of `typ`: the results block, the session listing,
//! the probe, word-initiation, and transfer views, the pattern summary,
//! and the replay report.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use typ_rs_core::analysis::{
    Interval, IntervalClass, SessionAnalysis, SessionMetrics, WordAnalysis, analyze,
};
use typ_rs_core::compose::ComposedPrompt;
use typ_rs_core::corpus::Corpus;
use typ_rs_core::metrics::{
    PatternTransfer, ProbeMetrics, RecentSeries, SessionSummary, SlotAggregate, Sustained,
    WordPerformance, accumulate_transfer, probe_trend, word_initiation_median_micros,
    word_performances,
};
use typ_rs_core::model::{ModelState, PatternEstimate, ROOT, SchedulerConfig, Weakness};
use typ_rs_core::scheduler::{TargetRole, TrainingHistory, eligible_patterns};
use typ_rs_core::session::{EventKind, Outcome, SessionState};
use typ_rs_store::StoredSession;

/// How many patterns each list of the pattern summary shows.
const LISTED_PATTERNS: usize = 10;

/// How many probe words the rolling probe window holds.
const PROBE_WINDOW: usize = 100;

/// What the results block is made from.
pub struct Ending<'a> {
    pub state: &'a SessionState,
    pub metrics: &'a SessionMetrics,
    /// The session's summary; `None` for an interrupted session, and when
    /// the model could not be loaded to make one.
    pub summary: Option<&'a SessionSummary>,
    /// The recent series before this session, which its figures are
    /// compared with; `None` before the first completed session.
    pub previous: Option<&'a RecentSeries>,
    /// Whether an interrupted session's observations entered the model;
    /// `None` when it is not known.
    pub observations_saved: Option<bool>,
    pub next: Option<&'a ComposedPrompt>,
}

/// The block printed when a session ends. For a completed session: its
/// figures, then the speed it translates to on standard text with its
/// change against the recent sessions (or `baseline recorded` when there
/// are none), then the patterns the next prompt will practise. Speed and
/// accuracy are reported only for a completed session, since a partial
/// prompt has no meaningful WPM: an interrupted session says how far it
/// got and whether its observations were kept.
pub fn results(ending: &Ending) -> String {
    let mut out = match ending.state.outcome() {
        Some(Outcome::Completed) => {
            let mut out = figures_line(ending.metrics);
            if let Some(summary) = ending.summary {
                let _ = write!(out, "\n{}", standard_text_line(summary, ending.previous));
            }
            out
        }
        _ => {
            let words = ending.state.words_completed();
            let mut out = format!("interrupted after {words} {}", plural(words, "word"));
            match ending.observations_saved {
                Some(true) => out.push_str("  observations saved"),
                Some(false) => {
                    out.push_str("  observations not saved: too few clean intervals");
                }
                None => {}
            }
            out
        }
    };
    if let Some(next) = ending.next {
        let _ = write!(out, "\n{}", next_line(next));
    }
    out
}

/// Gross WPM, raw and final accuracy, and consistency.
fn figures_line(metrics: &SessionMetrics) -> String {
    let wpm = metrics.gross_wpm.unwrap_or(0.0);
    let raw = 100.0 * metrics.raw_accuracy;
    let final_ = 100.0 * metrics.final_accuracy;
    let consistency = percent_or_dashes(metrics.consistency);
    format!("{wpm:.0} wpm  {raw:.1}% raw  {final_:.1}% final  {consistency} consistency")
}

/// The reference-equivalent WPM and its change against the recent series
/// before the session.
fn standard_text_line(summary: &SessionSummary, previous: Option<&RecentSeries>) -> String {
    match summary.reference_wpm {
        None => "-- wpm on standard text".to_string(),
        Some(reference) => match previous.and_then(|p| p.reference_wpm) {
            Some(recent) => {
                format!(
                    "{reference:.0} wpm on standard text  {:+.0} vs recent",
                    reference - recent
                )
            }
            None => format!("{reference:.0} wpm on standard text  baseline recorded"),
        },
    }
}

/// A figure formatted by `format`, or `--` when there is none.
fn or_dashes(value: Option<f64>, format: impl Fn(f64) -> String) -> String {
    value.map_or_else(|| "--".to_string(), format)
}

fn percent_or_dashes(value: Option<f64>) -> String {
    or_dashes(value, |v| format!("{:.0}%", 100.0 * v))
}

/// `next:` followed by the next prompt's targets in rank order and its
/// exploration target, or `none yet` when nothing was selected for it.
fn next_line(next: &ComposedPrompt) -> String {
    let targets: Vec<String> = next
        .targets
        .iter()
        .filter(|t| t.role == TargetRole::Target)
        .map(|t| visible(&t.pattern))
        .collect();
    let explore = next
        .targets
        .iter()
        .find(|t| t.role == TargetRole::Explore)
        .map(|t| visible(&t.pattern));
    match (targets.is_empty(), explore) {
        (true, None) => "next: none yet".to_string(),
        (true, Some(e)) => format!("next: exploring {e}"),
        (false, None) => format!("next: {}", targets.join(", ")),
        (false, Some(e)) => format!("next: {} (exploring {e})", targets.join(", ")),
    }
}

/// One line per completed session, most recent first: gross WPM, the speed
/// on standard text, raw accuracy, and consistency, from the session's
/// cached summary. A session without one (not yet applied) is analysed on
/// the spot and shows no speed on standard text.
pub fn session_listing(sessions: &[StoredSession]) -> String {
    if sessions.is_empty() {
        return "no completed sessions yet\n".to_string();
    }
    sessions
        .iter()
        .map(|session| {
            let (wpm, reference, raw, consistency) = match &session.summary {
                Some(s) => (s.gross_wpm, s.reference_wpm, s.raw_accuracy, s.consistency),
                None => {
                    let m = analyze(&session.replay()).metrics;
                    (m.gross_wpm, None, m.raw_accuracy, m.consistency)
                }
            };
            format!(
                "{:>4}  {}  {:>3} words  {:>3.0} wpm  {:>3} on standard text  {:>5.1}% raw  {:>4} consistency\n",
                session.id,
                session.started_at_local,
                session.prompt.word_count(),
                wpm.unwrap_or(0.0),
                or_dashes(reference, |r| format!("{r:.0}")),
                100.0 * raw,
                percent_or_dashes(consistency),
            )
        })
        .collect()
}

/// Each session replayed and analysed, in the order given.
pub fn analyses(sessions: &[StoredSession]) -> Vec<SessionAnalysis> {
    sessions.iter().map(|s| analyze(&s.replay())).collect()
}

/// How each word of each session was typed, from the sessions' analyses.
pub fn performances(
    sessions: &[StoredSession],
    analyses: &[SessionAnalysis],
) -> Vec<Vec<WordPerformance>> {
    sessions
        .iter()
        .zip(analyses)
        .map(|(s, analysis)| word_performances(analysis, &s.words))
        .collect()
}

/// Probe performance over the most recent hundred probe words clear of
/// recent targeted practice, taken from the sessions most recent first,
/// with the sustained marker when the evidence supports one; the
/// contaminated probes met among them go on their own line. Empty without
/// a probe word.
pub fn probe_section(performances: &[Vec<WordPerformance>]) -> String {
    let trend = probe_trend(
        performances.iter().flat_map(|words| words.iter().rev()),
        PROBE_WINDOW,
    );
    if trend.current.words == 0 && trend.contaminated.words == 0 {
        return String::new();
    }
    let mut out = String::from("\nprobes\n");
    let _ = write!(
        out,
        "  last {:>3} uncontaminated  {}",
        trend.current.words,
        probe_figures(&trend.current)
    );
    if let (Some(sustained), Some(previous)) = (trend.sustained, trend.previous) {
        let _ = write!(
            out,
            "  sustained {} from {}",
            match sustained {
                Sustained::Improvement => "improvement",
                Sustained::Decline => "decline",
            },
            or_dashes(previous.wpm, |w| format!("{w:.0} wpm"))
        );
    }
    out.push('\n');
    if trend.contaminated.words > 0 {
        let _ = writeln!(
            out,
            "  {:>3} contaminated among them  {}",
            trend.contaminated.words,
            probe_figures(&trend.contaminated)
        );
    }
    out
}

fn probe_figures(metrics: &ProbeMetrics) -> String {
    format!(
        "{} wpm  {} raw",
        or_dashes(metrics.wpm, |w| format!("{w:>3.0}")),
        or_dashes(metrics.raw_accuracy, |a| format!("{:.1}%", 100.0 * a))
    )
}

/// The median word-initiation latency of each session, oldest first.
/// Empty when no session has one.
pub fn word_initiation_line(performances: &[Vec<WordPerformance>]) -> String {
    let medians: Vec<String> = performances
        .iter()
        .rev()
        .filter_map(|words| word_initiation_median_micros(words))
        .map(|m| (m / 1000).to_string())
        .collect();
    if medians.is_empty() {
        return String::new();
    }
    format!(
        "\nword initiation (median ms, oldest first)\n  {}\n",
        medians.join("  ")
    )
}

/// For every pattern practised in the last `contamination_sessions`
/// sessions: its median clean latency and raw accuracy in the words used
/// for targeted practice over that span against those in words that were
/// not, over the sessions given. Empty without a practised pattern.
pub fn transfer_section(
    sessions: &[StoredSession],
    analyses: &[SessionAnalysis],
    history: &TrainingHistory,
    config: &SchedulerConfig,
) -> String {
    let span = config.contamination_sessions;
    let patterns: BTreeSet<&str> = history.recently_practised(span).collect();
    if patterns.is_empty() {
        return String::new();
    }
    let mut rows: BTreeMap<Box<str>, PatternTransfer> = BTreeMap::new();
    for (session, analysis) in sessions.iter().zip(analyses).take(span) {
        accumulate_transfer(
            &mut rows,
            &session.prompt,
            analysis,
            &session.words,
            &patterns,
            |word| !history.targeted_word_within(word, span),
        );
    }
    let mut out = String::from("\ntransfer to untargeted words\n");
    for (pattern, row) in &rows {
        let _ = writeln!(
            out,
            "  {:<3}  targeted {}  untargeted {}",
            visible(pattern),
            slot_figures(&row.targeted),
            slot_figures(&row.untargeted)
        );
    }
    out
}

fn slot_figures(slots: &SlotAggregate) -> String {
    format!(
        "{} {} raw  n {:>3}",
        slots
            .median_latency_micros()
            .map_or("  -- ms".to_string(), |l| format!("{:>4} ms", l / 1000)),
        slots
            .raw_accuracy()
            .map_or("   --".to_string(), |a| format!("{:>5.1}%", 100.0 * a)),
        slots.slots
    )
}

/// What the model believes about the user's patterns: the ten with the
/// highest absolute slowness among those with latency evidence, the ten
/// with the highest error probability among those with first-attempt
/// trials, each with its effective sample size; the ten eligible patterns
/// the user has typed with the highest weakness, as mean ± sd; and the
/// deferred candidates with the sessions remaining in their windows. Empty
/// when nothing has been observed. Estimates are as of the last session
/// applied.
pub fn pattern_summary(
    model: &ModelState,
    corpus: &Corpus,
    config: &SchedulerConfig,
    history: &TrainingHistory,
) -> String {
    let Some(at) = model.last_update() else {
        return String::new();
    };
    let estimates: Vec<Estimated> = model
        .patterns()
        .filter(|(pattern, _)| *pattern != ROOT)
        .map(|(pattern, stats)| Estimated {
            pattern,
            estimate: model.estimate(pattern, at, config),
            trials: stats.c + stats.e,
        })
        .collect();

    let slowest = ranked(estimates.iter().filter(|e| e.estimate.n_eff > 0.0), |e| {
        e.absolute_slowness
    });
    let error_prone = ranked(estimates.iter().filter(|e| e.trials > 0.0), |e| {
        e.error_probability
    });

    let mut out = String::new();
    if !slowest.is_empty() {
        let _ = writeln!(out, "\nslowest patterns");
        for e in slowest {
            let percent = 100.0 * (e.estimate.absolute_slowness.exp() - 1.0);
            let _ = writeln!(
                out,
                "  {:<3}  {percent:>+4.0}%  n_eff {:>5.1}",
                visible(e.pattern),
                e.estimate.n_eff
            );
        }
    }
    if !error_prone.is_empty() {
        let _ = writeln!(out, "\nmost error-prone patterns");
        for e in error_prone {
            let _ = writeln!(
                out,
                "  {:<3}  {:>4.1}%  n_eff {:>5.1}",
                visible(e.pattern),
                100.0 * e.estimate.error_probability,
                e.estimate.n_eff
            );
        }
    }

    let eligible = eligible_patterns(corpus, config);
    let mut weakest: Vec<(&str, Weakness)> = eligible
        .iter()
        .filter(|e| model.stats(&e.pattern).is_some())
        .map(|e| (e.pattern.as_ref(), model.weakness(&e.pattern, at, config)))
        .collect();
    weakest.sort_by(|a, b| b.1.mean.total_cmp(&a.1.mean).then_with(|| a.0.cmp(b.0)));
    weakest.truncate(LISTED_PATTERNS);
    if !weakest.is_empty() {
        let _ = writeln!(out, "\nweakest patterns");
        for (pattern, w) in &weakest {
            let _ = writeln!(
                out,
                "  {:<3}  {:>+5.2} ± {:.2}",
                visible(pattern),
                w.mean,
                w.sd
            );
        }
    }

    let deferred: Vec<(&str, usize)> = history.deferrals().collect();
    if !deferred.is_empty() {
        let _ = writeln!(out, "\ndeferred candidates");
        for (pattern, remaining) in deferred {
            let _ = writeln!(
                out,
                "  {:<3}  {remaining} {} remaining",
                visible(pattern),
                plural(remaining, "session")
            );
        }
    }
    out
}

/// One pattern's estimate together with how many first-attempt trials back
/// its error probability.
struct Estimated<'a> {
    pattern: &'a str,
    estimate: PatternEstimate,
    trials: f64,
}

/// The top patterns by `key`, highest first, ties in pattern order.
fn ranked<'a>(
    candidates: impl Iterator<Item = &'a Estimated<'a>>,
    key: impl Fn(&PatternEstimate) -> f64,
) -> Vec<&'a Estimated<'a>> {
    let mut ranked: Vec<&Estimated> = candidates.collect();
    ranked.sort_by(|a, b| {
        key(&b.estimate)
            .total_cmp(&key(&a.estimate))
            .then_with(|| a.pattern.cmp(b.pattern))
    });
    ranked.truncate(LISTED_PATTERNS);
    ranked
}

/// A pattern with its spaces made visible.
fn visible(pattern: &str) -> String {
    pattern.replace(' ', "␣")
}

/// How a stored session was interpreted: the results as the user saw them,
/// the developer's figures, every word's first attempt, own raw accuracy,
/// and attributed errors, and every interval's classification. Words never
/// reached are left out.
pub fn replay(session: &StoredSession, state: &SessionState, analysis: &SessionAnalysis) -> String {
    let mut out = String::new();
    let m = &analysis.metrics;
    let _ = writeln!(
        out,
        "session {}  {}  {}  {} {}",
        session.id,
        session.started_at_local,
        session.outcome.name(),
        state.word_count(),
        plural(state.word_count(), "word")
    );
    let ending = Ending {
        state,
        metrics: m,
        summary: None,
        previous: None,
        observations_saved: None,
        next: None,
    };
    let _ = writeln!(out, "{}", results(&ending));
    let _ = writeln!(
        out,
        "{} {}  {} uncorrected  error latency {}  {} clean {}",
        m.corrections,
        plural(m.corrections, "correction"),
        m.uncorrected_errors,
        m.error_latency_micros
            .map_or("-".to_string(), |l| format!("{} ms", l / 1000)),
        m.clean_intervals,
        plural(m.clean_intervals, "interval"),
    );

    let _ = writeln!(out, "\nwords");
    let reached = analysis
        .words
        .iter()
        .rposition(WordAnalysis::reached)
        .map_or(0, |i| i + 1);
    for (index, word) in analysis.words[..reached].iter().enumerate() {
        out.push_str(&word_line(index, word));
    }

    let _ = writeln!(out, "\nintervals");
    let _ = writeln!(out, " seq       at  latency  slot  key  class");
    for interval in &analysis.intervals {
        out.push_str(&interval_line(interval));
    }
    out
}

/// The differences between a session's summary as the current pipeline
/// computes it and the one cached for it, figure by figure; one line
/// saying so when there are none or when neither exists.
pub fn summary_diff(current: Option<&SessionSummary>, stored: Option<&SessionSummary>) -> String {
    let (current, stored) = match (current, stored) {
        (None, None) => return "no summary: the session was interrupted\n".to_string(),
        (Some(_), None) => {
            return "no stored summary: the session has not been applied\n".to_string();
        }
        (None, Some(_)) => {
            return "the current pipeline gives no summary but one is stored\n".to_string();
        }
        (Some(c), Some(s)) => (c, s),
    };
    let differences: Vec<String> = SUMMARY_FIGURES
        .iter()
        .filter_map(|(name, get)| {
            let (now, was) = (get(current), get(stored));
            let same = match (now, was) {
                (Some(a), Some(b)) => (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0),
                (None, None) => true,
                _ => false,
            };
            (!same).then(|| {
                format!(
                    "  {name:<32} stored {:>10}  current {:>10}\n",
                    format_figure(was),
                    format_figure(now)
                )
            })
        })
        .collect();
    if differences.is_empty() {
        "no differences between the stored summary and the current pipeline\n".to_string()
    } else {
        format!(
            "differences from the stored summary\n{}",
            differences.concat()
        )
    }
}

fn format_figure(value: Option<f64>) -> String {
    or_dashes(value, |v| format!("{v:.4}"))
}

/// One figure of a summary: its name and how to read it as a number.
type SummaryFigure = (&'static str, fn(&SessionSummary) -> Option<f64>);

/// Every figure of a summary by name.
const SUMMARY_FIGURES: &[SummaryFigure] = &[
    ("gross_wpm", |s| s.gross_wpm),
    ("raw_accuracy", |s| Some(s.raw_accuracy)),
    ("final_accuracy", |s| Some(s.final_accuracy)),
    ("consistency", |s| s.consistency),
    ("corrections", |s| Some(s.corrections as f64)),
    ("session_offset", |s| Some(s.session_offset)),
    ("adjusted_ratio", |s| s.adjusted_ratio),
    ("reference_wpm", |s| s.reference_wpm),
    ("recent_wpm", |s| s.recent.wpm),
    ("recent_adjusted_ratio", |s| s.recent.adjusted_ratio),
    ("recent_reference_wpm", |s| s.recent.reference_wpm),
    ("probe_words", |s| Some(s.probes.words as f64)),
    ("probe_wpm", |s| s.probes.wpm),
    ("probe_raw_accuracy", |s| s.probes.raw_accuracy),
    ("contaminated_probe_words", |s| {
        Some(s.contaminated_probes.words as f64)
    }),
    ("contaminated_probe_wpm", |s| s.contaminated_probes.wpm),
    ("contaminated_probe_raw_accuracy", |s| {
        s.contaminated_probes.raw_accuracy
    }),
];

fn word_line(index: usize, word: &WordAnalysis) -> String {
    let mut line = format!(
        "{index:>3} {}  first attempt {:?}",
        word.target, word.first_attempt
    );
    let history: String = word.attempt_history.iter().collect();
    if history != word.first_attempt {
        let _ = write!(line, "  history {history:?}");
    }
    match word.raw_accuracy() {
        Some(raw) => {
            let _ = write!(line, "  {:.0}% raw", 100.0 * raw);
        }
        None => line.push_str("  (not submitted)"),
    }
    line.push('\n');
    for error in &word.errors {
        let _ = write!(line, "      {} → {:?}", error.edit, error.pattern);
        if error.weight != 1.0 {
            let _ = write!(line, " ×{}", error.weight);
        }
        line.push('\n');
    }
    line
}

fn interval_line(interval: &Interval) -> String {
    let key = match (interval.kind, interval.actual) {
        (EventKind::Backspace, _) => '⌫',
        (_, Some(' ')) => '␣',
        (_, Some(c)) => c,
        (_, None) => '?',
    };
    let latency = interval
        .latency_micros
        .map_or("-".to_string(), |l| (l / 1000).to_string());
    let class = match &interval.class {
        IntervalClass::Clean => "clean".to_string(),
        IntervalClass::Hesitation { threshold_micros } => {
            format!("hesitation (over {} ms)", threshold_micros / 1000)
        }
        IntervalClass::Excluded(reasons) => format!(
            "excluded: {}",
            reasons
                .iter()
                .map(|r| r.name())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    format!(
        "{:>4} {:>8} {latency:>8}   {}:{}  {key}    {class}\n",
        interval.seq,
        interval.at_micros / 1000,
        interval.slot.word,
        interval.slot.position,
    )
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        noun.to_string()
    } else {
        format!("{noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use typ_rs_core::compose::{ComposedPrompt, WordRole};
    use typ_rs_core::metrics::ProbeMetrics;
    use typ_rs_core::prompt::Prompt;
    use typ_rs_core::scheduler::{SelectedTarget, TrainingEvent};
    use typ_rs_core::session::{EndCondition, Input, Key};

    /// `⎋` is an interrupt, `⌫` a backspace; one keystroke every 100 ms.
    fn typed(prompt: &str, script: &str) -> SessionState {
        let mut state = SessionState::new(
            Prompt::new(prompt.split(' ')),
            EndCondition::AfterWords(usize::MAX),
        );
        for (i, c) in script.chars().enumerate() {
            let key = match c {
                '⎋' => Key::Interrupt,
                '⌫' => Key::Backspace,
                c => Key::Char(c),
            };
            state.apply_event(Input::new(i as u64 * 100_000, key));
        }
        state
    }

    fn ending<'a>(state: &'a SessionState, metrics: &'a SessionMetrics) -> Ending<'a> {
        Ending {
            state,
            metrics,
            summary: None,
            previous: None,
            observations_saved: None,
            next: None,
        }
    }

    fn run(prompt: &str, script: &str) -> String {
        let state = typed(prompt, script);
        results(&ending(&state, &analyze(&state).metrics))
    }

    fn summary(reference_wpm: Option<f64>) -> SessionSummary {
        SessionSummary {
            gross_wpm: Some(100.0),
            raw_accuracy: 0.95,
            final_accuracy: 1.0,
            consistency: Some(0.8),
            corrections: 2,
            session_offset: 0.0,
            adjusted_ratio: reference_wpm.map(|_| 1.0),
            reference_wpm,
            recent: RecentSeries::default(),
            probes: ProbeMetrics::default(),
            contaminated_probes: ProbeMetrics::default(),
        }
    }

    fn target(pattern: &str, role: TargetRole) -> SelectedTarget {
        SelectedTarget {
            pattern: pattern.into(),
            role,
            weakness_mean: 0.5,
            weakness_sd: 0.2,
            priority: 0.1,
            planned_dose: 6,
        }
    }

    fn next_with(targets: Vec<SelectedTarget>) -> ComposedPrompt {
        let mut next = ComposedPrompt::probes(Prompt::new(["cat"]));
        next.targets = targets;
        next
    }

    #[test]
    fn a_completed_session_reports_speed_both_accuracies_and_consistency() {
        // 6 final characters ("cat dg") over 0.6 s is 120 wpm; "dg" for
        // "dog" is one omission in six first-attempt characters and leaves
        // 4 of 6 target characters correct; every clean interval is 100 ms.
        assert_eq!(
            run("cat dog", "cat dg "),
            "120 wpm  83.3% raw  66.7% final  100% consistency"
        );
    }

    #[test]
    fn consistency_is_shown_as_undefined_without_two_clean_intervals() {
        // Only the `x` interval is clean: what follows is the correction and
        // the keystrokes after it. 3 final characters over 0.4 s is 90 wpm.
        assert_eq!(
            run("cat", "cx⌫at"),
            "90 wpm  66.7% raw  100.0% final  -- consistency"
        );
    }

    #[test]
    fn the_second_line_shows_the_speed_on_standard_text_against_the_recent_series() {
        let state = typed("cat dog", "cat dog");
        let metrics = analyze(&state).metrics;
        let first = summary(Some(118.4));
        let mut ending = ending(&state, &metrics);
        ending.summary = Some(&first);
        assert_eq!(
            results(&ending).lines().nth(1).unwrap(),
            "118 wpm on standard text  baseline recorded"
        );
        let previous = RecentSeries {
            wpm: Some(90.0),
            adjusted_ratio: Some(1.0),
            reference_wpm: Some(110.0),
        };
        ending.previous = Some(&previous);
        assert_eq!(
            results(&ending).lines().nth(1).unwrap(),
            "118 wpm on standard text  +8 vs recent"
        );
        let slower = summary(Some(104.6));
        ending.summary = Some(&slower);
        assert_eq!(
            results(&ending).lines().nth(1).unwrap(),
            "105 wpm on standard text  -5 vs recent"
        );
        let unmeasured = summary(None);
        ending.summary = Some(&unmeasured);
        assert_eq!(
            results(&ending).lines().nth(1).unwrap(),
            "-- wpm on standard text"
        );
        ending.summary = None;
        assert_eq!(results(&ending).lines().count(), 1);
    }

    #[test]
    fn an_interrupted_session_reports_words_completed_and_no_speed() {
        assert_eq!(run("cat dog fox", "cat do⎋"), "interrupted after 1 word");
        assert_eq!(run("cat dog fox", "cat dog ⎋"), "interrupted after 2 words");
        assert_eq!(run("cat dog fox", "⎋"), "interrupted after 0 words");
    }

    #[test]
    fn an_interrupted_session_says_whether_its_observations_were_saved() {
        let state = typed("cat dog fox", "cat do⎋");
        let metrics = analyze(&state).metrics;
        let mut ending = ending(&state, &metrics);
        ending.observations_saved = Some(true);
        assert_eq!(
            results(&ending),
            "interrupted after 1 word  observations saved"
        );
        ending.observations_saved = Some(false);
        assert_eq!(
            results(&ending),
            "interrupted after 1 word  observations not saved: too few clean intervals"
        );
    }

    #[test]
    fn the_next_line_lists_the_targets_in_rank_order_and_the_exploration_target() {
        let state = typed("cat dog fox", "cat do⎋");
        let metrics = analyze(&state).metrics;
        let mut ending = ending(&state, &metrics);
        let none = next_with(vec![]);
        ending.next = Some(&none);
        assert_eq!(results(&ending), "interrupted after 1 word\nnext: none yet");
        let next = next_with(vec![
            target(" th", TargetRole::Target),
            target("ing", TargetRole::Target),
            target("he", TargetRole::Deferred),
            target("e ", TargetRole::Target),
            target("ou", TargetRole::Explore),
        ]);
        ending.next = Some(&next);
        assert_eq!(
            results(&ending).lines().nth(1).unwrap(),
            "next: ␣th, ing, e␣ (exploring ou)"
        );
        let only_explore = next_with(vec![target("ou", TargetRole::Explore)]);
        ending.next = Some(&only_explore);
        assert_eq!(
            results(&ending).lines().nth(1).unwrap(),
            "next: exploring ou"
        );
    }

    fn probe_words(count: usize, pace: f64, contaminated: bool) -> Vec<WordPerformance> {
        (0..count)
            .map(|index| WordPerformance {
                index,
                role: WordRole::Probe,
                contaminated,
                submitted: true,
                seconds: pace * 5.0,
                characters: 5,
                target_characters: 4,
                raw_accuracy: Some(1.0),
                initiation_micros: Some(400_000),
            })
            .collect()
    }

    #[test]
    fn the_probe_section_shows_the_window_the_marker_and_the_contaminated_probes() {
        assert_eq!(probe_section(&[]), "");
        assert_eq!(
            probe_section(&[probe_words(3, 0.1, false)]),
            "\nprobes\n  last   3 uncontaminated  120 wpm  100.0% raw\n"
        );

        // Sessions most recent first: a full window at 0.1 s a character
        // with two contaminated probes among it, then a full older window
        // at 0.2, then more that fall outside both.
        let mut recent = probe_words(50, 0.1, false);
        recent.extend(probe_words(2, 0.15, true));
        let sessions = vec![
            recent,
            probe_words(50, 0.1, false),
            probe_words(100, 0.2, false),
            probe_words(20, 0.4, false),
        ];
        assert_eq!(
            probe_section(&sessions),
            "\nprobes\n\
             \x20 last 100 uncontaminated  120 wpm  100.0% raw  sustained improvement from 60 wpm\n\
             \x20   2 contaminated among them   80 wpm  100.0% raw\n"
        );
    }

    #[test]
    fn the_word_initiation_line_lists_medians_oldest_first() {
        assert_eq!(word_initiation_line(&[]), "");
        let mut slow = probe_words(2, 0.1, false);
        slow[0].initiation_micros = Some(900_000);
        slow[1].initiation_micros = Some(700_000);
        let sessions = vec![probe_words(3, 0.1, false), slow];
        assert_eq!(
            word_initiation_line(&sessions),
            "\nword initiation (median ms, oldest first)\n  800  400\n"
        );
    }

    #[test]
    fn the_summary_diff_names_every_figure_that_differs() {
        let stored = summary(Some(100.0));
        let mut current = stored.clone();
        assert_eq!(
            summary_diff(Some(&current), Some(&stored)),
            "no differences between the stored summary and the current pipeline\n"
        );
        current.reference_wpm = Some(101.5);
        current.consistency = None;
        assert_eq!(
            summary_diff(Some(&current), Some(&stored)),
            "differences from the stored summary\n\
             \x20 consistency                      stored     0.8000  current         --\n\
             \x20 reference_wpm                    stored   100.0000  current   101.5000\n"
        );
        assert_eq!(
            summary_diff(None, None),
            "no summary: the session was interrupted\n"
        );
        assert_eq!(
            summary_diff(Some(&current), None),
            "no stored summary: the session has not been applied\n"
        );
    }

    #[test]
    fn the_pattern_summary_is_empty_until_something_has_been_observed() {
        let config = SchedulerConfig::default();
        assert_eq!(
            pattern_summary(
                &ModelState::new(),
                Corpus::bundled(),
                &config,
                &TrainingHistory::new()
            ),
            ""
        );
    }

    #[test]
    fn the_pattern_summary_lists_the_slowest_and_most_error_prone_patterns_with_evidence() {
        let config = SchedulerConfig::default();
        let mut model = ModelState::new();
        let session = |script: &str| {
            let mut state =
                SessionState::new(Prompt::new(["cat", "dog"]), EndCondition::AfterWords(2));
            let mut at = 0;
            for c in script.chars() {
                state.apply_event(Input::new(at, Key::Char(c)));
                at += if c == 'a' { 600_000 } else { 100_000 };
            }
            state
        };
        // In one clean session `t` after `a` is typed slowly (600 ms against
        // 100 ms elsewhere), so `cat` tops slowness; in another `x` is typed
        // for `o`, so `dog` tops errors. The second session's accuracy is
        // too low for its latencies to count, so the slowness is untouched.
        model.apply_session(&session("cat dog"), 1_000, Corpus::bundled(), &config);
        model.apply_session(&session("cat dxg "), 1_000, Corpus::bundled(), &config);

        let mut history = TrainingHistory::new();
        history.record(
            &[TrainingEvent {
                target: target("og", TargetRole::Deferred),
                achieved_dose: 0,
            }],
            [],
            &config,
        );
        let summary = pattern_summary(&model, Corpus::bundled(), &config, &history);
        let lines: Vec<&str> = summary.lines().collect();
        assert_eq!(lines[0], "");
        assert_eq!(lines[1], "slowest patterns");
        // `t` after `at` is six times the baseline, shrunk toward its parents
        // at every level: ln 6 / 11 for `t`, then (ln 6 + 10 × parent) / 11
        // for `at` and `cat`, which is a +56% slowdown.
        assert_eq!(lines[2], "  cat   +56%  n_eff   1.0", "{summary}");
        assert!(lines[3].starts_with("  at    +36%"), "{summary}");
        let errors = lines
            .iter()
            .position(|l| *l == "most error-prone patterns")
            .unwrap();
        assert_eq!(lines[errors - 1], "");
        assert!(lines[errors + 1].starts_with("  ␣do  "), "{summary}");
        // The weakest eligible patterns the user has typed, as mean ± sd:
        // the error on `o` after `d` makes ` do` and `do` the weakest.
        let weakest = lines.iter().position(|l| *l == "weakest patterns").unwrap();
        assert_eq!(lines[weakest - 1], "");
        assert!(lines[weakest + 1].starts_with("  ␣do  +"), "{summary}");
        assert!(lines[weakest + 1].contains(" ± "), "{summary}");
        assert!(lines[weakest + 2].starts_with("  do   +"), "{summary}");
        // Deferred candidates with their windows.
        let deferred = lines
            .iter()
            .position(|l| *l == "deferred candidates")
            .unwrap();
        assert_eq!(lines[deferred - 1], "");
        assert_eq!(lines[deferred + 1], "  og   2 sessions remaining");
        assert_eq!(lines.len(), deferred + 2);
        assert!(!summary.contains("\n   "), "{summary}");
    }
}
