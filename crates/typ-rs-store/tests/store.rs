use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

use rusqlite::Connection;
use tempfile::TempDir;
use typ_rs_core::compose::{self, ComposedPrompt, ComposedWord, Contamination, WordRole};
use typ_rs_core::corpus::{CORPUS_VERSION, Corpus};
use typ_rs_core::display::{CursorShape, CursorStyle};
use typ_rs_core::layout::Layout;
use typ_rs_core::metrics::{RecentSeries, summarize};
use typ_rs_core::model::{MODEL_VERSION, ModelState, SchedulerConfig};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::scheduler::{
    self, SelectedTarget, TargetRole, TrainingEvent, TrainingHistory, achieved_doses,
};
use typ_rs_core::session::{EndCondition, Input, Key, Outcome, SessionState};
use typ_rs_store::{
    DEFAULT_PROFILE, DEFAULT_WORDS, Error, Profile, SessionEnd, SessionId, SessionStart, Store,
    parse_words,
};

const SOURCE_OF_TRUTH: &[&str] = &[
    "profiles",
    "settings",
    "sessions",
    "prompts",
    "prompt_words",
    "prompt_targets",
    "input_events",
];

const CACHES: &[&str] = &[
    "pattern_stats",
    "context_model",
    "pattern_training_events",
    "session_metrics",
];

fn temp_db() -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("typ.db");
    (dir, path)
}

fn open(path: &Path) -> (Store, Profile) {
    let store = Store::open(path).unwrap();
    let profile = store.profile(DEFAULT_PROFILE).unwrap();
    (store, profile)
}

fn prompt(words: &str) -> Prompt {
    Prompt::new(words.split(' '))
}

/// A prompt of probes only, as the frequency-weighted composer makes.
fn probes(words: &str) -> ComposedPrompt {
    ComposedPrompt::probes(prompt(words))
}

/// Starts a session wanting as many words as `fallback` has, so a waiting
/// prompt of that length is shown and `fallback` composed otherwise.
fn start(
    store: &mut Store,
    profile: &Profile,
    started_at: i64,
    fallback: &str,
) -> (SessionId, Prompt) {
    start_with(
        store,
        profile,
        started_at,
        prompt(fallback).word_count(),
        fallback,
    )
}

fn start_with(
    store: &mut Store,
    profile: &Profile,
    started_at: i64,
    word_count: usize,
    fallback: &str,
) -> (SessionId, Prompt) {
    let started = store
        .start_session(
            profile,
            SessionStart {
                started_at,
                seed: 42,
                word_count,
            },
            || probes(fallback),
        )
        .unwrap();
    (started.id, started.prompt)
}

/// `⌫` is backspace, `⎋` an interrupt, `⟲` a resize, `¶` the character `p`
/// arriving inside a paste, `!` the character `x` arriving in a burst; one
/// input every 100 ms.
fn typed(prompt: Prompt, script: &str) -> SessionState {
    let mut state = SessionState::new(
        prompt.clone(),
        EndCondition::AfterWords(prompt.word_count()),
    );
    for (i, symbol) in script.chars().enumerate() {
        let at = i as u64 * 100_000;
        let input = match symbol {
            '⌫' => Input::new(at, Key::Backspace),
            '⎋' => Input::new(at, Key::Interrupt),
            '⟲' => Input::new(at, Key::Resize),
            '¶' => Input::new(at, Key::Char('p')).in_paste(),
            '!' => Input::new(at, Key::Char('x')).burst(),
            c => Input::new(at, Key::Char(c)),
        };
        state.apply_event(input);
    }
    state
}

/// Ends a session the way the binary does: the model is loaded, the session
/// applied to it, the achieved doses of the session's targets computed, the
/// session summarized against the recent series, and everything handed to
/// the store with the next prompt.
fn finish(
    store: &mut Store,
    profile: &Profile,
    id: SessionId,
    started_at: i64,
    state: &SessionState,
    next: &str,
) -> typ_rs_store::Result<()> {
    finish_with(store, profile, id, started_at, state, probes(next))
}

fn finish_with(
    store: &mut Store,
    profile: &Profile,
    id: SessionId,
    started_at: i64,
    state: &SessionState,
    next: ComposedPrompt,
) -> typ_rs_store::Result<()> {
    let mut model = store.model(profile).unwrap();
    let update = model.apply_session(state, started_at, Corpus::bundled(), store.config());
    let session = store.session(id).unwrap();
    let events = achieved_doses(state, &session.targets);
    let recent = store.recent_series(profile).unwrap();
    let summary = summarize(
        state,
        &update,
        &session.words,
        recent.as_ref(),
        store.config(),
    );
    store.finish_session(
        id,
        &SessionEnd {
            state,
            model: &model,
            events: &events,
            summary: summary.as_ref(),
            next_prompt: &next,
            ended_at: started_at + 60,
        },
    )
}

/// Runs a whole session through the store: start, type the script, finish
/// with `next` as the following prompt.
fn type_session(
    store: &mut Store,
    profile: &Profile,
    started_at: i64,
    fallback: &str,
    script: &str,
    next: &str,
) -> (SessionId, SessionState) {
    let (id, prompt) = start(store, profile, started_at, fallback);
    let state = typed(prompt, script);
    finish(store, profile, id, started_at, &state, next).unwrap();
    (id, state)
}

