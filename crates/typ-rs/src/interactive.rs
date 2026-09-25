//! Runs attempts interactively: reads input in batches, applies it, and
//! paints once per batch, until an attempt ends as a session or is left
//! untyped. A restart discards the attempt in progress and begins another
//! on a fresh prompt without leaving raw mode. Above the prompt, a header
//! shows the live WPM and the elapsed time, changing once per second on
//! ticks anchored to the session's start.

use std::io::{self, Stdout, Write};
use std::time::Instant;

use typ_rs_core::display::{CursorStyle, Header, Palette, Viewport, header, lay_out};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{EndCondition, Input, Key, SessionState};

use crate::input::{self, StampedEvent, TerminalEvent};
use crate::render::Painter;
use crate::terminal::{Guard, micros_since};

/// Rows kept free below the prompt, so that the results have somewhere to go
/// without scrolling the prompt away.
const RESERVED_ROWS: u16 = 2;

/// How often the header's figures change.
const TICK_MICROS: u64 = 1_000_000;

/// How the last attempt ended, and how long each frame took to render,
/// across every attempt of the run.
pub struct Run {
    pub attempt: Attempt,
    pub render_micros: Vec<u64>,
}

/// What the attempt on screen was when the loop ended.
pub enum Attempt {
    /// At least one character was typed, so the attempt is a session,
    /// ended by completion or interruption, to be saved.
    Session(SessionState),
    /// Left before its first typed character: a discarded attempt, of
    /// which nothing is kept.
    Discarded,
}

/// Shows `prompt` and runs attempts until one ends. On a restart, `restart`
/// composes and persists a fresh prompt, and the loop begins a new attempt
/// on it with the same end condition, painted in full where the old one
/// was. The clock is not reset: `t = 0` stays at raw-mode entry, and a new
/// attempt's first printable keystroke starts its timer like any session's.
/// An error from `restart` unwinds through the terminal guard, so the
/// terminal is restored before the caller sees it.
///
/// The header's live WPM is colored against `recent_wpm`, the recent series
/// of gross WPM for the profile (the recent average the results compare
/// with), read before raw mode is entered so that the database is never
/// touched while the user types; a restart keeps it. Once the session has
/// started, the loop wakes at every whole second after its start to repaint
/// the header even without input. A tick is not an event: nothing is
/// recorded or applied for it.
pub fn run<E: From<io::Error>>(
    prompt: Prompt,
    end: EndCondition,
    palette: Palette,
    cursor: CursorStyle,
    recent_wpm: Option<f64>,
    mut restart: impl FnMut() -> Result<Prompt, E>,
) -> Result<Run, E> {
    let guard = Guard::enter(cursor)?;
    let (columns, rows) = crossterm::terminal::size()?;
    let mut viewport = viewport(columns, rows);
    let mut state = SessionState::new(prompt, end);
    let mut screen = Screen {
        painter: Painter::new(palette),
        stdout: io::stdout(),
        guard,
    };
    let mut live = LiveHeader::new(&state, recent_wpm);
    let mut render_micros = Vec::new();
    let panic_after = induced_panic_after();

    screen.paint(&state, viewport, live.shown, true)?;

    while state.outcome().is_none() {
        let deadline = next_tick_micros(state.started_at_micros(), screen.guard.now_micros());
        let batch = input::read_batch(deadline, || screen.guard.now_micros())?;
        let outcome = apply_batch(&mut state, batch);
        if let Some((columns, rows)) = outcome.resized {
            viewport = self::viewport(columns, rows);
            screen.painter.terminal_resized_to(usize::from(columns));
        }
        if outcome.restart {
            state = SessionState::new(restart()?, end);
        }
        if panic_after.is_some_and(|n| state.events().len() >= n) {
            panic!("induced by TYP_PANIC_AFTER");
        }

        let header = live.at(&state, screen.guard.now_micros());
        let render_started = Instant::now();
        screen.paint(
            &state,
            viewport,
            header,
            outcome.resized.is_some() || outcome.restart,
        )?;
        render_micros.push(micros_since(render_started));
    }

    drop(screen);
    let attempt = if state.started_at_micros().is_some() {
        Attempt::Session(state)
    } else {
        Attempt::Discarded
    };
    Ok(Run {
        attempt,
        render_micros,
    })
}

/// What applying one batch of input did, beyond changing the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct BatchOutcome {
    /// The terminal's size after the last resize in the batch, if any.
    resized: Option<(u16, u16)>,
    /// A restart was asked for. Nothing after it in the batch was applied:
    /// it was typed at the prompt being discarded.
    restart: bool,
}

