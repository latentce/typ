//! The session-metrics cache: a completed session's summary as shown when it
//! ended, and the recent series the next session is compared with.

use rusqlite::{Connection, OptionalExtension, params};
use typ_rs_core::metrics::{ProbeMetrics, RecentSeries, SessionSummary};
use typ_rs_core::model::MODEL_VERSION;

use crate::sessions::SessionId;
use crate::{Error, Profile, Result, Store};

impl Store {
    /// The recent series as of the profile's latest completed session with
    /// a summary; `None` before the first. What the next session's figures
    /// are compared with and advance from.
    pub fn recent_series(&self, profile: &Profile) -> Result<Option<RecentSeries>> {
        recent_before(&self.conn, profile.id, None)
    }
}

/// The recent series as of the latest summarized session of the profile
/// that started before `before` (a start time and id), or of all of them.
pub(crate) fn recent_before(
    conn: &Connection,
    profile_id: i64,
    before: Option<(i64, SessionId)>,
) -> Result<Option<RecentSeries>> {
    let (started_at, id) = before.map_or((i64::MAX, i64::MAX), |(at, id)| (at, id.raw()));
    let recent = conn
        .prepare_cached(
            "SELECT m.recent_wpm, m.recent_adjusted_ratio, m.recent_reference_wpm
             FROM session_metrics m JOIN sessions s ON s.id = m.session_id
             WHERE m.profile_id = ?1
               AND (s.started_at < ?2 OR (s.started_at = ?2 AND s.id < ?3))
             ORDER BY s.started_at DESC, s.id DESC LIMIT 1",
        )?
        .query_row(params![profile_id, started_at, id], |row| {
            Ok(RecentSeries {
                wpm: row.get(0)?,
                adjusted_ratio: row.get(1)?,
                reference_wpm: row.get(2)?,
            })
        })
        .optional()?;
    Ok(recent)
}

pub(crate) fn load(conn: &Connection, session: SessionId) -> Result<Option<SessionSummary>> {
    let row = conn
        .prepare_cached(
            "SELECT gross_wpm, raw_accuracy, final_accuracy, consistency, corrections,
                    session_offset, adjusted_ratio, reference_wpm,
                    recent_wpm, recent_adjusted_ratio, recent_reference_wpm,
                    probe_words, probe_wpm, probe_raw_accuracy,
                    contaminated_probe_words, contaminated_probe_wpm,
                    contaminated_probe_raw_accuracy
             FROM session_metrics WHERE session_id = ?1",
        )?
        .query_row([session.raw()], |row| {
            Ok(SummaryRow {
                gross_wpm: row.get(0)?,
                raw_accuracy: row.get(1)?,
                final_accuracy: row.get(2)?,
                consistency: row.get(3)?,
                corrections: row.get(4)?,
                session_offset: row.get(5)?,
                adjusted_ratio: row.get(6)?,
                reference_wpm: row.get(7)?,
                recent: RecentSeries {
                    wpm: row.get(8)?,
                    adjusted_ratio: row.get(9)?,
                    reference_wpm: row.get(10)?,
                },
                probe_words: row.get(11)?,
                probe_wpm: row.get(12)?,
                probe_raw_accuracy: row.get(13)?,
                contaminated_probe_words: row.get(14)?,
                contaminated_probe_wpm: row.get(15)?,
                contaminated_probe_raw_accuracy: row.get(16)?,
            })
        })
        .optional()?;
    row.map(SummaryRow::decode).transpose()
}

/// One row of `session_metrics` as read, before its counts are validated.
struct SummaryRow {
    gross_wpm: Option<f64>,
    raw_accuracy: f64,
    final_accuracy: f64,
    consistency: Option<f64>,
    corrections: i64,
    session_offset: f64,
    adjusted_ratio: Option<f64>,
    reference_wpm: Option<f64>,
    recent: RecentSeries,
    probe_words: i64,
    probe_wpm: Option<f64>,
    probe_raw_accuracy: Option<f64>,
    contaminated_probe_words: i64,
    contaminated_probe_wpm: Option<f64>,
    contaminated_probe_raw_accuracy: Option<f64>,
}

impl SummaryRow {
    fn decode(self) -> Result<SessionSummary> {
        Ok(SessionSummary {
            gross_wpm: self.gross_wpm,
            raw_accuracy: self.raw_accuracy,
            final_accuracy: self.final_accuracy,
            consistency: self.consistency,
            corrections: count(self.corrections, "corrections")?,
            session_offset: self.session_offset,
            adjusted_ratio: self.adjusted_ratio,
            reference_wpm: self.reference_wpm,
            recent: self.recent,
            probes: ProbeMetrics {
                words: count(self.probe_words, "probe_words")?,
                wpm: self.probe_wpm,
                raw_accuracy: self.probe_raw_accuracy,
            },
            contaminated_probes: ProbeMetrics {
                words: count(self.contaminated_probe_words, "contaminated_probe_words")?,
                wpm: self.contaminated_probe_wpm,
                raw_accuracy: self.contaminated_probe_raw_accuracy,
            },
        })
    }
}

/// A count column, which can only be non-negative.
fn count(value: i64, column: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::Corrupt(format!("{column} {value} is negative")))
}

/// Writes a session's summary, stamped with the current model version,
/// replacing any already there for the session.
pub(crate) fn write(
    conn: &Connection,
    profile_id: i64,
    session: SessionId,
    summary: &SessionSummary,
) -> Result<()> {
    conn.prepare_cached(
        "INSERT OR REPLACE INTO session_metrics
             (session_id, profile_id, gross_wpm, raw_accuracy, final_accuracy, consistency,
              corrections, session_offset, adjusted_ratio, reference_wpm,
              recent_wpm, recent_adjusted_ratio, recent_reference_wpm,
              probe_words, probe_wpm, probe_raw_accuracy,
              contaminated_probe_words, contaminated_probe_wpm, contaminated_probe_raw_accuracy,
              model_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                 ?19, ?20)",
    )?
    .execute(params![
        session.raw(),
        profile_id,
        summary.gross_wpm,
        summary.raw_accuracy,
        summary.final_accuracy,
        summary.consistency,
        summary.corrections as i64,
        summary.session_offset,
        summary.adjusted_ratio,
        summary.reference_wpm,
        summary.recent.wpm,
        summary.recent.adjusted_ratio,
        summary.recent.reference_wpm,
        summary.probes.words as i64,
        summary.probes.wpm,
        summary.probes.raw_accuracy,
        summary.contaminated_probes.words as i64,
        summary.contaminated_probes.wpm,
        summary.contaminated_probes.raw_accuracy,
        MODEL_VERSION,
    ])?;
    Ok(())
}