/// Every row of the given tables, rendered as text, keyed by table.
fn dump_tables(path: &Path, tables: &[&'static str]) -> BTreeMap<&'static str, Vec<String>> {
    dump_tables_hiding(path, tables, &[])
}

/// Every row of the given tables, rendered as text and sorted, keyed by
/// table, with the listed `(table, column)` pairs left out.
fn dump_tables_hiding(
    path: &Path,
    tables: &[&'static str],
    hidden: &[(&str, &str)],
) -> BTreeMap<&'static str, Vec<String>> {
    let conn = Connection::open(path).unwrap();
    tables
        .iter()
        .map(|table| {
            let columns: Vec<String> = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let shown: Vec<&str> = columns
                .iter()
                .map(String::as_str)
                .filter(|c| !hidden.contains(&(table, c)))
                .collect();
            let mut rows = conn
                .prepare(&format!("SELECT {} FROM {table}", shown.join(", ")))
                .unwrap()
                .query_map([], |row| {
                    (0..shown.len())
                        .map(|i| row.get_ref(i).map(|v| format!("{v:?}")))
                        .collect::<Result<Vec<_>, _>>()
                        .map(|cells| cells.join("|"))
                })
                .unwrap()
                .collect::<Result<Vec<String>, _>>()
                .unwrap();
            rows.sort();
            (*table, rows)
        })
        .collect()
}

/// Every row of every source-of-truth table.
fn dump(path: &Path) -> BTreeMap<&'static str, Vec<String>> {
    dump_tables(path, SOURCE_OF_TRUTH)
}

fn sql_one<T: rusqlite::types::FromSql>(path: &Path, sql: &str) -> T {
    Connection::open(path)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

fn sql(path: &Path, statement: &str) {
    Connection::open(path)
        .unwrap()
        .execute_batch(statement)
        .unwrap();
}

// --- Migrations and profiles ------------------------------------------------

#[test]
fn opening_an_empty_file_runs_the_migrations_and_creates_the_default_profile() {
    let (_dir, path) = temp_db();
    let (_store, profile) = open(&path);
    assert_eq!(profile.name, DEFAULT_PROFILE);
    assert_eq!(profile.layout, "qwerty");

    let conn = Connection::open(&path).unwrap();
    let versions: Vec<i64> = conn
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(versions, vec![1, 2, 3, 4, 5, 6]);

    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for table in SOURCE_OF_TRUTH
        .iter()
        .chain(CACHES)
        .chain(&["schema_migrations", "next_prompt"])
    {
        assert!(
            tables.iter().any(|t| t == table),
            "missing {table}: {tables:?}"
        );
    }

    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
}

#[test]
fn reopening_does_not_rerun_migrations_or_duplicate_the_default_profile() {
    let (_dir, path) = temp_db();
    drop(open(&path));
    drop(open(&path));

    let conn = Connection::open(&path).unwrap();
    let migrations: i64 = conn
        .query_row("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    let profiles: i64 = conn
        .query_row("SELECT count(*) FROM profiles", [], |r| r.get(0))
        .unwrap();
    assert_eq!((migrations, profiles), (6, 1));
}

#[test]
fn an_unknown_profile_is_an_error() {
    let (_dir, path) = temp_db();
    let (store, _) = open(&path);
    assert!(matches!(
        store.profile("colemak"),
        Err(Error::NoSuchProfile(name)) if name == "colemak"
    ));
}

// --- Session lifecycle -------------------------------------------------------

#[test]
fn the_session_row_is_visible_to_another_connection_before_the_session_ends() {
    let (_dir, path) = temp_db();
    let (mut running, profile) = open(&path);
    let (id, _) = start(&mut running, &profile, 1_000, "cat dog");

    let (other, other_profile) = open(&path);
    let session = other.session(id).unwrap();
    assert_eq!(session.outcome, Outcome::Interrupted);
    assert_eq!(session.ended_at, None);
    assert!(session.events.is_empty());
    assert_eq!(session.prompt, prompt("cat dog"));
    assert!(
        other
            .completed_sessions(&other_profile, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_session_the_process_died_in_is_kept_with_no_events_and_hidden_from_the_listing() {
    let (_dir, path) = temp_db();
    let dead = {
        let (mut store, profile) = open(&path);
        start(&mut store, &profile, 1_000, "cat dog").0
    };

    let (mut store, profile) = open(&path);
    type_session(&mut store, &profile, 2_000, "unused", "cat dog", "fox");

    let session = store.session(dead).unwrap();
    assert_eq!(session.outcome, Outcome::Interrupted);
    assert!(session.events.is_empty());
    let listed = store.completed_sessions(&profile, 10).unwrap();
    assert_eq!(listed.len(), 1);
    assert_ne!(listed[0].id, dead);
}

#[test]
fn a_completed_session_persists_every_event_in_order_and_replays_identically() {
    let (_dir, path) = temp_db();
    let (id, state) = {
        let (mut store, profile) = open(&path);
        type_session(
            &mut store,
            &profile,
            1_000,
            "cat dog fox",
            "cxt⌫⌫at ¶⟲d!⌫og  fox",
            "next words",
        )
    };
    assert_eq!(state.outcome(), Some(Outcome::Completed));

    let (store, profile) = open(&path);
    let session = store.session(id).unwrap();
    assert_eq!(session.outcome, Outcome::Completed);
    assert_eq!(session.started_at, 1_000);
    assert_eq!(session.ended_at, Some(1_060));
    assert_eq!(session.events, state.events());
    assert_eq!(session.replay(), state);

    let listed = store.completed_sessions(&profile, 10).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert_eq!(listed[0].events, state.events());
}

#[test]
fn an_interrupted_session_keeps_its_events_and_its_prompt_is_not_reshown() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (id, state) = type_session(
        &mut store,
        &profile,
        1_000,
        "cat dog fox",
        "ca⎋",
        "fresh words",
    );
    assert_eq!(state.outcome(), Some(Outcome::Interrupted));

    let session = store.session(id).unwrap();
    assert_eq!(session.outcome, Outcome::Interrupted);
    assert_eq!(session.events.len(), 3);
    assert!(store.completed_sessions(&profile, 10).unwrap().is_empty());

    let (_, shown) = start(&mut store, &profile, 2_000, "fallback words");
    assert_eq!(shown, prompt("fresh words"));
}

#[test]
fn the_precomposed_prompt_is_shown_once_and_then_the_fallback_is_used() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (_, first) = start(&mut store, &profile, 1_000, "first fallback");
    assert_eq!(first, prompt("first fallback"));

    type_session(
        &mut store,
        &profile,
        2_000,
        "unused",
        "cat",
        "composed ahead",
    );

    let (_, second) = start(&mut store, &profile, 3_000, "second fallback");
    assert_eq!(second, prompt("composed ahead"));
    let (_, third) = start(&mut store, &profile, 4_000, "second fallback");
    assert_eq!(third, prompt("second fallback"));
}

// --- Settings and profiles ---------------------------------------------------

#[test]
fn settings_have_their_defaults_before_anything_is_set() {
    let (_dir, path) = temp_db();
    let (store, profile) = open(&path);
    assert_eq!(store.active_profile().unwrap(), DEFAULT_PROFILE);
    assert_eq!(store.words(&profile).unwrap(), DEFAULT_WORDS);
    assert_eq!(store.cursor().unwrap(), CursorStyle::default());
    assert_eq!(profile.layout, "qwerty");
    assert_eq!(profile.mode, "words");
}

#[test]
fn the_cursor_settings_round_trip_and_belong_to_the_whole_database() {
    let (_dir, path) = temp_db();
    {
        let (mut store, _) = open(&path);
        store.set_cursor_shape("block").unwrap();
        store.set_cursor_blink("on").unwrap();
        store.set_cursor_shape("underline").unwrap();
    }
    let (store, _) = open(&path);
    assert_eq!(
        store.cursor().unwrap(),
        CursorStyle {
            shape: CursorShape::Underline,
            blink: true
        }
    );
    let scoped: i64 = sql_one(
        &path,
        "SELECT count(*) FROM settings WHERE key LIKE 'cursor_%' AND profile_id IS NOT NULL",
    );
    assert_eq!(scoped, 0);
    let rows: i64 = sql_one(&path, "SELECT count(*) FROM settings");
    assert_eq!(rows, 2);
}

#[test]
fn bad_cursor_values_are_refused_and_leave_the_settings_unchanged() {
    let (_dir, path) = temp_db();
    let (mut store, _) = open(&path);
    store.set_cursor_shape("beam").unwrap();
    store.set_cursor_blink("off").unwrap();
    for bad in ["bar", "Block", "", "steady"] {
        let err = store.set_cursor_shape(bad).unwrap_err();
        assert!(matches!(err, Error::InvalidSetting(_)), "{bad:?}: {err}");
        assert!(err.to_string().contains("block, beam, underline"), "{err}");
    }
    for bad in ["yes", "true", "1", "", "On"] {
        let err = store.set_cursor_blink(bad).unwrap_err();
        assert!(matches!(err, Error::InvalidSetting(_)), "{bad:?}: {err}");
        assert!(err.to_string().contains("on or off"), "{err}");
    }
    assert_eq!(store.cursor().unwrap(), CursorStyle::default());
}

#[test]
fn a_corrupt_cursor_value_is_reported_not_used() {
    let (_dir, path) = temp_db();
    let (store, _) = open(&path);
    sql(
        &path,
        "INSERT INTO settings (profile_id, key, value) VALUES (NULL, 'cursor_shape', 'wedge')",
    );
    assert!(matches!(store.cursor(), Err(Error::Corrupt(_))));
}

#[test]
fn words_round_trip_and_the_last_value_set_wins() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        store.set_words(&profile, 30).unwrap();
        store.set_words(&profile, 120).unwrap();
    }
    let (store, profile) = open(&path);
    assert_eq!(store.words(&profile).unwrap(), 120);
    let rows: i64 = sql_one(&path, "SELECT count(*) FROM settings");
    assert_eq!(rows, 1);
}

#[test]
fn words_outside_the_range_are_refused_and_leave_the_setting_unchanged() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    store.set_words(&profile, 30).unwrap();
    for bad in [0, 9, 201, usize::MAX] {
        assert!(
            matches!(
                store.set_words(&profile, bad),
                Err(Error::InvalidSetting(_))
            ),
            "{bad}"
        );
        assert_eq!(store.words(&profile).unwrap(), 30);
    }
    store.set_words(&profile, 10).unwrap();
    store.set_words(&profile, 200).unwrap();
    assert_eq!(store.words(&profile).unwrap(), 200);
}

#[test]
fn parsing_words_accepts_only_whole_numbers_in_the_range() {
    assert_eq!(parse_words("50").unwrap(), 50);
    assert_eq!(parse_words("10").unwrap(), 10);
    assert_eq!(parse_words("200").unwrap(), 200);
    for bad in ["9", "201", "abc", "50.0", "-50", "", " 50"] {
        let err = parse_words(bad).unwrap_err();
        assert!(matches!(err, Error::InvalidSetting(_)), "{bad:?}: {err}");
        assert!(err.to_string().contains("10"), "{bad:?}: {err}");
        assert!(err.to_string().contains("200"), "{bad:?}: {err}");
    }
}

#[test]
fn a_corrupt_words_value_is_reported_not_used() {
    let (_dir, path) = temp_db();
    let (store, profile) = open(&path);
    sql(
        &path,
        "INSERT INTO settings (profile_id, key, value) VALUES (1, 'words', 'lots')",
    );
    assert!(matches!(store.words(&profile), Err(Error::Corrupt(_))));
}

#[test]
fn the_active_profile_round_trips_and_is_created_on_first_use() {
    let (_dir, path) = temp_db();
    {
        let (mut store, _) = open(&path);
        store.set_active_profile("alt").unwrap();
    }
    let (store, _) = open(&path);
    assert_eq!(store.active_profile().unwrap(), "alt");
    let alt = store.profile("alt").unwrap();
    assert_eq!(alt.name, "alt");
    assert_eq!(alt.layout, "qwerty");
    assert_eq!(alt.mode, "words");
    let created_at: i64 = sql_one(&path, "SELECT created_at FROM profiles WHERE name = 'alt'");
    assert!(created_at > 0);
    // The active profile is a setting of the whole database, not of a profile.
    let scope: Option<i64> = sql_one(
        &path,
        "SELECT profile_id FROM settings WHERE key = 'profile'",
    );
    assert_eq!(scope, None);
}

#[test]
fn profile_or_create_returns_the_existing_profile_or_a_new_one_once() {
    let (_dir, path) = temp_db();
    let (mut store, default) = open(&path);
    assert_eq!(store.profile_or_create(DEFAULT_PROFILE).unwrap(), default);

    let first = store.profile_or_create("alt").unwrap();
    let again = store.profile_or_create("alt").unwrap();
    assert_eq!(first, again);
    assert_eq!(store.profile("alt").unwrap(), first);
    let profiles: i64 = sql_one(&path, "SELECT count(*) FROM profiles");
    assert_eq!(profiles, 2);
}

#[test]
fn profile_names_are_plain_words() {
    let (_dir, path) = temp_db();
    let (mut store, _) = open(&path);
    for bad in ["", "my profile", "a/b", "tab\there", "ünïcode"] {
        let err = store.profile_or_create(bad).unwrap_err();
        assert!(matches!(err, Error::InvalidSetting(_)), "{bad:?}: {err}");
        let err = store.set_active_profile(bad).unwrap_err();
        assert!(matches!(err, Error::InvalidSetting(_)), "{bad:?}: {err}");
    }
    assert_eq!(store.active_profile().unwrap(), DEFAULT_PROFILE);
    let profiles: i64 = sql_one(&path, "SELECT count(*) FROM profiles");
    assert_eq!(profiles, 1);

    for good in ["Colemak", "layout-2024", "a.b_c", "x"] {
        assert_eq!(store.profile_or_create(good).unwrap().name, good);
    }
}

#[test]
fn a_layout_can_change_only_until_a_session_has_been_typed_on_the_profile() {
    let (_dir, path) = temp_db();
    let (mut store, _) = open(&path);
    let alt = store.profile_or_create("alt").unwrap();

    store.set_layout(&alt, "qwerty").unwrap();
    let err = store.set_layout(&alt, "dvorak").unwrap_err();
    assert!(matches!(err, Error::InvalidSetting(_)), "{err}");
    assert!(err.to_string().contains("qwerty"), "{err}");

    // A session the process died in was never typed, so it does not bind.
    start(&mut store, &alt, 1_000, "cat");
    store.set_layout(&alt, "qwerty").unwrap();

    type_session(&mut store, &alt, 2_000, "cat", "cat", "dog");
    let err = store.set_layout(&alt, "dvorak").unwrap_err();
    assert!(matches!(err, Error::InvalidSetting(_)), "{err}");
    assert!(err.to_string().contains("alt"), "{err}");
    assert!(err.to_string().contains("profile"), "{err}");
    store.set_layout(&alt, "qwerty").unwrap();
    assert_eq!(store.profile("alt").unwrap().layout, "qwerty");
}

#[test]
fn settings_are_isolated_per_profile() {
    let (_dir, path) = temp_db();
    let (mut store, default) = open(&path);
    let a = store.profile_or_create("a").unwrap();
    let b = store.profile_or_create("b").unwrap();
    store.set_words(&a, 30).unwrap();
    store.set_words(&b, 150).unwrap();

    assert_eq!(store.words(&a).unwrap(), 30);
    assert_eq!(store.words(&b).unwrap(), 150);
    assert_eq!(store.words(&default).unwrap(), DEFAULT_WORDS);
}

#[test]
fn sessions_statistics_and_the_next_prompt_are_isolated_per_profile() {
    let (_dir, path) = temp_db();
    let (mut store, _) = open(&path);
    let a = store.profile_or_create("a").unwrap();
    let b = store.profile_or_create("b").unwrap();
    let (a_id, _) = type_session(&mut store, &a, 1_000, "cat dog", "cat dog", "alpha next");
    let (b_id, _) = type_session(&mut store, &b, 2_000, "fox owl", "fxx owl", "beta next");

    let listed = |store: &Store, p: &Profile| -> Vec<SessionId> {
        store
            .completed_sessions(p, 10)
            .unwrap()
            .iter()
            .map(|s| s.id)
            .collect()
    };
    assert_eq!(listed(&store, &a), vec![a_id]);
    assert_eq!(listed(&store, &b), vec![b_id]);

    assert!(store.model(&a).unwrap().stats("cat").is_some());
    assert!(store.model(&b).unwrap().stats("cat").is_none());
    assert!(store.model(&b).unwrap().stats("fox").is_some());

    let (_, shown_b) = start(&mut store, &b, 3_000, "fallback words");
    assert_eq!(shown_b, prompt("beta next"));
    let (_, shown_a) = start(&mut store, &a, 4_000, "fallback words");
    assert_eq!(shown_a, prompt("alpha next"));
}

// --- The next prompt's generation context ------------------------------------

#[test]
fn the_next_prompt_records_what_it_was_composed_for() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    type_session(&mut store, &profile, 1_000, "cat", "cat", "one two three");

    let (words, corpus, model, layout): (i64, i64, i64, String) = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT word_count, corpus_version, model_version, layout FROM next_prompt",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(words, 3);
    assert_eq!(corpus, i64::from(CORPUS_VERSION));
    assert_eq!(model, i64::from(MODEL_VERSION));
    assert_eq!(layout, "qwerty");
}

