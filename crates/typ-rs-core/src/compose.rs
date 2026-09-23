//! Composes the prompt for a session.
//!
//! A prompt is a targeted share of words chosen to expose the session's
//! targets and a probe share drawn from the reference distribution. The
//! targeted share ramps up over the profile's first completed sessions so
//! that early, noisy estimates do not drive the whole prompt: the first
//! session is all probes.
//!
//! Targeted words are chosen one at a time from a pool of every word
//! containing a practised pattern plus a frequency-weighted sample of
//! others. Each pick is a softmax draw over a word score that rewards the
//! coverage the word adds (its exposures of targets still short of their
//! dose, saturating per target so every target gets its share), prefers
//! common words, and penalises words drilled recently, words that stack too
//! many targets, and long words. Probes are drawn from the reference
//! distribution with no filtering at all, so that their measurement stays
//! comparable across sessions; how much each overlaps recent practice is
//! recorded instead. The two are shuffled together and exposures of one
//! target are then spread out so the same motion is not drilled in
//! adjacent words.

use std::collections::{BTreeMap, BTreeSet};

use crate::corpus::{Corpus, ReferenceDistribution, WordId};
use crate::model::{ModelState, SchedulerConfig};
use crate::prompt::Prompt;
use crate::random::Rng;
use crate::scheduler::{
    self, SelectedTarget, TargetRole, TrainingHistory, WordChains, exposures_in_word,
};

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

/// A probe's overlap with targeted practice: this prompt's and that of the
/// last `contamination_sessions` sessions. Recorded so that probe results
/// can be read with it in mind, never used to filter probes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Contamination {
    /// The word itself was shown as targeted recently.
    pub recently_targeted_word: bool,
    /// The word's bigrams and trigrams that were targets or exploration
    /// targets recently, in pattern order.
    pub recently_targeted_patterns: Vec<Box<str>>,
}

/// One word of a composed prompt: its role and what was recorded when it
/// was chosen.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposedWord {
    pub role: WordRole,
    /// The practised patterns (targets and exploration target) the word
    /// exposes, in pattern order; empty for a probe, whose overlap is in its
    /// contamination instead.
    pub exposed_targets: Vec<Box<str>>,
    /// The score a targeted word was drawn with; `None` for a probe.
    pub selection_score: Option<f64>,
    /// A probe's overlap with recent practice; `None` for a targeted word,
    /// and for a probe that was never assessed.
    pub contamination: Option<Contamination>,
}

impl ComposedWord {
    /// A probe whose contamination was not assessed.
    fn probe() -> ComposedWord {
        ComposedWord {
            role: WordRole::Probe,
            exposed_targets: Vec::new(),
            selection_score: None,
            contamination: None,
        }
    }
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
        let words = vec![ComposedWord::probe(); prompt.word_count()];
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

