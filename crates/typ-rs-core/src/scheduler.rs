//! Which patterns a session practises, and what came of practising them.
//!
//! Every eligible bigram and trigram has a weakness posterior; the
//! scheduler draws one sample from each, turns it into a priority, and
//! walks the ranking to pick the session's [candidates](TargetRole), keeping
//! one pattern per back-off chain. A share of the candidates is withheld
//! at random as controls for a window of sessions, the rest become
//! targets, and one further pattern is drawn for its uncertainty alone. A
//! target that has had substantial practice without measurable change has
//! its priority scaled down until it has gone untargeted for a while.
//!
//! What was selected and what the user then typed is a [`TrainingEvent`];
//! the sequence of them over a profile's sessions is its
//! [`TrainingHistory`], which is where deferral windows and plateaus live.

use std::collections::{BTreeMap, BTreeSet};

use crate::corpus::Corpus;
use crate::model::{ModelState, SchedulerConfig};
use crate::prompt::Slot;
use crate::random::Rng;
use crate::session::{EventKind, SessionState};

/// Why a pattern was recorded against a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetRole {
    /// The prompt's targeted words are chosen to expose it.
    Target,
    /// A candidate withheld from targeting as a control, still appearing
    /// incidentally.
    Deferred,
    /// Chosen for its uncertainty rather than its rank.
    Explore,
}

impl TargetRole {
    /// The role's stable name, as stored. Never renamed.
    pub fn name(self) -> &'static str {
        match self {
            TargetRole::Target => "target",
            TargetRole::Deferred => "deferred",
            TargetRole::Explore => "explore",
        }
    }

    /// The role with the given stored name, if this version knows it.
    pub fn from_name(name: &str) -> Option<TargetRole> {
        match name {
            "target" => Some(TargetRole::Target),
            "deferred" => Some(TargetRole::Deferred),
            "explore" => Some(TargetRole::Explore),
            _ => None,
        }
    }

    /// Whether the prompt's words were chosen to expose the pattern.
    pub fn is_practised(self) -> bool {
        matches!(self, TargetRole::Target | TargetRole::Explore)
    }
}

/// A pattern recorded against a prompt, with what the model believed about
/// it at selection.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedTarget {
    pub pattern: Box<str>,
    pub role: TargetRole,
    pub weakness_mean: f64,
    pub weakness_sd: f64,
    pub priority: f64,
    /// Exposures the prompt should give it; zero for a deferred candidate.
    pub planned_dose: usize,
}

/// A selected target together with the exposures the session actually
/// typed.
#[derive(Debug, Clone, PartialEq)]
pub struct TrainingEvent {
    pub target: SelectedTarget,
    pub achieved_dose: usize,
}

/// A bigram or trigram that may be targeted, with its importance.
#[derive(Debug, Clone, PartialEq)]
pub struct EligiblePattern {
    pub pattern: Box<str>,
    /// `corpus_frequency^0.5`.
    pub importance: f64,
}

/// The bigrams and trigrams (space-containing ones included) that occur in
/// enough distinct corpus words and matter enough to be worth targeting.
/// Characters are a back-off level, never targets. In pattern-text order
/// within each level, bigrams first.
pub fn eligible_patterns(corpus: &Corpus, config: &SchedulerConfig) -> Vec<EligiblePattern> {
    corpus
        .bigrams()
        .iter()
        .chain(corpus.trigrams())
        .filter(|p| p.words().len() >= config.min_pattern_words)
        .map(|p| EligiblePattern {
            pattern: p.pattern.clone(),
            importance: p.frequency.sqrt(),
        })
        .filter(|p| p.importance >= config.importance_floor)
        .collect()
}

/// What has happened to one pattern over a profile's sessions.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PatternHistory {
    /// Sessions still to come in which the pattern stays out of candidacy.
    pub deferral_remaining: usize,
    /// Sessions in which it was a target or exploration target.
    pub sessions_practised: usize,
    /// Exposures typed across every session it was practised in.
    pub achieved_dose: usize,
    /// Its weakness mean at selection in each session it was practised
    /// in, in order.
    pub practised_means: Vec<f64>,
    /// The ordinal (from one) of the last session it was practised in.
    pub last_practised: Option<usize>,
}

/// The training events of a profile's sessions in order: the deferral
/// windows currently open, each pattern's record of practice, and the
/// words each session showed as targeted.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrainingHistory {
    sessions: usize,
    patterns: BTreeMap<Box<str>, PatternHistory>,
    /// One entry per session: the words shown as targeted in it.
    targeted_words: Vec<BTreeSet<Box<str>>>,
}

