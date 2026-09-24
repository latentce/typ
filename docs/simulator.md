# The simulator

`typ-sim` drives the real session pipeline with synthetic typists whose
truth is known, so that the scheduler's tunables can be set on evidence and
a biased estimator is caught before a user meets it. It composes prompts
with the real scheduler, has a learner type them into the same state
machine the terminal feeds, and applies each session through the same
steps as the terminal does at the end of a session: pattern statistics,
achieved doses, training history, the next prompt, the session summary.
Nothing in it re-implements the algorithm.

```
cargo run --release -p typ-sim -- --learner trainable --scheduler phase1 --sessions 40 --seed 1
cargo run --release -p typ-sim -- --list-tunables
cargo run --release -p typ-sim -- --set kappa=5 --set dose=8
```

Runs are seeded and reproducible. Every `SchedulerConfig` tunable can be
overridden with `--set name=value`.

## Learners

Every learner has a true log-latency and error probability for each bigram,
drawn once from the seed with a little QWERTY geometry in it (same-finger
and row-change costs), a typical clean keystroke of 200 ms, and types one
keystroke per slot with lognormal noise and the occasional hesitation. An
error is mostly a wrong key, three quarters of them noticed and corrected
with a backspace; now and then a letter is skipped or swapped with the next,
which goes uncorrected. They differ in what practice does:

| learner     | rule                                                                                         |
| ----------- | -------------------------------------------------------------------------------------------- |
| `trainable` | `ar` is 2.7× slower and 25 points more error-prone; every exposure, in any word, shrinks that by a power law |
| `awkward`   | `ch` is just as bad and never improves                                                       |
| `fatigue`   | nothing improves; every third session is tired, each session starts slow and ends slower     |
| `global`    | everything gets 0.6 % faster and more accurate per session, whatever was practised           |
| `memoriser` | each word gets faster the more often it has been typed; nothing transfers between words      |

`global` is the null learner: it has no practice-dependent improvement, so a
learning-gain estimate must read about zero on it.

## Schedulers

`phase1` is the scheduler `typ` ships. `weakest` targets the patterns with
the highest training value by posterior mean and nothing else: no sampling,
no deferral, no exploration, no plateau. `random` composes every prompt
from the reference distribution.

## What a run reports

- **reference loss**: the learner's expected seconds and errors per slot
  over the fixed reference sample, from its truth with no noise, before
  and after the run.
- **weakness** (learners with one): exposures typed, share of the
  shortfall remaining, sessions targeted, and when it was first targeted.
- **transfer**: how the patterns the scheduler invested in (practised in
  three or more sessions) were typed in words never used for targeted
  practice, early against late, beside every other slot of the same words.
  The difference is the pattern-specific speed-up.
- **doses**: planned against achieved exposures per target.
- **pipeline time**: the end-of-session steps, whose budget is 100 ms.
- **plateaus**: which targets were backed off for a plateau, and when.
- **gain check**: the naive before-and-after estimate over targets, the
  drift over every eligible pattern, and the corrected comparison of the
  targeted arm against the deferred arm on fresh observations after
  selection, with its standard error. The arms are of similar prior
  weakness by construction rather than by matching: every candidate of a
  session is deferred or targeted by the same coin toss, so the two arms
  are random halves of one pool. Matching each deferred candidate to the
  target with the closest prior estimate was tried first and rejected: the
  weakest-looking target has far more evidence behind its estimate than a
  control with the same estimate, so matched pairs differ in truth. Drift
  cancels rather than being subtracted: both arms are measured over the
  same sessions as residuals against the same moving baseline.

## What the runs showed

The integration tests in `crates/typ-sim/tests/` pin the results that
matter; run them with `cargo test --release -p typ-sim` for the timing
budget to be asserted at its real value.

**Phase 1 beats random** on reference loss for the trainable learner at
every seed tried, by drilling the weakness about twice as often as
ordinary text does. The greedy `weakest` baseline beats Phase 1 on this
learner: it targets the one weakness every session, while Phase 1 pays for
deferred controls, sampling noise, and the plateau back-off that a slowly
improving pattern also triggers. That cost is deliberate.

**The awkward transition plateaus** in every seed, typically after 10 to 20
sessions. It did not under the original plateau rule, which compared the
current weakness with the estimate at the pattern's *first* selection: that
estimate is shrunk toward the parent character before the pattern has
evidence of its own, so a truly weak, unchanging pattern drifts away from
it as evidence arrives and never looks settled. The rule now compares with
the estimate at the start of the practice window.

**The naive gain estimate is biased upward on every learner**, by about
+0.05 to +0.17 in weakness units depending on run length, learning or not:
regression to the mean from selecting what looked weak. Drift over all
eligible patterns stays near zero because weakness is measured against the
moving user baseline. The randomised comparison reads within its standard
error of zero on the null learner, but that standard error is wide, around
0.15 after 150 daily sessions: deferring a quarter of the candidates for
three sessions yields roughly 300 deferred selections with about six
incidental trials each, so the error component of the score rests on some
thirty errors. The speed component is precise (the arms agree to within a
few hundredths of log-latency); the error component would need an order of
magnitude more controls to be. That is a quantitative input to how many deferred controls
a learning-gain scheduler would need.

**The memoriser is not credited** with pattern improvement by the transfer
view: its practised patterns speed up no more than the other slots of the
same untargeted words, whatever it gained on the words it was drilled on.

## Tunables

Sweeps over the trainable and awkward learners (24 seeds each, 40 sessions)
measured how many exposures the weakness received, how soon it was first
targeted, and whether it plateaued. The defaults stand; the sweep
established where they sit rather than moving them:

- `log_ratio_variance_cap` (1.0) and `kappa` (10) sit on a trade-off.
  Halving the cap gave the trainable weakness 12 % more exposures but
  delayed first targeting of the awkward one from session 5 to 7 or 8 on
  average, and in one seed to session 19: the cap is what lets a pattern
  whose parent character is typed well be sampled high enough to be tried.
  Doubling it slowed discovery too, by adding noise. `kappa` 5, 7, and 15
  were all worse on discovery than 10.
- `dose` 8 and `temperature` 0.4 each gave a found weakness 5 to 25 % more
  exposures with discovery unchanged, and 17 % together. Both were rejected
  because at either setting a 50-word prompt can no longer give every
  target its dose: six practised patterns at eight exposures over forty
  targeted words is more than the coverage draw fits, and the sharpest
  draw is what makes the dose reliable.
- `slowness_share`, `max_targets`, `coverage_scale`, and the weakness
  weights moved the objective by less than the seed-to-seed noise.