    /// The words shown as targeted, in prompt order.
    pub fn targeted_words(&self) -> impl Iterator<Item = &str> {
        self.prompt
            .words()
            .iter()
            .zip(&self.words)
            .filter(|(_, meta)| meta.role == WordRole::Targeted)
            .map(|(word, _)| word.as_ref())
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
/// targets from the model and builds the prompt around them with the
/// targeted share the ramp gives, rounded to the nearest word. Before the
/// first completed session nothing is selected and every word is a probe.
/// The same inputs and seed compose the same prompt.
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
    let completed = model.context_model().completed_sessions;
    let targeted_count = (targeted_share(completed, config) * word_count as f64).round() as usize;
    let targeted_count = targeted_count.min(word_count);
    let targets = if targeted_count == 0 {
        Vec::new()
    } else {
        scheduler::select_targets(model, corpus, config, history, at, &mut rng)
    };
    compose(
        corpus,
        config,
        history,
        targets,
        targeted_count,
        word_count,
        &mut rng,
    )
}

/// Builds a prompt of `word_count` words around already selected targets:
/// `targeted_count` words chosen to expose the practised ones (fewer only
/// if the corpus runs out of distinct words), the rest probes, shuffled
/// together with exposures of one target spread apart. With no practised
/// pattern every word is a probe, in the order drawn. Every draw comes from
/// `rng`.
pub fn compose(
    corpus: &Corpus,
    config: &SchedulerConfig,
    history: &TrainingHistory,
    targets: Vec<SelectedTarget>,
    targeted_count: usize,
    word_count: usize,
    rng: &mut Rng,
) -> ComposedPrompt {
    let reference = ReferenceDistribution::new(corpus);
    let practised: Vec<&SelectedTarget> =
        targets.iter().filter(|t| t.role.is_practised()).collect();
    let mut words = if practised.is_empty() {
        Vec::new()
    } else {
        Selection::new(corpus, config, history, &reference, &practised, rng)
            .pick(targeted_count.min(word_count), rng)
    };
    let targeted_words: BTreeSet<&str> = words.iter().map(|w| corpus.text(w.id)).collect();
    let recent: BTreeSet<&str> = practised.iter().map(|t| t.pattern.as_ref()).collect();
    while words.len() < word_count {
        let id = reference.sample(rng);
        // A probe's exposures count for spacing, though it is not recorded
        // as exposing anything: it was not chosen for them.
        let mut exposed: Vec<Box<str>> = exposures_in_word(
            WordChains::new(corpus.text(id)).iter(),
            &recent,
            &BTreeSet::new(),
        )
        .into_keys()
        .map(Box::from)
        .collect();
        exposed.sort();
        words.push(PlacedWord {
            id,
            role: WordRole::Probe,
            exposed,
            score: None,
        });
    }
    if !targeted_words.is_empty() {
        words = arrange(words, config.min_exposure_gap, rng);
    }

    let composed = words
        .iter()
        .map(|w| {
            let text = corpus.text(w.id);
            let contamination = (w.role == WordRole::Probe).then(|| Contamination {
                recently_targeted_word: targeted_words.contains(text)
                    || history.targeted_word_within(text, config.contamination_sessions),
                recently_targeted_patterns: word_patterns(text)
                    .into_iter()
                    .filter(|p| {
                        recent.contains(p.as_str())
                            || history.practised_within(p, config.contamination_sessions)
                    })
                    .map(Box::from)
                    .collect(),
            });
            ComposedWord {
                role: w.role,
                exposed_targets: match w.role {
                    WordRole::Targeted => w.exposed.clone(),
                    WordRole::Probe => Vec::new(),
                },
                selection_score: w.score,
                contamination,
            }
        })
        .collect();
    ComposedPrompt {
        prompt: Prompt::new(words.iter().map(|w| corpus.text(w.id))),
        words: composed,
        targets,
    }
}

/// A word placed in the prompt being composed.
#[derive(Clone)]
struct PlacedWord {
    id: WordId,
    role: WordRole,
    /// The practised patterns the word exposes, in pattern order, whatever
    /// its role.
    exposed: Vec<Box<str>>,
    score: Option<f64>,
}

impl PlacedWord {
    /// Whether both words expose some one target.
    fn shares_target(&self, other: &PlacedWord) -> bool {
        self.exposed.iter().any(|p| other.exposed.contains(p))
    }
}

/// A word that may be chosen as targeted, with everything about its score
/// that does not change as the prompt fills.
struct CandidateWord {
    id: WordId,
    exposures: Vec<PatternExposures>,
    /// [`FREQUENCY_SHARE`] times the log of the word's frequency weight,
    /// less the recent-word, overload, and length penalties.
    fixed_score: f64,
    taken: bool,
}

/// A candidate word's exposures of one practised pattern.
struct PatternExposures {
    /// Index into the practised list.
    pattern: usize,
    /// How many slots count.
    count: usize,
    /// The coverage these exposures would add to a target that has none
    /// yet: `1 − exp(−count / dose)`.
    fresh_coverage: f64,
}

/// How far one practised pattern's coverage has come as the prompt fills.
struct TargetCoverage {
    weight: f64,
    dose: usize,
    /// Exposures given so far.
    given: usize,
    /// `exp(−given / dose)`: how much of a fresh exposure's coverage the
    /// next one is still worth.
    remaining: f64,
}

/// The state of choosing a prompt's targeted words.
struct Selection<'a> {
    config: &'a SchedulerConfig,
    practised: &'a [&'a SelectedTarget],
    candidates: Vec<CandidateWord>,
    /// One per practised pattern, in the same order.
    coverage: Vec<TargetCoverage>,
    /// The marginal gain that counts as one unit before the log.
    gain_unit: f64,
}

