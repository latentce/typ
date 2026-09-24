-- Cache: the figures of every completed session as shown when it ended and
-- listed by stats. Rebuilt with pattern_stats: the difficulty adjustment
-- depends on the model as it stood at session start, and the recent
-- series on the completed sessions before it, so the rows are recomputed
-- in session order whenever the model version changes. Speed columns are
-- NULL where the session had nothing to measure them with.
CREATE TABLE session_metrics (
    session_id                      INTEGER PRIMARY KEY REFERENCES sessions (id),
    profile_id                      INTEGER NOT NULL REFERENCES profiles (id),
    gross_wpm                       REAL,
    raw_accuracy                    REAL    NOT NULL,
    final_accuracy                  REAL    NOT NULL,
    consistency                     REAL,
    corrections                     INTEGER NOT NULL,
    session_offset                  REAL    NOT NULL,
    adjusted_ratio                  REAL,
    reference_wpm                   REAL,
    recent_wpm                      REAL,
    recent_adjusted_ratio           REAL,
    recent_reference_wpm            REAL,
    probe_words                     INTEGER NOT NULL,
    probe_wpm                       REAL,
    probe_raw_accuracy              REAL,
    contaminated_probe_words        INTEGER NOT NULL,
    contaminated_probe_wpm          REAL,
    contaminated_probe_raw_accuracy REAL,
    model_version                   INTEGER NOT NULL
) STRICT;

CREATE INDEX session_metrics_by_profile ON session_metrics (profile_id, session_id);
