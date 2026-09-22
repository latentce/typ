//! The pattern statistics cache: a profile's model as rows, and keeping it
//! in step with the stored sessions.
//!
//! Every ended session is applied to its profile's model exactly once;
//! `sessions.applied_model_version` records that it was, and under which
//! model version. A session whose marker is null (recorded by a version of
//! `typ` without statistics, or nulled by a rebuild) is applied the next
//! time the store is opened. A cache stamped with another model version is
//! discarded and rebuilt from every ended session, oldest first, so the
//! statistics never mix two model versions.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use rusqlite::{Connection, TransactionBehavior, params};
use typ_rs_core::model::{MODEL_VERSION, ModelState, PatternStats, SchedulerConfig};

use crate::sessions::load_sessions;
use crate::{Profile, Result, Store};

impl Store {
    /// The profile's pattern statistics as last written.
    pub fn model(&self, profile: &Profile) -> Result<ModelState> {
        load(&self.conn, profile.id)
    }

    /// Discards every profile's statistics and recomputes them from the
    /// stored sessions in one transaction; returns how many sessions were
    /// applied. An interrupted rebuild leaves the previous cache in place.
    pub fn rebuild(&mut self) -> Result<usize> {
        let config = self.config;
        self.in_transaction(|tx| {
            tx.execute_batch(
                "UPDATE sessions SET applied_model_version = NULL;
                 DELETE FROM pattern_stats",
            )?;
            apply_unmarked(tx, &config)
        })
    }

    /// Brings the cache in step with the binary: rebuilds it if it was
    /// written under another model version, otherwise applies any ended
    /// session not yet applied. Takes no write lock when there is nothing
    /// to do, so `typ stats` beside a running session never waits.
    pub(crate) fn reconcile(&mut self) -> Result<()> {
        let stale: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pattern_stats WHERE model_version <> ?1)
                 OR EXISTS (SELECT 1 FROM sessions
                            WHERE applied_model_version IS NOT NULL
                              AND applied_model_version <> ?1)",
            [MODEL_VERSION],
            |row| row.get(0),
        )?;
        if stale {
            self.rebuild()?;
            return Ok(());
        }
        let pending: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sessions
                            WHERE ended_at IS NOT NULL AND applied_model_version IS NULL)",
            [],
            |row| row.get(0),
        )?;
        if pending {
            let config = self.config;
            self.in_transaction(|tx| apply_unmarked(tx, &config))?;
        }
        Ok(())
    }

    /// Runs `f` inside one write transaction, committed only if it succeeds.
    fn in_transaction<T>(&mut self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }
}

/// Applies every ended session without a marker, oldest first, to its
/// profile's model, writes the models back, and sets the markers.
fn apply_unmarked(conn: &Connection, config: &SchedulerConfig) -> Result<usize> {
    let pending = load_sessions(
        conn,
        "WHERE s.ended_at IS NOT NULL AND s.applied_model_version IS NULL
         ORDER BY s.started_at, s.id",
        [],
    )?;
    let mut models: BTreeMap<i64, ModelState> = BTreeMap::new();
    for session in &pending {
        let model = match models.entry(session.profile_id) {
            Entry::Occupied(model) => model.into_mut(),
            Entry::Vacant(slot) => slot.insert(load(conn, session.profile_id)?),
        };
        model.apply_session(&session.replay(), session.started_at, config);
        conn.execute(
            "UPDATE sessions SET applied_model_version = ?2 WHERE id = ?1",
            params![session.id.raw(), MODEL_VERSION],
        )?;
    }
    for (profile_id, model) in &models {
        write(conn, *profile_id, model.dirty())?;
    }
    Ok(pending.len())
}

pub(crate) fn load(conn: &Connection, profile_id: i64) -> Result<ModelState> {
    let mut stmt = conn.prepare_cached(
        "SELECT pattern, s0, s1, s2, w2, c, e, h, last_update
         FROM pattern_stats WHERE profile_id = ?1",
    )?;
    let rows = stmt
        .query_map([profile_id], |row| {
            Ok((
                row.get::<_, String>(0)?.into_boxed_str(),
                PatternStats {
                    s0: row.get(1)?,
                    s1: row.get(2)?,
                    s2: row.get(3)?,
                    w2: row.get(4)?,
                    c: row.get(5)?,
                    e: row.get(6)?,
                    h: row.get(7)?,
                    last_update: row.get(8)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ModelState::from_rows(rows))
}

/// Writes the given patterns' rows, stamped with the current model version.
pub(crate) fn write<'a>(
    conn: &Connection,
    profile_id: i64,
    rows: impl Iterator<Item = (&'a str, &'a PatternStats)>,
) -> Result<()> {
    let mut upsert = conn.prepare_cached(
        "INSERT INTO pattern_stats
             (profile_id, pattern, s0, s1, s2, w2, c, e, h, last_update, model_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (profile_id, pattern) DO UPDATE SET
             s0 = excluded.s0, s1 = excluded.s1, s2 = excluded.s2, w2 = excluded.w2,
             c = excluded.c, e = excluded.e, h = excluded.h,
             last_update = excluded.last_update, model_version = excluded.model_version",
    )?;
    for (pattern, s) in rows {
        upsert.execute(params![
            profile_id,
            pattern,
            s.s0,
            s.s1,
            s.s2,
            s.w2,
            s.c,
            s.e,
            s.h,
            s.last_update,
            MODEL_VERSION,
        ])?;
    }
    Ok(())
}
