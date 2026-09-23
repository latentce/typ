-- Cache: what each selected pattern of a session's prompt came to. One row
-- per session and pattern recorded in prompt_targets, repeating what the
-- model believed at selection and adding the exposures the session actually
-- typed. The scheduler's deferral windows and plateau checks are replayed
-- from these rows in session order. Rebuilt with pattern_stats.
CREATE TABLE pattern_training_events (
    profile_id    INTEGER NOT NULL REFERENCES profiles (id),
    session_id    INTEGER NOT NULL REFERENCES sessions (id),
    pattern       TEXT    NOT NULL,
    role          TEXT    NOT NULL CHECK (role IN ('target', 'deferred', 'explore')),
    weakness_mean REAL    NOT NULL,
    weakness_sd   REAL    NOT NULL,
    priority      REAL    NOT NULL,
    planned_dose  INTEGER NOT NULL,
    achieved_dose INTEGER NOT NULL,
    model_version INTEGER NOT NULL,
    PRIMARY KEY (session_id, pattern)
) STRICT;

CREATE INDEX pattern_training_events_by_profile ON pattern_training_events (profile_id, session_id);