impl<'a> Selection<'a> {
    fn new(
        corpus: &'a Corpus,
        config: &'a SchedulerConfig,
        history: &TrainingHistory,
        reference: &ReferenceDistribution,
        practised: &'a [&'a SelectedTarget],
        rng: &mut Rng,
    ) -> Selection<'a> {
        let mut pool: BTreeSet<WordId> = practised
            .iter()
            .flat_map(|t| corpus.words_containing(&t.pattern))
            .copied()
            .collect();
        for _ in 0..config.pool_sample {
            pool.insert(reference.sample(rng));
        }

        let index: BTreeMap<&str, usize> = practised
            .iter()
            .enumerate()
            .map(|(i, t)| (t.pattern.as_ref(), i))
            .collect();
        let patterns: BTreeSet<&str> = index.keys().copied().collect();
        let coverage: Vec<TargetCoverage> = coverage_weights(practised)
            .into_iter()
            .zip(practised)
            .map(|(weight, t)| TargetCoverage {
                weight,
                dose: t.planned_dose.max(1),
                given: 0,
                remaining: 1.0,
            })
            .collect();
        let candidates = pool
            .into_iter()
            .map(|id| {
                let word = corpus.word(id);
                let exposures: Vec<PatternExposures> = exposures_in_word(
                    WordChains::new(&word.text).iter(),
                    &patterns,
                    &BTreeSet::new(),
                )
                .into_iter()
                .map(|(pattern, count)| {
                    let pattern = index[pattern];
                    PatternExposures {
                        pattern,
                        count,
                        fresh_coverage: coverage_of(count, coverage[pattern].dose),
                    }
                })
                .collect();
                let recent = history.targeted_word_within(&word.text, config.recent_word_sessions);
                let overload = exposures.len().saturating_sub(config.overload_targets);
                let excess_length =
                    usize::from(word.length).saturating_sub(config.long_word_length);
                let fixed_score = FREQUENCY_SHARE * word.frequency_weight.ln()
                    - f64::from(u8::from(recent)) * config.recent_word_penalty
                    - overload as f64 * config.overload_penalty
                    - excess_length as f64 * config.length_penalty;
                CandidateWord {
                    id,
                    exposures,
                    fixed_score,
                    taken: false,
                }
            })
            .collect();

        let top_fresh_gain = coverage
            .iter()
            .map(|c| c.weight * coverage_of(1, c.dose))
            .fold(0.0, f64::max);
        Selection {
            config,
            practised,
            candidates,
            coverage,
            gain_unit: top_fresh_gain / config.coverage_scale,
        }
    }

    /// Chooses up to `count` distinct words, each by a softmax draw over
    /// the current scores, updating the exposures after each.
    fn pick(mut self, count: usize, rng: &mut Rng) -> Vec<PlacedWord> {
        let mut chosen = Vec::with_capacity(count);
        let mut scores = vec![f64::NEG_INFINITY; self.candidates.len()];
        for _ in 0..count {
            for (score, c) in scores.iter_mut().zip(&self.candidates) {
                if !c.taken {
                    *score = c.fixed_score + self.coverage_term(c);
                }
            }
            let best = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let drawn = rng.weighted_index(
                scores
                    .iter()
                    .map(|s| ((s - best) / self.config.temperature).exp()),
            );
            let Some(i) = drawn else {
                break;
            };
            let score = scores[i];
            scores[i] = f64::NEG_INFINITY;
            let candidate = &mut self.candidates[i];
            candidate.taken = true;
            for e in &candidate.exposures {
                let target = &mut self.coverage[e.pattern];
                target.given += e.count;
                target.remaining = 1.0 - coverage_of(target.given, target.dose);
            }
            let mut exposed: Vec<Box<str>> = candidate
                .exposures
                .iter()
                .map(|e| self.practised[e.pattern].pattern.clone())
                .collect();
            exposed.sort();
            chosen.push(PlacedWord {
                id: candidate.id,
                role: WordRole::Targeted,
                exposed,
                score: Some(score),
            });
        }
        chosen
    }

    /// `ln(1 + gain)` for the coverage the candidate would add now, the gain
    /// in units of one fresh exposure of the top target scaled by the
    /// configured coverage scale. A target's next exposures are worth the
    /// fresh coverage they would give times what remains of it, and nothing
    /// once it has its dose: otherwise the hundreds of words containing a
    /// common target keep outweighing the few containing a rare one long
    /// after the common target has had enough. Once every target is dosed
    /// the remaining picks go by frequency and the penalties alone.
    fn coverage_term(&self, candidate: &CandidateWord) -> f64 {
        let gain: f64 = candidate
            .exposures
            .iter()
            .map(|e| {
                let target = &self.coverage[e.pattern];
                let added = if target.given >= target.dose {
                    0.0
                } else if target.given + e.count <= target.dose {
                    target.remaining * e.fresh_coverage
                } else {
                    target.remaining - REMAINING_AT_DOSE
                };
                target.weight * added
            })
            .sum();
        if self.gain_unit > 0.0 {
            (1.0 + gain / self.gain_unit).ln()
        } else {
            0.0
        }
    }
}

/// What remains of a fresh exposure's coverage once a target has its dose:
/// `exp(−dose / dose)`.
const REMAINING_AT_DOSE: f64 = 1.0 / std::f64::consts::E;

/// The weight of `ln(frequency_weight)` in the word score.
const FREQUENCY_SHARE: f64 = 0.5;

/// How much of a target's dose `exposures` exposures are worth: rises
/// toward one, so the next exposure of a target is worth less the more it
/// already has.
fn coverage_of(exposures: usize, dose: usize) -> f64 {
    1.0 - (-(exposures as f64) / dose as f64).exp()
}

