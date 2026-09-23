use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;
use typ_rs_core::corpus::CORPUS_VERSION;
use typ_rs_core::model::{MODEL_VERSION, ModelState, SchedulerConfig};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{EndCondition, Input, Key, Outcome, SessionState};
use typ_rs_store::{
    DEFAULT_PROFILE, DEFAULT_WORDS, Error, Profile, SessionId, SessionStart, Store, parse_words,
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

const CACHES: &[&str] = &["pattern_stats"];

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
            || prompt(fallback),
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
/// applied to it, and the result handed to the store with the next prompt.
fn finish(
    store: &mut Store,
    profile: &Profile,
    id: SessionId,
    started_at: i64,
    state: &SessionState,
    next: &str,
) -> typ_rs_store::Result<()> {
    let mut model = store.model(profile).unwrap();
    model.apply_session(state, started_at, store.config());
    store.finish_session(id, state, &model, prompt(next), started_at + 60)
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
    let conn = Connection::open(path).unwrap();
    tables
        .iter()
        .map(|table| {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))
                .unwrap();
            let columns = stmt.column_count();
            let rows = stmt
                .query_map([], |row| {
                    (0..columns)
                        .map(|i| row.get_ref(i).map(|v| format!("{v:?}")))
                        .collect::<Result<Vec<_>, _>>()
                        .map(|cells| cells.join("|"))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
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
    assert_eq!(versions, vec![1, 2, 3]);

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
    assert_eq!((migrations, profiles), (3, 1));
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
    assert_eq!(profile.layout, "qwerty");
    assert_eq!(profile.mode, "words");
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
/// many prompts are still waiting afterwards.
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
    model.apply_session(&state, 1_000, store.config());
    store
        .finish_session(id, &state, &model, self::prompt("next"), 1_060)
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
