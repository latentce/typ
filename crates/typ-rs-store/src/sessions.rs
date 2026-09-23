//! Session rows and their input events.

use std::fmt;
use std::num::ParseIntError;
use std::str::FromStr;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use typ_rs_core::corpus::CORPUS_VERSION;
use typ_rs_core::model::{MODEL_VERSION, ModelState};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{
    EndCondition, EventFlags, EventKind, InputEvent, Outcome, SEMANTICS_VERSION, SessionState,
};

use crate::prompts::{self, Context};
use crate::{Error, Profile, Result, Store, model};

/// A session's row id, the handle a user names a stored session by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(i64);

impl SessionId {
    pub(crate) fn raw(self) -> i64 {
        self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for SessionId {
    type Err = ParseIntError;

    fn from_str(s: &str) -> std::result::Result<SessionId, ParseIntError> {
        s.parse().map(SessionId)
    }
}

/// What is known about a session before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionStart {
    /// Wall clock, Unix seconds.
    pub started_at: i64,
    /// Seeds the randomness drawn at this session's end when the next prompt
    /// is composed.
    pub seed: u64,
    /// How many words the session is to have: the profile's setting, or an
    /// override for this session alone. A prompt composed ahead with another
    /// length is not shown.
    pub word_count: usize,
}

/// A session whose row exists and whose prompt is ready to be typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedSession {
    pub id: SessionId,
    pub prompt: Prompt,
}

/// A session read back from the database.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredSession {
    pub id: SessionId,
    pub(crate) profile_id: i64,
    /// Wall clock, Unix seconds.
    pub started_at: i64,
    /// `started_at` in the local time zone as `YYYY-MM-DD HH:MM`, formatted
    /// by SQLite since the standard library has no time-zone support.
    pub started_at_local: String,
    /// `None` while the session runs, and forever if the process died in it.
    pub ended_at: Option<i64>,
    /// `Interrupted` until the session ends, and for a session the process
    /// died in.
    pub outcome: Outcome,
    /// The editing rules the events were captured under.
    pub semantics_version: u32,
    pub prompt: Prompt,
    pub events: Vec<InputEvent>,
}

impl StoredSession {
    /// The state the session ended in, rebuilt from its events under the
    /// current editing rules; identical to the live state when
    /// `semantics_version` is the current one.
    pub fn replay(&self) -> SessionState {
        SessionState::replay(
            self.prompt.clone(),
            EndCondition::AfterWords(self.prompt.word_count()),
            &self.events,
        )
    }
}

