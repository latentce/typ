//! The plain-text output of `typ`: the results block, the progress view,
//! the session listing, the probe, word-initiation, and transfer views,
//! the pattern summary, and the replay report.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use comfy_table::presets::UTF8_FULL;
use comfy_table::{CellAlignment, ColumnConstraint, ContentArrangement, Table, Width};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use rgb::RGB8;
use textplots::{Chart, ColorPlot, LabelBuilder, LabelFormat, Plot, Shape};
use typ_rs_core::analysis::{
    Interval, IntervalClass, SessionAnalysis, SessionMetrics, WordAnalysis, analyze,
};
use typ_rs_core::compose::ComposedPrompt;
use typ_rs_core::corpus::Corpus;
use typ_rs_core::metrics::{
    PatternTransfer, ProbeMetrics, ProbeTrend, RecentSeries, SessionSummary, SlotAggregate,
    Sustained, WordPerformance, accumulate_transfer, probe_trend, word_initiation_median_micros,
    word_performances,
};
use typ_rs_core::model::{
    ModelState, PatternEstimate, ROOT, SchedulerConfig, Weakness, WeaknessComponents,
};
use typ_rs_core::scheduler::{TargetRole, TrainingHistory, eligible_patterns};
use typ_rs_core::session::{EventKind, Outcome, SessionState};
use typ_rs_store::{SessionId, StoredSession};

/// How many patterns each list of the pattern summary shows.
const LISTED_PATTERNS: usize = 10;

/// How many probe words the rolling probe window holds.
const PROBE_WINDOW: usize = 100;

/// How many sessions the progress view tables.
const TABLED_SESSIONS: usize = 10;

/// How many sessions apart the two speeds the headline compares are.
const SPEED_SPAN: usize = 10;

/// How many sessions each of the two accuracies the headline compares
/// averages over, and the accuracy chart's trend at each session.
const ACCURACY_SPAN: usize = 5;

/// How many patterns the focus block names.
const FOCUS_PATTERNS: usize = 5;

/// How many completed sessions the trend charts need; with fewer, the view
/// says how many more to complete where they would go.
const CHARTED_SESSIONS: usize = 2;

/// How many text rows a trend chart's body takes.
const CHART_ROWS: usize = 8;

/// The columns right of a chart's body for a space and a label of up to
/// three digits.
const Y_LABEL_MARGIN: usize = 4;

/// The fewest columns a chart body is drawn in, whatever the width: the
/// chart library draws nothing narrower.
const MIN_CHART_COLUMNS: usize = 17;

/// The least a speed axis spans, in WPM, and the multiple its ends are
/// rounded to.
const SPEED_SPAN_MIN: f64 = 20.0;
const SPEED_STEP: f64 = 10.0;

/// The least an accuracy axis spans, in percentage points, and the
/// multiple its bottom is rounded to; its top is always 100.
const ACCURACY_SPAN_MIN: f64 = 10.0;
const ACCURACY_STEP: f64 = 5.0;

/// The trend line's color: a mid-brightness green that reads on dark and
/// light backgrounds alike.
const TREND_GREEN: RGB8 = RGB8 {
    r: 46,
    g: 160,
    b: 67,
};

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
/// are none), then the patterns the next prompt will practice. Speed and
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

/// Gross WPM, raw and final accuracy, and consistency. The accuracies are
/// labeled as the progress view labels them, so that the two places the
/// user sees them agree.
fn figures_line(metrics: &SessionMetrics) -> String {
    let wpm = metrics.gross_wpm.unwrap_or(0.0);
    let raw = 100.0 * metrics.raw_accuracy;
    let final_ = 100.0 * metrics.final_accuracy;
    let consistency = percent_or_dashes(metrics.consistency);
    format!(
        "{wpm:.0} wpm  {raw:.1}% accuracy  {final_:.1}% after corrections  {consistency} consistency"
    )
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

/// As [`or_dashes`], right-aligned in a column `width` wide, so that a
/// missing figure takes the room of a present one.
fn or_dashes_in_column<T>(value: Option<T>, width: usize, format: impl Fn(T) -> String) -> String {
    let figure = value.map_or_else(|| "--".to_string(), format);
    format!("{figure:>width$}")
}

fn percent_or_dashes(value: Option<f64>) -> String {
    or_dashes(value, |v| format!("{:.0}%", 100.0 * v))
}

/// A share as a percentage with one decimal, six columns wide.
fn percent_in_column(value: Option<f64>) -> String {
    or_dashes_in_column(value, 6, |v| format!("{:.1}%", 100.0 * v))
}

/// A speed as whole WPM, three columns wide.
fn wpm_in_column(value: Option<f64>) -> String {
    or_dashes_in_column(value, 3, |v| format!("{v:.0}"))
}

/// `next:` followed by the patterns the next prompt practices, or `none
/// yet` when nothing was selected for it.
fn next_line(next: &ComposedPrompt) -> String {
    match practiced(next) {
        Some(patterns) => format!("next: {patterns}"),
        None => "next: none yet".to_string(),
    }
}

/// The patterns a prompt practices: its targets in rank order, then its
/// exploration target in parentheses; `None` when nothing was selected.
fn practiced(next: &ComposedPrompt) -> Option<String> {
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
        (true, None) => None,
        (true, Some(e)) => Some(format!("exploring {e}")),
        (false, None) => Some(targets.join(", ")),
        (false, Some(e)) => Some(format!("{} (exploring {e})", targets.join(", "))),
    }
}

/// The completed sessions most recent first under a header naming the
/// columns, one line per session: gross WPM, the speed on standard text,
/// raw accuracy, and consistency, from the session's cached summary. A
/// session without one (not yet applied) is analyzed on the spot and shows
/// no speed on standard text.
pub fn session_listing(sessions: &[StoredSession]) -> String {
    if sessions.is_empty() {
        return "no completed sessions yet\n".to_string();
    }
    let mut out = listing_header();
    for session in sessions {
        let listed = ListedSession::from_session(session, || analyze(&session.replay()).metrics);
        out.push_str(&listing_row(&listed));
    }
    out
}

