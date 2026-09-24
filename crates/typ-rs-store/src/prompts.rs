//! Prompt rows, the patterns selected for them, and the cached next prompt.

use rusqlite::{Connection, OptionalExtension, params};
use typ_rs_core::compose::{ComposedPrompt, ComposedWord, Contamination, WordRole};
use typ_rs_core::corpus::CORPUS_VERSION;
use typ_rs_core::model::MODEL_VERSION;
use typ_rs_core::prompt::Prompt;
use typ_rs_core::scheduler::{SelectedTarget, TargetRole};

use crate::{Error, Result, json};

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

/// Inserts a prompt with its words, each with its role, the targets it
/// exposes, its selection score if targeted, and its contamination if a
/// probe assessed for it, and every pattern selected for the prompt.
pub(crate) fn insert(
    conn: &Connection,
    profile_id: i64,
    composed: &ComposedPrompt,
    composed_at: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO prompts (profile_id, composed_at, corpus_version, word_count)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            profile_id,
            composed_at,
            CORPUS_VERSION,
            composed.prompt.word_count() as i64
        ],
    )?;
    let prompt_id = conn.last_insert_rowid();

    let mut insert_word = conn.prepare_cached(
        "INSERT INTO prompt_words
             (prompt_id, word_index, word, role, exposed_targets, selection_score, contamination)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for (index, (word, meta)) in composed
        .prompt
        .words()
        .iter()
        .zip(&composed.words)
        .enumerate()
    {
        let contamination = meta
            .contamination
            .as_ref()
            .map(|c| contamination_json(c, &composed.words, index));
        insert_word.execute(params![
            prompt_id,
            index as i64,
            word.as_ref(),
            meta.role.name(),
            json_strings(&meta.exposed_targets),
            meta.selection_score,
            contamination,
        ])?;
    }

    let mut insert_target = conn.prepare_cached(
        "INSERT INTO prompt_targets
             (prompt_id, pattern, role, weakness_mean, weakness_sd, priority, planned_dose)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for t in &composed.targets {
        insert_target.execute(params![
            prompt_id,
            t.pattern.as_ref(),
            t.role.name(),
            t.weakness_mean,
            t.weakness_sd,
            t.priority,
            t.planned_dose as i64,
        ])?;
    }
    Ok(prompt_id)
}

/// A JSON array of the given strings.
fn json_strings(items: &[Box<str>]) -> String {
    let quoted: Vec<String> = items.iter().map(|s| json_string(s)).collect();
    format!("[{}]", quoted.join(","))
}

/// A JSON string literal.
fn json_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A probe's contamination as stored: the roles of the words either side
/// of it (`null` at the prompt's edges), whether the word itself was
/// recently targeted, and which of its patterns were.
fn contamination_json(c: &Contamination, words: &[ComposedWord], index: usize) -> String {
    let role = |i: Option<usize>| {
        i.and_then(|i| words.get(i))
            .map_or("null".to_string(), |w| json_string(w.role.name()))
    };
    format!(
        "{{\"before\":{},\"after\":{},\"recent_word\":{},\"recent_patterns\":{}}}",
        role(index.checked_sub(1)),
        role(Some(index + 1)),
        c.recently_targeted_word,
        json_strings(&c.recently_targeted_patterns),
    )
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

/// The prompt's words shown as targeted, in prompt order.
pub(crate) fn load_targeted_words(conn: &Connection, prompt_id: i64) -> Result<Vec<Box<str>>> {
    let words = conn
        .prepare_cached(
            "SELECT word FROM prompt_words
             WHERE prompt_id = ?1 AND role = ?2 ORDER BY word_index",
        )?
        .query_map(params![prompt_id, WordRole::Targeted.name()], |row| {
            row.get::<_, String>(0).map(String::into_boxed_str)
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(words)
}

/// Every word of the prompt as composed: its role, the targets it exposes,
/// its selection score, and its contamination, in prompt order.
pub(crate) fn load_words(conn: &Connection, prompt_id: i64) -> Result<Vec<ComposedWord>> {
    let mut stmt = conn.prepare_cached(
        "SELECT role, exposed_targets, selection_score, contamination
         FROM prompt_words WHERE prompt_id = ?1 ORDER BY word_index",
    )?;
    let rows = stmt.query_map([prompt_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (role, exposed, score, contamination) = row?;
        Ok(ComposedWord {
            role: WordRole::from_name(&role)
                .ok_or_else(|| Error::Corrupt(format!("unknown word role {role:?}")))?,
            exposed_targets: json::string_array(&exposed)?,
            selection_score: score,
            contamination: contamination
                .as_deref()
                .map(json::contamination)
                .transpose()?,
        })
    })
    .collect()
}

/// The patterns selected for a prompt, targets first in the order they
/// were recorded.
pub(crate) fn load_targets(conn: &Connection, prompt_id: i64) -> Result<Vec<SelectedTarget>> {
    let mut stmt = conn.prepare_cached(
        "SELECT pattern, role, weakness_mean, weakness_sd, priority, planned_dose
         FROM prompt_targets WHERE prompt_id = ?1 ORDER BY rowid",
    )?;
    let rows = stmt.query_map([prompt_id], |row| {
        Ok(TargetColumns {
            pattern: row.get(0)?,
            role: row.get(1)?,
            weakness_mean: row.get(2)?,
            weakness_sd: row.get(3)?,
            priority: row.get(4)?,
            planned_dose: row.get(5)?,
        })
    })?;
    rows.map(|row| row?.decode()).collect()
}

/// A selected target as its columns come out of `prompt_targets` or
/// `pattern_training_events`, before validation.
pub(crate) struct TargetColumns {
    pub pattern: String,
    pub role: String,
    pub weakness_mean: f64,
    pub weakness_sd: f64,
    pub priority: f64,
    pub planned_dose: i64,
}

impl TargetColumns {
    pub(crate) fn decode(self) -> Result<SelectedTarget> {
        Ok(SelectedTarget {
            pattern: self.pattern.into_boxed_str(),
            role: TargetRole::from_name(&self.role)
                .ok_or_else(|| Error::Corrupt(format!("unknown target role {:?}", self.role)))?,
            weakness_mean: self.weakness_mean,
            weakness_sd: self.weakness_sd,
            priority: self.priority,
            planned_dose: dose(self.planned_dose, "planned_dose")?,
        })
    }
}

/// A dose column, which can only be a count.
pub(crate) fn dose(value: i64, column: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::Corrupt(format!("{column} {value} is negative")))
}

/// What a stored prompt carries into the session that shows it.
pub(crate) struct LoadedPrompt {
    pub id: i64,
    pub prompt: Prompt,
    pub targets: Vec<SelectedTarget>,
    /// One entry per word, as composed.
    pub words: Vec<ComposedWord>,
}

impl LoadedPrompt {
    /// The words shown as targeted, in prompt order.
    pub(crate) fn targeted_words(&self) -> Vec<Box<str>> {
        self.prompt
            .words()
            .iter()
            .zip(&self.words)
            .filter(|(_, meta)| meta.role == WordRole::Targeted)
            .map(|(word, _)| word.clone())
            .collect()
    }
}

pub(crate) fn load_prompt(conn: &Connection, prompt_id: i64) -> Result<LoadedPrompt> {
    Ok(LoadedPrompt {
        id: prompt_id,
        prompt: load(conn, prompt_id)?,
        targets: load_targets(conn, prompt_id)?,
        words: load_words(conn, prompt_id)?,
    })
}

/// Removes the prompt composed for the profile's next session, if one is
/// waiting, and returns it if it was composed for `wanted`. A prompt
/// composed for another context is discarded: the caller composes afresh.
pub(crate) fn take_next(
    conn: &Connection,
    profile_id: i64,
    wanted: &Context,
) -> Result<Option<LoadedPrompt>> {
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
        Ok(Some(load_prompt(conn, id)?))
    } else {
        Ok(None)
    }
}

/// The prompt waiting for the profile's next session; `None` when no
/// prompt is waiting. The prompt stays waiting.
pub(crate) fn next_waiting(conn: &Connection, profile_id: i64) -> Result<Option<LoadedPrompt>> {
    let waiting: Option<i64> = conn
        .query_row(
            "SELECT prompt_id FROM next_prompt WHERE profile_id = ?1",
            [profile_id],
            |row| row.get(0),
        )
        .optional()?;
    waiting.map(|id| load_prompt(conn, id)).transpose()
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
