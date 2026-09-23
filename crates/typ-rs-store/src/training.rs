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
    /// events replayed in session order. This is what the next prompt is
    /// composed against.
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
        if let Some(targets) = prompts::next_targets(&self.conn, profile.id)? {
            let events: Vec<TrainingEvent> = targets
                .into_iter()
                .map(|target| TrainingEvent {
                    target,
                    achieved_dose: 0,
                })
                .collect();
            history.record(&events, &self.config);
        }
        Ok(history)
    }
}

/// Replays the profile's ended sessions, oldest first, each with its
/// events; a session with none still counts as a session.
fn load_history(
    conn: &Connection,
    profile_id: i64,
    config: &SchedulerConfig,
) -> Result<TrainingHistory> {
    let mut stmt = conn.prepare_cached(
        "SELECT s.id, e.pattern, e.role, e.weakness_mean, e.weakness_sd,
                e.priority, e.planned_dose, e.achieved_dose
         FROM sessions s
         LEFT JOIN pattern_training_events e ON e.session_id = s.id
         WHERE s.profile_id = ?1 AND s.ended_at IS NOT NULL
         ORDER BY s.started_at, s.id, e.rowid",
    )?;
    let rows = stmt.query_map([profile_id], |row| {
        let session: i64 = row.get(0)?;
        let pattern: Option<String> = row.get(1)?;
        let columns = match pattern {
            Some(pattern) => Some((
                TargetColumns {
                    pattern,
                    role: row.get(2)?,
                    weakness_mean: row.get(3)?,
                    weakness_sd: row.get(4)?,
                    priority: row.get(5)?,
                    planned_dose: row.get(6)?,
                },
                row.get::<_, i64>(7)?,
            )),
            None => None,
        };
        Ok((session, columns))
    })?;

    let mut history = TrainingHistory::new();
    let mut current: Option<(i64, Vec<TrainingEvent>)> = None;
    for row in rows {
        let (session, columns) = row?;
        let event = match columns {
            Some((target, achieved)) => Some(TrainingEvent {
                target: target.decode()?,
                achieved_dose: dose(achieved, "achieved_dose")?,
            }),
            None => None,
        };
        match &mut current {
            Some((id, events)) if *id == session => events.extend(event),
            _ => {
                if let Some((_, events)) = current.take() {
                    history.record(&events, config);
                }
                current = Some((session, event.into_iter().collect()));
            }
        }
    }
    if let Some((_, events)) = current {
        history.record(&events, config);
    }
    Ok(history)
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
