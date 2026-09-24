//! Synthetic learners: typists with a known truth and a known learning
//! rule, so that what the model and scheduler make of their sessions can be
//! checked against what actually happened to them.
//!
//! A learner's truth is a log-latency and an error probability for every
//! bigram, drawn once from the seed with a little of the keyboard's
//! geometry in it, and the rule by which they change. Every learner types
//! the same way: one keystroke per slot with lognormal noise around its
//! true latency, a wrong key now and then, most of them noticed and
//! corrected, and the occasional hesitation. What differs is what
//! practice does to them.

use std::collections::BTreeMap;

use typ_rs_core::layout::Layout;
use typ_rs_core::prompt::Prompt;
use typ_rs_core::random::Rng;
use typ_rs_core::session::{EndCondition, Input, Key, SessionState};

/// How a learner changes with practice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// One bigram is much slower and more error-prone than the rest, and
    /// every time it is typed, in any word, the shortfall shrinks by a
    /// power law. The learner the scheduler is meant to help.
    Trainable,
    /// One bigram is much slower and more error-prone than the rest and
    /// stays so however much it is practised.
    Awkward,
    /// Nothing improves. Every third session is a tired one, the first
    /// words of every session are slow while the learner warms up, and the
    /// last ones slow down again.
    Fatigue,
    /// Everything gets uniformly faster and more accurate with every
    /// session, whatever was practised. The learner with no
    /// practice-dependent improvement, against which a gain estimate must
    /// read zero.
    Global,
    /// Each word gets faster the more often it has been typed, and nothing
    /// carries over to other words containing the same patterns.
    Memoriser,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Trainable,
        Kind::Awkward,
        Kind::Fatigue,
        Kind::Global,
        Kind::Memoriser,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Trainable => "trainable",
            Kind::Awkward => "awkward",
            Kind::Fatigue => "fatigue",
            Kind::Global => "global",
            Kind::Memoriser => "memoriser",
        }
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == name)
    }

    /// The bigram the learner is weak on, for the kinds that have one.
    pub fn weakness(self) -> Option<&'static str> {
        match self {
            Kind::Trainable => Some(TRAINABLE_WEAKNESS),
            Kind::Awkward => Some(AWKWARD_TRANSITION),
            _ => None,
        }
    }
}

/// The trainable learner's weakness: common enough to be found within a
/// few sessions, rare enough that untargeted practice barely touches it.
const TRAINABLE_WEAKNESS: &str = "ar";
/// The awkward learner's transition, of similar frequency.
const AWKWARD_TRANSITION: &str = "ch";

/// The shortfall a weak bigram adds to its log-latency and to its error
/// probability.
const WEAKNESS_LOG_LATENCY: f64 = 1.0;
const WEAKNESS_ERROR: f64 = 0.25;
/// The trainable weakness after `n` exposures is `(1 + n / scale)^-exponent`
/// of what it was: slow enough that the exposures ordinary text gives
/// over a few dozen sessions barely dent it.
const PRACTICE_SCALE: f64 = 200.0;
const PRACTICE_EXPONENT: f64 = 0.7;

/// The fatigue learner: the log-latency a tired session adds, what the
/// first word of a session adds and over how many words it fades, and what
/// each further word adds.
const TIRED_SESSION: f64 = 0.15;
const TIRED_EVERY: usize = 3;
const WARM_UP: f64 = 0.30;
const WARM_UP_WORDS: f64 = 6.0;
const FATIGUE_PER_WORD: f64 = 0.003;

/// The global learner's improvement per session, in log-latency and in
/// log error probability.
const GLOBAL_RATE: f64 = 0.006;

/// The memoriser: how much log-latency a fully familiar word saves, and
/// the number of typings at which half of that is reached. Errors on a
/// familiar word fall by half as much, proportionally.
const FAMILIARITY: f64 = 0.40;
const FAMILIARITY_SCALE: f64 = 2.0;

/// The truth every learner shares: a typical clean keystroke of 200 ms,
/// per-character and per-bigram spreads around it, extra cost for a
/// same-finger transition and for each row crossed, and the planning time
/// a word's first character carries.
const TYPICAL_SECONDS: f64 = 0.20;
const CHARACTER_SD: f64 = 0.08;
const BIGRAM_SD: f64 = 0.12;
const SAME_FINGER: f64 = 0.20;
const PER_ROW: f64 = 0.05;
const WORD_INITIATION: f64 = 0.35;
/// The typical error probability per slot and the spread of its log across
/// bigrams.
const BASE_ERROR: f64 = 0.02;
const ERROR_LOG_SD: f64 = 0.4;
const ERROR_BOUNDS: (f64, f64) = (0.002, 0.15);

