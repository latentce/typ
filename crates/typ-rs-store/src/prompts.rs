//! Prompt rows and the cached next prompt.

use rusqlite::{Connection, OptionalExtension, params};
use typ_rs_core::corpus::CORPUS_VERSION;
use typ_rs_core::model::MODEL_VERSION;
use typ_rs_core::prompt::Prompt;

use crate::{Error, Result};

/// What a prompt was composed for. A prompt composed ahead is shown only to
/// a session with the same context; a changed setting, a new corpus, a new
/// model version, or another layout makes it stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Context {
    pub word_count: usize,
    pub corpus_version: u32,
    pub model_version: u32,
    pub layout: String,
}

impl Context {
    /// The context of a prompt composed by this binary for the given
    /// settings.
    pub(crate) fn current(word_count: usize, layout: &str) -> Context {
        Context {
            word_count,
            corpus_version: CORPUS_VERSION,
            model_version: MODEL_VERSION,
            layout: layout.to_string(),
        }
    }
}

/// Inserts a prompt with its words. Every word of a frequency-weighted prompt
/// is a probe: nothing was chosen to expose a target.
pub(crate) fn insert(
    conn: &Connection,
    profile_id: i64,
    prompt: &Prompt,
    composed_at: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO prompts (profile_id, composed_at, corpus_version, word_count)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            profile_id,
            composed_at,
            CORPUS_VERSION,
            prompt.word_count() as i64
        ],
    )?;
    let prompt_id = conn.last_insert_rowid();

    let mut insert_word = conn.prepare_cached(
        "INSERT INTO prompt_words (prompt_id, word_index, word, role, exposed_targets)
         VALUES (?1, ?2, ?3, 'probe', '[]')",
    )?;
    for (index, word) in prompt.words().iter().enumerate() {
        insert_word.execute(params![prompt_id, index as i64, word.as_ref()])?;
    }
    Ok(prompt_id)
}

pub(crate) fn load(conn: &Connection, prompt_id: i64) -> Result<Prompt> {
    let words: Vec<String> = conn
        .prepare_cached("SELECT word FROM prompt_words WHERE prompt_id = ?1 ORDER BY word_index")?
        .query_map([prompt_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if words.is_empty() {
        return Err(Error::Corrupt(format!("prompt {prompt_id} has no words")));
    }
    Ok(Prompt::new(words))
}

/// Removes the prompt composed for the profile's next session, if one is
/// waiting, and returns it if it was composed for `wanted`. A prompt
/// composed for another context is discarded: the caller composes afresh.
pub(crate) fn take_next(
    conn: &Connection,
    profile_id: i64,
    wanted: &Context,
) -> Result<Option<(i64, Prompt)>> {
    let waiting: Option<(i64, i64, u32, u32, String)> = conn
        .query_row(
            "DELETE FROM next_prompt WHERE profile_id = ?1
             RETURNING prompt_id, word_count, corpus_version, model_version, layout",
            [profile_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((id, word_count, corpus_version, model_version, layout)) = waiting else {
        return Ok(None);
    };
    let composed_for = Context {
        word_count: usize::try_from(word_count).map_err(|_| {
            Error::Corrupt(format!("next prompt word_count {word_count} is negative"))
        })?,
        corpus_version,
        model_version,
        layout,
    };
    if composed_for == *wanted {
        Ok(Some((id, load(conn, id)?)))
    } else {
        Ok(None)
    }
}

pub(crate) fn set_next(
    conn: &Connection,
    profile_id: i64,
    prompt_id: i64,
    context: &Context,
) -> Result<()> {
    conn.execute(
        "INSERT INTO next_prompt
             (profile_id, prompt_id, word_count, corpus_version, model_version, layout)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (profile_id) DO UPDATE SET
             prompt_id = excluded.prompt_id, word_count = excluded.word_count,
             corpus_version = excluded.corpus_version,
             model_version = excluded.model_version, layout = excluded.layout",
        params![
            profile_id,
            prompt_id,
            context.word_count as i64,
            context.corpus_version,
            context.model_version,
            context.layout
        ],
    )?;
    Ok(())
}