/// Applies one batch of stamped events to `state` in order, stopping at a
/// restart. Every input after the first recorded one of the batch is
/// marked as arriving in a burst. An empty batch, a tick, does nothing.
fn apply_batch(state: &mut SessionState, batch: Vec<StampedEvent>) -> BatchOutcome {
    let mut outcome = BatchOutcome::default();
    let mut burst = false;
    for stamped in batch {
        let at = stamped.at_micros;
        match stamped.event {
            TerminalEvent::Key(key) => {
                burst |= state.apply_event(stamp(at, key, burst)).recorded;
            }
            TerminalEvent::Paste(text) => {
                for c in text.chars() {
                    burst |= state
                        .apply_event(stamp(at, Key::Char(c), burst).in_paste())
                        .recorded;
                }
            }
            TerminalEvent::Resize { columns, rows } => {
                outcome.resized = Some((columns, rows));
                burst |= state.apply_event(stamp(at, Key::Resize, burst)).recorded;
            }
            TerminalEvent::Restart => {
                outcome.restart = true;
                break;
            }
        }
    }
    outcome
}

/// The painter together with where its frames go and the guard that must
/// know how much is painted below the cursor.
struct Screen {
    painter: Painter,
    stdout: Stdout,
    guard: Guard,
}

impl Screen {
    fn paint(
        &mut self,
        state: &SessionState,
        viewport: Viewport,
        header: Header,
        repaint: bool,
    ) -> io::Result<()> {
        let frame = self
            .painter
            .frame(&lay_out(state, viewport, Some(header)), repaint);
        self.stdout.write_all(frame)?;
        self.stdout.flush()?;
        self.guard
            .set_rows_below_cursor(self.painter.rows_below_cursor());
        Ok(())
    }
}

/// An input read at `at`; `burst` marks every input after the first of a
/// batch.
fn stamp(at: u64, key: Key, burst: bool) -> Input {
    let input = Input::new(at, key);
    if burst { input.burst() } else { input }
}

/// When the header next changes: the first whole second after the session's
/// start that is strictly after `now`, so that a repaint that ran late
/// waits for the boundary after it rather than shifting every later tick.
/// `None` before the first printable keystroke, when there is nothing to
/// tick and the loop can block for input.
fn next_tick_micros(started_micros: Option<u64>, now_micros: u64) -> Option<u64> {
    let started = started_micros?;
    let ticks = match now_micros.checked_sub(started) {
        Some(since) => since / TICK_MICROS + 1,
        None => 0,
    };
    Some(started + ticks * TICK_MICROS)
}

/// The latest whole second after the session's start that is not after
/// `now`; `None` before the first printable keystroke.
fn latest_tick_micros(started_micros: Option<u64>, now_micros: u64) -> Option<u64> {
    let started = started_micros?;
    let since = now_micros.checked_sub(started)?;
    Some(started + since / TICK_MICROS * TICK_MICROS)
}

/// The header on screen, and when it is replaced. It is computed afresh
/// at each tick, when the session ends (frozen at the last event, whatever
/// the clock says), and while no session has started (the placeholder,
/// which a restart returns to). Every other frame, a keystroke's, reuses
/// the header the last tick showed, so that typing repaints the prompt but
/// never moves the figures between ticks.
struct LiveHeader {
    recent_wpm: Option<f64>,
    shown: Header,
    /// The tick `shown` was computed at; `None` for a placeholder.
    tick: Option<u64>,
}

impl LiveHeader {
    fn new(state: &SessionState, recent_wpm: Option<f64>) -> LiveHeader {
        LiveHeader {
            recent_wpm,
            shown: header(state, 0, recent_wpm),
            tick: None,
        }
    }

    /// The header to paint with a frame at `now_micros`.
    fn at(&mut self, state: &SessionState, now_micros: u64) -> Header {
        let tick = latest_tick_micros(state.started_at_micros(), now_micros);
        if tick.is_none() || tick != self.tick || state.outcome().is_some() {
            self.shown = header(state, tick.unwrap_or(now_micros), self.recent_wpm);
            self.tick = tick;
        }
        self.shown
    }
}

/// The prompt and its header may use every column and every row but the
/// reserved ones.
fn viewport(columns: u16, rows: u16) -> Viewport {
    Viewport {
        columns: usize::from(columns),
        rows: usize::from(rows.saturating_sub(RESERVED_ROWS).max(1)),
    }
}

