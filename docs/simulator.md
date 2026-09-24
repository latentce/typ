# The simulator

`typ-sim` runs the real scheduler against fake typists whose weaknesses are
known in advance. It exists to answer questions that cannot be answered on a
human: does the scheduler find a planted weakness, how fast, does it credit
itself with gains that are really regression to the mean, and does changing
a tunable help.

Nothing in it re-implements the algorithm. It composes prompts with the real
composer, feeds keystrokes into the real session state machine, and applies
each session through the same steps the binary runs at the end of a session.

## Running it

```
just sim                                            # trainable learner, phase1 scheduler, defaults
just sim --learner awkward --sessions 60 --seed 7
just sim --scheduler random                         # baseline: no targeting at all
just sim --set kappa=5 --set dose=8                 # override tunables
just sim --list-tunables
```

Runs are seeded and reproducible. `just sim-gate` runs the integration tests
in `crates/typ-sim/tests/` with optimisations on, which is the only way the
100 ms end-of-session budget is asserted at its real value.

## Learners

Every learner has a true log-latency and error probability for each bigram,
drawn once from the seed with a little QWERTY geometry mixed in (same-finger
and row-change costs). It types one keystroke per slot at about 200 ms with
lognormal noise and the occasional hesitation. Most errors are a wrong key,
three quarters of them noticed and corrected with a backspace; now and then a
letter is skipped or swapped with the next and left uncorrected.

They differ in what practice does to them:

| Learner | Planted weakness | What practice does |
| --- | --- | --- |
| `trainable` | `ar` is 2.7× slower and 25 points more error-prone | every exposure, in any word, shrinks the gap by a power law |
| `awkward` | `ch`, just as bad | nothing; it never improves |
| `fatigue` | none | nothing; every third session is tired, and each session starts slow and gets slower |
| `global` | none | everything gets 0.6% faster and more accurate per session regardless of what was practised |
| `memoriser` | none | each *word* gets faster the more it is typed; nothing transfers between words |

`global` is the null learner. It has no practice-dependent improvement, so
any estimate of learning gain must read about zero on it or the estimator is
biased.

## Schedulers

| Scheduler | What it does |
| --- | --- |
| `phase1` | What `typ` ships: sampling, deferral, exploration, plateau. |
| `weakest` | Greedy: targets the highest posterior-mean training value, nothing else. |
| `random` | Every prompt drawn from the reference distribution. |

## What a run reports

| Section | Meaning |
| --- | --- |
| **reference loss** | The learner's expected seconds and errors per slot over the fixed reference sample, from its true parameters with no noise, before and after the run. The ground-truth outcome. |
| **weakness** | For learners with one: exposures typed, share of the shortfall remaining, sessions targeted, when it was first targeted. |
| **transfer** | How the patterns the scheduler invested in (practised in 3+ sessions) were typed in words never used for targeted practice, early versus late, beside the other slots of the same words. The difference is the pattern-specific speed-up. |
| **doses** | Planned against achieved exposures per target. |
| **pipeline time** | The end-of-session steps against their 100 ms budget. |
| **plateaus** | Which targets were backed off, and when. |
| **gain check** | Three estimates of learning gain, described below. |

### The gain check

The obvious way to measure whether practice worked is to compare a target's
weakness before and after. That estimate is biased: targets are *selected*
for looking weak, and some of that is noise that would have reverted anyway.
The gain check reports three figures side by side:

1. **Naive**: before-and-after over targets. Biased upward.
2. **Drift**: the same over every eligible pattern. Should sit near zero,
   since weakness is measured against the moving user baseline.
3. **Randomised**: the targeted arm against the deferred arm, on fresh
   observations after selection, with a standard error.

The randomised comparison works because deferral is a coin toss over the
same candidate pool: the two arms are random halves of one population, so
they have the same prior weakness by construction. Matching each deferred
candidate to the target with the nearest prior estimate was tried first and
rejected: the weakest-looking target has far more evidence behind its
estimate than a control with the same estimate, so matched pairs differ in
truth.

## What the runs showed

The integration tests pin these results.

**Phase 1 beats random** on reference loss for the trainable learner at every
seed tried, by drilling the weakness about twice as often as ordinary text
does. The greedy `weakest` baseline beats Phase 1 on this learner: it targets
the one weakness every session, while Phase 1 pays for deferred controls,
sampling noise, and the plateau back-off that a slowly improving pattern also
triggers. That cost is deliberate; `weakest` has no way to know whether it is
working.

**The awkward transition plateaus** in every seed, typically after 10 to 20
sessions. It did not under the original plateau rule, which compared the
current weakness with the estimate at the pattern's *first* selection. That
first estimate is shrunk toward the parent character before the pattern has
evidence of its own, so a truly weak, unchanging pattern drifts away from it
as evidence arrives and never looks settled. The rule now compares with the
estimate at the start of the practice window.

**The naive gain estimate is biased upward on every learner**, by about +0.05
to +0.17 in weakness units depending on run length, learning or not. Drift
stays near zero. The randomised comparison reads within its standard error of
zero on the null learner, but that standard error is wide: about 0.15 after
150 daily sessions. Deferring a quarter of candidates for three sessions
yields roughly 300 deferred selections with about six incidental exposures
each, so the error component of the score rests on some thirty errors. The
speed component is precise (arms agree to within a few hundredths of
log-latency); the error component would need an order of magnitude more
controls. That is a quantitative input to any future scheduler that wants to
act on measured gain.

**The memoriser is not credited** with pattern improvement by the transfer
view: its practised patterns speed up no more than the other slots of the
same untargeted words, whatever it gained on the words it was drilled on.

## Tunable sweeps

Sweeps over the trainable and awkward learners (24 seeds each, 40 sessions)
measured how many exposures the weakness received, how soon it was first
targeted, and whether it plateaued. The defaults stand; the sweep established
where they sit rather than moving them.

| Tunable | Finding |
| --- | --- |
| `log_ratio_variance_cap` (1.0), `kappa` (10) | On a trade-off. Halving the cap gave the trainable weakness 12% more exposures but delayed first targeting of the awkward one from session 5 to 7 or 8 on average, and to 19 in one seed: the cap is what lets a pattern whose parent character is typed well be sampled high enough to be tried. Doubling it slowed discovery by adding noise. `kappa` 5, 7, and 15 were all worse on discovery than 10. |
| `dose` (6), `temperature` (0.25) | `dose` 8 and `temperature` 0.4 each gave a found weakness 5 to 25% more exposures with discovery unchanged, 17% together. Both rejected: at either setting a 50-word prompt can no longer give every target its dose, and a reliable dose is what the sharp draw buys. |
| `slowness_share`, `max_targets`, `coverage_scale`, weakness weights | Moved the objective by less than seed-to-seed noise. |
