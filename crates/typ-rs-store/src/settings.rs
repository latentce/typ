//! Settings and profiles: what the user has chosen to keep between runs.
//!
//! Two kinds of setting share the `settings` table. A per-profile setting
//! (`words`, the session length) has the profile's id; a whole-database
//! setting (`profile`, which profile a run uses when none is named, and the
//! cursor's shape and blink, which are how the user likes their terminal
//! rather than a condition their statistics depend on) has a null profile
//! id. Each is read with its default when unset. A profile's layout is a
//! column of `profiles`, not a setting, because a profile is bound to its
//! layout: it can be chosen while nothing has been typed on the profile and
//! is fixed from the first typed session on, so the statistics of one
//! profile never mix two layouts.

use std::ops::RangeInclusive;

use rusqlite::{Connection, OptionalExtension, params};
use typ_rs_core::display::{CursorShape, CursorStyle};
use typ_rs_core::layout::Layout;

use crate::{Error, Profile, Result, Store, WORDS_MODE, unix_now};

/// Words per session unless the profile says otherwise.
pub const DEFAULT_WORDS: usize = 50;

/// The session lengths a profile may be set to.
pub const WORDS_RANGE: RangeInclusive<usize> = 10..=200;

const WORDS_KEY: &str = "words";
const PROFILE_KEY: &str = "profile";
const CURSOR_SHAPE_KEY: &str = "cursor_shape";
const CURSOR_BLINK_KEY: &str = "cursor_blink";

/// Parses a session length as the user typed it: a whole number within
/// [`WORDS_RANGE`].
pub fn parse_words(text: &str) -> Result<usize> {
    words_in_range(text)
        .ok_or_else(|| Error::InvalidSetting(format!("{}, not {text:?}", words_requirement())))
}

fn words_in_range(text: &str) -> Option<usize> {
    text.parse().ok().filter(|n| WORDS_RANGE.contains(n))
}

fn words_requirement() -> String {
    format!(
        "words must be a number from {} to {}",
        WORDS_RANGE.start(),
        WORDS_RANGE.end()
    )
}

impl Store {
    /// The profile a run uses when none is named on the command line.
    pub fn active_profile(&self) -> Result<String> {
        Ok(get(&self.conn, None, PROFILE_KEY)?
            .unwrap_or_else(|| crate::DEFAULT_PROFILE.to_string()))
    }

    /// Makes `name` the profile runs use by default, creating it if it does
    /// not exist yet.
    pub fn set_active_profile(&mut self, name: &str) -> Result<()> {
        self.profile_or_create(name)?;
        set(&self.conn, None, PROFILE_KEY, name)
    }

    /// How the cursor is shown during a session.
    pub fn cursor(&self) -> Result<CursorStyle> {
        let default = CursorStyle::default();
        let shape = match get(&self.conn, None, CURSOR_SHAPE_KEY)? {
            None => default.shape,
            Some(value) => CursorShape::from_name(&value)
                .ok_or_else(|| Error::Corrupt(format!("the cursor shape setting is {value:?}")))?,
        };
        let blink = match get(&self.conn, None, CURSOR_BLINK_KEY)? {
            None => default.blink,
            Some(value) => CursorStyle::blink_from_name(&value)
                .ok_or_else(|| Error::Corrupt(format!("the cursor blink setting is {value:?}")))?,
        };
        Ok(CursorStyle { shape, blink })
    }

    /// Sets the cursor's shape by name; an unknown name is refused and
    /// nothing changes.
    pub fn set_cursor_shape(&mut self, shape: &str) -> Result<()> {
        let Some(shape) = CursorShape::from_name(shape) else {
            let known: Vec<&str> = CursorShape::all().iter().map(|s| s.name()).collect();
            return Err(Error::InvalidSetting(format!(
                "unknown cursor shape {shape:?}; the shapes are {}",
                known.join(", ")
            )));
        };
        set(&self.conn, None, CURSOR_SHAPE_KEY, shape.name())
    }