/// Types one session so that a three-word prompt waits for the next, alters
/// the waiting prompt's recorded context with `change` if given, and starts
/// a session wanting `word_count` words. Returns the prompt shown and how
/// many prompts are still waiting afterward.
fn start_after_context_change(change: Option<&str>, word_count: usize) -> (Prompt, i64) {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    type_session(&mut store, &profile, 1_000, "cat", "cat", "one two three");
    if let Some(change) = change {
        sql(&path, change);
    }
    let (_, shown) = start_with(&mut store, &profile, 2_000, word_count, "fresh fallback");
    let waiting: i64 = sql_one(&path, "SELECT count(*) FROM next_prompt");
    (shown, waiting)
}

#[test]
fn a_waiting_prompt_whose_context_matches_is_shown() {
    let (shown, waiting) = start_after_context_change(None, 3);
    assert_eq!(shown, prompt("one two three"));
    assert_eq!(waiting, 0);
}

#[test]
fn a_waiting_prompt_is_replaced_when_the_word_count_differs() {
    let (shown, waiting) = start_after_context_change(None, 2);
    assert_eq!(shown, prompt("fresh fallback"));
    assert_eq!(waiting, 0);
}

#[test]
fn a_waiting_prompt_is_replaced_when_the_corpus_version_differs() {
    let (shown, waiting) = start_after_context_change(
        Some("UPDATE next_prompt SET corpus_version = corpus_version + 1"),
        3,
    );
    assert_eq!(shown, prompt("fresh fallback"));
    assert_eq!(waiting, 0);
}

