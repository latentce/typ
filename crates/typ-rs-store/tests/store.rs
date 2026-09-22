use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{EndCondition, Input, Key, Outcome, SessionState};
use typ_rs_store::{DEFAULT_PROFILE, Error, Profile, SessionId, SessionStart, Store};

const SOURCE_OF_TRUTH: &[&str] = &[
    "profiles",
    "settings",
    "sessions",
    "prompts",
    "prompt_words",
    "prompt_targets",
    "input_events",
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

fn start(
    store: &mut Store,
    profile: &Profile,
    started_at: i64,
    fallback: &str,
) -> (SessionId, Prompt) {
    let started = store
        .start_session(
            profile,
            SessionStart {
                started_at,
                seed: 42,
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
    store
        .finish_session(id, &state, self::prompt(next), started_at + 60)
        .unwrap();
    (id, state)
}

/// Every row of every source-of-truth table, rendered as text, keyed by table.
fn dump(path: &Path) -> BTreeMap<&'static str, Vec<String>> {
    let conn = Connection::open(path).unwrap();
    SOURCE_OF_TRUTH
        .iter()
        .map(|table| {
            let mut stmt = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
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
    assert_eq!(versions, vec![1]);

    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for table in SOURCE_OF_TRUTH
        .iter()
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
    assert_eq!((migrations, profiles), (1, 1));
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

    let (_, shown) = start(&mut store, &profile, 2_000, "fallback");
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
    store
        .finish_session(id, &state, self::prompt("next"), 1_060)
        .unwrap();

    let again = store.finish_session(id, &state, self::prompt("other"), 1_120);
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