    /// Sets whether the cursor blinks, from `on` or `off`; anything else is
    /// refused and nothing changes.
    pub fn set_cursor_blink(&mut self, blink: &str) -> Result<()> {
        let Some(blink) = CursorStyle::blink_from_name(blink) else {
            return Err(Error::InvalidSetting(format!(
                "cursor blink is {} or {}, not {blink:?}",
                CursorStyle::blink_name(true),
                CursorStyle::blink_name(false)
            )));
        };
        set(
            &self.conn,
            None,
            CURSOR_BLINK_KEY,
            CursorStyle::blink_name(blink),
        )
    }

    /// The profile with the given name, created on first use with the
    /// default layout if there is none. Names are plain words: ASCII
    /// letters, digits, `.`, `-`, and `_`.
    pub fn profile_or_create(&mut self, name: &str) -> Result<Profile> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        {
            return Err(Error::InvalidSetting(format!(
                "profile names use letters, digits, \".\", \"-\" and \"_\", not {name:?}"
            )));
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO profiles (name, layout, mode, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![name, Layout::default().name(), WORDS_MODE, unix_now()],
        )?;
        self.profile(name)
    }

    /// The profile's session length.
    pub fn words(&self, profile: &Profile) -> Result<usize> {
        match get(&self.conn, Some(profile.id), WORDS_KEY)? {
            None => Ok(DEFAULT_WORDS),
            Some(value) => words_in_range(&value).ok_or_else(|| {
                Error::Corrupt(format!(
                    "profile {:?} has the session length {value:?}",
                    profile.name
                ))
            }),
        }
    }

    /// Sets the profile's session length; a value outside [`WORDS_RANGE`]
    /// is refused and nothing changes.
    pub fn set_words(&mut self, profile: &Profile, words: usize) -> Result<()> {
        if !WORDS_RANGE.contains(&words) {
            return Err(Error::InvalidSetting(format!(
                "{}, not {words}",
                words_requirement()
            )));
        }
        set(&self.conn, Some(profile.id), WORDS_KEY, &words.to_string())
    }

    /// Binds the profile to a layout. Setting the layout it already has is
    /// always allowed; changing it is allowed only while no session has been
    /// typed on the profile, since its statistics describe typing on one
    /// keyboard. Another layout afterward means another profile.
    pub fn set_layout(&mut self, profile: &Profile, layout: &str) -> Result<()> {
        if layout == profile.layout {
            return Ok(());
        }
        let typed_on: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sessions WHERE profile_id = ?1 AND ended_at IS NOT NULL)",
            [profile.id],
            |row| row.get(0),
        )?;
        if typed_on {
            return Err(Error::InvalidSetting(format!(
                "profile {:?} has sessions typed on {}; a layout cannot change after that, so use another profile for {layout}",
                profile.name, profile.layout
            )));
        }
        if Layout::by_name(layout).is_none() {
            let known: Vec<&str> = Layout::all().iter().map(|l| l.name()).collect();
            return Err(Error::InvalidSetting(format!(
                "unknown layout {layout:?}; the layouts are {}",
                known.join(", ")
            )));
        }
        self.conn.execute(
            "UPDATE profiles SET layout = ?2 WHERE id = ?1",
            params![profile.id, layout],
        )?;
        Ok(())
    }
}

/// A NULL `profile_id` is the whole-database scope; `ifnull` maps it to 0,
/// which no profile has, so one comparison serves both scopes.
fn get(conn: &Connection, profile_id: Option<i64>, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM settings WHERE ifnull(profile_id, 0) = ifnull(?1, 0) AND key = ?2",
            params![profile_id, key],
            |row| row.get(0),
        )
        .optional()?)
}

fn set(conn: &Connection, profile_id: Option<i64>, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO settings (profile_id, key, value) VALUES (?1, ?2, ?3)",
        params![profile_id, key, value],
    )?;
    Ok(())
}
