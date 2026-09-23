-- Cache: the prompt composed for a profile's next session, now with the
-- context it was composed for. It is shown only if the session about to
-- start wants the same word count, corpus, model version, and layout;
-- otherwise it is discarded and a fresh prompt composed on the spot, so a
-- changed setting takes effect on the very next run. Prompts already waiting
-- were composed without a recorded context and are dropped rather than
-- guessed at; the next run composes one.
DROP TABLE next_prompt;

CREATE TABLE next_prompt (
    profile_id     INTEGER PRIMARY KEY REFERENCES profiles (id),
    prompt_id      INTEGER NOT NULL REFERENCES prompts (id),
    word_count     INTEGER NOT NULL,
    corpus_version INTEGER NOT NULL,
    model_version  INTEGER NOT NULL,
    layout         TEXT    NOT NULL
) STRICT;
