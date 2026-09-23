//! Composes the prompt for a session.
//!
//! A prompt is a targeted share of words chosen to expose the session's
//! targets and a probe share drawn from the reference distribution. The
//! targeted share ramps up over the profile's first completed sessions so
//! that early, noisy estimates do not drive the whole prompt: the first
//! session is all probes. The picker is deliberately simple: it cycles
//! through the targets and draws one word containing each, weighted by
//! frequency, never repeating a word.

use std::collections::BTreeSet;

use crate::corpus::{Corpus, ReferenceDistribution, WordId};
use crate::model::{ModelState, SchedulerConfig};
use crate::prompt::Prompt;
use crate::random::Rng;
use crate::scheduler::{self, SelectedTarget, TrainingHistory};

/// Why a word is in the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordRole {
    /// Chosen to expose one of the session's targets.
    Targeted,
    /// Drawn from the reference distribution, independent of the user.
    Probe,
}

impl WordRole {
    /// The role's stable name, as stored. Never renamed.
    pub fn name(self) -> &'static str {
        match self {
            WordRole::Targeted => "targeted",
            WordRole::Probe => "probe",
        }
    }

    /// The role with the given stored name, if this version knows it.
    pub fn from_name(name: &str) -> Option<WordRole> {
        match name {
            "targeted" => Some(WordRole::Targeted),
            "probe" => Some(WordRole::Probe),
            _ => None,
        }
    }
}

/// One word of a composed prompt: its role and the selected patterns it
/// contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedWord {
    pub role: WordRole,
    /// The practised patterns (targets and exploration target) the word's
    /// space-padded text contains; empty for a probe.
    pub exposed_targets: Vec<Box<str>>,
}

/// A prompt together with why each word is in it and what was selected for
/// it.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposedPrompt {
    pub prompt: Prompt,
    /// One entry per word of the prompt.
    pub words: Vec<ComposedWord>,
    /// Every target, deferred candidate, and exploration target recorded
    /// for the prompt.
    pub targets: Vec<SelectedTarget>,
}

impl ComposedPrompt {
    /// A prompt in which every word is a probe and nothing was selected.
    pub fn probes(prompt: Prompt) -> ComposedPrompt {
        let words = vec![
            ComposedWord {
                role: WordRole::Probe,
                exposed_targets: Vec::new(),
            };
            prompt.word_count()
        ];
        ComposedPrompt {
            prompt,
            words,
            targets: Vec::new(),
        }
    }

    /// The patterns the prompt's words were chosen to expose: targets first,
    /// then the exploration target.
    pub fn practised(&self) -> impl Iterator<Item = &SelectedTarget> {
        self.targets.iter().filter(|t| t.role.is_practised())
    }
}

/// Draws `word_count` words from the reference distribution. The same seed
/// composes the same prompt for a given corpus version.
pub fn frequency_weighted(corpus: &Corpus, word_count: usize, seed: u64) -> ComposedPrompt {
    ComposedPrompt::probes(Prompt::new(
        ReferenceDistribution::new(corpus)
            .seeded_sampler(seed)
            .take(word_count)
            .map(|id| corpus.text(id)),
    ))
}

/// The share of a prompt that is targeted after `completed_sessions`
/// completed sessions: zero before the first, the start share after it,
/// rising linearly to the full share over `ramp_sessions` completed
/// sessions and staying there.
pub fn targeted_share(completed_sessions: u32, config: &SchedulerConfig) -> f64 {
    if completed_sessions == 0 {
        return 0.0;
    }
    let steps = config.ramp_sessions.saturating_sub(1);
    if steps == 0 {
        return config.ramp_full_share;
    }
    let progress = (completed_sessions as usize - 1).min(steps) as f64 / steps as f64;
    config.ramp_start_share + (config.ramp_full_share - config.ramp_start_share) * progress
}

/// Composes the next prompt for a profile as of `at`: selects the session's
/// targets from the model, fills the targeted share (rounded to the nearest
/// word) with words containing them, fills the rest with probes, and
/// shuffles. Before the first completed session nothing is selected and
/// every word is a probe. The same inputs and seed compose the same prompt.
pub fn next_prompt(
    model: &ModelState,
    corpus: &Corpus,
    config: &SchedulerConfig,
    history: &TrainingHistory,
    at: i64,
    word_count: usize,
    seed: u64,
) -> ComposedPrompt {
    let mut rng = Rng::seeded(seed);
    let reference = ReferenceDistribution::new(corpus);
    let completed = model.context_model().completed_sessions;
    let targeted_count = (targeted_share(completed, config) * word_count as f64).round() as usize;
    let targeted_count = targeted_count.min(word_count);
    if targeted_count == 0 {
        return ComposedPrompt::probes(Prompt::new(
            (0..word_count).map(|_| corpus.text(reference.sample(&mut rng))),
        ));
    }

    let targets = scheduler::select_targets(model, corpus, config, history, at, &mut rng);
    let practised: Vec<&str> = targets
        .iter()
        .filter(|t| t.role.is_practised())
        .map(|t| t.pattern.as_ref())
        .collect();

    let mut words: Vec<(WordId, WordRole)> = Vec::new();
    let mut used: BTreeSet<WordId> = BTreeSet::new();
    if !practised.is_empty() {
        let mut exhausted = 0;
        let mut turn = 0;
        while words.len() < targeted_count && exhausted < practised.len() {
            let pattern = practised[turn % practised.len()];
            turn += 1;
            let pool: Vec<WordId> = corpus
                .words_containing(pattern)
                .iter()
                .copied()
                .filter(|id| !used.contains(id))
                .collect();
            let drawn = rng.weighted_index(pool.iter().map(|&id| corpus.word(id).frequency_weight));
            match drawn {
                Some(i) => {
                    used.insert(pool[i]);
                    words.push((pool[i], WordRole::Targeted));
                    exhausted = 0;
                }
                None => exhausted += 1,
            }
        }
    }
    while words.len() < word_count {
        words.push((reference.sample(&mut rng), WordRole::Probe));
    }
    rng.shuffle(&mut words);

    let composed = words
        .iter()
        .map(|&(id, role)| {
            let exposed_targets = match role {
                WordRole::Targeted => {
                    let padded = format!(" {} ", corpus.text(id));
                    practised
                        .iter()
                        .filter(|p| padded.contains(**p))
                        .map(|p| Box::from(*p))
                        .collect()
                }
                WordRole::Probe => Vec::new(),
            };
            ComposedWord {
                role,
                exposed_targets,
            }
        })
        .collect();
    ComposedPrompt {
        prompt: Prompt::new(words.iter().map(|&(id, _)| corpus.text(id))),
        words: composed,
        targets,
    }
}