/// One session's figures as the listing and the progress view show them.
#[cfg_attr(test, derive(Clone))]
struct ListedSession<'a> {
    id: SessionId,
    /// The local start time, `YYYY-MM-DD HH:MM`.
    when: &'a str,
    words: usize,
    wpm: Option<f64>,
    reference: Option<f64>,
    /// The recent-series speed on standard text with the session included.
    recent_reference: Option<f64>,
    raw_accuracy: f64,
    final_accuracy: f64,
    consistency: Option<f64>,
}

impl<'a> ListedSession<'a> {
    /// The session's figures from its cached summary, or from `metrics`
    /// when it has none: then its speed on standard text and its recent
    /// series are unknown.
    fn from_session(
        session: &'a StoredSession,
        metrics: impl FnOnce() -> SessionMetrics,
    ) -> ListedSession<'a> {
        let id = session.id;
        let when = session.started_at_local.as_str();
        let words = session.prompt.word_count();
        match &session.summary {
            Some(s) => ListedSession {
                id,
                when,
                words,
                wpm: s.gross_wpm,
                reference: s.reference_wpm,
                recent_reference: s.recent.reference_wpm,
                raw_accuracy: s.raw_accuracy,
                final_accuracy: s.final_accuracy,
                consistency: s.consistency,
            },
            None => {
                let m = metrics();
                ListedSession {
                    id,
                    when,
                    words,
                    wpm: m.gross_wpm,
                    reference: None,
                    recent_reference: None,
                    raw_accuracy: m.raw_accuracy,
                    final_accuracy: m.final_accuracy,
                    consistency: m.consistency,
                }
            }
        }
    }
}

/// The listing's column labels, each right-aligned over the word that
/// names the same figure in the rows beneath.
fn listing_header() -> String {
    format!(
        "{:>4}  {:<16}  {:>9}  {:>7}  {:>20}  {:>10}  {:>16}\n",
        "id", "when", "words", "wpm", "on standard text", "raw", "consistency"
    )
}

fn listing_row(session: &ListedSession) -> String {
    format!(
        "{:>4}  {}  {:>3} words  {:>3.0} wpm  {} on standard text  {:>5.1}% raw  {:>4} consistency\n",
        session.id,
        session.when,
        session.words,
        session.wpm.unwrap_or(0.0),
        wpm_in_column(session.reference),
        100.0 * session.raw_accuracy,
        percent_or_dashes(session.consistency),
    )
}

