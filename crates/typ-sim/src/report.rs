//! The text a run is reported as.

use std::fmt::Write;

use typ_rs_core::model::SchedulerConfig;
use typ_rs_core::scheduler::TargetRole;

use crate::measure::{self, Arm, EarlyLate, Estimate, GainCheck, SlotSample, Transfer};
use crate::run::Run;

/// The full report of a run, as printed.
pub fn report(run: &Run) -> String {
    let mut out = String::new();
    let o = &run.options;
    let _ = writeln!(
        out,
        "learner {}  scheduler {}  sessions {}  words {}  seed {}",
        o.learner.name(),
        o.scheduler.name(),
        o.sessions,
        o.words,
        o.seed
    );
    let start = run.initial_loss;
    let end = run.final_loss();
    let _ = writeln!(
        out,
        "reference loss   {:.1} ms/char {:.2}% errors  →  {:.1} ms/char {:.2}% errors",
        start.seconds_per_character * 1000.0,
        start.error_rate * 100.0,
        end.seconds_per_character * 1000.0,
        end.error_rate * 100.0
    );

    if let Some(pattern) = o.learner.weakness() {
        let sessions: Vec<usize> = run
            .sessions
            .iter()
            .filter(|s| {
                s.events.iter().any(|e| {
                    e.target.pattern.as_ref() == pattern && e.target.role == TargetRole::Target
                })
            })
            .map(|s| s.ordinal)
            .collect();
        let _ = writeln!(
            out,
            "weakness         {pattern:?}: {} exposures, {:.0}% remaining, targeted in {} sessions{}",
            run.learner.weakness_exposures().unwrap_or(0),
            run.learner.weakness_remaining().unwrap_or(1.0) * 100.0,
            sessions.len(),
            sessions.first().map_or(String::new(), |first| format!(
                ", first for session {first}"
            ))
        );
    }

    let transfer = measure::transfer(run);
    out.push_str(&transfer_lines(&transfer));

    let doses = measure::doses(run);
    let _ = writeln!(
        out,
        "doses            {} targets, planned {:.1}, achieved {:.1} per target",
        doses.targets,
        doses.mean_planned(),
        doses.mean_achieved()
    );
    let _ = writeln!(
        out,
        "pipeline time    max {:.1} ms, mean {:.1} ms",
        run.max_pipeline_micros() as f64 / 1000.0,
        run.mean_pipeline_micros() as f64 / 1000.0
    );
    if run.plateaus.is_empty() {
        out.push_str("plateaus         none\n");
    } else {
        let listed: Vec<String> = run
            .plateaus
            .iter()
            .map(|(p, s)| format!("{p:?} after session {s}"))
            .collect();
        let _ = writeln!(out, "plateaus         {}", listed.join(", "));
    }

    out.push_str(&gain_lines(&measure::gain_check(run), &o.config));

    let counts = run.target_counts();
    if !counts.is_empty() {
        let listed: Vec<String> = counts
            .iter()
            .take(8)
            .map(|(p, n)| format!("{p:?}×{n}"))
            .collect();
        let _ = writeln!(out, "targets          {}", listed.join(" "));
    }
    out
}

fn transfer_lines(transfer: &Transfer) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "transfer         practiced patterns in untargeted words: {}",
        window_text(&transfer.practiced)
    );
    let _ = writeln!(
        out,
        "                 other slots of the same words:            {}",
        window_text(&transfer.other)
    );
    let _ = writeln!(
        out,
        "                 pattern-specific speed-up {}",
        transfer
            .difference()
            .map_or("n/a".to_string(), |d| format!("{d:+.3}"))
    );
    out
}

fn window_text(window: &EarlyLate) -> String {
    let side = |a: &SlotSample| {
        format!(
            "{} ms {}",
            a.median_latency_micros()
                .map_or("n/a".to_string(), |m| format!("{:.0}", m as f64 / 1000.0)),
            a.error_rate()
                .map_or("n/a".to_string(), |e| format!("{:.1}%", e * 100.0))
        )
    };
    format!(
        "early {} → late {} ({} slots, ln ratio {})",
        side(&window.early),
        side(&window.late),
        window.early.slots + window.late.slots,
        window
            .speed_up()
            .map_or("n/a".to_string(), |s| format!("{s:+.3}"))
    )
}

fn gain_lines(check: &GainCheck, config: &SchedulerConfig) -> String {
    let estimate = |e: &Estimate| format!("{:+.3} (n={})", e.mean, e.count);
    let arm = |a: &Arm| {
        format!(
            "{} selections: mean residual {}, mean error rate {}",
            a.selections,
            a.mean_residual()
                .map_or("n/a".to_string(), |r| format!("{r:+.3}")),
            a.mean_error_rate()
                .map_or("n/a".to_string(), |e| format!("{:.2}%", e * 100.0)),
        )
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "gain check       naive, posterior drop over targets {}",
        estimate(&check.naive)
    );
    let _ = writeln!(
        out,
        "                 drift, posterior drop over all eligible {}",
        estimate(&check.drift)
    );
    let c = &check.corrected;
    let _ = writeln!(
        out,
        "                 corrected, fresh outcomes over {} sessions: {} ± {}",
        c.sessions,
        c.gain(config)
            .map_or("n/a".to_string(), |g| format!("{g:+.3}")),
        c.standard_error(config)
            .map_or("n/a".to_string(), |g| format!("{g:.3}"))
    );
    let _ = writeln!(out, "                   targets   {}", arm(&c.targets));
    let _ = writeln!(out, "                   deferred  {}", arm(&c.deferred));
    out
}
