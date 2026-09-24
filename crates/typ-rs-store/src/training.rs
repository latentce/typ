//! The training-events cache: what came of every pattern selected for a
//! session, and the profile's training history replayed from it.

use rusqlite::{Connection, params};
use typ_rs_core::model::{MODEL_VERSION, SchedulerConfig};
use typ_rs_core::scheduler::{TrainingEvent, TrainingHistory};

use crate::prompts::{self, TargetColumns, dose};
use crate::sessions::SessionId;
use crate::{Profile, Result, Store};

impl Store {
    /// The profile's training history: every ended session's training
    /// events and targeted words replayed in session order. This is what
    /// the next prompt is composed against.
    pub fn training_history(&self, profile: &Profile) -> Result<TrainingHistory> {
        load_history(&self.conn, profile.id, &self.config)
    }

    /// The training history with the prompt waiting for the next session
    /// counted as one more, so that a deferral decided at the last
    /// session's end already shows. The waiting prompt has not been typed,
    /// so its events carry no achieved dose. For showing the user, not for
    /// composing: a waiting prompt may yet be discarded.
    pub fn training_history_with_waiting_prompt(
        &self,
        profile: &Profile,
    ) -> Result<TrainingHistory> {
        let mut history = self.training_history(profile)?;
        if let Some(waiting) = prompts::next_waiting(&self.conn, profile.id)? {
            let targeted_words = waiting.targeted_words();
            let events: Vec<TrainingEvent> = waiting
                .targets
                .into_iter()
                .map(|target| TrainingEvent {
                    target,
                    achieved_dose: 0,
                })
                .collect();
            history.record(
                &events,
                targeted_words.iter().map(AsRef::as_ref),
                &self.config,
            );
        }
        Ok(history)
    }
}

/// Replays the profile's ended sessions, oldest first, each with its
/// events and the words its prompt showed as targeted; a session with
/// neither still counts as a session.
fn load_history(
    conn: &Connection,
    profile_id: i64,
    config: &SchedulerConfig,
) -> Result<TrainingHistory> {
    let sessions: Vec<(i64, i64)> = conn
        .prepare_cached(
            "SELECT id, prompt_id FROM sessions
             WHERE profile_id = ?1 AND ended_at IS NOT NULL
             ORDER BY started_at, id",
        )?
        .query_map([profile_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut history = TrainingHistory::new();
    for (session_id, prompt_id) in sessions {
        let events = load_events(conn, session_id)?;
        let targeted_words = prompts::load_targeted_words(conn, prompt_id)?;
        history.record(&events, targeted_words.iter().map(AsRef::as_ref), config);
    }
    Ok(history)
}

/// One session's training events in the order they were written.
fn load_events(conn: &Connection, session_id: i64) -> Result<Vec<TrainingEvent>> {
    let mut stmt = conn.prepare_cached(
        "SELECT pattern, role, weakness_mean, weakness_sd, priority, planned_dose, achieved_dose
         FROM pattern_training_events WHERE session_id = ?1 ORDER BY rowid",
    )?;
    let rows = stmt.query_map([session_id], |row| {
        Ok((
            TargetColumns {
                pattern: row.get(0)?,
                role: row.get(1)?,
                weakness_mean: row.get(2)?,
                weakness_sd: row.get(3)?,
                priority: row.get(4)?,
                planned_dose: row.get(5)?,
            },
            row.get::<_, i64>(6)?,
        ))
    })?;
    rows.map(|row| {
        let (target, achieved) = row?;
        Ok(TrainingEvent {
            target: target.decode()?,
            achieved_dose: dose(achieved, "achieved_dose")?,
        })
    })
    .collect()
}

/// Writes one session's training events, stamped with the current model
/// version, replacing any already there for the session.
pub(crate) fn write(
    conn: &Connection,
    profile_id: i64,
    session: SessionId,
    events: &[TrainingEvent],
) -> Result<()> {
    let mut insert = conn.prepare_cached(
        "INSERT OR REPLACE INTO pattern_training_events
             (profile_id, session_id, pattern, role, weakness_mean, weakness_sd,
              priority, planned_dose, achieved_dose, model_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for e in events {
        let t = &e.target;
        insert.execute(params![
            profile_id,
            session.raw(),
            t.pattern.as_ref(),
            t.role.name(),
            t.weakness_mean,
            t.weakness_sd,
            t.priority,
            t.planned_dose as i64,
            e.achieved_dose as i64,
            MODEL_VERSION,
        ])?;
    }
    Ok(())
}