#[test]
fn a_waiting_prompt_is_replaced_when_the_model_version_differs() {
    let (shown, waiting) = start_after_context_change(
        Some("UPDATE next_prompt SET model_version = model_version + 1"),
        3,
    );
    assert_eq!(shown, prompt("fresh fallback"));
    assert_eq!(waiting, 0);
}

#[test]
fn a_waiting_prompt_is_replaced_when_the_layout_differs() {
    let (shown, waiting) =
        start_after_context_change(Some("UPDATE next_prompt SET layout = 'dvorak'"), 3);
    assert_eq!(shown, prompt("fresh fallback"));
    assert_eq!(waiting, 0);
}

#[test]
fn completed_sessions_are_listed_most_recent_first_up_to_the_limit() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let ids: Vec<SessionId> = [3_000, 1_000, 2_000]
        .into_iter()
        .map(|at| type_session(&mut store, &profile, at, "cat", "cat", "cat").0)
        .collect();

    let listed: Vec<SessionId> = store
        .completed_sessions(&profile, 2)
        .unwrap()
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(listed, vec![ids[0], ids[2]]);
    assert_eq!(store.completed_sessions(&profile, 10).unwrap().len(), 3);
}

// --- Source of truth ---------------------------------------------------------

#[test]
fn a_session_cannot_be_finished_twice() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (id, prompt) = start(&mut store, &profile, 1_000, "cat");
    let state = typed(prompt, "cat");
    finish(&mut store, &profile, id, 1_000, &state, "next").unwrap();

    let again = finish(&mut store, &profile, id, 1_060, &state, "other");
    assert!(matches!(again, Err(Error::SessionAlreadyEnded(ended)) if ended == id));
    assert_eq!(store.session(id).unwrap().ended_at, Some(1_060));
}

#[test]
fn source_of_truth_rows_are_never_modified_after_a_session_ends() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    type_session(&mut store, &profile, 1_000, "cat dog", "cat dog", "fox");
    let before = dump(&path);
    assert_eq!(before["sessions"].len(), 1);
    assert_eq!(before["input_events"].len(), 7);

    type_session(&mut store, &profile, 2_000, "unused", "fo⎋", "owl");
    start(&mut store, &profile, 3_000, "unused");
    store.completed_sessions(&profile, 10).unwrap();
    let after = dump(&path);

    for table in SOURCE_OF_TRUTH {
        for row in &before[table] {
            assert!(
                after[table].contains(row),
                "{table} row changed or disappeared: {row}"
            );
        }
    }
    assert_eq!(after["sessions"].len(), 3);
}

// --- Restarting and discarding attempts ---------------------------------------

/// Every row of every source-of-truth table and every cache, with the
/// columns that hold a prompt's id or the wall-clock moment the profile was
/// created left out, so that two databases that recorded the same session
/// compare equal however their prompt ids were handed out.
fn dump_comparable(path: &Path) -> BTreeMap<&'static str, Vec<String>> {
    let tables: Vec<&'static str> = SOURCE_OF_TRUTH
        .iter()
        .chain(CACHES)
        .chain(&["next_prompt"])
        .copied()
        .collect();
    dump_tables_hiding(
        path,
        &tables,
        &[
            ("profiles", "created_at"),
            ("prompts", "id"),
            ("prompt_words", "prompt_id"),
            ("prompt_targets", "prompt_id"),
            ("sessions", "prompt_id"),
            ("next_prompt", "prompt_id"),
        ],
    )
}

#[test]
fn a_restarted_session_finishes_exactly_as_one_started_on_the_final_prompt() {
    let (_direct_dir, direct) = temp_db();
    {
        let (mut store, profile) = open(&direct);
        type_session(&mut store, &profile, 1_030, "fox owl", "fox owl", "next");
    }

    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (id, first) = start(&mut store, &profile, 1_000, "cat dog");
    assert_eq!(first, prompt("cat dog"));
    let restarted = store
        .restart_session(id, &probes("fox owl"), 1_030)
        .unwrap();
    assert_eq!(restarted.id, id);
    assert_eq!(restarted.prompt, prompt("fox owl"));
    let state = typed(restarted.prompt, "fox owl");
    finish(&mut store, &profile, id, 1_030, &state, "next").unwrap();

    assert_eq!(dump_comparable(&path), dump_comparable(&direct));
    let session = store.session(id).unwrap();
    assert_eq!(session.started_at, 1_030);
    assert_eq!(session.prompt, prompt("fox owl"));
    let discarded_words: i64 = sql_one(
        &path,
        "SELECT count(*) FROM prompt_words WHERE word IN ('cat', 'dog')",
    );
    assert_eq!(discarded_words, 0);
    let prompts: i64 = sql_one(&path, "SELECT count(*) FROM prompts");
    assert_eq!(prompts, 2, "the final prompt and the one composed ahead");
}

#[test]
fn restarting_twice_leaves_one_prompt_and_one_session_with_the_targets_of_the_last() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (id, _) = start(&mut store, &profile, 1_000, "cat dog fox");
    store
        .restart_session(id, &probes("owl elk ant"), 1_010)
        .unwrap();
    let last = store
        .restart_session(id, &targeted("bat dog the"), 1_020)
        .unwrap();
    assert_eq!(last.id, id);
    assert_eq!(last.targets, targeted("bat dog the").targets);
    assert_eq!(last.words, targeted("bat dog the").words);
    assert_eq!(last.targeted_words, vec![Box::from("bat")]);

    let prompts: i64 = sql_one(&path, "SELECT count(*) FROM prompts");
    let sessions: i64 = sql_one(&path, "SELECT count(*) FROM sessions");
    let targets: i64 = sql_one(&path, "SELECT count(*) FROM prompt_targets");
    assert_eq!((prompts, sessions, targets), (1, 1, 3));
    let session = store.session(id).unwrap();
    assert_eq!(session.prompt, prompt("bat dog the"));
    assert_eq!(session.targets, last.targets);
    assert_eq!(session.started_at, 1_020);
    assert_eq!(session.ended_at, None);
}

#[test]
fn a_session_that_has_ended_cannot_be_restarted() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (id, _) = type_session(&mut store, &profile, 1_000, "cat", "cat", "next");
    let before = dump(&path);

    let refused = store.restart_session(id, &probes("dog"), 2_000);
    assert!(matches!(refused, Err(Error::SessionAlreadyEnded(ended)) if ended == id));
    assert_eq!(dump(&path), before);
    let missing = SessionId::from_str("99").unwrap();
    assert!(matches!(
        store.restart_session(missing, &probes("dog"), 2_000),
        Err(Error::NoSuchSession(_))
    ));
}

