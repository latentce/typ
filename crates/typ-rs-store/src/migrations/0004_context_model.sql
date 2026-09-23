-- Cache: pattern_stats now also keeps, per pattern, the weighted sum of
-- each context feature over its latency observations, so that the sums
-- divided by s0 are the pattern's mean context. The table is recreated
-- rather than altered: the model version changes with it, so the cache is
-- rebuilt from the stored sessions on the next open anyway.
DROP TABLE pattern_stats;

CREATE TABLE pattern_stats (
    profile_id         INTEGER NOT NULL REFERENCES profiles (id),
    pattern            TEXT    NOT NULL,
    s0                 REAL    NOT NULL,
    s1                 REAL    NOT NULL,
    s2                 REAL    NOT NULL,
    w2                 REAL    NOT NULL,
    c                  REAL    NOT NULL,
    e                  REAL    NOT NULL,
    h                  REAL    NOT NULL,
    first_of_word      REAL    NOT NULL,
    last_of_word       REAL    NOT NULL,
    word_length        REAL    NOT NULL,
    log_word_frequency REAL    NOT NULL,
    boundary           REAL    NOT NULL,
    same_finger        REAL    NOT NULL,
    same_hand          REAL    NOT NULL,
    row_change         REAL    NOT NULL,
    key_distance       REAL    NOT NULL,
    last_update        INTEGER NOT NULL,
    model_version      INTEGER NOT NULL,
    PRIMARY KEY (profile_id, pattern)
) STRICT;

-- Cache: a profile's context model. completed_sessions counts the completed
-- sessions applied to the profile, which sets the fit cadence; fitted says
-- whether the coefficients have been fitted at all (until then they are
-- zero and so is every context effect). The feature columns hold the
-- fitted weight of each feature. Rebuilt with pattern_stats.
CREATE TABLE context_model (
    profile_id         INTEGER PRIMARY KEY REFERENCES profiles (id),
    completed_sessions INTEGER NOT NULL,
    fitted             INTEGER NOT NULL CHECK (fitted IN (0, 1)),
    intercept          REAL    NOT NULL,
    first_of_word      REAL    NOT NULL,
    last_of_word       REAL    NOT NULL,
    word_length        REAL    NOT NULL,
    log_word_frequency REAL    NOT NULL,
    boundary           REAL    NOT NULL,
    same_finger        REAL    NOT NULL,
    same_hand          REAL    NOT NULL,
    row_change         REAL    NOT NULL,
    key_distance       REAL    NOT NULL,
    model_version      INTEGER NOT NULL
) STRICT;
