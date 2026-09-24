//! The gate any change to the scheduler, and any future learning-gain
//! scheduler, must pass: what the real pipeline makes of learners whose
//! truth is known. Slow: each test runs dozens of sessions.

use typ_rs_core::model::SchedulerConfig;
use typ_sim::learner::Kind;
use typ_sim::measure::{doses, gain_check, transfer};
use typ_sim::report::report;
use typ_sim::run::{Options, Run, simulate};
use typ_sim::schedule::Scheduler;

fn run(learner: Kind, scheduler: Scheduler, sessions: usize, seed: u64) -> Run {
    simulate(Options {
        learner,
        scheduler,
        sessions,
        words: 50,
        seed,
        config: SchedulerConfig::default(),
    })
}

#[test]
fn phase1_beats_random_on_reference_loss_for_the_trainable_learner() {
    for seed in [1, 2] {
        let phase1 = run(Kind::Trainable, Scheduler::Phase1, 30, seed);
        let random = run(Kind::Trainable, Scheduler::Random, 30, seed);
        let (p, r) = (phase1.final_loss(), random.final_loss());
        assert!(
            p.seconds_per_character < r.seconds_per_character && p.error_rate < r.error_rate,
            "seed {seed}: phase1 {p:?} against random {r:?}\n{}\n{}",
            report(&phase1),
            report(&random)
        );
        // The weakness is found within the first few sessions and then
        // drilled far more than ordinary text would.
        assert!(
            phase1.learner.weakness_exposures() > random.learner.weakness_exposures(),
            "{}",
            report(&phase1)
        );
    }
}

#[test]
fn the_memorizer_is_not_credited_with_pattern_improvement() {
    let run = run(Kind::Memorizer, Scheduler::Phase1, 60, 1);
    let transfer = transfer(&run);
    let difference = transfer.difference().expect("both windows have slots");
    assert!(
        difference < 0.05,
        "practiced patterns sped up beyond other slots by {difference:+.3}\n{}",
        report(&run)
    );
    assert!(transfer.practiced.early.slots >= 100 && transfer.practiced.late.slots >= 100);
}

#[test]
fn the_permanently_awkward_transition_triggers_plateau() {
    let run = run(Kind::Awkward, Scheduler::Phase1, 40, 1);
    let pattern = Kind::Awkward.weakness().unwrap();
    let plateaued = run.plateaus.get(pattern);
    assert!(
        plateaued.is_some_and(|&session| session <= 30),
        "{pattern:?} did not plateau: {:?}\n{}",
        run.plateaus,
        report(&run)
    );
    // It was drilled before being backed off, and its weakness held.
    let targeted = run
        .sessions
        .iter()
        .filter(|s| {
            s.events
                .iter()
                .any(|e| e.target.pattern.as_ref() == pattern)
        })
        .count();
    assert!(targeted >= 4, "{}", report(&run));
    assert_eq!(run.learner.weakness_remaining(), Some(1.0));
}

/// The budget is asserted for an optimized build, the one a user runs;
/// an unoptimized test build is allowed ten times as long so that a gross
/// regression still shows there.
#[test]
fn the_end_of_session_pipeline_stays_within_its_budget_at_50_words() {
    let run = run(Kind::Trainable, Scheduler::Phase1, 20, 1);
    let budget_micros = if cfg!(debug_assertions) {
        1_000_000
    } else {
        100_000
    };
    assert!(
        run.max_pipeline_micros() <= budget_micros,
        "slowest end of session took {} ms\n{}",
        run.max_pipeline_micros() as f64 / 1000.0,
        report(&run)
    );
    let d = doses(&run);
    assert!(
        d.targets > 0 && d.mean_achieved() >= 4.0,
        "{}",
        report(&run)
    );
}

/// The learner that gets faster with every session whatever it practices.
/// A naive before-and-after estimate credits its targets with gain that
/// is regression to the mean; the randomized comparison against deferred
/// controls must not.
#[test]
fn the_null_learner_check_passes() {
    let run = run(Kind::Global, Scheduler::Phase1, 150, 1);
    let config = &run.options.config;
    let check = gain_check(&run);
    let text = report(&run);

    assert!(check.naive.count >= 300, "{text}");
    assert!(
        check.naive.mean > 0.02,
        "the naive estimate should show regression to the mean: {:+.3}\n{text}",
        check.naive.mean
    );
    assert!(
        check.drift.mean.abs() < 0.02,
        "drift should be near zero against the moving baseline: {:+.3}\n{text}",
        check.drift.mean
    );

    let corrected = &check.corrected;
    assert!(corrected.sessions >= 100, "{text}");
    let gain = corrected.gain(config).unwrap();
    let se = corrected.standard_error(config).unwrap();
    assert!(
        gain.abs() <= 3.0 * se,
        "corrected gain {gain:+.3} is beyond three standard errors of {se:.3}\n{text}"
    );
    // The speed component alone is precise enough to be held to a fixed
    // tolerance; the error component rests on a few dozen errors and is
    // held only to its standard error above.
    let speed =
        corrected.deferred.mean_residual().unwrap() - corrected.targets.mean_residual().unwrap();
    assert!(
        speed.abs() < 0.05,
        "the arms differ in speed by {speed:+.3} log-latency\n{text}"
    );
}

#[test]
fn a_seeded_run_is_reproducible() {
    let a = run(Kind::Fatigue, Scheduler::Phase1, 6, 9);
    let b = run(Kind::Fatigue, Scheduler::Phase1, 6, 9);
    for (x, y) in a.sessions.iter().zip(&b.sessions) {
        assert_eq!(x.composed, y.composed);
        assert_eq!(x.analysis, y.analysis);
        assert_eq!(x.events, y.events);
        assert_eq!(x.loss, y.loss);
    }
    assert_eq!(a.model, b.model);
    let c = run(Kind::Fatigue, Scheduler::Phase1, 6, 10);
    assert_ne!(a.sessions[0].composed.prompt, c.sessions[0].composed.prompt);
}

#[test]
fn every_scheduler_starts_with_a_probes_only_session_and_then_targets_or_not() {
    for scheduler in Scheduler::ALL {
        let run = run(Kind::Fatigue, scheduler, 3, 2);
        assert!(run.sessions[0].composed.targets.is_empty(), "{scheduler:?}");
        let later_targets = run.sessions[2].composed.targets.len();
        match scheduler {
            Scheduler::Random => assert_eq!(later_targets, 0),
            Scheduler::Weakest | Scheduler::Phase1 => assert!(later_targets > 0),
        }
    }
}