/// Debug builds panic after this many recorded events when `TYP_PANIC_AFTER`
/// is set, to check by hand that a crash leaves the terminal usable.
#[cfg(debug_assertions)]
fn induced_panic_after() -> Option<usize> {
    std::env::var("TYP_PANIC_AFTER").ok()?.parse().ok()
}

#[cfg(not(debug_assertions))]
fn induced_panic_after() -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use typ_rs_core::display::Tone;
    use typ_rs_core::session::{EventKind, Outcome};

    fn state() -> SessionState {
        SessionState::new(Prompt::new(["cat", "dog"]), EndCondition::AfterWords(2))
    }

    fn at(at_micros: u64, event: TerminalEvent) -> StampedEvent {
        StampedEvent { at_micros, event }
    }

    fn chars(text: &str, from_micros: u64) -> Vec<StampedEvent> {
        text.chars()
            .enumerate()
            .map(|(i, c)| {
                at(
                    from_micros + i as u64 * 1_000,
                    TerminalEvent::Key(Key::Char(c)),
                )
            })
            .collect()
    }

    #[test]
    fn a_restart_stops_the_batch_and_drops_what_follows_it() {
        let mut state = state();
        let mut batch = chars("ca", 100);
        batch.push(at(300, TerminalEvent::Restart));
        batch.extend(chars("t d", 400));

        let outcome = apply_batch(&mut state, batch);
        assert_eq!(
            outcome,
            BatchOutcome {
                resized: None,
                restart: true
            }
        );
        assert_eq!(state.typed(0), &['c', 'a']);
        assert_eq!(state.events().len(), 2);
        assert_eq!(state.current_word(), 0);
    }

    #[test]
    fn a_batch_without_a_restart_applies_everything_and_reports_the_resize() {
        let mut state = state();
        let mut batch = chars("cat", 100);
        batch.push(at(
            400,
            TerminalEvent::Resize {
                columns: 60,
                rows: 20,
            },
        ));
        batch.push(at(500, TerminalEvent::Key(Key::Char(' '))));
        batch.push(at(600, TerminalEvent::Key(Key::Other)));

        let outcome = apply_batch(&mut state, batch);
        assert_eq!(
            outcome,
            BatchOutcome {
                resized: Some((60, 20)),
                restart: false
            }
        );
        assert_eq!(state.current_word(), 1);
        let kinds: Vec<EventKind> = state.events().iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::Char,
                EventKind::Char,
                EventKind::Char,
                EventKind::Resize,
                EventKind::Space
            ]
        );
        let bursts: Vec<bool> = state.events().iter().map(|e| e.flags.burst).collect();
        assert_eq!(bursts, vec![false, true, true, true, true]);
        assert!(state.events()[0].flags.first_of_session);
    }

    #[test]
    fn a_paste_with_a_tab_in_it_records_paste_events_and_does_not_restart() {
        let mut state = state();
        let batch = vec![at(100, TerminalEvent::Paste("ca\tt".into()))];

        let outcome = apply_batch(&mut state, batch);
        assert_eq!(outcome, BatchOutcome::default());
        assert!(state.typed(0).is_empty());
        assert_eq!(state.events().len(), 3);
        assert!(state.events().iter().all(|e| e.flags.in_paste));
        assert_eq!(state.started_at_micros(), None);
    }

    #[test]
    fn an_empty_batch_applies_nothing_and_records_nothing() {
        let mut state = state();
        apply_batch(&mut state, chars("ca", 100));
        let before = state.clone();

        let outcome = apply_batch(&mut state, Vec::new());
        assert_eq!(outcome, BatchOutcome::default());
        assert_eq!(state, before);
    }

    const STARTED: u64 = 5_000_000;

    #[test]
    fn there_is_no_tick_deadline_before_the_session_starts() {
        assert_eq!(next_tick_micros(None, 0), None);
        assert_eq!(next_tick_micros(None, 30_000_000), None);
    }

    #[test]
    fn tick_deadlines_fall_on_whole_seconds_after_the_start() {
        let next = |now| next_tick_micros(Some(STARTED), now);
        assert_eq!(next(STARTED), Some(STARTED + 1_000_000));
        assert_eq!(next(STARTED + 1), Some(STARTED + 1_000_000));
        assert_eq!(next(STARTED + 999_999), Some(STARTED + 1_000_000));
        assert_eq!(next(STARTED + 1_000_000), Some(STARTED + 2_000_000));
        assert_eq!(next(STARTED + 1_000_001), Some(STARTED + 2_000_000));
    }

    #[test]
    fn a_late_now_skips_to_the_next_boundary_rather_than_drifting() {
        assert_eq!(
            next_tick_micros(Some(STARTED), STARTED + 2_700_000),
            Some(STARTED + 3_000_000)
        );
        assert_eq!(
            next_tick_micros(Some(STARTED), STARTED + 59_999_999),
            Some(STARTED + 60_000_000)
        );
    }

    #[test]
    fn the_header_shows_the_latest_tick_not_after_now() {
        let latest = |now| latest_tick_micros(Some(STARTED), now);
        assert_eq!(latest(STARTED), Some(STARTED));
        assert_eq!(latest(STARTED + 999_999), Some(STARTED));
        assert_eq!(latest(STARTED + 1_000_000), Some(STARTED + 1_000_000));
        assert_eq!(latest(STARTED + 2_700_000), Some(STARTED + 2_000_000));
        assert_eq!(latest_tick_micros(None, 42), None);
    }

    fn typed_at(state: &mut SessionState, text: &str, at_micros: u64) {
        for c in text.chars() {
            state.apply_event(Input::new(at_micros, Key::Char(c)));
        }
    }

    #[test]
    fn the_header_starts_as_the_placeholder_and_changes_only_on_ticks() {
        let mut state = SessionState::new(
            Prompt::new(["cat", "dog", "fox"]),
            EndCondition::AfterWords(3),
        );
        let mut live = LiveHeader::new(&state, None);
        assert_eq!(live.at(&state, 100).tone, Tone::Placeholder);
        assert_eq!(live.at(&state, 4_000_000).tone, Tone::Placeholder);

        // The first keystroke starts the clock: the placeholder becomes a
        // figure-less header with the terminal's own color.
        typed_at(&mut state, "c", STARTED);
        let started = live.at(&state, STARTED + 10);
        assert_eq!(started.wpm, None);
        assert_eq!(started.tone, Tone::Neutral);
        assert_eq!(started.text(), "— wpm   0:00");

        // The first tick shows one character over one second.
        assert_eq!(live.at(&state, STARTED + 1_000_000).text(), "12 wpm   0:01");

        // Keystrokes between ticks repaint with the header the tick showed.
        typed_at(&mut state, "at do", STARTED + 1_500_000);
        assert_eq!(live.at(&state, STARTED + 1_500_000).text(), "12 wpm   0:01");
        assert_eq!(live.at(&state, STARTED + 1_999_999).text(), "12 wpm   0:01");

        // The next tick counts them all.
        assert_eq!(live.at(&state, STARTED + 2_000_000).text(), "36 wpm   0:02");
    }

    #[test]
    fn the_header_freezes_the_moment_the_session_ends_between_ticks() {
        let mut state = SessionState::new(Prompt::new(["cat", "dog"]), EndCondition::AfterWords(2));
        let mut live = LiveHeader::new(&state, Some(1.0));
        typed_at(&mut state, "cat do", STARTED);
        assert_eq!(live.at(&state, STARTED + 1_000_000).text(), "72 wpm   0:01");

        typed_at(&mut state, "g", STARTED + 1_500_000);
        assert_eq!(state.outcome(), Some(Outcome::Completed));
        // Seven characters over a second and a half, as the results count.
        let frozen = live.at(&state, STARTED + 1_500_000);
        assert_eq!(frozen.text(), "56 wpm   0:01");
        assert_eq!(frozen.tone, Tone::Faster);
        assert_eq!(live.at(&state, STARTED + 9_000_000), frozen);
    }

    #[test]
    fn a_restart_returns_the_header_to_the_placeholder_with_the_same_comparison() {
        let mut state = SessionState::new(Prompt::new(["cat", "dog"]), EndCondition::AfterWords(2));
        let mut live = LiveHeader::new(&state, Some(10.0));
        typed_at(&mut state, "cat do", STARTED);
        assert_eq!(live.at(&state, STARTED + 1_000_000).tone, Tone::Faster);

        let mut fresh = SessionState::new(Prompt::new(["fox", "owl"]), EndCondition::AfterWords(2));
        assert_eq!(live.at(&fresh, STARTED + 1_200_000).tone, Tone::Placeholder);

        typed_at(&mut fresh, "fox ow", STARTED + 2_000_000);
        assert_eq!(live.at(&fresh, STARTED + 2_000_000).text(), "— wpm   0:00");
        let ticked = live.at(&fresh, STARTED + 3_000_000);
        assert_eq!(ticked.text(), "72 wpm   0:01");
        assert_eq!(ticked.tone, Tone::Faster);
    }
}
