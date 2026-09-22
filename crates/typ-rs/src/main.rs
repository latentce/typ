mod input;
mod interactive;
mod render;
mod terminal;

use std::error::Error;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::LazyLock;

use clap::{Parser, Subcommand};
use typ_rs_core::compose;
use typ_rs_core::corpus::{CORPUS_VERSION, Corpus};
use typ_rs_core::display::Palette;
use typ_rs_core::metrics::{final_accuracy, gross_wpm};
use typ_rs_core::session::{EndCondition, Outcome, SessionState};
use typ_rs_store::{DEFAULT_PROFILE, SessionStart, Store, StoredSession, unix_now};

const PROMPT_WORDS: usize = 50;

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
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List recent completed sessions
    Stats,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let outcome = match cli.command {
        None => session(),
        Some(Command::Stats) => stats(),
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
/// the user types.
fn session() -> Result<(), Box<dyn Error>> {
    check_terminal()?;
    let mut store = open_store()?;
    let profile = store.profile(DEFAULT_PROFILE)?;
    let corpus = Corpus::bundled();
    let seed = getrandom::u64()?;
    let fallback_seed = getrandom::u64()?;
    let started = store.start_session(
        &profile,
        SessionStart {
            started_at: unix_now(),
            seed,
        },
        || compose::frequency_weighted(corpus, PROMPT_WORDS, fallback_seed),
    )?;
    let end = EndCondition::AfterWords(started.prompt.word_count());
    let palette = Palette::from_no_color(std::env::var("NO_COLOR").ok().as_deref());

    let run = interactive::run(started.prompt, end, palette)?;

    // Save before printing, but compute the results first so that a
    // persistence failure still shows them, followed by the error.
    let results = results(&run.state);
    let next = compose::frequency_weighted(corpus, PROMPT_WORDS, seed);
    let saved = store.finish_session(started.id, &run.state, next, unix_now());
    println!("{results}");
    if std::env::var_os("TYP_DIAGNOSTICS").is_some_and(|v| !v.is_empty()) {
        eprintln!("{}", render_diagnostics(&run.render_micros));
    }
    saved.map_err(|e| format!("the session was not saved: {e}"))?;
    Ok(())
}

fn stats() -> Result<(), Box<dyn Error>> {
    let store = open_store()?;
    let profile = store.profile(DEFAULT_PROFILE)?;
    let sessions = store.completed_sessions(&profile, LISTED_SESSIONS)?;
    print!("{}", session_listing(&sessions));
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

/// The line printed when a session ends. Speed is reported only for a
/// completed session: a partial prompt has no meaningful WPM.
fn results(state: &SessionState) -> String {
    match state.outcome() {
        Some(Outcome::Completed) => {
            let wpm = gross_wpm(state).unwrap_or(0.0);
            let accuracy = 100.0 * final_accuracy(state);
            format!("{wpm:.0} wpm  {accuracy:.1}% accuracy")
        }
        _ => {
            let words = state.words_completed();
            let noun = if words == 1 { "word" } else { "words" };
            format!("interrupted after {words} {noun}")
        }
    }
}

/// One line per completed session, most recent first, with the same figures
/// the session printed when it ended.
fn session_listing(sessions: &[StoredSession]) -> String {
    if sessions.is_empty() {
        return "no completed sessions yet\n".to_string();
    }
    sessions
        .iter()
        .map(|session| {
            let state = session.replay();
            let wpm = gross_wpm(&state).unwrap_or(0.0);
            let accuracy = 100.0 * final_accuracy(&state);
            format!(
                "{}  {:>3} words  {wpm:>3.0} wpm  {accuracy:>5.1}% accuracy\n",
                session.started_at_local,
                session.prompt.word_count()
            )
        })
        .collect()
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
    use typ_rs_core::prompt::Prompt;
    use typ_rs_core::session::{Input, Key};

    fn run(prompt: &str, script: &str) -> SessionState {
        let mut state = SessionState::new(
            Prompt::new(prompt.split(' ')),
            EndCondition::AfterWords(usize::MAX),
        );
        for (i, c) in script.chars().enumerate() {
            let key = if c == '⎋' {
                Key::Interrupt
            } else {
                Key::Char(c)
            };
            // One keystroke every 100 ms.
            state.apply_event(Input::new(i as u64 * 100_000, key));
        }
        state
    }

    #[test]
    fn a_completed_session_reports_gross_wpm_and_final_accuracy() {
        // 6 final characters ("cat dg") over 0.6 s is 120 wpm; "dg" for
        // "dog" leaves 4 of 6 target characters correct.
        let state = run("cat dog", "cat dg ");
        assert_eq!(results(&state), "120 wpm  66.7% accuracy");
    }

    #[test]
    fn an_interrupted_session_reports_words_completed_and_no_speed() {
        assert_eq!(
            results(&run("cat dog fox", "cat do⎋")),
            "interrupted after 1 word"
        );
        assert_eq!(
            results(&run("cat dog fox", "cat dog ⎋")),
            "interrupted after 2 words"
        );
        assert_eq!(
            results(&run("cat dog fox", "⎋")),
            "interrupted after 0 words"
        );
    }

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
