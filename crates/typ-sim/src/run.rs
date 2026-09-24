//! One simulated run: a learner typing a series of sessions, each composed
//! by the chosen scheduler and applied through the same steps as a
//! session typed in the terminal.

use std::collections::BTreeMap;
use std::time::Instant;

use typ_rs_core::analysis::SessionAnalysis;
use typ_rs_core::compose::ComposedPrompt;
use typ_rs_core::corpus::{Corpus, ReferenceDistribution};
use typ_rs_core::metrics::{RecentSeries, SessionSummary, summarize};
use typ_rs_core::model::{AppliedObservations, ModelState, SchedulerConfig};
use typ_rs_core::scheduler::{
    TargetRole, TrainingEvent, TrainingHistory, achieved_doses, eligible_patterns,
};

use crate::learner::{Kind, Learner, Loss};
use crate::schedule::Scheduler;

/// What a run is made of.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub learner: Kind,
    pub scheduler: Scheduler,
    pub sessions: usize,
    pub words: usize,
    pub seed: u64,
    pub config: SchedulerConfig,
}

/// One session of a run and what came of it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    /// From one.
    pub ordinal: usize,
    pub composed: ComposedPrompt,
    pub analysis: SessionAnalysis,
    /// What the session's observations were measured against; `None` when
    /// they were not applied.
    pub applied: Option<AppliedObservations>,
    /// What came of every pattern selected for the prompt.
    pub events: Vec<TrainingEvent>,
    /// The deferred candidates whose window opened with this session, as
    /// opposed to those still inside one.
    pub newly_deferred: Vec<Box<str>>,
    pub summary: Option<SessionSummary>,
    /// The weakness mean of every eligible pattern as the model stood when
    /// the prompt was composed; empty for the first session, composed
    /// before the model had seen anything.
    pub weakness_at_composition: BTreeMap<Box<str>, f64>,
    /// How long the end-of-session steps took: applying the session,
    /// counting doses, recording the history, composing the next prompt,
    /// and summarizing.
    pub pipeline_micros: u64,
    /// The learner's true reference loss once the session was typed.
    pub loss: Loss,
}

impl SessionRecord {
    /// The targets and exploration target of the session.
    pub fn practiced(&self) -> impl Iterator<Item = &TrainingEvent> {
        self.events.iter().filter(|e| e.target.role.is_practiced())
    }
}

/// A finished run.
#[derive(Debug, Clone)]
pub struct Run {
    pub options: Options,
    /// The learner's reference loss before any session.
    pub initial_loss: Loss,
    pub sessions: Vec<SessionRecord>,
    /// Every practiced pattern whose priority was at some point scaled down
    /// for a plateau, with the ordinal of the session after which that was
    /// first so.
    pub plateaus: BTreeMap<Box<str>, usize>,
    pub model: ModelState,
    /// The learner as it stands after the last session.
    pub learner: Learner,
}

impl Run {
    pub fn final_loss(&self) -> Loss {
        self.sessions.last().map_or(self.initial_loss, |s| s.loss)
    }

    pub fn max_pipeline_micros(&self) -> u64 {
        self.sessions
            .iter()
            .map(|s| s.pipeline_micros)
            .max()
            .unwrap_or(0)
    }

    pub fn mean_pipeline_micros(&self) -> u64 {
        if self.sessions.is_empty() {
            return 0;
        }
        self.sessions.iter().map(|s| s.pipeline_micros).sum::<u64>() / self.sessions.len() as u64
    }

    /// How often each pattern was a target, most often first.
    pub fn target_counts(&self) -> Vec<(&str, usize)> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &self.sessions {
            for e in &s.events {
                if e.target.role == TargetRole::Target {
                    *counts.entry(&e.target.pattern).or_default() += 1;
                }
            }
        }
        let mut counts: Vec<(&str, usize)> = counts.into_iter().collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        counts
    }
}

/// Sessions are a day apart, so the statistics decay as they would for a
/// daily user.
const DAY_SECONDS: i64 = 86_400;
const FIRST_SESSION_AT: i64 = 1_700_000_000;