/// Each session replayed and analyzed, in the order given.
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
    let trend = trend(performances);
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
    if let Some(marker) = sustained_marker(&trend) {
        let _ = write!(out, "  {marker}");
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

/// The probe trend over the sessions' words, most recent first.
fn trend(performances: &[Vec<WordPerformance>]) -> ProbeTrend {
    probe_trend(
        performances.iter().flat_map(|words| words.iter().rev()),
        PROBE_WINDOW,
    )
}

/// `sustained improvement from N wpm` or `sustained decline from N wpm`
/// when the evidence supports one.
fn sustained_marker(trend: &ProbeTrend) -> Option<String> {
    let (sustained, previous) = trend.sustained.zip(trend.previous)?;
    Some(format!(
        "sustained {} from {}",
        match sustained {
            Sustained::Improvement => "improvement",
            Sustained::Decline => "decline",
        },
        or_dashes(previous.wpm, |w| format!("{w:.0} wpm"))
    ))
}

fn probe_figures(metrics: &ProbeMetrics) -> String {
    format!(
        "{} wpm  {} raw",
        wpm_in_column(metrics.wpm),
        percent_in_column(metrics.raw_accuracy)
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

/// For every pattern practiced in the last `contamination_sessions`
/// sessions: its median clean latency and raw accuracy in the words used
/// for targeted practice over that span against those in words that were
/// not, over the sessions given. Empty without a practiced pattern.
pub fn transfer_section(
    sessions: &[StoredSession],
    analyses: &[SessionAnalysis],
    history: &TrainingHistory,
    config: &SchedulerConfig,
) -> String {
    let span = config.contamination_sessions;
    let patterns: BTreeSet<&str> = history.recently_practiced(span).collect();
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
        "{} ms {} raw  n {:>3}",
        or_dashes_in_column(slots.median_latency_micros(), 4, |l| (l / 1000).to_string()),
        percent_in_column(slots.raw_accuracy()),
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

/// How the progress view is drawn: how many columns it may take and
/// whether it may use color.
#[derive(Debug, Clone, Copy)]
pub struct Rendering {
    pub width: u16,
    pub color: bool,
}

/// What the progress view is made from.
pub struct Progress<'a> {
    /// The completed sessions loaded, most recent first: the ten the table
    /// shows and enough more for the probe windows.
    pub sessions: &'a [StoredSession],
    /// Each session's analysis, in the same order.
    pub analyses: &'a [SessionAnalysis],
    /// How each word of each session was typed, in the same order.
    pub performances: &'a [Vec<WordPerformance>],
    pub model: &'a ModelState,
    pub corpus: &'a Corpus,
    pub config: &'a SchedulerConfig,
    /// The prompt waiting for the next session, if one is.
    pub waiting: Option<&'a ComposedPrompt>,
}

/// The user's progress: a headline saying how speed on standard text and
/// accuracy have moved, a chart of each over the recent sessions (the
/// speed chart only once two sessions have a speed on standard text), a
/// table of the most recent sessions, and the patterns the trainer is
/// focusing on. `no completed sessions yet` and nothing else before the
/// first completed session; after one, a line saying how many more the
/// trend charts need in place of the charts.
pub fn progress(view: &Progress, rendering: Rendering) -> String {
    if view.sessions.is_empty() {
        return "no completed sessions yet\n".to_string();
    }
    let figures: Vec<ListedSession> = view
        .sessions
        .iter()
        .zip(view.analyses)
        .map(|(session, analysis)| {
            ListedSession::from_session(session, || analysis.metrics.clone())
        })
        .collect();

    let mut out = headline(&figures, &trend(view.performances), rendering.color);
    if figures.len() < CHARTED_SESSIONS {
        let more = CHARTED_SESSIONS - figures.len();
        let _ = write!(
            out,
            "\ncomplete {more} more {} to see your trend\n",
            plural(more, "session")
        );
    } else {
        let charts = [
            speed_chart(&figures, rendering),
            accuracy_chart(&figures, rendering),
        ];
        for chart in charts.into_iter().flatten() {
            out.push('\n');
            out.push_str(&chart);
        }
    }
    out.push('\n');
    out.push_str(&session_table(
        &figures[..figures.len().min(TABLED_SESSIONS)],
        rendering.width,
    ));
    let focus = focus(
        view.model,
        view.corpus,
        view.config,
        view.waiting,
        rendering.width,
    );
    if !focus.is_empty() {
        out.push('\n');
        out.push_str(&focus);
    }
    out
}

/// Three lines over the sessions given, most recent first. The
/// recent-series speed on standard text, with its change since the
/// session ten back, or the oldest when there are fewer; both are taken
/// among the sessions that have a cached recent series, so a session
/// stored without a summary is passed over rather than shown as `--`.
/// The mean raw accuracy of the last five sessions, with its change
/// against the mean of the sessions before them, up to five, when there
/// are any. The probe trend: the sustained marker, or why there is none.
fn headline(figures: &[ListedSession], trend: &ProbeTrend, color: bool) -> String {
    let recent: Vec<f64> = figures.iter().filter_map(|f| f.recent_reference).collect();
    let mut out = match recent.first() {
        None => "-- wpm on standard text".to_string(),
        Some(current) => {
            let mut line = format!("{current:.0} wpm on standard text");
            if recent.len() > 1 {
                let earlier = recent[SPEED_SPAN.min(recent.len() - 1)];
                let _ = write!(line, "  {}", delta(current - earlier, 0, color));
            }
            line
        }
    };

    let latest = mean_accuracy(&figures[..figures.len().min(ACCURACY_SPAN)]);
    let _ = write!(out, "\n{latest:.1}% accuracy");
    if figures.len() > ACCURACY_SPAN {
        let before = &figures[ACCURACY_SPAN..figures.len().min(2 * ACCURACY_SPAN)];
        let _ = write!(out, "  {}", delta(latest - mean_accuracy(before), 1, color));
    }

    let probes = match sustained_marker(trend) {
        Some(marker) => marker,
        None if trend.previous.is_some() => "no sustained change on probes yet".to_string(),
        None => "not enough probes yet to call a trend".to_string(),
    };
    let _ = write!(out, "\n{probes}\n");
    out
}

/// A change as an arrow and the signed difference to `decimals` places:
/// `▲ +6` in green, `▼ -2` in red, or `= 0` in the terminal's own color
/// when the change rounds to nothing.
fn delta(change: f64, decimals: usize, color: bool) -> String {
    let signed = format!("{change:+.decimals$}");
    let magnitude = signed.trim_start_matches(['+', '-']);
    if magnitude.chars().all(|c| c == '0' || c == '.') {
        return format!("= {magnitude}");
    }
    let (arrow, foreground) = if change > 0.0 {
        ("▲", Color::Green)
    } else {
        ("▼", Color::Red)
    };
    if color {
        format!(
            "{}{arrow} {signed}{ResetColor}",
            SetForegroundColor(foreground)
        )
    } else {
        format!("{arrow} {signed}")
    }
}

/// One session as a trend chart plots it: its local start date, its own
/// figure, and the trend's figure at it.
struct Plotted<'a> {
    date: &'a str,
    point: f64,
    trend: f64,
}

/// The speed chart over the sessions given, most recent first: each
/// session's speed on standard text as a point and its recent series as
/// the line through them. A session without a speed on standard text is
/// left out of both and of the count, rather than drawn at zero. `None`
/// with fewer than two sessions to draw.
fn speed_chart(figures: &[ListedSession], rendering: Rendering) -> Option<String> {
    let plotted: Vec<Plotted> = figures
        .iter()
        .rev()
        .filter_map(|f| {
            Some(Plotted {
                date: date(f.when),
                point: f.reference?,
                trend: f.recent_reference?,
            })
        })
        .collect();
    trend_chart("speed on standard text", &plotted, speed_range, rendering)
}

/// The accuracy chart over the sessions given, most recent first: each
/// session's raw accuracy as a point and, as the line, the mean over the
/// five sessions up to and including it. Every session has an accuracy,
/// so every one is drawn. `None` with fewer than two sessions.
fn accuracy_chart(figures: &[ListedSession], rendering: Rendering) -> Option<String> {
    let count = figures.len();
    let plotted: Vec<Plotted> = figures
        .iter()
        .rev()
        .enumerate()
        .map(|(i, f)| {
            let newest = count - 1 - i;
            let window = &figures[newest..(newest + ACCURACY_SPAN).min(count)];
            Plotted {
                date: date(f.when),
                point: 100.0 * f.raw_accuracy,
                trend: mean_accuracy(window),
            }
        })
        .collect();
    trend_chart("accuracy", &plotted, accuracy_range, rendering)
}

/// The mean raw accuracy of the sessions, in percent.
fn mean_accuracy(sessions: &[ListedSession]) -> f64 {
    100.0 * sessions.iter().map(|f| f.raw_accuracy).sum::<f64>() / sessions.len() as f64
}

/// The date of a local start time, `YYYY-MM-DD HH:MM`.
fn date(when: &str) -> &str {
    when.split(' ').next().unwrap_or(when)
}

/// A titled, footed line chart of the sessions given, oldest first, as
/// many of the most recent as fit the width: `subject · N sessions` over
/// the body, each session's figure as a point and the trend as a line
/// through the same positions, on the y axis `range` gives for the values
/// drawn, then the first and last drawn sessions' dates at the two ends of
/// the width. `None` with fewer than two sessions, since one point is not
/// a trend.
fn trend_chart(
    subject: &str,
    plotted: &[Plotted],
    range: impl Fn(&[f64]) -> (f64, f64),
    rendering: Rendering,
) -> Option<String> {
    let fitting = points_that_fit(rendering.width);
    let plotted = &plotted[plotted.len().saturating_sub(fitting)..];
    if plotted.len() < 2 {
        return None;
    }
    let values: Vec<f64> = plotted.iter().flat_map(|p| [p.point, p.trend]).collect();
    let (bottom, top) = range(&values);
    let at = |get: fn(&Plotted) -> f64| -> Vec<(f32, f32)> {
        plotted
            .iter()
            .enumerate()
            .map(|(i, p)| (i as f32, get(p) as f32))
            .collect()
    };
    let points = at(|p| p.point);
    let line = at(|p| p.trend);

    let mut out = format!(
        "{subject} · {} {}\n",
        plotted.len(),
        plural(plotted.len(), "session")
    );
    out.push_str(&braille_chart(&points, &line, bottom, top, rendering));
    let (first, last) = (plotted[0].date, plotted[plotted.len() - 1].date);
    let width = chart_columns(rendering.width) + Y_LABEL_MARGIN;
    let _ = writeln!(
        out,
        "{first}{last:>rest$}",
        rest = width
            .saturating_sub(first.chars().count())
            .max(last.chars().count() + 1)
    );
    Some(out)
}

/// The columns a chart body takes at the width given: what is left after
/// the label margin, and never fewer than the library draws.
fn chart_columns(width: u16) -> usize {
    (width as usize)
        .saturating_sub(Y_LABEL_MARGIN)
        .max(MIN_CHART_COLUMNS)
}

/// How many sessions a chart at the width given can tell apart: one per
/// column of its body, each column being two braille dots wide.
fn points_that_fit(width: u16) -> usize {
    chart_columns(width)
}

/// The y axis for speeds: the data rounded out to multiples of ten, then
/// widened by ten at each end until it spans at least twenty, so that its
/// labels stay round and a few points of noise are not drawn as a cliff.
/// Never below zero.
fn speed_range(values: &[f64]) -> (f64, f64) {
    let (min, max) = extent(values);
    let mut bottom = (min / SPEED_STEP).floor() * SPEED_STEP;
    let mut top = (max / SPEED_STEP).ceil() * SPEED_STEP;
    while top - bottom < SPEED_SPAN_MIN {
        bottom -= SPEED_STEP;
        top += SPEED_STEP;
    }
    if bottom < 0.0 {
        top -= bottom;
        bottom = 0.0;
    }
    (bottom, top)
}

/// The y axis for accuracies, in percent: 100 at the top, the data minimum
/// rounded down to a multiple of five at the bottom, and never narrower
/// than ten points.
fn accuracy_range(values: &[f64]) -> (f64, f64) {
    let (min, _) = extent(values);
    let bottom = ((min / ACCURACY_STEP).floor() * ACCURACY_STEP).min(100.0 - ACCURACY_SPAN_MIN);
    (bottom, 100.0)
}

/// The smallest and largest of the values.
fn extent(values: &[f64]) -> (f64, f64) {
    values
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), &v| {
            (min.min(v), max.max(v))
        })
}

