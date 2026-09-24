//! `typ-sim`: runs a synthetic learner through a series of sessions and
//! reports what came of it.

use std::process::ExitCode;

use clap::Parser;
use typ_rs_core::model::SchedulerConfig;
use typ_sim::learner::Kind;
use typ_sim::run::{Options, simulate};
use typ_sim::schedule::Scheduler;

#[derive(Parser)]
#[command(name = "typ-sim", version, about)]
struct Cli {
    /// Which synthetic learner types: trainable, awkward, fatigue, global,
    /// or memorizer.
    #[arg(long, default_value = "trainable", value_parser = parse_learner)]
    learner: Kind,
    /// Which scheduler composes the prompts: random, weakest, or phase1.
    #[arg(long, default_value = "phase1", value_parser = parse_scheduler)]
    scheduler: Scheduler,
    #[arg(long, default_value_t = 40)]
    sessions: usize,
    #[arg(long, default_value_t = 50)]
    words: usize,
    /// Seeds the learner and every composition; the same seed reproduces
    /// the run exactly.
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Overrides one tunable, as `name=value`; repeatable. `--list-tunables`
    /// shows the names.
    #[arg(long = "set", value_name = "NAME=VALUE")]
    overrides: Vec<String>,
    /// Lists every tunable with its default and exits.
    #[arg(long)]
    list_tunables: bool,
}

fn parse_learner(s: &str) -> Result<Kind, String> {
    Kind::from_name(s).ok_or_else(|| {
        let names: Vec<&str> = Kind::ALL.iter().map(|k| k.name()).collect();
        format!("unknown learner {s:?}; one of {}", names.join(", "))
    })
}

fn parse_scheduler(s: &str) -> Result<Scheduler, String> {
    Scheduler::from_name(s).ok_or_else(|| {
        let names: Vec<&str> = Scheduler::ALL.iter().map(|k| k.name()).collect();
        format!("unknown scheduler {s:?}; one of {}", names.join(", "))
    })
}

/// Applies `name=value` overrides to the default config.
fn config_from(overrides: &[String]) -> Result<SchedulerConfig, String> {
    let mut config = SchedulerConfig::default();
    for pair in overrides {
        let (name, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("--set {pair:?} is not NAME=VALUE"))?;
        let value: f64 = value
            .trim()
            .parse()
            .map_err(|_| format!("--set {pair:?}: {value:?} is not a number"))?;
        config
            .set(name.trim(), value)
            .map_err(|e| format!("--set {pair:?}: {e}"))?;
    }
    Ok(config)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.list_tunables {
        for (name, value) in SchedulerConfig::default().tunables() {
            println!("{name} = {value}");
        }
        return ExitCode::SUCCESS;
    }
    let config = match config_from(&cli.overrides) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("typ-sim: {e}");
            return ExitCode::FAILURE;
        }
    };
    let run = simulate(Options {
        learner: cli.learner,
        scheduler: cli.scheduler,
        sessions: cli.sessions,
        words: cli.words,
        seed: cli.seed,
        config,
    });
    print!("{}", typ_sim::report::report(&run));
    ExitCode::SUCCESS
}