#[test]
fn a_discarded_attempt_leaves_sessions_as_before_and_its_prompt_waiting_for_the_same_length() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    type_session(&mut store, &profile, 1_000, "cat", "cat", "one two three");
    let before = dump(&path);
    let history_before = store.training_history(&profile).unwrap();

    let (id, shown) = start_with(&mut store, &profile, 2_000, 3, "unused unused unused");
    assert_eq!(shown, prompt("one two three"));
    store.discard_session(id).unwrap();

    let after = dump(&path);
    assert_eq!(after["sessions"], before["sessions"]);
    assert_eq!(after["prompts"], before["prompts"]);
    assert_eq!(store.training_history(&profile).unwrap(), history_before);
    let (words, corpus, model, layout): (i64, i64, i64, String) = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT word_count, corpus_version, model_version, layout FROM next_prompt",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (words, corpus, model, layout),
        (
            3,
            i64::from(CORPUS_VERSION),
            i64::from(MODEL_VERSION),
            "qwerty".to_string()
        )
    );

    let (id, shown) = start_with(&mut store, &profile, 3_000, 2, "two words");
    assert_eq!(shown, prompt("two words"));
    store.discard_session(id).unwrap();
    let (_, shown) = start_with(&mut store, &profile, 4_000, 2, "other pair");
    assert_eq!(shown, prompt("two words"));
}

#[test]
fn discarding_after_a_restart_puts_the_restarted_prompt_back_and_forgets_the_first() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (id, _) = start(&mut store, &profile, 1_000, "cat dog");
    store
        .restart_session(id, &probes("fox owl"), 1_010)
        .unwrap();
    store.discard_session(id).unwrap();

    let sessions: i64 = sql_one(&path, "SELECT count(*) FROM sessions");
    let prompts: i64 = sql_one(&path, "SELECT count(*) FROM prompts");
    assert_eq!((sessions, prompts), (0, 1));
    let (_, shown) = start(&mut store, &profile, 2_000, "unused unused");
    assert_eq!(shown, prompt("fox owl"));
}

#[test]
fn a_session_with_events_or_one_that_has_ended_cannot_be_discarded() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (ended, _) = type_session(&mut store, &profile, 1_000, "cat", "cat", "dog");
    let refused = store.discard_session(ended);
    assert!(matches!(refused, Err(Error::SessionAlreadyEnded(id)) if id == ended));

    let (typed_id, _) = start(&mut store, &profile, 2_000, "unused");
    sql(
        &path,
        &format!(
            "INSERT INTO input_events
                 (session_id, seq, at_micros, kind, expected, actual, word_index, position,
                  first_of_session, after_resize, in_paste, burst, long_pause)
             VALUES ({typed_id}, 0, 100, 'char', 'd', 'd', 0, 0, 1, 0, 0, 0, 0)"
        ),
    );
    let before = dump(&path);
    let refused = store.discard_session(typed_id);
    assert!(matches!(refused, Err(Error::SessionTyped(id)) if id == typed_id));
    assert_eq!(dump(&path), before);
    let missing = SessionId::from_str("99").unwrap();
    assert!(matches!(
        store.discard_session(missing),
        Err(Error::NoSuchSession(_))
    ));
}

#[test]
fn rebuilding_with_a_restarted_session_present_leaves_the_source_of_truth_alone() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        type_history(&mut store, &profile);
        let (id, _) = start(&mut store, &profile, 5_000_000, "cat dog");
        let restarted = store
            .restart_session(id, &targeted("cat dog the"), 5_000_010)
            .unwrap();
        let state = typed(restarted.prompt, "cat dog the");
        finish(&mut store, &profile, id, 5_000_010, &state, "next").unwrap();
    }
    let truth_before = dump(&path);
    let caches_before = dump_tables(&path, CACHES);

    let (mut store, _) = open(&path);
    assert_eq!(store.rebuild().unwrap(), 5);
    assert_eq!(dump(&path), truth_before);
    assert_eq!(dump_tables(&path, CACHES), caches_before);
}

// --- Pattern statistics ------------------------------------------------------

/// Types a varied set of sessions: two completed, one interrupted with too
/// few clean intervals to count, one interrupted with enough, over several
/// weeks so that decay is exercised.
fn type_history(store: &mut Store, profile: &Profile) {
    let day = 86_400;
    type_session(
        store,
        profile,
        1_000,
        "the quick brown fox",
        "the quikc⌫⌫ck brown fox",
        "next",
    );
    type_session(
        store,
        profile,
        1_000 + 3 * day,
        "unused",
        "cat dog fox owl cat dog fox owl",
        "unused",
    );
    type_session(store, profile, 1_000 + 10 * day, "unused", "ca⎋", "unused");
    let long = "the quick brown fox jumps over the lazy dog";
    type_session(
        store,
        profile,
        1_000 + 50 * day,
        long,
        "the quick brown fox jumps over the la⎋",
        "unused",
    );
}

#[test]
fn the_config_is_stored_with_every_session_and_the_marker_is_set_when_it_ends() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (id, prompt) = start(&mut store, &profile, 1_000, "cat");
    let config_json: String = sql_one(&path, "SELECT config_json FROM sessions");
    assert_eq!(
        SchedulerConfig::from_json(&config_json).unwrap(),
        *store.config()
    );
    let marker: Option<i64> = sql_one(&path, "SELECT applied_model_version FROM sessions");
    assert_eq!(marker, None);

    let state = typed(prompt, "cat");
    finish(&mut store, &profile, id, 1_000, &state, "next").unwrap();
    let marker: Option<i64> = sql_one(&path, "SELECT applied_model_version FROM sessions");
    assert_eq!(marker, Some(i64::from(MODEL_VERSION)));
    let version: i64 = sql_one(&path, "SELECT DISTINCT model_version FROM pattern_stats");
    assert_eq!(version, i64::from(MODEL_VERSION));
}

#[test]
fn the_model_read_back_is_the_one_written_and_grows_with_each_session() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    assert_eq!(store.model(&profile).unwrap(), ModelState::new());

    let (id, prompt) = start(&mut store, &profile, 1_000, "cat dog");
    let state = typed(prompt, "cat dog");
    let mut model = store.model(&profile).unwrap();
    model.apply_session(&state, 1_000, Corpus::bundled(), store.config());
    store
        .finish_session(
            id,
            &SessionEnd {
                state: &state,
                model: &model,
                events: &[],
                summary: None,
                next_prompt: &probes("next"),
                ended_at: 1_060,
            },
        )
        .unwrap();
    model.mark_clean();

    let (store, profile) = open(&path);
    let loaded = store.model(&profile).unwrap();
    assert_eq!(loaded, model);
    assert!(loaded.stats("cat").is_some());
    assert!(loaded.user_baseline().is_some());
}

#[test]
fn rebuilding_reproduces_the_caches_exactly_and_leaves_the_source_of_truth_alone() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        type_history(&mut store, &profile);
    }
    let truth_before = dump(&path);
    let caches_before = dump_tables(&path, CACHES);
    assert!(caches_before["pattern_stats"].len() > 20);

    let (mut store, _) = open(&path);
    let reapplied = store.rebuild().unwrap();
    assert_eq!(reapplied, 4);

    assert_eq!(dump(&path), truth_before);
    assert_eq!(dump_tables(&path, CACHES), caches_before);
    let unmarked: i64 = sql_one(
        &path,
        "SELECT count(*) FROM sessions WHERE applied_model_version IS NULL",
    );
    assert_eq!(unmarked, 0);
}