impl Store {
    /// Records that a session is about to start and returns its prompt: the
    /// one composed ahead for the profile if there is one and it was
    /// composed for a session like this one (`start.word_count` words, this
    /// corpus and model version, the profile's layout), otherwise the one
    /// `compose` produces, which must have `start.word_count` words. A
    /// waiting prompt is consumed either way, so it is never shown twice and
    /// a stale one is gone. The row is committed with status `interrupted`
    /// before this returns, so a session the process dies in is still
    /// recorded. The store's config is recorded with the row as what the
    /// session ran under.
    pub fn start_session(
        &mut self,
        profile: &Profile,
        start: SessionStart,
        compose: impl FnOnce() -> Prompt,
    ) -> Result<StartedSession> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let wanted = Context::current(start.word_count, &profile.layout);
        let (prompt_id, prompt) = match prompts::take_next(&tx, profile.id, &wanted)? {
            Some(waiting) => waiting,
            None => {
                let prompt = compose();
                let id = prompts::insert(&tx, profile.id, &prompt, start.started_at)?;
                (id, prompt)
            }
        };
        tx.execute(
            "INSERT INTO sessions
                 (profile_id, prompt_id, started_at, status, mode,
                  corpus_version, semantics_version, config_json, seed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                profile.id,
                prompt_id,
                start.started_at,
                Outcome::Interrupted.name(),
                profile.mode,
                CORPUS_VERSION,
                SEMANTICS_VERSION,
                self.config.to_json(),
                start.seed as i64,
            ],
        )?;
        let id = SessionId(tx.last_insert_rowid());
        tx.commit()?;
        Ok(StartedSession { id, prompt })
    }

    /// Ends a session in one transaction: writes its events, sets its status
    /// from the state's outcome, writes the pattern statistics the session
    /// changed (`model` is the profile's model with the session applied),
    /// marks the session applied under the current model version, and stores
    /// `next_prompt` as the prompt for the profile's next session, recorded
    /// as composed for its own length, this corpus and model version, and
    /// the profile's layout. A session can end only once.
    pub fn finish_session(
        &mut self,
        id: SessionId,
        state: &SessionState,
        model: &ModelState,
        next_prompt: Prompt,
        ended_at: i64,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (profile_id, layout, already_ended): (i64, String, Option<i64>) = tx
            .query_row(
                "SELECT s.profile_id, p.layout, s.ended_at
                 FROM sessions s JOIN profiles p ON p.id = s.profile_id
                 WHERE s.id = ?1",
                [id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(Error::NoSuchSession(id))?;
        if already_ended.is_some() {
            return Err(Error::SessionAlreadyEnded(id));
        }

        insert_events(&tx, id, state.events())?;
        let outcome = state.outcome().unwrap_or(Outcome::Interrupted);
        tx.execute(
            "UPDATE sessions SET status = ?2, ended_at = ?3, applied_model_version = ?4
             WHERE id = ?1",
            params![id.0, outcome.name(), ended_at, MODEL_VERSION],
        )?;
        model::write(&tx, profile_id, model.dirty())?;

        let next_id = prompts::insert(&tx, profile_id, &next_prompt, ended_at)?;
        let context = Context::current(next_prompt.word_count(), &layout);
        prompts::set_next(&tx, profile_id, next_id, &context)?;
        tx.commit()?;
        Ok(())
    }

    /// Reads one session back, whether or not it has ended.
    pub fn session(&self, id: SessionId) -> Result<StoredSession> {
        load_sessions(&self.conn, "WHERE s.id = ?1", [id.0])?
            .pop()
            .ok_or(Error::NoSuchSession(id))
    }

    /// The profile's completed sessions, most recent first. A session with
    /// no events is never listed; today that follows from completion needing
    /// a keystroke, but the listing checks rather than relies on it.
    pub fn completed_sessions(
        &self,
        profile: &Profile,
        limit: usize,
    ) -> Result<Vec<StoredSession>> {
        load_sessions(
            &self.conn,
            "WHERE s.profile_id = ?1 AND s.status = 'completed'
               AND EXISTS (SELECT 1 FROM input_events e WHERE e.session_id = s.id)
             ORDER BY s.started_at DESC, s.id DESC LIMIT ?2",
            params![profile.id, limit as i64],
        )
    }
}

/// Loads the sessions matching `clause`.
pub(crate) fn load_sessions(
    conn: &Connection,
    clause: &str,
    bindings: impl rusqlite::Params,
) -> Result<Vec<StoredSession>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT s.id, s.started_at,
                strftime('%Y-%m-%d %H:%M', s.started_at, 'unixepoch', 'localtime'),
                s.ended_at, s.status, s.semantics_version, s.prompt_id, s.profile_id
         FROM sessions s {clause}"
    ))?;
    let rows: Vec<SessionRow> = stmt
        .query_map(bindings, |row| {
            Ok(SessionRow {
                id: SessionId(row.get(0)?),
                started_at: row.get(1)?,
                started_at_local: row.get(2)?,
                ended_at: row.get(3)?,
                status: row.get(4)?,
                semantics_version: row.get(5)?,
                prompt_id: row.get(6)?,
                profile_id: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    rows.into_iter()
        .map(|row| {
            Ok(StoredSession {
                id: row.id,
                profile_id: row.profile_id,
                started_at: row.started_at,
                started_at_local: row.started_at_local,
                ended_at: row.ended_at,
                outcome: Outcome::from_name(&row.status).ok_or_else(|| {
                    Error::Corrupt(format!("unknown session status {:?}", row.status))
                })?,
                semantics_version: row.semantics_version,
                prompt: prompts::load(conn, row.prompt_id)?,
                events: load_events(conn, row.id)?,
            })
        })
        .collect()
}

/// One row of `sessions` as read, before its prompt and events are loaded.
struct SessionRow {
    id: SessionId,
    started_at: i64,
    started_at_local: String,
    ended_at: Option<i64>,
    status: String,
    semantics_version: u32,
    prompt_id: i64,
    profile_id: i64,
}

fn insert_events(conn: &Connection, session: SessionId, events: &[InputEvent]) -> Result<()> {
    let mut insert = conn.prepare_cached(
        "INSERT INTO input_events
             (session_id, seq, at_micros, kind, expected, actual, word_index, position,
              first_of_session, after_resize, in_paste, burst, long_pause)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    for e in events {
        insert.execute(params![
            session.0,
            e.seq,
            e.at_micros as i64,
            e.kind.name(),
            e.expected.map(String::from),
            e.actual.map(String::from),
            e.word_index as i64,
            e.position as i64,
            e.flags.first_of_session,
            e.flags.after_resize,
            e.flags.in_paste,
            e.flags.burst,
            e.flags.long_pause,
        ])?;
    }
    Ok(())
}

fn load_events(conn: &Connection, session: SessionId) -> Result<Vec<InputEvent>> {
    let mut stmt = conn.prepare_cached(
        "SELECT seq, at_micros, kind, expected, actual, word_index, position,
                first_of_session, after_resize, in_paste, burst, long_pause
         FROM input_events WHERE session_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map([session.0], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            EventFlags {
                first_of_session: row.get(7)?,
                after_resize: row.get(8)?,
                in_paste: row.get(9)?,
                burst: row.get(10)?,
                long_pause: row.get(11)?,
            },
        ))
    })?;

    rows.map(|row| {
        let (seq, at_micros, kind, expected, actual, word_index, position, flags) = row?;
        let kind = EventKind::from_name(&kind)
            .ok_or_else(|| Error::Corrupt(format!("unknown event kind {kind:?}")))?;
        let actual = single_char(actual)?;
        if kind == EventKind::Char && actual.is_none() {
            return Err(Error::Corrupt(format!(
                "event {seq} of session {session} is a char with no character"
            )));
        }
        Ok(InputEvent {
            seq,
            at_micros: non_negative(at_micros, "at_micros")?,
            kind,
            expected: single_char(expected)?,
            actual,
            word_index: index(word_index, "word_index")?,
            position: index(position, "position")?,
            flags,
        })
    })
    .collect()
}

fn non_negative(value: i64, column: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::Corrupt(format!("{column} {value} is negative")))
}

fn index(value: i64, column: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::Corrupt(format!("{column} {value} is negative")))
}

fn single_char(text: Option<String>) -> Result<Option<char>> {
    match text {
        None => Ok(None),
        Some(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Ok(Some(c)),
                _ => Err(Error::Corrupt(format!(
                    "event character column holds {s:?}, not one character"
                ))),
            }
        }
    }
}