impl TrainingHistory {
    /// The history of a profile with no sessions.
    pub fn new() -> TrainingHistory {
        TrainingHistory::default()
    }

    /// Sessions recorded so far.
    pub fn sessions(&self) -> usize {
        self.sessions
    }

    /// What has happened to a pattern; `None` if it was never selected.
    pub fn pattern(&self, pattern: &str) -> Option<&PatternHistory> {
        self.patterns.get(pattern)
    }

    /// Every pattern still inside its deferral window, with the sessions
    /// remaining, in pattern order.
    pub fn deferrals(&self) -> impl Iterator<Item = (&str, usize)> {
        self.patterns
            .iter()
            .filter(|(_, h)| h.deferral_remaining > 0)
            .map(|(p, h)| (p.as_ref(), h.deferral_remaining))
    }

    /// Whether the pattern is inside a deferral window.
    pub fn is_deferred(&self, pattern: &str) -> bool {
        self.pattern(pattern)
            .is_some_and(|h| h.deferral_remaining > 0)
    }

    /// Whether the pattern was a target or exploration target in any of the
    /// last `sessions` sessions recorded.
    pub fn practised_within(&self, pattern: &str, sessions: usize) -> bool {
        self.pattern(pattern)
            .and_then(|h| h.last_practised)
            .is_some_and(|last| last + sessions > self.sessions)
    }

    /// Every pattern that was a target or exploration target in any of the
    /// last `sessions` sessions recorded, in pattern order.
    pub fn recently_practised(&self, sessions: usize) -> impl Iterator<Item = &str> {
        self.patterns
            .keys()
            .map(AsRef::as_ref)
            .filter(move |p| self.practised_within(p, sessions))
    }

    /// Whether the word was shown as targeted in any of the last `sessions`
    /// sessions recorded.
    pub fn targeted_word_within(&self, word: &str, sessions: usize) -> bool {
        self.targeted_words
            .iter()
            .rev()
            .take(sessions)
            .any(|words| words.contains(word))
    }

    /// Records one session's events and the words its prompt showed as
    /// targeted, the next session in order. A deferred event for a pattern
    /// not inside a window opens one of `deferral_window` sessions, this
    /// one included. Every open window then runs down by this session,
    /// whether or not the pattern was logged in it, so a session that
    /// selected nothing still counts.
    pub fn record<'a>(
        &mut self,
        events: impl IntoIterator<Item = &'a TrainingEvent>,
        targeted_words: impl IntoIterator<Item = &'a str>,
        config: &SchedulerConfig,
    ) {
        self.sessions += 1;
        let session = self.sessions;
        self.targeted_words
            .push(targeted_words.into_iter().map(Box::from).collect());
        for event in events {
            let t = &event.target;
            let h = self.patterns.entry(t.pattern.clone()).or_default();
            match t.role {
                TargetRole::Deferred => {
                    if h.deferral_remaining == 0 {
                        h.deferral_remaining = config.deferral_window;
                    }
                }
                TargetRole::Target | TargetRole::Explore => {
                    h.sessions_practised += 1;
                    h.achieved_dose += event.achieved_dose;
                    h.practised_means.push(t.weakness_mean);
                    h.last_practised = Some(session);
                }
            }
        }
        for h in self.patterns.values_mut() {
            h.deferral_remaining = h.deferral_remaining.saturating_sub(1);
        }
    }

    /// How much a pattern's priority is scaled for having plateaued: one
    /// unless it has been practised in enough sessions with enough dose and
    /// its weakness mean has moved less than its current uncertainty since
    /// the selection `plateau_min_sessions` practised sessions ago; then
    /// the plateau factor, recovering linearly to one over the configured
    /// number of untargeted sessions. The comparison looks back over the
    /// practice window rather than to the first selection because a
    /// pattern's first estimate is its noisiest, shrunk toward its parent
    /// before it has evidence of its own: a pattern that is truly weak and
    /// never changes still drifts away from it as evidence arrives.
    pub fn plateau_factor(
        &self,
        pattern: &str,
        weakness_mean: f64,
        weakness_sd: f64,
        config: &SchedulerConfig,
    ) -> f64 {
        let Some(h) = self.pattern(pattern) else {
            return 1.0;
        };
        let Some(last) = h.last_practised else {
            return 1.0;
        };
        let window = config.plateau_min_sessions.max(1);
        let Some(&reference) = h
            .practised_means
            .len()
            .checked_sub(window)
            .and_then(|i| h.practised_means.get(i))
        else {
            return 1.0;
        };
        let plateaued = h.sessions_practised >= config.plateau_min_sessions
            && h.achieved_dose > config.plateau_min_dose
            && (weakness_mean - reference).abs() < weakness_sd;
        if !plateaued {
            return 1.0;
        }
        let untargeted = self.sessions.saturating_sub(last);
        if untargeted >= config.plateau_recovery_sessions {
            return 1.0;
        }
        let recovered = untargeted as f64 / config.plateau_recovery_sessions as f64;
        config.plateau_factor + (1.0 - config.plateau_factor) * recovered
    }
}