#[test]
fn sessions_with_no_marker_are_applied_in_start_order_when_the_store_is_next_opened() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        type_history(&mut store, &profile);
    }
    let caches_before = dump_tables(&path, CACHES);

    // As recorded by a version of typ with no statistics: every marker
    // null and no cache at all. Opening applies them all, oldest first.
    sql(
        &path,
        "DELETE FROM pattern_stats;
         DELETE FROM context_model;
         UPDATE sessions SET applied_model_version = NULL",
    );
    let (store, _) = open(&path);
    drop(store);
    assert_eq!(dump_tables(&path, CACHES), caches_before);
    let unmarked: i64 = sql_one(
        &path,
        "SELECT count(*) FROM sessions WHERE applied_model_version IS NULL",
    );
    assert_eq!(unmarked, 0);
}

#[test]
fn only_the_sessions_with_no_marker_are_applied_on_open() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        type_history(&mut store, &profile);
    }
    let caches_before = dump_tables(&path, CACHES);

    // One session loses its marker: it is applied again on top of the
    // existing cache, so the cache changes, and its marker is set.
    sql(
        &path,
        "UPDATE sessions SET applied_model_version = NULL WHERE id = 2",
    );
    let (store, _) = open(&path);
    drop(store);
    assert_ne!(dump_tables(&path, CACHES), caches_before);
    let marker: Option<i64> = sql_one(
        &path,
        "SELECT applied_model_version FROM sessions WHERE id = 2",
    );
    assert_eq!(marker, Some(i64::from(MODEL_VERSION)));
}

#[test]
fn a_running_session_or_one_the_process_died_in_is_not_applied_on_open() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        type_session(&mut store, &profile, 1_000, "cat dog", "cat dog", "fox");
        start(&mut store, &profile, 2_000, "unused");
    }
    let caches_before = dump_tables(&path, CACHES);

    let (store, _) = open(&path);
    drop(store);
    assert_eq!(dump_tables(&path, CACHES), caches_before);
    let markers: Vec<Option<i64>> = Connection::open(&path)
        .unwrap()
        .prepare("SELECT applied_model_version FROM sessions ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(markers, vec![Some(i64::from(MODEL_VERSION)), None]);
}

#[test]
fn a_cache_from_another_model_version_is_rebuilt_on_open() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        type_history(&mut store, &profile);
    }
    let truth_before = dump(&path);
    let caches_before = dump_tables(&path, CACHES);

    // An older binary's cache: different stamp, different numbers.
    sql(
        &path,
        "UPDATE pattern_stats SET model_version = model_version + 1, s1 = s1 + 100, c = 0;
         UPDATE sessions SET applied_model_version = applied_model_version + 1",
    );
    assert_ne!(dump_tables(&path, CACHES), caches_before);

    let (store, _) = open(&path);
    drop(store);
    assert_eq!(dump_tables(&path, CACHES), caches_before);
    assert_eq!(dump(&path), truth_before);
}

#[test]
fn a_marker_from_another_model_version_alone_triggers_a_rebuild() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        type_history(&mut store, &profile);
    }
    let caches_before = dump_tables(&path, CACHES);
    sql(
        &path,
        "DELETE FROM pattern_stats;
         UPDATE sessions SET applied_model_version = applied_model_version + 1",
    );

    let (store, _) = open(&path);
    drop(store);
    assert_eq!(dump_tables(&path, CACHES), caches_before);
}

#[test]
fn an_interrupted_session_too_short_to_count_is_still_marked_applied() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (id, _) = type_session(&mut store, &profile, 1_000, "cat dog", "ca⎋", "fox");
    let marker: Option<i64> = sql_one(
        &path,
        &format!("SELECT applied_model_version FROM sessions WHERE id = {id}"),
    );
    assert_eq!(marker, Some(i64::from(MODEL_VERSION)));
    assert_eq!(store.model(&profile).unwrap(), ModelState::new());
}

// --- Context model -----------------------------------------------------------

/// Enough distinct words that a fit has something to work with.
const RICH: &str = "the of and to in is you that it he was for on are as with his \
    they at be this have from or one had by word but not what all were we when";

/// Types completed sessions of `RICH`, the `i`th one `i` days in.
fn type_completed_sessions(store: &mut Store, profile: &Profile, sessions: std::ops::Range<usize>) {
    for i in sessions {
        type_session(
            store,
            profile,
            1_000 + i as i64 * 86_400,
            RICH,
            RICH,
            "next",
        );
    }
}

#[test]
fn the_context_model_row_tracks_completed_sessions_and_is_fitted_at_the_fifth() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    assert_eq!(
        store.model(&profile).unwrap().context_model(),
        ModelState::new().context_model()
    );

    type_completed_sessions(&mut store, &profile, 0..4);
    let (sessions, fitted): (i64, i64) = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT completed_sessions, fitted FROM context_model",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((sessions, fitted), (4, 0));
    assert_eq!(
        store.model(&profile).unwrap().context_model().coefficients,
        None
    );

    type_completed_sessions(&mut store, &profile, 4..5);
    let (sessions, fitted, version): (i64, i64, i64) = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT completed_sessions, fitted, model_version FROM context_model",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (sessions, fitted, version),
        (5, 1, i64::from(MODEL_VERSION))
    );
    let loaded = store.model(&profile).unwrap();
    assert!(loaded.context_model().coefficients.is_some());

    // Loaded, applied to, and written back: the same as one long-lived model.
    let mut long_lived = ModelState::new();
    for i in 0..5 {
        let state = typed(prompt(RICH), RICH);
        long_lived.apply_session(
            &state,
            1_000 + i * 86_400,
            Corpus::bundled(),
            store.config(),
        );
    }
    long_lived.mark_clean();
    for (pattern, stats) in long_lived.patterns() {
        assert_eq!(loaded.stats(pattern), Some(*stats), "{pattern:?}");
    }
    assert_eq!(loaded.context_model(), long_lived.context_model());
    assert_eq!(loaded, long_lived);
}

#[test]
fn rebuilding_reproduces_the_fitted_context_model_exactly() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        type_completed_sessions(&mut store, &profile, 0..7);
    }
    let caches_before = dump_tables(&path, CACHES);
    assert_eq!(caches_before["context_model"].len(), 1);

    let (mut store, _) = open(&path);
    assert_eq!(store.rebuild().unwrap(), 7);
    assert_eq!(dump_tables(&path, CACHES), caches_before);
}

#[test]
fn a_context_model_from_another_model_version_alone_triggers_a_rebuild() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        type_completed_sessions(&mut store, &profile, 0..5);
    }
    let caches_before = dump_tables(&path, CACHES);
    sql(
        &path,
        "UPDATE context_model SET model_version = model_version + 1, intercept = 42",
    );
    assert_ne!(dump_tables(&path, CACHES), caches_before);

    let (store, _) = open(&path);
    drop(store);
    assert_eq!(dump_tables(&path, CACHES), caches_before);
}

#[test]
fn the_model_carries_the_profile_layout_and_refuses_one_this_build_does_not_know() {
    let (_dir, path) = temp_db();
    let (store, profile) = open(&path);
    assert_eq!(store.model(&profile).unwrap().layout(), Layout::QWERTY);

    sql(&path, "UPDATE profiles SET layout = 'colemak'");
    let (store, profile) = open(&path);
    let error = store.model(&profile).unwrap_err();
    assert!(matches!(error, Error::UnknownLayout { .. }), "{error:?}");
    assert!(error.to_string().contains("colemak"), "{error}");
}

// --- Targets and training events ------------------------------------------------

fn target(pattern: &str, role: TargetRole) -> SelectedTarget {
    SelectedTarget {
        pattern: pattern.into(),
        role,
        weakness_mean: 0.5,
        weakness_sd: 0.25,
        priority: 0.125,
        planned_dose: if role == TargetRole::Deferred { 0 } else { 6 },
    }
}

