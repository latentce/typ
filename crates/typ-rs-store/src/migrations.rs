//! Brings a database file up to the current schema.
//!
//! Each migration is a numbered SQL file applied once, in order, inside one
//! transaction; `schema_migrations` records which have run. A schema is only
//! ever extended by appending a migration, never by editing an applied one.

use rusqlite::{Connection, TransactionBehavior, params};

use crate::{Error, Result};

const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_initial.sql"),
    include_str!("migrations/0002_pattern_stats.sql"),
    include_str!("migrations/0003_next_prompt_context.sql"),
    include_str!("migrations/0004_context_model.sql"),
    include_str!("migrations/0005_training_events.sql"),
    include_str!("migrations/0006_session_metrics.sql"),
];

pub(crate) fn run(conn: &mut Connection, now: i64) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             applied_at INTEGER NOT NULL
         ) STRICT",
    )?;
    let applied: i64 = tx.query_row(
        "SELECT ifnull(max(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    let supported = MIGRATIONS.len() as i64;
    if applied > supported {
        return Err(Error::SchemaTooNew {
            found: applied,
            supported,
        });
    }
    let applied = usize::try_from(applied)
        .map_err(|_| Error::Corrupt(format!("schema version {applied} is negative")))?;

    for (i, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![i as i64 + 1, now],
        )?;
    }
    tx.commit()?;
    Ok(())
}