/// How a learner types: lognormal noise around the true latency, a
/// hesitation now and then, most wrong keys noticed after a moment and
/// retyped a little slower than usual.
const NOISE_SD: f64 = 0.22;
const HESITATION_CHANCE: f64 = 0.008;
const HESITATION_MICROS: u64 = 1_700_000;
const CORRECTION_CHANCE: f64 = 0.75;
const NOTICE_SECONDS: f64 = 0.40;
/// The share of errors at a letter slot that skip the letter or swap it
/// with the next; neither is noticed in time to correct.
const OMISSION_SHARE: f64 = 0.15;
const TRANSPOSITION_SHARE: f64 = 0.15;
const RETYPE_PENALTY: f64 = 0.10;
/// Time spent reading before the first keystroke.
const READING_MICROS: u64 = 800_000;
const MIN_LATENCY_MICROS: u64 = 30_000;

const ALPHABET: &str = "abcdefghijklmnopqrstuvwxyz ";
const CHARS: usize = 27;

/// The learner's expected performance over a text, with no noise: what
/// its typing costs on the reference distribution.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loss {
    /// Expected seconds per slot, word-initiation time included.
    pub seconds_per_character: f64,
    /// Expected first-attempt errors per slot.
    pub error_rate: f64,
}

/// A synthetic typist.
#[derive(Debug, Clone)]
pub struct Learner {
    kind: Kind,
    rng: Rng,
    skill: Skill,
    /// Exposures of the weak bigram so far.
    practice: u32,
    /// How often each word has been typed, for the memoriser.
    words_typed: BTreeMap<Box<str>, u32>,
    /// Sessions typed so far.
    sessions: usize,
}

/// The static truth: log-latency offsets from the typical keystroke by
/// character and by bigram, and the error probability of each bigram,
/// indexed by the previous and the expected character.
#[derive(Debug, Clone)]
struct Skill {
    character: [f64; CHARS],
    bigram: Vec<[f64; CHARS]>,
    error: Vec<[f64; CHARS]>,
}

/// One error as it comes out of the fingers.
enum Slip {
    Substitution,
    Omission,
    /// Swapped with the following letter.
    Transposition(char),
}

/// Where a slot sits: its word's text and position, and, when typed in a
/// session, which word of the session it is in.
struct Context<'a> {
    previous: char,
    expected: char,
    word: &'a str,
    position: usize,
    session_word: Option<usize>,
}

impl Learner {
    /// A learner of the given kind whose truth and every keystroke are
    /// determined by `seed`.
    pub fn new(kind: Kind, seed: u64) -> Learner {
        let mut rng = Rng::seeded(seed);
        let skill = Skill::draw(&mut rng);
        Learner {
            kind,
            rng,
            skill,
            practice: 0,
            words_typed: BTreeMap::new(),
            sessions: 0,
        }
    }

    /// How many times the learner's weak bigram has been typed; `None` for
    /// a learner without one.
    pub fn weakness_exposures(&self) -> Option<u32> {
        self.kind.weakness().map(|_| self.practice)
    }

    /// What is left of the learner's weakness as a share of what it began
    /// with: one for the awkward learner, falling with practice for the
    /// trainable one; `None` for a learner without one.
    pub fn weakness_remaining(&self) -> Option<f64> {
        match self.kind {
            Kind::Trainable => Some(self.remaining_weakness()),
            Kind::Awkward => Some(1.0),
            _ => None,
        }
    }

