//! Persistence for `typ`: profiles and their settings, prompts with the
//! patterns selected for them, sessions, and input events in a local SQLite
//! file, plus the caches derived from them: the pattern statistics, the
//! context model, the training events, each completed session's summary,
//! and the prompt composed ahead for the next session.
//!
//! The database is opened once per process. Sessions, prompts, and input
//! events are the source of truth: a session's row is written before it
//! starts and its events at the end, and neither is changed once it has
//! ended. While an attempt is in flight its row may still move: a restart
//! moves it to a fresh prompt and deletes the old one, and an attempt left
//! untyped is deleted with its prompt put back to wait for the next run.
//! Everything else is a cache rebuilt from them: the pattern statistics
//! whenever the model version changes, the next prompt when a session
//! ends. Nothing here runs while a session is being typed.

mod json;
mod migrations;
mod model;
mod prompts;
mod sessions;
mod settings;
mod summaries;
mod training;

use std::fmt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};
use typ_rs_core::model::SchedulerConfig;

pub use model::RecomputedSummary;
pub use sessions::{SessionEnd, SessionId, SessionStart, StartedSession, StoredSession};
pub use settings::{DEFAULT_WORDS, WORDS_RANGE, parse_words};

/// The profile every session belongs to until the user names another.
pub const DEFAULT_PROFILE: &str = "default";

/// The only session mode: a fixed number of words.
const WORDS_MODE: &str = "words";

/// How long a connection waits for another process's transaction to finish
/// before giving up. Transactions are short, so this is never approached in
/// normal use.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum Error {
    /// SQLite refused or failed an operation.
    Database(rusqlite::Error),
    /// The file was written by a newer `typ` whose schema this one does not know.
    SchemaTooNew { found: i64, supported: i64 },
    /// No profile row has the given name.
    NoSuchProfile(String),
    /// The profile is bound to a layout this build has no geometry for, so
    /// its model cannot be read or extended.
    UnknownLayout { profile: String, layout: String },
    /// No session row has the given id.
    NoSuchSession(SessionId),
    /// The session has already ended; its rows are never changed again.
    SessionAlreadyEnded(SessionId),
    /// The session has input events stored for it, so it was typed and is
    /// not a discarded attempt.
    SessionTyped(SessionId),
    /// A setting was given a value it cannot take, or a change it does not
    /// allow; nothing was changed. The message is complete on its own.
    InvalidSetting(String),
    /// A row that cannot be interpreted, such as a prompt with no words.
    Corrupt(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Database(e) => write!(f, "database error: {e}"),
            Error::SchemaTooNew { found, supported } => write!(
                f,
                "the database schema is version {found}, newer than the version {supported} this build supports"
            ),
            Error::NoSuchProfile(name) => write!(f, "no profile named {name:?}"),
            Error::UnknownLayout { profile, layout } => write!(
                f,
                "profile {profile:?} is bound to layout {layout:?}, which this build does not know"
            ),
            Error::NoSuchSession(id) => write!(f, "no session {id}"),
            Error::SessionAlreadyEnded(id) => write!(f, "session {id} has already ended"),
            Error::SessionTyped(id) => write!(f, "session {id} has been typed"),
            Error::InvalidSetting(message) => f.write_str(message),
            Error::Corrupt(what) => write!(f, "corrupt database: {what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Database(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        Error::Database(e)
    }
}

/// An isolated set of statistics for one typing condition: one layout and
/// one session mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    id: i64,
    pub name: String,
    /// The layout the profile is bound to; see [`Store::set_layout`].
    pub layout: String,
    pub mode: String,
}

/// An open database.
pub struct Store {
    conn: Connection,
    config: SchedulerConfig,
}

impl Store {
    /// Opens or creates the database file, brings its schema up to date,
    /// makes sure the default profile exists, and brings the pattern
    /// statistics in step with this binary: rebuilt if they were written
    /// under another model version, otherwise extended with any ended
    /// session not yet applied. The parent directory must already exist.
    pub fn open(path: &Path) -> Result<Store> {
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;

        let now = unix_now();
        migrations::run(&mut conn, now)?;
        let mut store = Store {
            conn,
            config: SchedulerConfig::default(),
        };
        store.profile_or_create(DEFAULT_PROFILE)?;
        store.reconcile()?;
        Ok(store)
    }

    /// The tunables every session run through this store is recorded with
    /// and applied under.
    pub fn config(&self) -> &SchedulerConfig {
        &self.config
    }

    /// Looks a profile up by name.
    pub fn profile(&self, name: &str) -> Result<Profile> {
        self.conn
            .query_row(
                "SELECT id, name, layout, mode FROM profiles WHERE name = ?1",
                [name],
                |row| {
                    Ok(Profile {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        layout: row.get(2)?,
                        mode: row.get(3)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::NoSuchProfile(name.to_string()))
    }
}

/// The wall clock in Unix seconds, the form every stored time takes.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