/// A prompt whose first word is targeted (exposing `at`) and the rest are
/// probes, with `at` a target, `og` deferred, and `he` the exploration
/// target.
fn targeted(words: &str) -> ComposedPrompt {
    let mut composed = probes(words);
    composed.words[0] = ComposedWord {
        role: WordRole::Targeted,
        exposed_targets: vec!["at".into()],
        selection_score: Some(-1.5),
        contamination: None,
    };
    composed.words[1].contamination = Some(Contamination {
        recently_targeted_word: true,
        recently_targeted_patterns: vec!["og".into()],
    });
    composed.targets = vec![
        target("at", TargetRole::Target),
        target("og", TargetRole::Deferred),
        target("he", TargetRole::Explore),
    ];
    composed
}

#[test]
fn a_prompt_is_stored_with_its_word_roles_and_targets_and_read_back_with_the_session() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let composed = targeted("cat dog the");
    let started = store
        .start_session(
            &profile,
            SessionStart {
                started_at: 1_000,
                seed: 1,
                word_count: 3,
            },
            || composed.clone(),
        )
        .unwrap();
    assert_eq!(started.prompt, composed.prompt);
    assert_eq!(started.targets, composed.targets);
    assert_eq!(store.session(started.id).unwrap().targets, composed.targets);

    assert_eq!(started.targeted_words, vec![Box::from("cat")]);

    type WordRow = (String, String, String, Option<f64>, Option<String>);
    let rows: Vec<WordRow> = Connection::open(&path)
        .unwrap()
        .prepare(
            "SELECT word, role, exposed_targets, selection_score, contamination
             FROM prompt_words ORDER BY word_index",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "cat".to_string(),
                "targeted".to_string(),
                r#"["at"]"#.to_string(),
                Some(-1.5),
                None,
            ),
            (
                "dog".to_string(),
                "probe".to_string(),
                "[]".to_string(),
                None,
                Some(
                    r#"{"before":"targeted","after":"probe","recent_word":true,"recent_patterns":["og"]}"#
                        .to_string()
                ),
            ),
            ("the".to_string(), "probe".to_string(), "[]".to_string(), None, None),
        ]
    );
    let targets: Vec<(String, String, i64)> = Connection::open(&path)
        .unwrap()
        .prepare("SELECT pattern, role, planned_dose FROM prompt_targets ORDER BY rowid")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        targets,
        vec![
            ("at".to_string(), "target".to_string(), 6),
            ("og".to_string(), "deferred".to_string(), 0),
            ("he".to_string(), "explore".to_string(), 6),
        ]
    );
}

#[test]
fn a_waiting_prompt_composed_ahead_keeps_its_targets_for_the_session_that_shows_it() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (id, prompt) = start(&mut store, &profile, 1_000, "cat dog");
    let state = typed(prompt, "cat dog");
    finish_with(
        &mut store,
        &profile,
        id,
        1_000,
        &state,
        targeted("cat dog the"),
    )
    .unwrap();

    let shown = store
        .start_session(
            &profile,
            SessionStart {
                started_at: 2_000,
                seed: 1,
                word_count: 3,
            },
            || panic!("a prompt is waiting"),
        )
        .unwrap();
    assert_eq!(shown.prompt, self::prompt("cat dog the"));
    assert_eq!(shown.targeted_words, vec![Box::from("cat")]);
    let sessions = store.completed_sessions(&profile, 10).unwrap();
    assert!(
        sessions[0].targets.is_empty(),
        "the first prompt was probes only"
    );
    let running = store.session(SessionId::from_str("2").unwrap()).unwrap();
    assert_eq!(running.targets, targeted("cat dog the").targets);
}

#[test]
fn training_events_record_the_achieved_dose_of_every_selected_pattern() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (id, prompt) = start(&mut store, &profile, 1_000, "cat dog");
    let state = typed(prompt, "cat dog");
    finish_with(
        &mut store,
        &profile,
        id,
        1_000,
        &state,
        targeted("cat that the"),
    )
    .unwrap();
    let (id, prompt) = start(&mut store, &profile, 2_000, "unused unused unused");
    // `at` is typed in "cat" and "that"; `he` in "the"; `og` nowhere.
    let state = typed(prompt, "cat that the");
    finish(&mut store, &profile, id, 2_000, &state, "next").unwrap();

    let rows: Vec<(i64, String, String, i64, i64, i64)> = Connection::open(&path)
        .unwrap()
        .prepare(
            "SELECT session_id, pattern, role, planned_dose, achieved_dose, model_version
             FROM pattern_training_events ORDER BY session_id, rowid",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let v = i64::from(MODEL_VERSION);
    assert_eq!(
        rows,
        vec![
            (2, "at".to_string(), "target".to_string(), 6, 2, v),
            (2, "og".to_string(), "deferred".to_string(), 0, 0, v),
            (2, "he".to_string(), "explore".to_string(), 6, 1, v),
        ]
    );
}

#[test]
fn the_training_history_replays_the_sessions_and_optionally_the_waiting_prompt() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    assert_eq!(
        store.training_history(&profile).unwrap(),
        TrainingHistory::new()
    );

    let (id, prompt) = start(&mut store, &profile, 1_000, "cat dog");
    let state = typed(prompt, "cat dog");
    finish_with(
        &mut store,
        &profile,
        id,
        1_000,
        &state,
        targeted("cat that the"),
    )
    .unwrap();
    // The history for composing has one session and no deferral yet; the
    // one for showing counts the waiting prompt's fresh deferral of `og`.
    let composing = store.training_history(&profile).unwrap();
    assert_eq!(composing.sessions(), 1);
    assert_eq!(composing.deferrals().count(), 0);
    let showing = store
        .training_history_with_waiting_prompt(&profile)
        .unwrap();
    assert_eq!(showing.sessions(), 2);
    assert_eq!(showing.deferrals().collect::<Vec<_>>(), vec![("og", 2)]);

    // Typed, it is a session like any other, and the probes-only prompt
    // now waiting counts as one more with nothing selected, running the
    // window down.
    let (id, prompt) = start(&mut store, &profile, 2_000, "unused unused unused");
    let state = typed(prompt, "cat that the");
    finish(&mut store, &profile, id, 2_000, &state, "next").unwrap();
    let history = store.training_history(&profile).unwrap();
    assert_eq!(history.sessions(), 2);
    assert_eq!(history.deferrals().collect::<Vec<_>>(), vec![("og", 2)]);
    let showing = store
        .training_history_with_waiting_prompt(&profile)
        .unwrap();
    assert_eq!(showing.sessions(), 3);
    assert_eq!(showing.deferrals().collect::<Vec<_>>(), vec![("og", 1)]);
    let at = history.pattern("at").unwrap();
    assert_eq!((at.sessions_practiced, at.achieved_dose), (1, 2));
    assert_eq!(at.practiced_means, vec![0.5]);
    // The second session's prompt showed "cat" as targeted; the waiting
    // prompt's probes-only words add nothing.
    assert!(history.targeted_word_within("cat", 1));
    assert!(!history.targeted_word_within("that", 2));
    assert!(showing.targeted_word_within("cat", 2));
    assert!(!showing.targeted_word_within("cat", 1));

    // A whole history built the same way in memory agrees.
    let mut expected = TrainingHistory::new();
    expected.record(&[], [], store.config());
    expected.record(
        &[
            TrainingEvent {
                target: target("at", TargetRole::Target),
                achieved_dose: 2,
            },
            TrainingEvent {
                target: target("og", TargetRole::Deferred),
                achieved_dose: 0,
            },
            TrainingEvent {
                target: target("he", TargetRole::Explore),
                achieved_dose: 1,
            },
        ],
        ["cat"],
        store.config(),
    );
    assert_eq!(history, expected);
}