/// Keeps the learner's draws apart from the compositions'.
const LEARNER_SALT: u64 = 0x6c65_6172_6e65_7200;

/// Runs every session of a run and returns what happened.
pub fn simulate(options: Options) -> Run {
    let corpus = Corpus::bundled();
    let config = options.config;
    let sample = ReferenceDistribution::new(corpus).fixed_sample(corpus);
    let eligible: Vec<Box<str>> = eligible_patterns(corpus, &config)
        .into_iter()
        .map(|e| e.pattern)
        .collect();

    let mut learner = Learner::new(options.learner, options.seed ^ LEARNER_SALT);
    let mut model = ModelState::new();
    let mut history = TrainingHistory::new();
    let mut recent: Option<RecentSeries> = None;
    let mut plateaus: BTreeMap<Box<str>, usize> = BTreeMap::new();
    let initial_loss = learner.reference_loss(&sample);

    let mut composed = options.scheduler.compose(
        &model,
        &config,
        &history,
        FIRST_SESSION_AT - 60,
        options.words,
        session_seed(options.seed, 0),
    );
    let mut weakness_at_composition = BTreeMap::new();
    let mut sessions = Vec::with_capacity(options.sessions);

    for index in 0..options.sessions {
        let started_at = FIRST_SESSION_AT + index as i64 * DAY_SECONDS;
        let state = learner.type_prompt(&composed.prompt);
        let elapsed = state.events().last().map_or(0, |e| e.at_micros) / 1_000_000;
        let ended_at = started_at + elapsed as i64 + 1;

        let newly_deferred: Vec<Box<str>> = composed
            .targets
            .iter()
            .filter(|t| t.role == TargetRole::Deferred && !history.is_deferred(&t.pattern))
            .map(|t| t.pattern.clone())
            .collect();

        let clock = Instant::now();
        let update = model.apply_session(&state, started_at, corpus, &config);
        let events = achieved_doses(&state, &composed.targets);
        history.record(&events, composed.targeted_words(), &config);
        let next = options.scheduler.compose(
            &model,
            &config,
            &history,
            ended_at,
            options.words,
            session_seed(options.seed, index + 1),
        );
        let summary = summarize(&state, &update, &composed.words, recent.as_ref(), &config);
        let pipeline_micros = clock.elapsed().as_micros() as u64;

        if let Some(summary) = &summary {
            recent = Some(summary.recent);
        }
        for pattern in plateaued(&model, &history, ended_at, &config) {
            plateaus.entry(pattern).or_insert(index + 1);
        }
        let next_weakness: BTreeMap<Box<str>, f64> = eligible
            .iter()
            .map(|p| (p.clone(), model.weakness(p, ended_at, &config).mean))
            .collect();

        sessions.push(SessionRecord {
            ordinal: index + 1,
            composed,
            analysis: update.analysis,
            applied: update.applied,
            events,
            newly_deferred,
            summary,
            weakness_at_composition,
            pipeline_micros,
            loss: learner.reference_loss(&sample),
        });
        composed = next;
        weakness_at_composition = next_weakness;
    }

    Run {
        options,
        initial_loss,
        sessions,
        plateaus,
        model,
        learner,
    }
}

/// The recently practiced patterns whose priority the history is scaling
/// down for a plateau as of `at`.
fn plateaued(
    model: &ModelState,
    history: &TrainingHistory,
    at: i64,
    config: &SchedulerConfig,
) -> Vec<Box<str>> {
    history
        .recently_practiced(config.plateau_recovery_sessions.max(1))
        .filter(|p| {
            let w = model.weakness(p, at, config);
            history.plateau_factor(p, w.mean, w.sd, config) < 1.0
        })
        .map(Box::from)
        .collect()
}

/// The seed the prompt of session `index` (from zero) is composed with.
fn session_seed(seed: u64, index: usize) -> u64 {
    seed ^ (index as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}
