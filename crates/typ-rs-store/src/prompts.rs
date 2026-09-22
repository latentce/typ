//! Prompt rows and the cached next prompt.

use rusqlite::{Connection, OptionalExtension, params};
use typ_rs_core::corpus::CORPUS_VERSION;
use typ_rs_core::prompt::Prompt;

use crate::{Error, Result};

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

/// Removes and returns the prompt composed for the profile's next session,
/// if one is waiting.
pub(crate) fn take_next(conn: &Connection, profile_id: i64) -> Result<Option<(i64, Prompt)>> {
    let prompt_id: Option<i64> = conn
        .query_row(
            "DELETE FROM next_prompt WHERE profile_id = ?1 RETURNING prompt_id",
            [profile_id],
            |row| row.get(0),
        )
        .optional()?;
    match prompt_id {
        Some(id) => Ok(Some((id, load(conn, id)?))),
        None => Ok(None),
    }
}

pub(crate) fn set_next(conn: &Connection, profile_id: i64, prompt_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO next_prompt (profile_id, prompt_id) VALUES (?1, ?2)
         ON CONFLICT (profile_id) DO UPDATE SET prompt_id = excluded.prompt_id",
        params![profile_id, prompt_id],
    )?;
    Ok(())
}
