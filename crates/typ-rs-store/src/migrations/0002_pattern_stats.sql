-- Cache: the decaying statistics of every pattern a profile's user has
-- typed, one row per pattern. The empty pattern is the root of the chain:
-- the user as a whole, whose latency sums are over log-latency itself so
-- that s1 / s0 is the user baseline. Every other row's latency sums are
-- over the session-adjusted log-latency residual. Sums are as of
-- last_update; reading them at a later time decays them first. Rebuilt
-- from the stored sessions whenever model_version differs from the
-- binary's.
CREATE TABLE pattern_stats (
    profile_id    INTEGER NOT NULL REFERENCES profiles (id),
    pattern       TEXT    NOT NULL,
    s0            REAL    NOT NULL,
    s1            REAL    NOT NULL,
    s2            REAL    NOT NULL,
    w2            REAL    NOT NULL,
    c             REAL    NOT NULL,
    e             REAL    NOT NULL,
    h             REAL    NOT NULL,
    last_update   INTEGER NOT NULL,
    model_version INTEGER NOT NULL,
    PRIMARY KEY (profile_id, pattern)
) STRICT;