#[test]
fn a_targeted_history_rebuilds_to_identical_caches() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        let corpus = Corpus::bundled();
        let words: Vec<&str> = corpus
            .words()
            .iter()
            .take(60)
            .map(|w| w.text.as_ref())
            .collect();
        let words = words.join(" ");
        // Five sessions composed by the real scheduler, each typed in full.
        let (mut id, mut prompt) = start(&mut store, &profile, 1_000, &words);
        for i in 0..5 {
            let started_at = 1_000 + i * 86_400;
            let text = prompt.text();
            let state = typed(prompt, &text);
            let mut model = store.model(&profile).unwrap();
            let update = model.apply_session(&state, started_at, corpus, store.config());
            let session = store.session(id).unwrap();
            let mut history = store.training_history(&profile).unwrap();
            let events = scheduler::achieved_doses(&state, &session.targets);
            history.record(&events, [], store.config());
            let next = compose::next_prompt(
                &model,
                corpus,
                store.config(),
                &history,
                started_at + 60,
                30,
                i as u64,
            );
            let recent = store.recent_series(&profile).unwrap();
            let summary = summarize(
                &state,
                &update,
                &session.words,
                recent.as_ref(),
                store.config(),
            );
            store
                .finish_session(
                    id,
                    &SessionEnd {
                        state: &state,
                        model: &model,
                        events: &events,
                        summary: summary.as_ref(),
                        next_prompt: &next,
                        ended_at: started_at + 60,
                    },
                )
                .unwrap();
            (id, prompt) = start_with(&mut store, &profile, started_at + 86_400, 30, "unused");
        }
    }
    let truth_before = dump(&path);
    let caches_before = dump_tables(&path, CACHES);
    assert!(caches_before["pattern_training_events"].len() >= 4);
    assert_eq!(caches_before["session_metrics"].len(), 5);
    let targets: i64 = sql_one(&path, "SELECT count(*) FROM prompt_targets");
    assert!(targets >= 4, "{targets}");

    let (mut store, _) = open(&path);
    assert_eq!(store.rebuild().unwrap(), 5);
    assert_eq!(dump(&path), truth_before);
    assert_eq!(dump_tables(&path, CACHES), caches_before);
}

#[test]
fn training_events_from_another_model_version_alone_trigger_a_rebuild() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        let (id, prompt) = start(&mut store, &profile, 1_000, "cat dog");
        let state = typed(prompt, "cat dog");
        finish_with(
            &mut store,
            &profile,
            id,
            1_000,
            &state,
            targeted("cat that the"),
        )
        .unwrap();
        let (id, prompt) = start(&mut store, &profile, 2_000, "unused unused unused");
        let state = typed(prompt, "cat that the");
        finish(&mut store, &profile, id, 2_000, &state, "next").unwrap();
    }
    let caches_before = dump_tables(&path, CACHES);
    sql(
        &path,
        "UPDATE pattern_training_events SET model_version = model_version + 1, achieved_dose = 99",
    );
    assert_ne!(dump_tables(&path, CACHES), caches_before);

    let (store, _) = open(&path);
    drop(store);
    assert_eq!(dump_tables(&path, CACHES), caches_before);
}

// --- Session summaries -----------------------------------------------------------

#[test]
fn a_completed_session_caches_its_summary_and_an_interrupted_one_does_not() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    assert_eq!(store.recent_series(&profile).unwrap(), None);

    let (completed, _) = type_session(&mut store, &profile, 1_000, "cat dog", "cat dog", "fox");
    let summary = store.session(completed).unwrap().summary.unwrap();
    assert_eq!(summary.gross_wpm, Some(140.0));
    assert_eq!(summary.raw_accuracy, 1.0);
    assert!(summary.reference_wpm.is_some());
    // The first completed session is its own baseline.
    assert_eq!(summary.recent.wpm, summary.gross_wpm);
    assert_eq!(summary.recent.reference_wpm, summary.reference_wpm);
    assert_eq!(store.recent_series(&profile).unwrap(), Some(summary.recent));
    let (rows, version): (i64, i64) = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT count(*), max(model_version) FROM session_metrics",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((rows, version), (1, i64::from(MODEL_VERSION)));

    let (interrupted, _) = type_session(&mut store, &profile, 2_000, "cat dog", "ca⎋", "fox");
    assert_eq!(store.session(interrupted).unwrap().summary, None);
    let rows: i64 = sql_one(&path, "SELECT count(*) FROM session_metrics");
    assert_eq!(rows, 1);
    assert_eq!(store.recent_series(&profile).unwrap(), Some(summary.recent));
}

#[test]
fn the_recent_series_advances_with_each_completed_session() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let (first, _) = type_session(&mut store, &profile, 1_000, "cat dog", "cat dog", "fox");
    // Twice as fast the second time.
    let (id, prompt) = start(&mut store, &profile, 2_000, "cat dog");
    let mut state = SessionState::new(prompt, EndCondition::AfterWords(2));
    for (i, c) in "cat dog".chars().enumerate() {
        state.apply_event(Input::new(i as u64 * 50_000, Key::Char(c)));
    }
    finish(&mut store, &profile, id, 2_000, &state, "fox").unwrap();

    let first = store.session(first).unwrap().summary.unwrap();
    let second = store.session(id).unwrap().summary.unwrap();
    assert_eq!(second.gross_wpm, Some(280.0));
    let expected = first.recent.advanced(
        &RecentSeries {
            wpm: second.gross_wpm,
            adjusted_ratio: second.adjusted_ratio,
            reference_wpm: second.reference_wpm,
        },
        store.config().recent_half_life_sessions,
    );
    assert_eq!(second.recent, expected);
    assert!(second.recent.wpm.unwrap() > 140.0 && second.recent.wpm.unwrap() < 280.0);
    assert_eq!(store.recent_series(&profile).unwrap(), Some(second.recent));
}

#[test]
fn a_prompts_words_read_back_with_their_roles_and_contamination() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    let composed = targeted("cat dog the");
    let started = store
        .start_session(
            &profile,
            SessionStart {
                started_at: 1_000,
                seed: 1,
                word_count: 3,
            },
            || composed.clone(),
        )
        .unwrap();
    assert_eq!(started.words, composed.words);
    assert_eq!(store.session(started.id).unwrap().words, composed.words);

    // Typed and summarized: "dog" is the one contaminated probe, "the" the
    // one clear of practice.
    let state = typed(started.prompt, "cat dog the");
    finish(&mut store, &profile, started.id, 1_000, &state, "next").unwrap();
    let summary = store.session(started.id).unwrap().summary.unwrap();
    assert_eq!(summary.probes.words, 1);
    assert_eq!(summary.contaminated_probes.words, 1);
}

#[test]
fn session_metrics_from_another_model_version_alone_trigger_a_rebuild() {
    let (_dir, path) = temp_db();
    {
        let (mut store, profile) = open(&path);
        type_history(&mut store, &profile);
    }
    let caches_before = dump_tables(&path, CACHES);
    assert_eq!(caches_before["session_metrics"].len(), 2);
    sql(
        &path,
        "UPDATE session_metrics SET model_version = model_version + 1, gross_wpm = 999",
    );
    assert_ne!(dump_tables(&path, CACHES), caches_before);

    let (store, _) = open(&path);
    drop(store);
    assert_eq!(dump_tables(&path, CACHES), caches_before);
}

#[test]
fn recomputing_a_summary_through_the_current_pipeline_reproduces_the_cached_one() {
    let (_dir, path) = temp_db();
    let (mut store, profile) = open(&path);
    type_history(&mut store, &profile);

    for id in 1..=4 {
        let id = SessionId::from_str(&id.to_string()).unwrap();
        let session = store.session(id).unwrap();
        let recomputed = store.recompute_summary(id).unwrap();
        assert_eq!(recomputed.stored, session.summary, "{id}");
        assert_eq!(recomputed.current, session.summary, "{id}");
        assert_eq!(
            recomputed.current.is_some(),
            session.outcome == Outcome::Completed,
            "{id}"
        );
    }
    // Nothing was written.
    let caches = dump_tables(&path, CACHES);
    assert_eq!(caches["session_metrics"].len(), 2);
}
