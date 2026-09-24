//! The pattern statistics and context model caches: a profile's model as
//! rows, and keeping every cache (the training events included) in step
//! with the stored sessions.
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
use typ_rs_core::corpus::Corpus;
use typ_rs_core::layout::Layout;
use typ_rs_core::metrics::{RecentSeries, SessionSummary, summarize};
use typ_rs_core::model::context::{Coefficients, FEATURE_COUNT, Feature, Features};
use typ_rs_core::model::{ContextModel, MODEL_VERSION, ModelState, PatternStats, SchedulerConfig};
use typ_rs_core::scheduler::achieved_doses;
use typ_rs_core::session::SessionState;

use crate::sessions::{SessionId, StoredSession, load_sessions};
use crate::{Error, Profile, Result, Store, summaries, training};

/// A session's summary as the current pipeline computes it beside the one
/// cached for it.
#[derive(Debug, Clone, PartialEq)]
pub struct RecomputedSummary {
    /// From replaying the profile's sessions up to this one through the
    /// current model; `None` for an interrupted session.
    pub current: Option<SessionSummary>,
    /// The cached row; `None` for an interrupted session or one not yet
    /// applied.
    pub stored: Option<SessionSummary>,
}

impl Store {
    /// The profile's pattern statistics and context model as last written,
    /// for the layout the profile is bound to.
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
                 DELETE FROM pattern_stats;
                 DELETE FROM context_model;
                 DELETE FROM pattern_training_events;
                 DELETE FROM session_metrics",
            )?;
            apply_unmarked(tx, &config)
        })
    }

    /// Runs the profile's ended sessions up to and including `id`, oldest
    /// first, through a fresh model as a rebuild would, and returns the
    /// summary that gives the session beside the cached one. Nothing is
    /// written: this is how a change to the pipeline is checked against
    /// real data.
    pub fn recompute_summary(&self, id: SessionId) -> Result<RecomputedSummary> {
        let target = self.session(id)?;
        let earlier = load_sessions(
            &self.conn,
            "WHERE s.profile_id = ?1 AND s.ended_at IS NOT NULL
               AND (s.started_at < ?2 OR (s.started_at = ?2 AND s.id <= ?3))
             ORDER BY s.started_at, s.id",
            params![target.profile_id, target.started_at, id.raw()],
        )?;
        let corpus = Corpus::bundled();
        let mut model = ModelState::from_rows(
            load(&self.conn, target.profile_id)?.layout(),
            [],
            ContextModel::default(),
        );
        let mut recent: Option<RecentSeries> = None;
        let mut current = None;
        for session in &earlier {
            let state = session.replay();
            let summary = apply_one(
                &mut model,
                session,
                &state,
                recent.as_ref(),
                corpus,
                &self.config,
            );
            if let Some(summary) = &summary {
                recent = Some(summary.recent);
            }
            if session.id == id {
                current = summary;
            }
        }
        Ok(RecomputedSummary {
            current,
            stored: target.summary,
        })
    }

    /// Brings the cache in step with the binary: rebuilds it if it was
    /// written under another model version, otherwise applies any ended
    /// session not yet applied. Takes no write lock when there is nothing
    /// to do, so `typ stats` beside a running session never waits.
    pub(crate) fn reconcile(&mut self) -> Result<()> {
        let stale: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pattern_stats WHERE model_version <> ?1)
                 OR EXISTS (SELECT 1 FROM context_model WHERE model_version <> ?1)
                 OR EXISTS (SELECT 1 FROM pattern_training_events WHERE model_version <> ?1)
                 OR EXISTS (SELECT 1 FROM session_metrics WHERE model_version <> ?1)
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
/// profile's model, writes its training events and its summary, writes the
/// models back, and sets the markers. Each profile's recent series starts
/// from its latest summarised session before the first pending one and is
/// carried forward from there.
fn apply_unmarked(conn: &Connection, config: &SchedulerConfig) -> Result<usize> {
    let pending = load_sessions(
        conn,
        "WHERE s.ended_at IS NOT NULL AND s.applied_model_version IS NULL
         ORDER BY s.started_at, s.id",
        [],
    )?;
    let corpus = Corpus::bundled();
    let mut models: BTreeMap<i64, (ModelState, Option<RecentSeries>)> = BTreeMap::new();
    for session in &pending {
        let (model, recent) = match models.entry(session.profile_id) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(slot) => slot.insert((
                load(conn, session.profile_id)?,
                summaries::recent_before(
                    conn,
                    session.profile_id,
                    Some((session.started_at, session.id)),
                )?,
            )),
        };
        let state = session.replay();
        let summary = apply_one(model, session, &state, recent.as_ref(), corpus, config);
        training::write(
            conn,
            session.profile_id,
            session.id,
            &achieved_doses(&state, &session.targets),
        )?;
        if let Some(summary) = &summary {
            summaries::write(conn, session.profile_id, session.id, summary)?;
            *recent = Some(summary.recent);
        }
        conn.execute(
            "UPDATE sessions SET applied_model_version = ?2 WHERE id = ?1",
            params![session.id.raw(), MODEL_VERSION],
        )?;
    }
    for (profile_id, (model, _)) in &models {
        write(conn, *profile_id, model)?;
    }
    Ok(pending.len())
}