/// The body of a trend chart: `points` as dots and `line` as a line, both
/// at x positions `0..n`, on a braille canvas [`CHART_ROWS`] rows tall and
/// as wide as the width allows, with `bottom` and `top` as whole-number
/// labels at the right of its last and first rows. The line is drawn first
/// and in green when color is on, the points after it in the terminal's own
/// color, so that a cell both fall in shows the point. The library colors
/// through `colored`, which honors `NO_COLOR` on its own as well. The one
/// place the chart library is used; its x labels are left off in favor of
/// the footer.
fn braille_chart(
    points: &[(f32, f32)],
    line: &[(f32, f32)],
    bottom: f64,
    top: f64,
    rendering: Rendering,
) -> String {
    // The canvas draws its right-hand column and top row of dots as a
    // column and row of their own, so it is one column and one row larger
    // than its size in dots suggests.
    let dots_wide = 2 * (chart_columns(rendering.width) - 1) as u32;
    let dots_high = 4 * (CHART_ROWS - 1) as u32;
    let xmax = (points.len().max(2) - 1) as f32;
    let dots = Shape::Points(points);
    let trend = Shape::Lines(line);
    let mut chart =
        Chart::new_with_y_range(dots_wide, dots_high, 0.0, xmax, bottom as f32, top as f32);
    let styled = chart
        .x_label_format(LabelFormat::None)
        .y_label_format(LabelFormat::Custom(Box::new(|v| format!("{v:.0}"))));
    let with_trend = if rendering.color {
        styled.linecolorplot(&trend, TREND_GREEN)
    } else {
        styled.lineplot(&trend)
    };
    let drawn = with_trend.lineplot(&dots);
    drawn.figures();
    let text = drawn.to_string();
    // The library ends with a row for x labels, blank here.
    let mut rows: Vec<&str> = text.lines().collect();
    rows.pop();
    let mut body = rows.join("\n");
    body.push('\n');
    body
}

/// A column of a bordered table.
struct Column {
    name: &'static str,
    /// Right-aligned, as numbers are.
    numeric: bool,
}

const SESSION_COLUMNS: &[Column] = &[
    Column {
        name: "#",
        numeric: true,
    },
    Column {
        name: "when",
        numeric: false,
    },
    Column {
        name: "words",
        numeric: true,
    },
    Column {
        name: "wpm",
        numeric: true,
    },
    Column {
        name: "on standard text",
        numeric: true,
    },
    Column {
        name: "accuracy",
        numeric: true,
    },
    Column {
        name: "after corrections",
        numeric: true,
    },
];

const FOCUS_COLUMNS: &[Column] = &[
    Column {
        name: "pattern",
        numeric: false,
    },
    Column {
        name: "why",
        numeric: false,
    },
];

/// The sessions as a bordered table in the order given: id, local start
/// time, words, gross WPM, speed on standard text, raw accuracy, and final
/// accuracy, with `--` where a figure is unknown.
fn session_table(figures: &[ListedSession], width: u16) -> String {
    let rows: Vec<Vec<String>> = figures
        .iter()
        .map(|f| {
            vec![
                f.id.to_string(),
                f.when.to_string(),
                f.words.to_string(),
                or_dashes(f.wpm, |v| format!("{v:.0}")),
                or_dashes(f.reference, |v| format!("{v:.0}")),
                format!("{:.1}%", 100.0 * f.raw_accuracy),
                format!("{:.1}%", 100.0 * f.final_accuracy),
            ]
        })
        .collect();
    bordered_table(SESSION_COLUMNS, &rows, width)
}

