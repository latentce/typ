mod input;
mod interactive;
mod render;
mod report;
mod terminal;

use std::error::Error;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::LazyLock;

use clap::{Parser, Subcommand, ValueEnum};
use typ_rs_core::analysis::analyze;
use typ_rs_core::compose;
use typ_rs_core::corpus::{CORPUS_VERSION, Corpus};
use typ_rs_core::display::Palette;
use typ_rs_core::metrics::summarize;
use typ_rs_core::scheduler;
use typ_rs_core::session::{EndCondition, SEMANTICS_VERSION, SessionState};
use typ_rs_store::{
    Profile, SessionEnd, SessionId, SessionStart, StartedSession, Store, parse_words, unix_now,
};

/// Narrower than this and no useful prompt can be shown.
const MIN_COLUMNS: u16 = 20;

/// How many completed sessions `typ stats` lists.
const LISTED_SESSIONS: usize = 10;

/// How many completed sessions `typ stats` reads probe words from: enough
/// for two full probe windows once prompts are mostly targeted.
const PROBE_SESSIONS: usize = 50;

const DATABASE_FILE: &str = "typ.db";

static VERSION: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}\n\
         License: MIT\n\
         Corpus: English words (corpus version {CORPUS_VERSION}) derived from the\n\
         Google Books Ngram Viewer Exports v3 via orgtre/google-books-ngram-frequency,\n\
         licensed CC BY 3.0 <https://creativecommons.org/licenses/by/3.0/>.",
        env!("CARGO_PKG_VERSION")
    )
});

