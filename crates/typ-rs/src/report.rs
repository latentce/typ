//! The plain-text output of `typ`: the results line, the session listing,
//! the pattern summary, and the replay report.

use std::fmt::Write;

use typ_rs_core::analysis::{
    Interval, IntervalClass, SessionAnalysis, SessionMetrics, WordAnalysis, analyze,
};
use typ_rs_core::model::{ModelState, PatternEstimate, ROOT, SchedulerConfig};
use typ_rs_core::session::{EventKind, Outcome, SessionState};
use typ_rs_store::StoredSession;

/// How many patterns each list of the pattern summary shows.
const LISTED_PATTERNS: usize = 10;

/// The line printed when a session ends. Speed and accuracy are reported
/// only for a completed session: a partial prompt has no meaningful WPM.
pub fn results(state: &SessionState, metrics: &SessionMetrics) -> String {
    match state.outcome() {
        Some(Outcome::Completed) => {
            let wpm = metrics.gross_wpm.unwrap_or(0.0);
            let raw = 100.0 * metrics.raw_accuracy;
            let final_ = 100.0 * metrics.final_accuracy;
            let consistency = match metrics.consistency {
                Some(c) => format!("{:.0}%", 100.0 * c),
                None => "--".to_string(),
            };
            format!("{wpm:.0} wpm  {raw:.1}% raw  {final_:.1}% final  {consistency} consistency")
        }
        _ => {
            let words = state.words_completed();
            format!("interrupted after {words} {}", plural(words, "word"))
        }
    }
}

/// One line per completed session, most recent first, with the same figures
/// the session printed when it ended.
pub fn session_listing(sessions: &[StoredSession]) -> String {
    if sessions.is_empty() {
        return "no completed sessions yet\n".to_string();
    }
    sessions
        .iter()
        .map(|session| {
            let metrics = analyze(&session.replay()).metrics;
            let wpm = metrics.gross_wpm.unwrap_or(0.0);
            let accuracy = 100.0 * metrics.final_accuracy;
            format!(
                "{:>4}  {}  {:>3} words  {wpm:>3.0} wpm  {accuracy:>5.1}% accuracy\n",
                session.id,
                session.started_at_local,
                session.prompt.word_count()
            )
        })
        .collect()
}

/// The patterns the model believes the user is slowest on and most
/// error-prone on: the ten with the highest absolute slowness among those
/// with latency evidence, and the ten with the highest error probability
/// among those with first-attempt trials, each with its effective sample
/// size. Empty when nothing has been observed. Estimates are as of the last
/// session applied.
pub fn pattern_summary(model: &ModelState, config: &SchedulerConfig) -> String {
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
/// the developer's figures, every word's first attempt and attributed
/// errors, and every interval's classification. Words never reached are
/// left out.
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
    let _ = writeln!(out, "{}", results(state, m));
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

fn word_line(index: usize, word: &WordAnalysis) -> String {
    let mut line = format!(
        "{index:>3} {}  first attempt {:?}",
        word.target, word.first_attempt
    );
    let history: String = word.attempt_history.iter().collect();
    if history != word.first_attempt {
        let _ = write!(line, "  history {history:?}");
    }
    if !word.submitted {
        line.push_str("  (not submitted)");
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
    use typ_rs_core::prompt::Prompt;
    use typ_rs_core::session::{EndCondition, Input, Key};

    /// `⎋` is an interrupt, `⌫` a backspace; one keystroke every 100 ms.
    fn run(prompt: &str, script: &str) -> String {
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
        results(&state, &analyze(&state).metrics)
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
    fn an_interrupted_session_reports_words_completed_and_no_speed() {
        assert_eq!(run("cat dog fox", "cat do⎋"), "interrupted after 1 word");
        assert_eq!(run("cat dog fox", "cat dog ⎋"), "interrupted after 2 words");
        assert_eq!(run("cat dog fox", "⎋"), "interrupted after 0 words");
    }

    #[test]
    fn the_pattern_summary_is_empty_until_something_has_been_observed() {
        let config = SchedulerConfig::default();
        assert_eq!(pattern_summary(&ModelState::new(), &config), "");
    }

    #[test]
    fn the_pattern_summary_lists_the_slowest_and_most_error_prone_patterns_with_evidence() {
        let config = SchedulerConfig::default();
        let mut model = ModelState::new();
        // `t` after `a` is typed slowly (600 ms against 100 ms elsewhere) and
        // `x` is typed for `o`, so `cat` tops slowness and `dog` errors.
        let mut state = SessionState::new(Prompt::new(["cat", "dog"]), EndCondition::AfterWords(2));
        let mut at = 0;
        for c in "cat dxg ".chars() {
            state.apply_event(Input::new(at, Key::Char(c)));
            at += if c == 'a' { 600_000 } else { 100_000 };
        }
        model.apply_session(&state, 1_000, &config);

        let summary = pattern_summary(&model, &config);
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
        assert!(!summary.contains("\n   "), "{summary}");
    }
}