/// How much each practised pattern's coverage counts: its priority, except
/// that the exploration target, chosen for its uncertainty rather than its
/// rank, counts at least as much as the average target so it is actually
/// practised. A weight that would still be zero takes the average too, or
/// one when no pattern has a positive priority, so that coverage drives
/// selection whatever the priorities were.
fn coverage_weights(practised: &[&SelectedTarget]) -> Vec<f64> {
    let positive: Vec<f64> = practised
        .iter()
        .filter(|t| t.role == TargetRole::Target && t.priority > 0.0)
        .map(|t| t.priority)
        .collect();
    let mean_target = if positive.is_empty() {
        1.0
    } else {
        positive.iter().sum::<f64>() / positive.len() as f64
    };
    practised
        .iter()
        .map(|t| match t.role {
            TargetRole::Explore => t.priority.max(mean_target),
            _ if t.priority > 0.0 => t.priority,
            _ => mean_target,
        })
        .collect()
}

/// Every bigram and trigram of a word read as space-padded text, in
/// pattern order.
fn word_patterns(word: &str) -> BTreeSet<String> {
    WordChains::new(word)
        .iter()
        .flat_map(|chain| {
            chain
                .char_indices()
                .map(|(i, _)| chain[i..].to_string())
                .filter(|level| level.chars().count() >= 2)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Shuffles the words and spaces out exposures, trying afresh from another
/// shuffle while any two words exposing the same target are still within
/// `gap` of each other, up to a fixed number of attempts; the arrangement
/// with the fewest such pairs is kept.
fn arrange(mut words: Vec<PlacedWord>, gap: usize, rng: &mut Rng) -> Vec<PlacedWord> {
    let mut best: Option<(usize, Vec<PlacedWord>)> = None;
    for _ in 0..ARRANGEMENT_ATTEMPTS {
        rng.shuffle(&mut words);
        space_exposures(&mut words, gap);
        let remaining = close_pairs(&words, gap);
        if remaining == 0 {
            return words;
        }
        if best.as_ref().is_none_or(|(fewest, _)| remaining < *fewest) {
            best = Some((remaining, words.clone()));
        }
    }
    best.map_or(words, |(_, words)| words)
}

/// How many shuffles [`arrange`] tries before settling for the best.
const ARRANGEMENT_ATTEMPTS: usize = 8;

/// How many pairs of words within `gap` of each other expose the same
/// target.
fn close_pairs(words: &[PlacedWord], gap: usize) -> usize {
    (0..words.len())
        .flat_map(|i| (i + 1..(i + gap).min(words.len())).map(move |j| (i, j)))
        .filter(|&(i, j)| words[i].shares_target(&words[j]))
        .count()
}

/// Moves words so that two exposing the same target are at least `gap`
/// positions apart where possible. Walking the prompt, a word that repeats
/// a target exposed within the previous `gap - 1` words is swapped with
/// the first later word that does not; a displaced word is checked again
/// when the walk reaches its new position. When no later word will do,
/// as happens toward the end, the word or the one it clashes with is
/// moved to the first position anywhere in the prompt where both then sit
/// clear of their neighbours on either side.
fn space_exposures(words: &mut [PlacedWord], gap: usize) {
    if gap < 2 {
        return;
    }
    let window_before = |position: usize| position.saturating_sub(gap - 1)..position;
    // Whether `words[candidate]` would repeat a target of a word within
    // `gap` before `position`, counting `words[..position]` only.
    let clashes_before = |words: &[PlacedWord], position: usize, candidate: usize| {
        words[window_before(position)]
            .iter()
            .any(|w| w.shares_target(&words[candidate]))
    };
    // Whether the word at `position` repeats a target of any word within
    // `gap` of it on either side.
    let clashes_around = |words: &[PlacedWord], position: usize| {
        let to = (position + gap).min(words.len());
        (window_before(position).start..to)
            .filter(|&q| q != position)
            .any(|q| words[q].shares_target(&words[position]))
    };
    for i in 0..words.len() {
        if !clashes_before(words, i, i) {
            continue;
        }
        if let Some(j) = (i + 1..words.len()).find(|&j| !clashes_before(words, i, j)) {
            words.swap(i, j);
            continue;
        }
        let clashing: Vec<usize> = window_before(i)
            .filter(|&q| words[q].shares_target(&words[i]))
            .collect();
        'relocate: for m in std::iter::once(i).chain(clashing) {
            for k in (0..words.len()).filter(|&k| k != m) {
                words.swap(m, k);
                if !clashes_around(words, m) && !clashes_around(words, k) {
                    break 'relocate;
                }
                words.swap(m, k);
            }
        }
    }
}
