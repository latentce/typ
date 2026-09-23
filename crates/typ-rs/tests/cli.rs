use std::path::Path;
use std::process::Command;

use typ_rs_core::compose::ComposedPrompt;
use typ_rs_core::corpus::{CORPUS_VERSION, Corpus};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::scheduler::{SelectedTarget, TargetRole, achieved_doses};
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
/// session as started at `started_at`, applied to the profile's statistics
/// the way a live session is. Whatever prompt is waiting is replaced by
/// `prompt` again, as probes only, so every session in a test types
/// `prompt`. `⌫` is backspace and `⎋` an interrupt.
fn store_session(store: &mut Store, started_at: i64, prompt: &str, script: &str) {
    store_session_with(store, started_at, prompt, script, Vec::new());
}

/// As [`store_session`], with the given patterns selected for the prompt
/// waiting afterwards.
fn store_session_with(
    store: &mut Store,
    started_at: i64,
    prompt: &str,
    script: &str,
    next_targets: Vec<SelectedTarget>,
) {
    let profile = store.profile(DEFAULT_PROFILE).unwrap();
    let words = || Prompt::new(prompt.split(' '));
    let started = store
        .start_session(
            &profile,
            SessionStart {
                started_at,
                seed: 1,
                word_count: words().word_count(),
            },
            || ComposedPrompt::probes(words()),
        )
        .unwrap();
    assert_eq!(started.prompt, words());
    let mut state = SessionState::new(
        started.prompt.clone(),
        EndCondition::AfterWords(started.prompt.word_count()),
    );
    for (i, c) in script.chars().enumerate() {
        let key = match c {
            '⎋' => Key::Interrupt,
            '⌫' => Key::Backspace,
            c => Key::Char(c),
        };
        state.apply_event(Input::new(i as u64 * 100_000, key));
    }
    let mut model = store.model(&profile).unwrap();
    model.apply_session(&state, started_at, Corpus::bundled(), store.config());
    let events = achieved_doses(&state, &started.targets);
    let mut next = ComposedPrompt::probes(words());
    next.targets = next_targets;
    store
        .finish_session(started.id, &state, &model, &events, &next, started_at + 60)
        .unwrap();
}

fn target(pattern: &str, role: TargetRole) -> SelectedTarget {
    SelectedTarget {
        pattern: pattern.into(),
        role,
        weakness_mean: 0.5,
        weakness_sd: 0.2,
        priority: 0.1,
        planned_dose: 6,
    }
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
    let mut sections = run.stdout.split("\n\n");
    assert_eq!(
        sections.next().unwrap(),
        "   2  2024-01-16 08:00    2 words  140 wpm  100.0% accuracy\n\
         \x20  1  2024-01-15 10:30    2 words  120 wpm   66.7% accuracy"
    );
    let slowest = sections.next().unwrap();
    assert!(slowest.starts_with("slowest patterns\n  "), "{slowest}");
    // Every keystroke took 100 ms, so nothing is slower than the baseline.
    assert!(
        slowest
            .lines()
            .skip(1)
            .all(|l| l.contains("   +0%  n_eff ")),
        "{slowest}"
    );
    let errors = sections.next().unwrap();
    // `dg` for `dog` omitted the `o`: the pattern ending there heads the
    // list, with the whole chain behind it.
    assert!(
        errors.starts_with("most error-prone patterns\n  ␣do  "),
        "{errors}"
    );
    assert!(
        errors.lines().nth(2).unwrap().starts_with("  do  "),
        "{errors}"
    );
    assert!(
        errors.lines().nth(3).unwrap().starts_with("  o   "),
        "{errors}"
    );
    let weakest = sections.next().unwrap();
    assert!(
        weakest.starts_with("weakest patterns\n  ␣do  +"),
        "{weakest}"
    );
    assert!(weakest.lines().nth(1).unwrap().contains(" ± "), "{weakest}");
    assert_eq!(sections.next(), None);
}

#[test]
fn stats_shows_the_deferred_candidates_with_their_windows() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_314_600, "cat dog", "cat dog");
        // The prompt composed ahead defers `og` and targets `at`.
        store_session_with(
            &mut store,
            1_705_392_000,
            "cat dog",
            "cat dog",
            vec![
                target("at", TargetRole::Target),
                target("og", TargetRole::Deferred),
            ],
        );
    }

    let run = typ_in(dir.path(), &["stats"]);
    assert!(run.ok, "{}", run.stderr);
    let deferred = run.stdout.split("\n\n").last().unwrap();
    assert_eq!(
        deferred,
        "deferred candidates\n  og   2 sessions remaining\n"
    );
}