/// A local terminal typing trainer that targets the character patterns you are weak on.
#[derive(Parser)]
#[command(name = "typ", version = VERSION.as_str())]
struct Cli {
    /// Words in this session, instead of the profile's setting
    #[arg(long, value_name = "N")]
    words: Option<String>,
    /// Use this profile for this run, instead of the active one; created
    /// if it does not exist
    #[arg(long, global = true, value_name = "NAME")]
    profile: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show recent sessions, probe and word-initiation trends, transfer to
    /// untargeted words, the patterns you are weakest on, and the
    /// candidates being held back
    Stats,
    /// Show a setting, or set it for every run to come
    Config {
        key: SettingKey,
        /// The new value; omit it to show the current one
        value: Option<String>,
    },
    /// Recompute every statistic from the stored sessions
    Rebuild,
    /// Show how a stored session was interpreted: every word's first
    /// attempt and attributed errors, and every interval's classification
    Replay {
        /// The session id, as listed by `typ stats`
        session_id: SessionId,
        /// Instead, show where the current pipeline's figures for the
        /// session differ from the ones stored for it
        #[arg(long)]
        diff: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum SettingKey {
    /// Words per session, 10 to 200
    Words,
    /// The profile's keyboard layout; fixed once a session has been typed
    /// on the profile
    Layout,
    /// The profile used when --profile is not given; created if new
    Profile,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let profile = cli.profile.as_deref();
    let outcome = match cli.command {
        None => session(cli.words.as_deref(), profile),
        Some(_) if cli.words.is_some() => Err("--words applies only to a session".into()),
        Some(Command::Stats) => stats(profile),
        Some(Command::Config { key, value }) => config(profile, key, value),
        Some(Command::Rebuild | Command::Replay { .. }) if profile.is_some() => {
            Err("--profile applies only to a session, stats, or config".into())
        }
        Some(Command::Rebuild) => rebuild(),
        Some(Command::Replay { session_id, diff }) => replay(session_id, diff),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("typ: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Runs one session: the row is written before raw mode is entered and the
/// events are saved after it is left, so the database is never touched while
/// the user types. Afterward the session is applied to the profile's
/// pattern statistics, what came of its targets is recorded, the next
/// prompt is composed from the updated model, and everything is saved in
/// one transaction. The prompt composed ahead for the next run is sized for
/// the stored setting, not for a `--words` override, which touches this
/// session only; a prompt composed on the spot because none fit is targeted
/// all the same. The override is parsed here rather than by clap so that a
/// bad value is refused on one line, as `typ config words` refuses it.
fn session(words: Option<&str>, profile: Option<&str>) -> Result<(), Box<dyn Error>> {
    let words = words.map(parse_words).transpose()?;
    check_terminal()?;
    let mut store = open_store()?;
    let profile = open_profile(&mut store, profile)?;
    let stored_words = store.words(&profile)?;
    let words = words.unwrap_or(stored_words);
    let corpus = Corpus::bundled();
    let config = *store.config();
    let seed = getrandom::u64()?;
    let fallback_seed = getrandom::u64()?;
    let started_at = unix_now();
    let start = SessionStart {
        started_at,
        seed,
        word_count: words,
    };
    let model_at_start = store.model(&profile)?;
    let history_at_start = store.training_history(&profile)?;
    let started = store.start_session(&profile, start, || {
        compose::next_prompt(
            &model_at_start,
            corpus,
            &config,
            &history_at_start,
            started_at,
            words,
            fallback_seed,
        )
    })?;
    let end = EndCondition::AfterWords(started.prompt.word_count());
    let palette = Palette::from_no_color(std::env::var("NO_COLOR").ok().as_deref());

    let run = interactive::run(started.prompt.clone(), end, palette)?;

    let ended = end_session(
        &mut store,
        &profile,
        &FinishedRun {
            started: &started,
            state: &run.state,
            started_at,
            ended_at: unix_now(),
            next_words: stored_words,
            seed,
        },
    );
    println!("{}", ended.results);
    if std::env::var_os("TYP_DIAGNOSTICS").is_some_and(|v| !v.is_empty()) {
        eprintln!("{}", render_diagnostics(&run.render_micros));
    }
    ended
        .saved
        .map_err(|e| format!("the session was not saved: {e}"))?;
    Ok(())
}

/// A session that has been typed, with what its end needs to know.
struct FinishedRun<'a> {
    started: &'a StartedSession,
    state: &'a SessionState,
    /// Wall clock, Unix seconds.
    started_at: i64,
    ended_at: i64,
    /// How many words the next prompt is to have.
    next_words: usize,
    /// Seeds the composition of the next prompt.
    seed: u64,
}

/// What ending a session produced: the results block, and whether the
/// session was saved.
struct Ended {
    results: String,
    saved: typ_rs_store::Result<()>,
}

/// Ends a finished session: applies it to the profile's model, records
/// what came of its targets, composes the next prompt from the updated
/// model, summarizes the session against the recent series, saves
/// everything in one transaction, and returns the results block to print.
/// The results are composed before the save so that a persistence failure
/// still shows them; they come from the same analysis the statistics were
/// built from, unless the model could not even be loaded, when there is no
/// next prompt to report either.
fn end_session(store: &mut Store, profile: &Profile, run: &FinishedRun) -> Ended {
    let corpus = Corpus::bundled();
    let config = *store.config();
    let state = run.state;
    let loaded = store.model(profile).and_then(|model| {
        Ok((
            model,
            store.training_history(profile)?,
            store.recent_series(profile)?,
        ))
    });
    match loaded {
        Ok((mut model, mut history, recent)) => {
            let update = model.apply_session(state, run.started_at, corpus, &config);
            let events = scheduler::achieved_doses(state, &run.started.targets);
            history.record(
                &events,
                run.started.targeted_words.iter().map(AsRef::as_ref),
                &config,
            );
            let next = compose::next_prompt(
                &model,
                corpus,
                &config,
                &history,
                run.ended_at,
                run.next_words,
                run.seed,
            );
            let summary = summarize(state, &update, &run.started.words, recent.as_ref(), &config);
            let results = report::results(&report::Ending {
                state,
                metrics: &update.analysis.metrics,
                summary: summary.as_ref(),
                previous: recent.as_ref(),
                observations_saved: Some(update.applied.is_some()),
                next: Some(&next),
            });
            let saved = store.finish_session(
                run.started.id,
                &SessionEnd {
                    state,
                    model: &model,
                    events: &events,
                    summary: summary.as_ref(),
                    next_prompt: &next,
                    ended_at: run.ended_at,
                },
            );
            Ended { results, saved }
        }
        Err(e) => Ended {
            results: report::results(&report::Ending {
                state,
                metrics: &analyze(state).metrics,
                summary: None,
                previous: None,
                observations_saved: None,
                next: None,
            }),
            saved: Err(e),
        },
    }
}

fn stats(profile: Option<&str>) -> Result<(), Box<dyn Error>> {
    let mut store = open_store()?;
    let profile = open_profile(&mut store, profile)?;
    let sessions = store.completed_sessions(&profile, PROBE_SESSIONS)?;
    let listed = &sessions[..sessions.len().min(LISTED_SESSIONS)];
    print!("{}", report::session_listing(listed));
    let analyses = report::analyses(&sessions);
    let performances = report::performances(&sessions, &analyses);
    print!("{}", report::probe_section(&performances));
    print!(
        "{}",
        report::word_initiation_line(&performances[..listed.len()])
    );
    let ended_history = store.training_history(&profile)?;
    print!(
        "{}",
        report::transfer_section(&sessions, &analyses, &ended_history, store.config())
    );
    let model = store.model(&profile)?;
    let history_with_waiting = store.training_history_with_waiting_prompt(&profile)?;
    print!(
        "{}",
        report::pattern_summary(
            &model,
            Corpus::bundled(),
            store.config(),
            &history_with_waiting
        )
    );
    Ok(())
}

/// Shows a setting's current value, or sets it. `words` and `layout` are
/// the profile's (the active one, or `--profile`); `profile` is the active
/// profile itself, which `--profile` does not touch.
fn config(
    profile: Option<&str>,
    key: SettingKey,
    value: Option<String>,
) -> Result<(), Box<dyn Error>> {
    let mut store = open_store()?;
    let profile = open_profile(&mut store, profile)?;
    match (key, value) {
        (SettingKey::Words, None) => println!("{}", store.words(&profile)?),
        (SettingKey::Words, Some(value)) => store.set_words(&profile, parse_words(&value)?)?,
        (SettingKey::Layout, None) => println!("{}", profile.layout),
        (SettingKey::Layout, Some(value)) => store.set_layout(&profile, &value)?,
        (SettingKey::Profile, None) => println!("{}", store.active_profile()?),
        (SettingKey::Profile, Some(value)) => store.set_active_profile(&value)?,
    }
    Ok(())
}

fn rebuild() -> Result<(), Box<dyn Error>> {
    let mut store = open_store()?;
    let sessions = store.rebuild()?;
    println!(
        "rebuilt the statistics from {sessions} {}",
        if sessions == 1 { "session" } else { "sessions" }
    );
    Ok(())
}

/// Runs a stored session's events through the current analysis. The events
/// are applied under the current editing rules, so a session recorded under
/// older ones is flagged: its interpretation may differ from what the user
/// saw. The hesitation threshold follows the session's own running median,
/// since the user baseline in force when it was applied is not stored, so a
/// borderline pause may class differently here than it did in the model.
/// With `--diff`, the profile's sessions are instead replayed through the
/// current model up to this one and its figures compared with the stored
/// summary.
fn replay(session_id: SessionId, diff: bool) -> Result<(), Box<dyn Error>> {
    let store = open_store()?;
    let session = store.session(session_id)?;
    if session.semantics_version != SEMANTICS_VERSION {
        eprintln!(
            "typ: session {session_id} was recorded under editing rules version {}; replaying under version {SEMANTICS_VERSION}",
            session.semantics_version
        );
    }
    if diff {
        let recomputed = store.recompute_summary(session_id)?;
        print!(
            "{}",
            report::summary_diff(recomputed.current.as_ref(), recomputed.stored.as_ref())
        );
        return Ok(());
    }
    let state = session.replay();
    let analysis = analyze(&state);
    print!("{}", report::replay(&session, &state, &analysis));
    Ok(())
}

/// A session needs a real terminal that is wide enough to show a prompt.
fn check_terminal() -> Result<(), String> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err("needs an interactive terminal".to_string());
    }
    let (columns, _) = crossterm::terminal::size()
        .map_err(|e| format!("could not determine the terminal size: {e}"))?;
    if columns < MIN_COLUMNS {
        return Err(format!(
            "needs a terminal at least {MIN_COLUMNS} columns wide; this one is {columns}"
        ));
    }
    Ok(())
}

fn open_store() -> Result<Store, Box<dyn Error>> {
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = dir.join(DATABASE_FILE);
    Store::open(&path).map_err(|e| format!("could not open {}: {e}", path.display()).into())
}

/// The profile this run works with: the one named by `--profile`, otherwise
/// the active one. Either is created on first use.
fn open_profile(store: &mut Store, name: Option<&str>) -> Result<Profile, Box<dyn Error>> {
    let name = match name {
        Some(name) => name.to_string(),
        None => store.active_profile()?,
    };
    Ok(store.profile_or_create(&name)?)
}

/// `$TYP_DATA_DIR` if set, otherwise `typ` under the platform's data
/// directory (`~/.local/share` on Linux).
fn data_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("TYP_DATA_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::data_dir()
        .map(|d| d.join("typ"))
        .ok_or_else(|| "could not determine the data directory; set TYP_DATA_DIR".to_string())
}

fn render_diagnostics(render_micros: &[u64]) -> String {
    let batches = render_micros.len();
    let total: u64 = render_micros.iter().sum();
    let mean = if batches == 0 {
        0
    } else {
        total / batches as u64
    };
    let max = render_micros.iter().copied().max().unwrap_or(0);
    format!("render: {batches} batches, mean {mean} µs, max {max} µs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use typ_rs_core::compose::ComposedPrompt;
    use typ_rs_core::prompt::Prompt;
    use typ_rs_core::session::{Input, Key};
    use typ_rs_store::DEFAULT_PROFILE;

    #[test]
    fn render_diagnostics_summarize_the_batches() {
        assert_eq!(
            render_diagnostics(&[100, 300, 200]),
            "render: 3 batches, mean 200 µs, max 300 µs"
        );
        assert_eq!(
            render_diagnostics(&[]),
            "render: 0 batches, mean 0 µs, max 0 µs"
        );
    }

    #[test]
    fn the_results_are_composed_before_the_save_and_survive_a_store_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        let profile = store.profile(DEFAULT_PROFILE).unwrap();
        let prompt = || Prompt::new(["cat", "dog"]);
        let started = store
            .start_session(
                &profile,
                SessionStart {
                    started_at: 1_000,
                    seed: 1,
                    word_count: 2,
                },
                || ComposedPrompt::probes(prompt()),
            )
            .unwrap();
        let mut state = SessionState::new(prompt(), EndCondition::AfterWords(2));
        for (i, c) in "cat dog".chars().enumerate() {
            state.apply_event(Input::new(i as u64 * 100_000, Key::Char(c)));
        }

        let run = FinishedRun {
            started: &started,
            state: &state,
            started_at: 1_000,
            ended_at: 1_060,
            next_words: 2,
            seed: 7,
        };
        let ended = end_session(&mut store, &profile, &run);
        assert!(ended.saved.is_ok(), "{:?}", ended.saved);
        let lines: Vec<&str> = ended.results.lines().collect();
        assert_eq!(
            lines[0],
            "140 wpm  100.0% raw  100.0% final  100% consistency"
        );
        assert!(
            lines[1].ends_with(" wpm on standard text  baseline recorded"),
            "{}",
            lines[1]
        );
        assert!(lines[2].starts_with("next: "), "{}", lines[2]);

        // Ending the same session again fails to save, but the results
        // are there to print all the same, with the model's next targets.
        let again = end_session(&mut store, &profile, &run);
        assert!(again.saved.is_err());
        let lines: Vec<&str> = again.results.lines().collect();
        assert_eq!(
            lines[0],
            "140 wpm  100.0% raw  100.0% final  100% consistency"
        );
        assert!(lines[1].contains(" vs recent"), "{}", lines[1]);
        assert!(lines[2].starts_with("next: "), "{}", lines[2]);
    }
}