/// The patterns the trainer is working on: the five eligible patterns the
/// user has typed with the highest weakness, each tagged with why, then
/// the patterns the waiting prompt practices. Empty when the model has
/// observed nothing.
fn focus(
    model: &ModelState,
    corpus: &Corpus,
    config: &SchedulerConfig,
    waiting: Option<&ComposedPrompt>,
    width: u16,
) -> String {
    let Some(at) = model.last_update() else {
        return String::new();
    };
    let eligible = eligible_patterns(corpus, config);
    let mut weakest: Vec<(&str, WeaknessComponents)> = eligible
        .iter()
        .filter(|e| model.stats(&e.pattern).is_some())
        .map(|e| {
            (
                e.pattern.as_ref(),
                model.weakness_components(&e.pattern, at, config),
            )
        })
        .collect();
    weakest.sort_by(|a, b| {
        let mean = |(_, components): &(&str, WeaknessComponents)| components.weakness.mean;
        mean(b).total_cmp(&mean(a)).then_with(|| a.0.cmp(b.0))
    });
    weakest.truncate(FOCUS_PATTERNS);

    let mut out = String::from("focus\n");
    if !weakest.is_empty() {
        let rows: Vec<Vec<String>> = weakest
            .iter()
            .map(|(pattern, components)| vec![visible(pattern), why(components).to_string()])
            .collect();
        out.push_str(&bordered_table(FOCUS_COLUMNS, &rows, width));
    }
    if let Some(patterns) = waiting.and_then(practiced) {
        let _ = writeln!(out, "next session practices: {patterns}");
    }
    out
}

/// Why a pattern is weak: `both` when the user is more error-prone and
/// slower on it than on their typing as a whole, `error-prone` or `slow`
/// when only one holds, and `slow` when neither does, since the other
/// components of weakness (inconsistency and hesitation) are about speed.
fn why(components: &WeaknessComponents) -> &'static str {
    match (components.error_excess > 0.0, components.speed_excess > 0.0) {
        (true, true) => "both",
        (true, false) => "error-prone",
        (false, _) => "slow",
    }
}