#[test]
fn rebuild_reports_how_many_sessions_it_reapplied_and_changes_nothing_visible() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_314_600, "cat dog", "cat dg ");
        store_session(&mut store, 1_705_392_000, "cat dog", "cat dog");
        store_session(&mut store, 1_705_400_000, "cat dog", "ca⎋");
    }
    let before = typ_in(dir.path(), &["stats"]);

    let run = typ_in(dir.path(), &["rebuild"]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(run.stdout, "rebuilt the statistics from 3 sessions\n");
    assert_eq!(run.stderr, "");

    let after = typ_in(dir.path(), &["stats"]);
    assert_eq!(after.stdout, before.stdout);

    let run = typ(&["rebuild"]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(run.stdout, "rebuilt the statistics from 0 sessions\n");
}

#[test]
fn replay_shows_how_every_word_and_interval_of_a_stored_session_was_interpreted() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_314_600, "cat dog", "cxt⌫⌫at dog");
    }

    let run = typ_in(dir.path(), &["replay", "1"]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "session 1  2024-01-15 10:30  completed  2 words\n\
         84 wpm  83.3% raw  100.0% final  100% consistency\n\
         4 corrections  0 uncorrected  error latency 300 ms  5 clean intervals\n\
         \n\
         words\n\
         \x20 0 cat  first attempt \"cxt\"  history \"cxtat\"  67% raw\n\
         \x20     substitution x at 1 → \" ca\"\n\
         \x20 1 dog  first attempt \"dog\"  100% raw\n\
         \n\
         intervals\n\
         \x20seq       at  latency  slot  key  class\n\
         \x20  0        0        -   0:0  c    excluded: first_of_session\n\
         \x20  1      100      100   0:1  x    clean\n\
         \x20  2      200      100   0:2  t    clean\n\
         \x20  3      300      100   0:3  ⌫    excluded: backspace\n\
         \x20  4      400      100   0:2  ⌫    excluded: backspace, after_correction\n\
         \x20  5      500      100   0:1  a    excluded: replacement, after_correction\n\
         \x20  6      600      100   0:2  t    excluded: replacement, after_correction\n\
         \x20  7      700      100   0:3  ␣    excluded: after_correction\n\
         \x20  8      800      100   1:0  d    clean\n\
         \x20  9      900      100   1:1  o    clean\n\
         \x20 10     1000      100   1:2  g    clean\n"
    );
}

#[test]
fn replay_of_an_interrupted_session_marks_the_unsubmitted_word_and_shows_no_speed() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_314_600, "cat dog fox", "cat do⎋");
    }

    let run = typ_in(dir.path(), &["replay", "1"]);
    assert!(run.ok, "{}", run.stderr);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        lines[0],
        "session 1  2024-01-15 10:30  interrupted  3 words"
    );
    assert_eq!(lines[1], "interrupted after 1 word");
    assert_eq!(lines[6], "  1 dog  first attempt \"do\"  (not submitted)");
    assert!(!run.stdout.contains("fox"), "{}", run.stdout);
}

#[test]
fn replay_of_an_unknown_session_fails_with_one_line() {
    let run = typ(&["replay", "7"]);
    assert!(!run.ok);
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr, "typ: no session 7\n");
}

// --- Configuration -----------------------------------------------------------

/// Runs `typ` expecting success with nothing on stderr; returns stdout.
fn ok(data_dir: &Path, args: &[&str]) -> String {
    let run = typ_in(data_dir, args);
    assert!(run.ok, "{args:?}: {}", run.stderr);
    assert_eq!(run.stderr, "", "{args:?}");
    run.stdout
}

/// Runs `typ` expecting failure with one line on stderr and nothing on
/// stdout; returns that line.
fn one_line_error(data_dir: &Path, args: &[&str]) -> String {
    let run = typ_in(data_dir, args);
    assert!(!run.ok, "{args:?}: {}", run.stdout);
    assert_eq!(run.stdout, "", "{args:?}");
    assert_eq!(run.stderr.lines().count(), 1, "{args:?}: {:?}", run.stderr);
    run.stderr
}

#[test]
fn config_shows_the_defaults_on_a_fresh_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(ok(dir.path(), &["config", "words"]), "50\n");
    assert_eq!(ok(dir.path(), &["config", "layout"]), "qwerty\n");
    assert_eq!(ok(dir.path(), &["config", "profile"]), "default\n");
}