/// Applies one stored session, replayed as `state`, to the model and
/// summarises it, exactly as the session's own end did.
fn apply_one(
    model: &mut ModelState,
    session: &StoredSession,
    state: &SessionState,
    recent: Option<&RecentSeries>,
    corpus: &Corpus,
    config: &SchedulerConfig,
) -> Option<SessionSummary> {
    let update = model.apply_session(state, session.started_at, corpus, config);
    summarize(state, &update, &session.words, recent, config)
}

/// The feature columns, in feature order, as they appear in both caches.
fn feature_columns() -> String {
    Feature::ALL
        .iter()
        .map(|f| f.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `?first, ?first+1, ...`: one placeholder per feature column, numbered
/// on from the `first - 1` fixed columns before them.
fn feature_placeholders(first: usize) -> String {
    (first..first + FEATURE_COUNT)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Reads the feature columns, which start at column index `first`.
fn read_features(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<Features> {
    let mut features = [0.0; FEATURE_COUNT];
    for (i, f) in features.iter_mut().enumerate() {
        *f = row.get(first + i)?;
    }
    Ok(features)
}

pub(crate) fn load(conn: &Connection, profile_id: i64) -> Result<ModelState> {
    let (name, layout): (String, String) = conn.query_row(
        "SELECT name, layout FROM profiles WHERE id = ?1",
        [profile_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let layout = Layout::by_name(&layout).ok_or_else(|| Error::UnknownLayout {
        profile: name,
        layout,
    })?;

    let mut stmt = conn.prepare_cached(&format!(
        "SELECT pattern, s0, s1, s2, w2, c, e, h, last_update, {}
         FROM pattern_stats WHERE profile_id = ?1",
        feature_columns()
    ))?;
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
                    features: read_features(row, 9)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let context = conn
        .prepare_cached(&format!(
            "SELECT completed_sessions, fitted, intercept, {}
             FROM context_model WHERE profile_id = ?1",
            feature_columns()
        ))?
        .query_row([profile_id], |row| {
            let completed_sessions: u32 = row.get(0)?;
            let fitted: bool = row.get(1)?;
            let coefficients = fitted.then_some(Coefficients {
                intercept: row.get(2)?,
                weights: read_features(row, 3)?,
            });
            Ok(ContextModel {
                completed_sessions,
                coefficients,
            })
        })
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(ContextModel::default()),
            e => Err(e),
        })?;

    Ok(ModelState::from_rows(layout, rows, context))
}

/// Writes the model's changed pattern rows and its context model, stamped
/// with the current model version.
pub(crate) fn write(conn: &Connection, profile_id: i64, model: &ModelState) -> Result<()> {
    let columns = feature_columns();
    let updates: Vec<String> = Feature::ALL
        .iter()
        .map(|f| format!("{0} = excluded.{0}", f.name()))
        .collect();
    let mut upsert = conn.prepare_cached(&format!(
        "INSERT INTO pattern_stats
             (profile_id, pattern, s0, s1, s2, w2, c, e, h, last_update, model_version, {columns})
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, {})
         ON CONFLICT (profile_id, pattern) DO UPDATE SET
             s0 = excluded.s0, s1 = excluded.s1, s2 = excluded.s2, w2 = excluded.w2,
             c = excluded.c, e = excluded.e, h = excluded.h,
             last_update = excluded.last_update, model_version = excluded.model_version,
             {}",
        feature_placeholders(12),
        updates.join(", "),
    ))?;
    for (pattern, s) in model.dirty() {
        let mut values: Vec<rusqlite::types::Value> = vec![
            profile_id.into(),
            pattern.to_string().into(),
            s.s0.into(),
            s.s1.into(),
            s.s2.into(),
            s.w2.into(),
            s.c.into(),
            s.e.into(),
            s.h.into(),
            s.last_update.into(),
            MODEL_VERSION.into(),
        ];
        values.extend(s.features.iter().map(|&f| f.into()));
        upsert.execute(rusqlite::params_from_iter(values))?;
    }

    let context = model.context_model();
    let coefficients = context.coefficients.unwrap_or(Coefficients {
        intercept: 0.0,
        weights: [0.0; FEATURE_COUNT],
    });
    let mut values: Vec<rusqlite::types::Value> = vec![
        profile_id.into(),
        context.completed_sessions.into(),
        context.coefficients.is_some().into(),
        coefficients.intercept.into(),
        MODEL_VERSION.into(),
    ];
    values.extend(coefficients.weights.iter().map(|&w| w.into()));
    conn.prepare_cached(&format!(
        "INSERT OR REPLACE INTO context_model
             (profile_id, completed_sessions, fitted, intercept, model_version, {columns})
         VALUES (?1, ?2, ?3, ?4, ?5, {})",
        feature_placeholders(6)
    ))?
    .execute(rusqlite::params_from_iter(values))?;
    Ok(())
}