/// `rows` under `columns` in a bordered table no wider than `width`,
/// numeric columns right-aligned. A column is never narrower than its
/// widest cell, so that a tight width wraps headers but never figures.
/// The one place the table library is used.
fn bordered_table(columns: &[Column], rows: &[Vec<String>], width: u16) -> String {
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_width(width)
        .set_header(columns.iter().map(|c| c.name));
    for row in rows {
        table.add_row(row.iter().map(String::as_str));
    }
    for (index, column) in columns.iter().enumerate() {
        let Some(drawn) = table.column_mut(index) else {
            continue;
        };
        let widest = rows
            .iter()
            .map(|row| row[index].chars().count())
            .max()
            .unwrap_or(0);
        let padding = drawn.padding_width();
        drawn.set_constraint(ColumnConstraint::LowerBoundary(Width::Fixed(
            widest as u16 + padding,
        )));
        if column.numeric {
            drawn.set_cell_alignment(CellAlignment::Right);
        }
    }
    format!("{table}\n")
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
            "120 wpm  83.3% accuracy  66.7% after corrections  100% consistency"
        );
    }

    #[test]
    fn consistency_is_shown_as_undefined_without_two_clean_intervals() {
        // Only the `x` interval is clean: what follows is the correction and
        // the keystrokes after it. 3 final characters over 0.4 s is 90 wpm.
        assert_eq!(
            run("cat", "cx⌫at"),
            "90 wpm  66.7% accuracy  100.0% after corrections  -- consistency"
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
    fn a_missing_probe_figure_takes_the_room_of_a_present_one() {
        let full = ProbeMetrics {
            words: 4,
            wpm: Some(150.0),
            raw_accuracy: Some(0.9167),
        };
        let empty = ProbeMetrics::default();
        assert_eq!(probe_figures(&full), "150 wpm   91.7% raw");
        assert_eq!(probe_figures(&empty), " -- wpm      -- raw");
        assert_eq!(probe_figures(&full).len(), probe_figures(&empty).len());
    }

    #[test]
    fn a_missing_slot_figure_takes_the_room_of_a_present_one() {
        let full = SlotAggregate {
            slots: 2,
            errors: 0.0,
            clean_latencies_micros: vec![100_000, 100_000],
        };
        let empty = SlotAggregate::default();
        assert_eq!(slot_figures(&full), " 100 ms 100.0% raw  n   2");
        assert_eq!(slot_figures(&empty), "  -- ms     -- raw  n   0");
        assert_eq!(slot_figures(&full).len(), slot_figures(&empty).len());
    }

    fn listed(reference: Option<f64>, consistency: Option<f64>) -> ListedSession<'static> {
        ListedSession {
            id: "2".parse().unwrap(),
            when: "2024-01-16 08:00",
            words: 2,
            wpm: Some(140.0),
            reference,
            recent_reference: reference,
            raw_accuracy: 1.0,
            final_accuracy: 1.0,
            consistency,
        }
    }

    #[test]
    fn the_listing_header_sits_over_the_columns_of_a_row() {
        let header = listing_header();
        let row = listing_row(&listed(Some(120.0), Some(1.0)));
        assert_eq!(
            row,
            "   2  2024-01-16 08:00    2 words  140 wpm  120 on standard text  100.0% raw  100% consistency\n"
        );
        assert_eq!(header.len(), row.len());
        // `id` ends where the id does; `when` starts where the date does;
        // every other label stands over the same word in the row.
        assert_eq!(header.find("id").unwrap() + 2, row.find('2').unwrap() + 1);
        assert_eq!(header.find("when"), row.find("2024"));
        for label in ["words", "wpm", "on standard text", "raw", "consistency"] {
            assert_eq!(
                header.find(label),
                row.find(label),
                "{label}\n{header}{row}"
            );
        }
    }

    #[test]
    fn a_listing_row_with_missing_figures_is_as_wide_as_one_without() {
        let full = listing_row(&listed(Some(120.0), Some(1.0)));
        let sparse = listing_row(&listed(None, None));
        assert_eq!(
            sparse,
            "   2  2024-01-16 08:00    2 words  140 wpm   -- on standard text  100.0% raw    -- consistency\n"
        );
        assert_eq!(full.len(), sparse.len());
    }

    // --- The progress view -------------------------------------------------

    #[test]
    fn a_change_is_an_arrow_and_the_signed_difference_judged_after_rounding() {
        assert_eq!(delta(6.4, 0, false), "▲ +6");
        assert_eq!(delta(-2.0, 0, false), "▼ -2");
        assert_eq!(delta(0.3, 0, false), "= 0");
        assert_eq!(delta(-0.3, 0, false), "= 0");
        assert_eq!(delta(1.26, 1, false), "▲ +1.3");
        assert_eq!(delta(-0.04, 1, false), "= 0.0");
    }

    #[test]
    fn a_change_is_green_up_red_down_and_plain_at_zero_when_color_is_on() {
        crossterm::style::Colored::set_ansi_color_disabled(false);
        let up = delta(6.0, 0, true);
        assert!(up.starts_with("\x1b[38;5;10m▲ +6"), "{up:?}");
        assert!(up.ends_with("\x1b[0m"), "{up:?}");
        let down = delta(-2.0, 0, true);
        assert!(down.starts_with("\x1b[38;5;9m▼ -2"), "{down:?}");
        assert!(down.ends_with("\x1b[0m"), "{down:?}");
        assert_eq!(delta(0.0, 0, true), "= 0");
    }

    /// A session's figures for the headline: its recent-series speed on
    /// standard text and its raw accuracy.
    fn figures(recent_reference: Option<f64>, raw_accuracy: f64) -> ListedSession<'static> {
        ListedSession {
            recent_reference,
            raw_accuracy,
            ..listed(Some(120.0), Some(1.0))
        }
    }

    fn no_trend() -> ProbeTrend {
        ProbeTrend {
            current: ProbeMetrics::default(),
            contaminated: ProbeMetrics::default(),
            previous: None,
            sustained: None,
        }
    }

    #[test]
    fn the_headline_shows_speed_and_accuracy_without_changes_for_one_session() {
        assert_eq!(
            headline(&[figures(Some(118.4), 0.964)], &no_trend(), false),
            "118 wpm on standard text\n\
             96.4% accuracy\n\
             not enough probes yet to call a trend\n"
        );
        assert_eq!(
            headline(&[figures(None, 1.0)], &no_trend(), false)
                .lines()
                .next()
                .unwrap(),
            "-- wpm on standard text"
        );
    }

    #[test]
    fn the_headline_compares_speed_with_the_oldest_session_until_there_are_eleven() {
        // Most recent first: the speed is compared with the oldest session's
        // recent series, skipping a session that has none.
        let three = [
            figures(Some(124.0), 1.0),
            figures(None, 1.0),
            figures(Some(118.0), 1.0),
        ];
        assert_eq!(
            headline(&three, &no_trend(), false).lines().next().unwrap(),
            "124 wpm on standard text  ▲ +6"
        );
        // From eleven on, with the session ten back.
        let mut twelve: Vec<ListedSession> = (0..12)
            .map(|i| figures(Some(100.0 + i as f64), 1.0))
            .collect();
        assert_eq!(
            headline(&twelve, &no_trend(), false)
                .lines()
                .next()
                .unwrap(),
            "100 wpm on standard text  ▼ -10"
        );
        twelve[10].recent_reference = Some(100.0);
        assert_eq!(
            headline(&twelve, &no_trend(), false)
                .lines()
                .next()
                .unwrap(),
            "100 wpm on standard text  = 0"
        );
    }

    #[test]
    fn the_headline_compares_the_accuracy_of_the_last_five_sessions_with_the_five_before() {
        // Five sessions: nothing to compare with.
        let five: Vec<ListedSession> = (0..5).map(|_| figures(Some(120.0), 0.95)).collect();
        assert_eq!(
            headline(&five, &no_trend(), false).lines().nth(1).unwrap(),
            "95.0% accuracy"
        );
        // Seven: the last five against the two before them.
        let mut seven = five.clone();
        seven.push(figures(Some(120.0), 0.90));
        seven.push(figures(Some(120.0), 0.92));
        assert_eq!(
            headline(&seven, &no_trend(), false).lines().nth(1).unwrap(),
            "95.0% accuracy  ▲ +4.0"
        );
        // Twelve: the sessions beyond the tenth are not compared.
        let mut twelve = seven.clone();
        twelve.extend((0..3).map(|_| figures(Some(120.0), 0.99)));
        twelve.extend((0..2).map(|_| figures(Some(120.0), 0.10)));
        assert_eq!(
            headline(&twelve, &no_trend(), false)
                .lines()
                .nth(1)
                .unwrap(),
            "95.0% accuracy  ▼ -0.8"
        );
    }

    #[test]
    fn the_headline_names_the_probe_trend_when_there_is_one() {
        let window = ProbeMetrics {
            words: 100,
            wpm: Some(120.0),
            raw_accuracy: Some(1.0),
        };
        let previous = ProbeMetrics {
            wpm: Some(60.0),
            ..window
        };
        let improving = ProbeTrend {
            current: window,
            contaminated: ProbeMetrics::default(),
            previous: Some(previous),
            sustained: Some(Sustained::Improvement),
        };
        let one = [figures(Some(120.0), 1.0)];
        assert_eq!(
            headline(&one, &improving, false).lines().nth(2).unwrap(),
            "sustained improvement from 60 wpm"
        );
        let declining = ProbeTrend {
            sustained: Some(Sustained::Decline),
            ..improving
        };
        assert_eq!(
            headline(&one, &declining, false).lines().nth(2).unwrap(),
            "sustained decline from 60 wpm"
        );
        let steady = ProbeTrend {
            sustained: None,
            ..improving
        };
        assert_eq!(
            headline(&one, &steady, false).lines().nth(2).unwrap(),
            "no sustained change on probes yet"
        );
    }

    #[test]
    fn the_session_table_has_a_header_row_and_right_aligned_figures_and_placeholders() {
        let full = ListedSession {
            final_accuracy: 0.9876,
            ..listed(Some(120.0), Some(1.0))
        };
        let sparse = ListedSession {
            id: "13".parse().unwrap(),
            wpm: None,
            raw_accuracy: 0.8333,
            ..listed(None, None)
        };
        assert_eq!(
            session_table(&[full, sparse], 100),
            "┌────┬──────────────────┬───────┬─────┬──────────────────┬──────────┬───────────────────┐\n\
             │  # ┆ when             ┆ words ┆ wpm ┆ on standard text ┆ accuracy ┆ after corrections │\n\
             ╞════╪══════════════════╪═══════╪═════╪══════════════════╪══════════╪═══════════════════╡\n\
             │  2 ┆ 2024-01-16 08:00 ┆     2 ┆ 140 ┆              120 ┆   100.0% ┆             98.8% │\n\
             ├╌╌╌╌┼╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌┼╌╌╌╌╌┼╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┤\n\
             │ 13 ┆ 2024-01-16 08:00 ┆     2 ┆  -- ┆               -- ┆    83.3% ┆            100.0% │\n\
             └────┴──────────────────┴───────┴─────┴──────────────────┴──────────┴───────────────────┘\n"
        );
    }

    #[test]
    fn the_focus_tag_follows_the_error_and_speed_excess() {
        let components = |error_excess: f64, speed_excess: f64| WeaknessComponents {
            error_excess,
            speed_excess,
            inconsistency: 0.5,
            hesitation_excess: 0.5,
            weakness: Weakness { mean: 1.0, sd: 0.1 },
        };
        assert_eq!(why(&components(0.2, 0.1)), "both");
        assert_eq!(why(&components(0.2, 0.0)), "error-prone");
        assert_eq!(why(&components(0.0, 0.1)), "slow");
        assert_eq!(why(&components(-0.1, -0.1)), "slow");
    }

    #[test]
    fn the_practiced_patterns_are_the_targets_in_rank_order_then_the_exploration_target() {
        assert_eq!(practiced(&next_with(vec![])), None);
        assert_eq!(
            practiced(&next_with(vec![target("ou", TargetRole::Explore)])).as_deref(),
            Some("exploring ou")
        );
        assert_eq!(
            practiced(&next_with(vec![
                target(" th", TargetRole::Target),
                target("he", TargetRole::Deferred),
                target("e ", TargetRole::Target),
            ]))
            .as_deref(),
            Some("␣th, e␣")
        );
        assert_eq!(
            practiced(&next_with(vec![
                target(" th", TargetRole::Target),
                target("ou", TargetRole::Explore),
            ]))
            .as_deref(),
            Some("␣th (exploring ou)")
        );
    }

    #[test]
    fn the_focus_block_is_empty_until_something_has_been_observed() {
        let config = SchedulerConfig::default();
        let waiting = next_with(vec![target("th", TargetRole::Target)]);
        assert_eq!(
            focus(
                &ModelState::new(),
                Corpus::bundled(),
                &config,
                Some(&waiting),
                80
            ),
            ""
        );
    }

    #[test]
    fn the_focus_block_tags_the_weakest_patterns_and_names_the_next_practice() {
        let config = SchedulerConfig::default();
        let mut model = ModelState::new();
        // `x` typed for `o` in "dog" makes the chain ending at `o` the
        // weakest, for its errors; nothing was slow.
        let mut state = SessionState::new(Prompt::new(["cat", "dog"]), EndCondition::AfterWords(2));
        for (i, c) in "cat dxg ".chars().enumerate() {
            state.apply_event(Input::new(i as u64 * 100_000, Key::Char(c)));
        }
        model.apply_session(&state, 1_000, Corpus::bundled(), &config);

        let waiting = next_with(vec![
            target(" do", TargetRole::Target),
            target("do", TargetRole::Target),
            target("ou", TargetRole::Explore),
        ]);
        let block = focus(&model, Corpus::bundled(), &config, Some(&waiting), 80);
        let lines: Vec<&str> = block.lines().collect();
        assert_eq!(lines[0], "focus", "{block}");
        assert_eq!(lines[2], "│ pattern ┆ why         │", "{block}");
        assert_eq!(lines[4], "│ ␣do     ┆ error-prone │", "{block}");
        assert_eq!(
            lines.last().unwrap(),
            &"next session practices: ␣do, do (exploring ou)",
            "{block}"
        );
        // Five patterns, each on a row of its own between rules.
        assert_eq!(lines.len(), 2 + 2 + 5 * 2 + 1, "{block}");

        let without_waiting = focus(&model, Corpus::bundled(), &config, None, 80);
        assert!(
            !without_waiting.contains("next session"),
            "{without_waiting}"
        );
        assert!(without_waiting.ends_with("┘\n"), "{without_waiting}");
    }

    // --- The trend charts --------------------------------------------------

    #[test]
    fn the_speed_axis_is_rounded_to_tens_and_spans_at_least_twenty() {
        assert_eq!(speed_range(&[103.0, 128.0]), (100.0, 130.0));
        // Already round: nothing moves.
        assert_eq!(speed_range(&[100.0, 130.0]), (100.0, 130.0));
        // Within one ten: widened by ten at each end, so the labels stay
        // multiples of ten.
        assert_eq!(speed_range(&[112.0, 118.0]), (100.0, 130.0));
        assert_eq!(speed_range(&[120.0, 120.0]), (110.0, 130.0));
        // Never below zero: the widened axis is shifted up instead.
        assert_eq!(speed_range(&[5.0, 5.0]), (0.0, 30.0));
    }

    #[test]
    fn the_accuracy_axis_tops_at_a_hundred_and_spans_at_least_ten() {
        assert_eq!(accuracy_range(&[83.3, 100.0]), (80.0, 100.0));
        assert_eq!(accuracy_range(&[96.0, 99.0]), (90.0, 100.0));
        assert_eq!(accuracy_range(&[100.0, 100.0]), (90.0, 100.0));
        assert_eq!(accuracy_range(&[90.0, 95.0]), (90.0, 100.0));
        assert_eq!(accuracy_range(&[62.5]), (60.0, 100.0));
    }

    #[test]
    fn one_session_fits_per_column_after_the_label_margin() {
        assert_eq!(points_that_fit(80), 76);
        assert_eq!(points_that_fit(40), 36);
        // The chart is never drawn narrower than the library allows.
        assert_eq!(points_that_fit(10), MIN_CHART_COLUMNS);
    }

    /// A session's figures for the charts: when it started, its speed on
    /// standard text and the recent series with it, and its raw accuracy.
    fn charted<'a>(
        when: &'a str,
        reference: Option<f64>,
        recent_reference: Option<f64>,
        raw_accuracy: f64,
    ) -> ListedSession<'a> {
        ListedSession {
            when,
            reference,
            recent_reference,
            raw_accuracy,
            ..listed(None, None)
        }
    }

    fn plain(width: u16) -> Rendering {
        Rendering {
            width,
            color: false,
        }
    }

    /// The whole-number label at the right end of a chart row.
    fn label(row: &str) -> u32 {
        row.rsplit_once(' ')
            .unwrap_or_else(|| panic!("{row:?}"))
            .1
            .parse()
            .unwrap_or_else(|_| panic!("{row:?}"))
    }

    fn has_braille(rows: &[&str]) -> bool {
        rows.iter()
            .flat_map(|row| row.chars())
            .any(|c| ('\u{2801}'..='\u{28ff}').contains(&c))
    }

    #[test]
    fn the_speed_chart_titles_labels_and_foots_a_two_point_series() {
        // Most recent first, as the view holds them.
        let figures = [
            charted("2024-01-17 08:00", Some(128.0), Some(124.0), 1.0),
            charted("2024-01-15 10:30", Some(103.0), Some(103.0), 1.0),
        ];
        let chart = speed_chart(&figures, plain(80)).unwrap();
        let rows: Vec<&str> = chart.lines().collect();
        assert_eq!(rows[0], "speed on standard text · 2 sessions");
        // Eight rows of chart between the title and the footer, labeled at
        // the top and bottom.
        assert_eq!(rows.len(), 1 + CHART_ROWS + 1, "{chart}");
        assert_eq!(label(rows[1]), 130, "{chart}");
        assert_eq!(label(rows[CHART_ROWS]), 100, "{chart}");
        assert!(has_braille(&rows[1..=CHART_ROWS]), "{chart}");
        // The footer spans the chart's width with a date at each end; no
        // row carries a numeric x label.
        let footer = rows[CHART_ROWS + 1];
        assert!(footer.starts_with("2024-01-15"), "{footer:?}");
        assert!(footer.ends_with("2024-01-17"), "{footer:?}");
        assert_eq!(footer.chars().count(), 80, "{footer:?}");
        assert!(!chart.contains("0.0"), "{chart}");
        assert!(!chart.contains('\x1b'), "{chart}");
    }

    #[test]
    fn the_speed_chart_leaves_out_sessions_without_a_speed_on_standard_text() {
        let figures = [
            charted("2024-01-17 08:00", Some(128.0), Some(124.0), 1.0),
            charted("2024-01-16 08:00", None, None, 1.0),
            charted("2024-01-15 10:30", Some(103.0), Some(103.0), 1.0),
        ];
        let chart = speed_chart(&figures, plain(80)).unwrap();
        assert!(
            chart.starts_with("speed on standard text · 2 sessions\n"),
            "{chart}"
        );
        // One point is not a trend: the chart is left out.
        assert_eq!(speed_chart(&figures[..2], plain(80)), None);
    }

    #[test]
    fn the_accuracy_chart_counts_every_session_and_tops_at_a_hundred() {
        let figures = [
            charted("2024-01-17 08:00", Some(128.0), Some(124.0), 1.0),
            charted("2024-01-16 08:00", None, None, 0.833),
            charted("2024-01-15 10:30", Some(103.0), Some(103.0), 1.0),
        ];
        let chart = accuracy_chart(&figures, plain(80)).unwrap();
        let rows: Vec<&str> = chart.lines().collect();
        assert_eq!(rows[0], "accuracy · 3 sessions");
        assert_eq!(label(rows[1]), 100, "{chart}");
        assert_eq!(label(rows[CHART_ROWS]), 80, "{chart}");
        assert!(has_braille(&rows[1..=CHART_ROWS]), "{chart}");
        let footer = rows[CHART_ROWS + 1];
        assert!(footer.starts_with("2024-01-15"), "{footer:?}");
        assert!(footer.ends_with("2024-01-17"), "{footer:?}");
    }

    #[test]
    fn a_chart_keeps_the_most_recent_sessions_that_fit() {
        // Sixty sessions, oldest first: twenty-eight a month from January.
        let dates: Vec<String> = (0..60)
            .map(|i| format!("2024-{:02}-{:02} 08:00", 1 + i / 28, 1 + i % 28))
            .collect();
        let figures: Vec<ListedSession> = dates
            .iter()
            .rev()
            .map(|d| charted(d.as_str(), Some(120.0), Some(120.0), 1.0))
            .collect();
        // At 40 columns, 36 sessions fit: the 36 most recent, so the footer
        // runs from the 25th session to the last.
        let chart = speed_chart(&figures, plain(40)).unwrap();
        let rows: Vec<&str> = chart.lines().collect();
        assert_eq!(rows[0], "speed on standard text · 36 sessions");
        let footer = rows[CHART_ROWS + 1];
        assert!(footer.starts_with("2024-01-25"), "{footer:?}");
        assert!(footer.ends_with("2024-03-04"), "{footer:?}");
        assert_eq!(footer.chars().count(), 40, "{footer:?}");
    }

    #[test]
    fn the_progress_view_says_so_without_a_completed_session() {
        let view = Progress {
            sessions: &[],
            analyses: &[],
            performances: &[],
            model: &ModelState::new(),
            corpus: Corpus::bundled(),
            config: &SchedulerConfig::default(),
            waiting: None,
        };
        let rendering = Rendering {
            width: 80,
            color: false,
        };
        assert_eq!(progress(&view, rendering), "no completed sessions yet\n");
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