#[test]
fn config_sets_a_value_silently_and_reads_it_back() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(ok(dir.path(), &["config", "words", "30"]), "");
    assert_eq!(ok(dir.path(), &["config", "words"]), "30\n");
    assert_eq!(ok(dir.path(), &["config", "layout", "qwerty"]), "");
    assert_eq!(ok(dir.path(), &["config", "layout"]), "qwerty\n");
    assert_eq!(ok(dir.path(), &["config", "profile", "alt"]), "");
    assert_eq!(ok(dir.path(), &["config", "profile"]), "alt\n");
}

#[test]
fn config_rejects_invalid_values_with_one_line_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    ok(dir.path(), &["config", "words", "30"]);

    for bad in ["5", "201", "abc", "30.0"] {
        let line = one_line_error(dir.path(), &["config", "words", bad]);
        assert!(line.starts_with("typ: "), "{line}");
        assert!(line.contains("10") && line.contains("200"), "{line}");
        assert!(line.contains(bad), "{line}");
    }
    let line = one_line_error(dir.path(), &["config", "layout", "dvorak"]);
    assert!(line.contains("dvorak") && line.contains("qwerty"), "{line}");
    let line = one_line_error(dir.path(), &["config", "profile", "my profile"]);
    assert!(line.contains("my profile"), "{line}");

    assert_eq!(ok(dir.path(), &["config", "words"]), "30\n");
    assert_eq!(ok(dir.path(), &["config", "layout"]), "qwerty\n");
    assert_eq!(ok(dir.path(), &["config", "profile"]), "default\n");
}

#[test]
fn an_unknown_setting_fails() {
    let run = typ(&["config", "colour"]);
    assert!(!run.ok);
    assert!(run.stderr.contains("colour"), "{}", run.stderr);
}

#[test]
fn switching_profile_switches_whose_settings_and_sessions_are_shown() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_392_000, "cat dog", "cat dog");
    }
    ok(dir.path(), &["config", "words", "30"]);
    assert!(ok(dir.path(), &["stats"]).starts_with("   1  2024-01-16"));

    ok(dir.path(), &["config", "profile", "alt"]);
    assert_eq!(ok(dir.path(), &["config", "words"]), "50\n");
    assert_eq!(ok(dir.path(), &["stats"]), "no completed sessions yet\n");

    ok(dir.path(), &["config", "profile", "default"]);
    assert_eq!(ok(dir.path(), &["config", "words"]), "30\n");
    assert!(ok(dir.path(), &["stats"]).starts_with("   1  2024-01-16"));
}

#[test]
fn the_profile_flag_applies_to_one_run_and_does_not_change_the_active_profile() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_392_000, "cat dog", "cat dog");
    }
    ok(dir.path(), &["--profile", "alt", "config", "words", "30"]);
    assert_eq!(ok(dir.path(), &["config", "profile"]), "default\n");
    assert_eq!(ok(dir.path(), &["config", "words"]), "50\n");
    assert_eq!(
        ok(dir.path(), &["config", "words", "--profile", "alt"]),
        "30\n"
    );
    assert_eq!(
        ok(dir.path(), &["stats", "--profile", "alt"]),
        "no completed sessions yet\n"
    );
    assert!(ok(dir.path(), &["stats"]).starts_with("   1  2024-01-16"));
}

#[test]
fn the_words_flag_is_validated_and_only_applies_to_a_session() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["5", "abc"] {
        let line = one_line_error(dir.path(), &["--words", bad]);
        assert!(line.contains("10") && line.contains("200"), "{line}");
        assert!(line.contains(bad), "{line}");
    }
    let line = one_line_error(dir.path(), &["--words", "20", "stats"]);
    assert!(line.contains("--words"), "{line}");
    assert!(!dir.path().join("typ.db").exists());
}

#[test]
fn the_profile_flag_is_rejected_where_it_would_have_no_effect() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        &["rebuild", "--profile", "alt"][..],
        &["--profile", "alt", "replay", "1"][..],
    ] {
        let line = one_line_error(dir.path(), args);
        assert!(line.contains("--profile"), "{args:?}: {line}");
    }
    assert!(!dir.path().join("typ.db").exists());
}

#[test]
fn a_profile_is_created_the_first_time_it_is_named_whatever_the_command() {
    let dir = tempfile::tempdir().unwrap();
    ok(dir.path(), &["--profile", "a", "config", "profile"]);
    ok(dir.path(), &["--profile", "b", "stats"]);
    ok(dir.path(), &["--profile", "c", "config", "layout"]);
    let db = Store::open(&dir.path().join("typ.db")).unwrap();
    for name in ["a", "b", "c"] {
        assert_eq!(db.profile(name).unwrap().layout, "qwerty", "{name}");
    }
}
