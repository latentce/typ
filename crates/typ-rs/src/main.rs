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
use typ_rs_core::scheduler;
use typ_rs_core::session::{EndCondition, SEMANTICS_VERSION};
use typ_rs_store::{Profile, SessionId, SessionStart, Store, parse_words, unix_now};

/// Narrower than this and no useful prompt can be shown.
const MIN_COLUMNS: u16 = 20;

/// How many completed sessions `typ stats` lists.
const LISTED_SESSIONS: usize = 10;

const DATABASE_FILE: &str = "typ.db";

static VERSION: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}\n\
         Licence: MIT\n\
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
    /// List recent completed sessions, the patterns you are slowest, most
    /// error-prone, and weakest on, and the candidates being held back
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
        Some(Command::Replay { session_id }) => replay(session_id),
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
/// the user types. Afterwards the session is applied to the profile's
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

    let run = interactive::run(started.prompt, end, palette)?;

    // Save before printing, but compute the results first so that a
    // persistence failure still shows them, followed by the error. The
    // results come from the same analysis the statistics were built from,
    // unless the model could not even be loaded; then there is no next
    // prompt to report either.
    let ended_at = unix_now();
    let loaded = store
        .model(&profile)
        .and_then(|model| Ok((model, store.training_history(&profile)?)));
    let (results, saved) = match loaded {
        Ok((mut model, mut history)) => {
            let update = model.apply_session(&run.state, started_at, corpus, &config);
            let events = scheduler::achieved_doses(&run.state, &started.targets);
            history.record(&events, &config);
            let next = compose::next_prompt(
                &model,
                corpus,
                &config,
                &history,
                ended_at,
                stored_words,
                seed,
            );
            let results = report::results(&run.state, &update.analysis.metrics, Some(&next));
            let saved =
                store.finish_session(started.id, &run.state, &model, &events, &next, ended_at);
            (results, saved)
        }
        Err(e) => (
            report::results(&run.state, &analyze(&run.state).metrics, None),
            Err(e),
        ),
    };
    println!("{results}");
    if std::env::var_os("TYP_DIAGNOSTICS").is_some_and(|v| !v.is_empty()) {
        eprintln!("{}", render_diagnostics(&run.render_micros));
    }
    saved.map_err(|e| format!("the session was not saved: {e}"))?;
    Ok(())
}

fn stats(profile: Option<&str>) -> Result<(), Box<dyn Error>> {
    let mut store = open_store()?;
    let profile = open_profile(&mut store, profile)?;
    let sessions = store.completed_sessions(&profile, LISTED_SESSIONS)?;
    print!("{}", report::session_listing(&sessions));
    let model = store.model(&profile)?;
    let history = store.training_history_with_waiting_prompt(&profile)?;
    print!(
        "{}",
        report::pattern_summary(&model, Corpus::bundled(), store.config(), &history)
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
fn replay(session_id: SessionId) -> Result<(), Box<dyn Error>> {
    let store = open_store()?;
    let session = store.session(session_id)?;
    if session.semantics_version != SEMANTICS_VERSION {
        eprintln!(
            "typ: session {session_id} was recorded under editing rules version {}; replaying under version {SEMANTICS_VERSION}",
            session.semantics_version
        );
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

    #[test]
    fn render_diagnostics_summarise_the_batches() {
        assert_eq!(
            render_diagnostics(&[100, 300, 200]),
            "render: 3 batches, mean 200 µs, max 300 µs"
        );
        assert_eq!(
            render_diagnostics(&[]),
            "render: 0 batches, mean 0 µs, max 0 µs"
        );
    }
}
