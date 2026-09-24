-- Source of truth: written while a session runs and at its end, never
-- modified afterward. Everything derived from these tables is a cache that
-- can be rebuilt from them. Times are Unix seconds unless the column says
-- otherwise; JSON columns hold text.

CREATE TABLE profiles (
    id         INTEGER PRIMARY KEY,
    name       TEXT    NOT NULL UNIQUE,
    layout     TEXT    NOT NULL,
    mode       TEXT    NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

-- A NULL profile_id is a setting for the whole database, not one profile.
CREATE TABLE settings (
    profile_id INTEGER REFERENCES profiles (id),
    key        TEXT    NOT NULL,
    value      TEXT    NOT NULL
) STRICT;

CREATE UNIQUE INDEX settings_scope_key ON settings (ifnull(profile_id, 0), key);

CREATE TABLE prompts (
    id             INTEGER PRIMARY KEY,
    profile_id     INTEGER NOT NULL REFERENCES profiles (id),
    composed_at    INTEGER NOT NULL,
    corpus_version INTEGER NOT NULL,
    word_count     INTEGER NOT NULL
) STRICT;

CREATE TABLE prompt_words (
    prompt_id       INTEGER NOT NULL REFERENCES prompts (id),
    word_index      INTEGER NOT NULL,
    word            TEXT    NOT NULL,
    role            TEXT    NOT NULL CHECK (role IN ('targeted', 'probe')),
    exposed_targets TEXT    NOT NULL,  -- JSON array of pattern texts
    selection_score REAL,              -- targeted words only
    contamination   TEXT,              -- JSON; probes only, NULL when not assessed
    PRIMARY KEY (prompt_id, word_index)
) STRICT;

CREATE TABLE prompt_targets (
    prompt_id     INTEGER NOT NULL REFERENCES prompts (id),
    pattern       TEXT    NOT NULL,
    role          TEXT    NOT NULL CHECK (role IN ('target', 'deferred', 'explore')),
    weakness_mean REAL    NOT NULL,
    weakness_sd   REAL    NOT NULL,
    priority      REAL    NOT NULL,
    planned_dose  INTEGER NOT NULL,
    PRIMARY KEY (prompt_id, pattern)
) STRICT;

-- A session row is written before the session starts, with status
-- 'interrupted' and no ended_at, so a session the process died in is
-- recorded as interrupted. ended_at is set exactly once, when the session
-- ends; nothing in the row changes after that.
CREATE TABLE sessions (
    id                    INTEGER PRIMARY KEY,
    profile_id            INTEGER NOT NULL REFERENCES profiles (id),
    prompt_id             INTEGER NOT NULL REFERENCES prompts (id),
    started_at            INTEGER NOT NULL,
    ended_at              INTEGER,
    status                TEXT    NOT NULL CHECK (status IN ('completed', 'interrupted')),
    mode                  TEXT    NOT NULL,
    corpus_version        INTEGER NOT NULL,
    semantics_version     INTEGER NOT NULL,
    config_json           TEXT    NOT NULL,
    seed                  INTEGER NOT NULL,  -- u64 stored in its two's-complement form
    applied_model_version INTEGER
) STRICT;

CREATE INDEX sessions_by_profile_and_start ON sessions (profile_id, started_at);

CREATE TABLE input_events (
    session_id       INTEGER NOT NULL REFERENCES sessions (id),
    seq              INTEGER NOT NULL,
    at_micros        INTEGER NOT NULL,  -- from raw-mode entry
    kind             TEXT    NOT NULL,  -- open enumeration
    expected         TEXT,              -- one character; NULL unless a keystroke
    actual           TEXT,              -- one character; NULL for backspace and non-keystrokes
    word_index       INTEGER NOT NULL,
    position         INTEGER NOT NULL,
    first_of_session INTEGER NOT NULL CHECK (first_of_session IN (0, 1)),
    after_resize     INTEGER NOT NULL CHECK (after_resize IN (0, 1)),
    in_paste         INTEGER NOT NULL CHECK (in_paste IN (0, 1)),
    burst            INTEGER NOT NULL CHECK (burst IN (0, 1)),
    long_pause       INTEGER NOT NULL CHECK (long_pause IN (0, 1)),
    PRIMARY KEY (session_id, seq)
) STRICT;

-- Cache: the prompt composed for a profile's next session, consumed when
-- that session starts.
CREATE TABLE next_prompt (
    profile_id INTEGER PRIMARY KEY REFERENCES profiles (id),
    prompt_id  INTEGER NOT NULL REFERENCES prompts (id)
) STRICT;
