use std::path::Path;
use std::process::Command;

use typ_rs_core::corpus::CORPUS_VERSION;
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{EndCondition, Input, Key, SessionState};
use typ_rs_store::{DEFAULT_PROFILE, SessionStart, Store};

struct Run {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// Runs `typ` with stdin closed and stdout captured, so it is not attached
/// to a terminal, against a database under `data_dir`. The time zone is fixed
/// so that listed times are predictable.
fn typ_in(data_dir: &Path, args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_typ"))
        .args(args)
        .env("TYP_DATA_DIR", data_dir)
        .env("TZ", "UTC")
        .output()
        .expect("typ binary runs");
    Run {
        ok: output.status.success(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

fn typ(args: &[&str]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    typ_in(dir.path(), args)
}

/// Types `script` against `prompt`, one key per 100 ms, and stores the
/// session as started at `started_at`. Whatever prompt is waiting is
/// replaced by `prompt` again, so every session in a test types `prompt`.
fn store_session(store: &mut Store, started_at: i64, prompt: &str, script: &str) {
    let profile = store.profile(DEFAULT_PROFILE).unwrap();
    let words = || Prompt::new(prompt.split(' '));
    let started = store
        .start_session(
            &profile,
            SessionStart {
                started_at,
                seed: 1,
            },
            words,
        )
        .unwrap();
    assert_eq!(started.prompt, words());
    let mut state = SessionState::new(
        started.prompt.clone(),
        EndCondition::AfterWords(started.prompt.word_count()),
    );
    for (i, c) in script.chars().enumerate() {
        let key = if c == '⎋' {
            Key::Interrupt
        } else {
            Key::Char(c)
        };
        state.apply_event(Input::new(i as u64 * 100_000, key));
    }
    store
        .finish_session(started.id, &state, words(), started_at + 60)
        .unwrap();
}

#[test]
fn version_shows_the_licence_and_the_corpus_attribution_under_both_flags() {
    for flag in ["--version", "-V"] {
        let run = typ(&[flag]);
        let stdout = &run.stdout;

        assert!(run.ok);
        assert!(
            stdout.starts_with(&format!("typ {}\n", env!("CARGO_PKG_VERSION"))),
            "{flag}: {stdout}"
        );
        assert!(stdout.contains("Licence: MIT"), "{flag}: {stdout}");
        assert!(stdout.contains("Google Books"), "{flag}: {stdout}");
        assert!(stdout.contains("CC BY 3.0"), "{flag}: {stdout}");
        assert!(
            stdout.contains(&format!("corpus version {CORPUS_VERSION}")),
            "{flag}: {stdout}"
        );
    }
}

#[test]
fn a_session_refuses_to_start_without_an_interactive_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let run = typ_in(dir.path(), &[]);

    assert!(!run.ok);
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr.lines().count(), 1, "{:?}", run.stderr);
    assert!(
        run.stderr.contains("interactive terminal"),
        "{:?}",
        run.stderr
    );
    assert!(!dir.path().join("typ.db").exists());
}

#[test]
fn stats_on_a_fresh_data_directory_creates_the_database_and_lists_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let run = typ_in(dir.path(), &["stats"]);

    assert!(run.ok, "{}", run.stderr);
    assert_eq!(run.stdout, "no completed sessions yet\n");
    assert_eq!(run.stderr, "");
    assert!(dir.path().join("typ.db").is_file());
}

#[test]
fn stats_lists_completed_sessions_most_recent_first_and_skips_interrupted_ones() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        // 2024-01-15 10:30:00 UTC: "cat dg" is 6 final characters over 0.6 s.
        store_session(&mut store, 1_705_314_600, "cat dog", "cat dg ");
        // 2024-01-16 08:00:00 UTC: 7 characters over 0.6 s, all correct.
        store_session(&mut store, 1_705_392_000, "cat dog", "cat dog");
        store_session(&mut store, 1_705_400_000, "cat dog", "ca⎋");
    }

    let run = typ_in(dir.path(), &["stats"]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "2024-01-16 08:00    2 words  140 wpm  100.0% accuracy\n\
         2024-01-15 10:30    2 words  120 wpm   66.7% accuracy\n"
    );
}