    /// Types the prompt from start to finish as one session: an event log
    /// with realistic timestamps, applied through the same state machine
    /// the terminal feeds. Whatever the learner learns from typing it has
    /// been learnt by the time this returns.
    pub fn type_prompt(&mut self, prompt: &Prompt) -> SessionState {
        let mut state = SessionState::new(
            prompt.clone(),
            EndCondition::AfterWords(prompt.word_count()),
        );
        let mut at = READING_MICROS;
        let mut previous = ' ';
        for (index, word) in prompt.words().iter().enumerate() {
            let letters: Vec<char> = word.chars().collect();
            let slots = letters
                .iter()
                .copied()
                .chain(std::iter::once(' '))
                .enumerate();
            let mut transposed = false;
            for (position, expected) in slots {
                if state.outcome().is_some() {
                    break;
                }
                let context = Context {
                    previous,
                    expected,
                    word,
                    position,
                    session_word: Some(index),
                };
                let mean = self.log_latency(&context);
                at += self.latency_micros(mean);
                if transposed {
                    // The second half of a transposition was typed a slot
                    // early; this slot gets the first half, late.
                    transposed = false;
                    state.apply_event(Input::new(at, Key::Char(letters[position - 1])));
                } else if self.rng.chance(self.error_probability(&context)) {
                    let following = letters.get(position + 1).copied();
                    match self.slip(following) {
                        Slip::Substitution => {
                            let wrong = self.wrong_key(expected);
                            state.apply_event(Input::new(at, Key::Char(wrong)));
                            if self.rng.chance(CORRECTION_CHANCE) {
                                at += self.latency_micros(NOTICE_SECONDS.ln());
                                state.apply_event(Input::new(at, Key::Backspace));
                                at += self.latency_micros(mean + RETYPE_PENALTY);
                                state.apply_event(Input::new(at, Key::Char(expected)));
                            } else if expected == ' ' {
                                // A wrong key where the space was due is an
                                // extra character; the word is still left
                                // with a space.
                                at += self.latency_micros(mean);
                                state.apply_event(Input::new(at, Key::Char(' ')));
                            }
                        }
                        Slip::Omission => {}
                        Slip::Transposition(next) => {
                            state.apply_event(Input::new(at, Key::Char(next)));
                            transposed = true;
                        }
                    }
                } else {
                    state.apply_event(Input::new(at, Key::Char(expected)));
                }
                if self
                    .kind
                    .weakness()
                    .is_some_and(|w| bigram_is(previous, expected, w))
                {
                    self.practice += 1;
                }
                previous = expected;
            }
            if self.kind == Kind::Memoriser {
                *self.words_typed.entry(word.clone()).or_default() += 1;
            }
        }
        self.sessions += 1;
        state
    }

    /// What the learner's typing costs on `sample`, as it stands now: the
    /// expected seconds and errors per slot with no noise and no warm-up or
    /// fatigue, the spaces between words included.
    pub fn reference_loss(&self, sample: &Prompt) -> Loss {
        let mut seconds = 0.0;
        let mut errors = 0.0;
        let mut slots = 0usize;
        let mut previous = ' ';
        let last = sample.word_count() - 1;
        for (index, word) in sample.words().iter().enumerate() {
            let space = (index < last).then_some(' ');
            for (position, expected) in word.chars().chain(space).enumerate() {
                let context = Context {
                    previous,
                    expected,
                    word,
                    position,
                    session_word: None,
                };
                seconds += self.log_latency(&context).exp();
                errors += self.error_probability(&context);
                slots += 1;
                previous = expected;
            }
        }
        Loss {
            seconds_per_character: seconds / slots as f64,
            error_rate: errors / slots as f64,
        }
    }

    /// The true log-latency of a slot: the typical keystroke, the
    /// character's and bigram's offsets, the planning time of a word's
    /// first character, and whatever the learner's kind adds.
    fn log_latency(&self, context: &Context) -> f64 {
        let (p, c) = (index_of(context.previous), index_of(context.expected));
        let mut log = TYPICAL_SECONDS.ln() + self.skill.character[c] + self.skill.bigram[p][c];
        if context.position == 0 {
            log += WORD_INITIATION;
        }
        match self.kind {
            Kind::Trainable => {
                if bigram_is(context.previous, context.expected, TRAINABLE_WEAKNESS) {
                    log += WEAKNESS_LOG_LATENCY * self.remaining_weakness();
                }
            }
            Kind::Awkward => {
                if bigram_is(context.previous, context.expected, AWKWARD_TRANSITION) {
                    log += WEAKNESS_LOG_LATENCY;
                }
            }
            Kind::Fatigue => {
                if self.sessions % TIRED_EVERY == TIRED_EVERY - 1 {
                    log += TIRED_SESSION;
                }
                if let Some(word) = context.session_word {
                    log += WARM_UP * (-(word as f64) / WARM_UP_WORDS).exp();
                    log += FATIGUE_PER_WORD * word as f64;
                }
            }
            Kind::Global => log -= GLOBAL_RATE * self.sessions as f64,
            Kind::Memoriser => log -= FAMILIARITY * self.familiarity(context.word),
        }
        log
    }

