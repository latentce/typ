//! The schedulers a run can compose its prompts with: the one `typ` ships
//! and two baselines it is measured against.

use typ_rs_core::compose::{self, ComposedPrompt};
use typ_rs_core::corpus::Corpus;
use typ_rs_core::model::{ModelState, SchedulerConfig};
use typ_rs_core::random::Rng;
use typ_rs_core::scheduler::{
    SelectedTarget, TargetRole, TrainingHistory, eligible_patterns, same_chain,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheduler {
    /// Every word drawn from the reference distribution; no targeting at
    /// all. What practicing without a scheduler looks like.
    Random,
    /// The patterns with the highest training value by their posterior
    /// mean, and nothing else: no sampling, no deferral, no exploration, no
    /// plateau. The greedy scheduler Thompson sampling is meant to improve
    /// on.
    Weakest,
    /// The scheduler `typ` ships: Thompson sampling over the weakness
    /// posteriors, with deferred controls, an exploration target, and
    /// plateau back-off.
    Phase1,
}

impl Scheduler {
    pub const ALL: [Scheduler; 3] = [Scheduler::Random, Scheduler::Weakest, Scheduler::Phase1];

    pub fn name(self) -> &'static str {
        match self {
            Scheduler::Random => "random",
            Scheduler::Weakest => "weakest",
            Scheduler::Phase1 => "phase1",
        }
    }

    pub fn from_name(name: &str) -> Option<Scheduler> {
        Scheduler::ALL.into_iter().find(|s| s.name() == name)
    }

    /// Composes the next prompt from the bundled corpus as this scheduler
    /// would, from the model and history as they stand at `at`. The
    /// targeted share follows the same ramp for every scheduler that
    /// targets.
    pub fn compose(
        self,
        model: &ModelState,
        config: &SchedulerConfig,
        history: &TrainingHistory,
        at: i64,
        word_count: usize,
        seed: u64,
    ) -> ComposedPrompt {
        let corpus = Corpus::bundled();
        match self {
            Scheduler::Random => compose::frequency_weighted(corpus, word_count, seed),
            Scheduler::Phase1 => {
                compose::next_prompt(model, corpus, config, history, at, word_count, seed)
            }
            Scheduler::Weakest => {
                let mut rng = Rng::seeded(seed);
                let completed = model.context_model().completed_sessions;
                let share = compose::targeted_share(completed, config);
                let targeted_count = ((share * word_count as f64).round() as usize).min(word_count);
                let targets = if targeted_count == 0 {
                    Vec::new()
                } else {
                    weakest_targets(model, corpus, config, at)
                };
                compose::compose(
                    corpus,
                    config,
                    history,
                    targets,
                    targeted_count,
                    word_count,
                    &mut rng,
                )
            }
        }
    }
}

/// The `max_targets` eligible patterns with the highest training value by
/// posterior mean, one per back-off chain.
fn weakest_targets(
    model: &ModelState,
    corpus: &Corpus,
    config: &SchedulerConfig,
    at: i64,
) -> Vec<SelectedTarget> {
    let mut ranked: Vec<SelectedTarget> = eligible_patterns(corpus, config)
        .into_iter()
        .map(|e| {
            let weakness = model.weakness(&e.pattern, at, config);
            let priority = model.training_value(&e.pattern, e.importance, at, config);
            SelectedTarget {
                pattern: e.pattern,
                role: TargetRole::Target,
                weakness_mean: weakness.mean,
                weakness_sd: weakness.sd,
                priority: priority.max(0.0),
                planned_dose: config.dose,
            }
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.priority
            .total_cmp(&a.priority)
            .then_with(|| a.pattern.cmp(&b.pattern))
    });
    let mut targets: Vec<SelectedTarget> = Vec::new();
    for r in ranked {
        if targets.len() == config.max_targets || r.priority <= 0.0 {
            break;
        }
        if targets.iter().any(|t| same_chain(&t.pattern, &r.pattern)) {
            continue;
        }
        targets.push(r);
    }
    targets
}