/// One eligible pattern as ranked for a session.
struct Ranked {
    pattern: Box<str>,
    importance: f64,
    weakness_mean: f64,
    weakness_sd: f64,
    priority: f64,
}

/// Selects the patterns for a session's prompt as of `at`: up to
/// `max_targets` targets, the candidates newly deferred, every eligible
/// pattern whose deferral window is still running, and one exploration
/// target. Only patterns with a positive priority can be candidates: a
/// sample that came out at or below zero says the pattern is not worth
/// practising this session. Targets come first in rank order; every draw
/// comes from `rng`.
pub fn select_targets(
    model: &ModelState,
    corpus: &Corpus,
    config: &SchedulerConfig,
    history: &TrainingHistory,
    at: i64,
    rng: &mut Rng,
) -> Vec<SelectedTarget> {
    let mut ranked: Vec<Ranked> = eligible_patterns(corpus, config)
        .into_iter()
        .map(|e| {
            let weakness = model.weakness(&e.pattern, at, config);
            let slowness = model.estimate(&e.pattern, at, config).absolute_slowness;
            let plateau = history.plateau_factor(&e.pattern, weakness.mean, weakness.sd, config);
            let sampled = rng.normal(weakness.mean, weakness.sd);
            Ranked {
                priority: e.importance
                    * (sampled + config.slowness_share * slowness).max(0.0)
                    * plateau,
                pattern: e.pattern,
                importance: e.importance,
                weakness_mean: weakness.mean,
                weakness_sd: weakness.sd,
            }
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.priority
            .total_cmp(&a.priority)
            .then_with(|| a.pattern.cmp(&b.pattern))
    });

    let mut candidates: Vec<&Ranked> = Vec::new();
    for r in &ranked {
        if candidates.len() == config.candidates || r.priority <= 0.0 {
            break;
        }
        if history.is_deferred(&r.pattern)
            || candidates
                .iter()
                .any(|c| same_chain(&c.pattern, &r.pattern))
        {
            continue;
        }
        candidates.push(r);
    }

    let mut targets = Vec::new();
    let mut deferred = Vec::new();
    for c in candidates {
        if rng.chance(config.deferral_probability) {
            deferred.push(c.selected(TargetRole::Deferred, 0));
        } else if targets.len() < config.max_targets {
            targets.push(c.selected(TargetRole::Target, config.dose));
        }
    }
    for r in &ranked {
        if history.is_deferred(&r.pattern) {
            deferred.push(r.selected(TargetRole::Deferred, 0));
        }
    }

    let excluded: BTreeSet<&str> = targets
        .iter()
        .chain(&deferred)
        .map(|t| t.pattern.as_ref())
        .collect();
    let explorable: Vec<&Ranked> = ranked
        .iter()
        .filter(|r| !excluded.contains(r.pattern.as_ref()))
        .collect();
    let explore = rng
        .weighted_index(explorable.iter().map(|r| r.importance * r.weakness_sd))
        .map(|i| explorable[i].selected(TargetRole::Explore, config.dose));

    targets.extend(deferred);
    targets.extend(explore);
    targets
}

impl Ranked {
    fn selected(&self, role: TargetRole, planned_dose: usize) -> SelectedTarget {
        SelectedTarget {
            pattern: self.pattern.clone(),
            role,
            weakness_mean: self.weakness_mean,
            weakness_sd: self.weakness_sd,
            priority: self.priority,
            planned_dose,
        }
    }
}

/// Whether one pattern is an ancestor or descendant of the other: patterns
/// back off by dropping their first character, so two are in one chain
/// when one is a suffix of the other. A pattern is not in a chain with
/// itself.
pub fn same_chain(a: &str, b: &str) -> bool {
    a != b && (a.ends_with(b) || b.ends_with(a))
}