    /// The true probability that the slot is typed wrong on the first
    /// attempt.
    fn error_probability(&self, context: &Context) -> f64 {
        let (p, c) = (index_of(context.previous), index_of(context.expected));
        let mut probability = self.skill.error[p][c];
        match self.kind {
            Kind::Trainable => {
                if bigram_is(context.previous, context.expected, TRAINABLE_WEAKNESS) {
                    probability += WEAKNESS_ERROR * self.remaining_weakness();
                }
            }
            Kind::Awkward => {
                if bigram_is(context.previous, context.expected, AWKWARD_TRANSITION) {
                    probability += WEAKNESS_ERROR;
                }
            }
            Kind::Fatigue => {}
            Kind::Global => probability *= (-GLOBAL_RATE * self.sessions as f64).exp(),
            Kind::Memoriser => probability *= 1.0 - 0.5 * self.familiarity(context.word),
        }
        probability.min(1.0)
    }

    /// What is left of the trainable weakness after the practice so far.
    fn remaining_weakness(&self) -> f64 {
        (1.0 + f64::from(self.practice) / PRACTICE_SCALE).powf(-PRACTICE_EXPONENT)
    }

    /// How familiar the memoriser is with a word, from zero to one.
    fn familiarity(&self, word: &str) -> f64 {
        let typed = self.words_typed.get(word).copied().unwrap_or(0);
        1.0 - 1.0 / (1.0 + f64::from(typed) / FAMILIARITY_SCALE)
    }

    /// A keystroke's latency in microseconds: noise around the true
    /// log-latency, and now and then a hesitation on top.
    fn latency_micros(&mut self, log_latency: f64) -> u64 {
        let seconds = (log_latency + self.rng.normal(0.0, NOISE_SD)).exp();
        let mut micros = ((seconds * 1_000_000.0) as u64).max(MIN_LATENCY_MICROS);
        if self.rng.chance(HESITATION_CHANCE) {
            micros += HESITATION_MICROS;
        }
        micros
    }

    /// What kind of slip an error at a letter slot is: mostly a wrong key,
    /// sometimes the letter skipped, sometimes swapped with the letter after
    /// it. Where the space is due, or the letter is the word's last, only a
    /// wrong key is possible.
    fn slip(&mut self, following: Option<char>) -> Slip {
        let draw = self.rng.unit();
        match following {
            Some(next) if draw < TRANSPOSITION_SHARE => Slip::Transposition(next),
            Some(_) if draw < TRANSPOSITION_SHARE + OMISSION_SHARE => Slip::Omission,
            _ => Slip::Substitution,
        }
    }

    /// A letter other than the expected one.
    fn wrong_key(&mut self, expected: char) -> char {
        loop {
            let i = (self.rng.unit() * 26.0) as usize;
            let c = ALPHABET.as_bytes()[i.min(25)] as char;
            if c != expected {
                return c;
            }
        }
    }
}

impl Skill {
    fn draw(rng: &mut Rng) -> Skill {
        let layout = Layout::QWERTY;
        let keys: Vec<_> = ALPHABET.chars().map(|c| layout.key(c)).collect();
        let character = std::array::from_fn(|_| rng.normal(0.0, CHARACTER_SD));
        let mut bigram = vec![[0.0; CHARS]; CHARS];
        let mut error = vec![[0.0; CHARS]; CHARS];
        for p in 0..CHARS {
            for c in 0..CHARS {
                let mut geometry = 0.0;
                if let (Some(from), Some(to)) = (keys[p], keys[c]) {
                    if from.hand.is_some() && from.hand == to.hand && from.finger == to.finger {
                        geometry += SAME_FINGER;
                    }
                    if from.hand.is_some() && to.hand.is_some() {
                        geometry += PER_ROW * f64::from(from.row.abs_diff(to.row));
                    }
                }
                bigram[p][c] = rng.normal(0.0, BIGRAM_SD) + geometry;
                error[p][c] = (BASE_ERROR * rng.normal(0.0, ERROR_LOG_SD).exp())
                    .clamp(ERROR_BOUNDS.0, ERROR_BOUNDS.1);
            }
        }
        Skill {
            character,
            bigram,
            error,
        }
    }
}

/// The index of a character in the alphabet; anything else counts as a
/// space, which the corpus never produces.
fn index_of(c: char) -> usize {
    ALPHABET.find(c).unwrap_or(CHARS - 1)
}

fn bigram_is(previous: char, expected: char, bigram: &str) -> bool {
    let mut chars = bigram.chars();
    chars.next() == Some(previous) && chars.next() == Some(expected)
}