/// How many exposures each selected pattern actually received over the
/// words the user submitted. A slot exposes at most one practised pattern
/// (target or exploration target): the deepest in its back-off chain. A
/// deferred candidate is not practised, so its incidental exposures are
/// counted independently: every slot whose chain contains it. In both
/// cases at most two slots within one word count toward a pattern's dose.
/// A word's following space is a slot when the space was typed.
pub fn achieved_doses(state: &SessionState, targets: &[SelectedTarget]) -> Vec<TrainingEvent> {
    let practised: BTreeSet<&str> = targets
        .iter()
        .filter(|t| t.role.is_practised())
        .map(|t| t.pattern.as_ref())
        .collect();
    let deferred: BTreeSet<&str> = targets
        .iter()
        .filter(|t| t.role == TargetRole::Deferred)
        .map(|t| t.pattern.as_ref())
        .collect();
    let mut exposures: BTreeMap<&str, usize> = BTreeMap::new();
    let prompt = state.prompt();
    let ended_on_space = state
        .events()
        .last()
        .is_some_and(|e| e.kind == EventKind::Space);
    for word in 0..state.words_completed() {
        let len = prompt.word(word).chars().count();
        let has_space_slot = word + 1 < state.word_count() || ended_on_space;
        let chains: Vec<String> = (0..len + usize::from(has_space_slot))
            .map(|position| prompt.pattern_ending_at(Slot { word, position }))
            .collect();
        let chains = chains.iter().map(String::as_str);
        for (pattern, count) in exposures_in_word(chains, &practised, &deferred) {
            *exposures.entry(pattern).or_default() += count;
        }
    }
    targets
        .iter()
        .map(|t| TrainingEvent {
            target: t.clone(),
            achieved_dose: exposures.get(t.pattern.as_ref()).copied().unwrap_or(0),
        })
        .collect()
}

/// How many exposures of each practised pattern (target or exploration
/// target) one word gives on its own, read as space-padded text with its
/// following space as a slot; what a word is worth when it is chosen for a
/// prompt. The same rules as [`achieved_doses`]: a slot exposes only the
/// deepest practised pattern in its chain and at most two slots count
/// toward one pattern. Patterns with no exposure are absent.
pub fn word_exposures<'t>(word: &str, targets: &'t [SelectedTarget]) -> BTreeMap<&'t str, usize> {
    let practised: BTreeSet<&str> = targets
        .iter()
        .filter(|t| t.role.is_practised())
        .map(|t| t.pattern.as_ref())
        .collect();
    exposures_in_word(WordChains::new(word).iter(), &practised, &BTreeSet::new())
}

/// The chain ending at each slot of a word standing alone: the word read as
/// space-padded text, so its first character follows a space and its last
/// is followed by one, with the following space itself a slot. As in a
/// prompt's first word, the first character has only the one space before
/// it.
pub(crate) struct WordChains {
    padded: String,
    /// The byte range of each slot's chain within `padded`.
    ranges: Vec<(usize, usize)>,
}

impl WordChains {
    pub(crate) fn new(word: &str) -> WordChains {
        let padded = format!(" {word} ");
        let starts: Vec<usize> = padded.char_indices().map(|(i, _)| i).collect();
        let ranges = (1..starts.len())
            .map(|slot| {
                let from = starts[slot.saturating_sub(2)];
                let to = starts.get(slot + 1).copied().unwrap_or(padded.len());
                (from, to)
            })
            .collect();
        WordChains { padded, ranges }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &str> {
        self.ranges.iter().map(|&(from, to)| &self.padded[from..to])
    }
}

/// Counts one word's exposures from the chains ending at each of its slots.
pub(crate) fn exposures_in_word<'p, 'c>(
    chains: impl Iterator<Item = &'c str>,
    practised: &BTreeSet<&'p str>,
    deferred: &BTreeSet<&'p str>,
) -> BTreeMap<&'p str, usize> {
    let mut in_word: BTreeMap<&'p str, usize> = BTreeMap::new();
    let mut expose = |level: &str| {
        let level = practised
            .get(level)
            .or_else(|| deferred.get(level))
            .copied()
            .expect("only selected patterns are exposed");
        let count = in_word.entry(level).or_default();
        if *count < MAX_EXPOSURES_PER_WORD {
            *count += 1;
        }
    };
    for chain in chains {
        let levels = chain.char_indices().map(|(i, _)| &chain[i..]);
        if let Some(level) = levels.clone().find(|l| practised.contains(l)) {
            expose(level);
        }
        for level in levels.filter(|l| deferred.contains(l)) {
            expose(level);
        }
    }
    in_word
}

/// At most this many slots within one word count toward a pattern's dose.
const MAX_EXPOSURES_PER_WORD: usize = 2;
