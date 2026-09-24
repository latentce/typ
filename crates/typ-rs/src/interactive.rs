//! Runs attempts interactively: reads input in batches, applies it, and
//! paints once per batch, until an attempt ends as a session or is left
//! untyped. A restart discards the attempt in progress and begins another
//! on a fresh prompt without leaving raw mode.

use std::io::{self, Stdout, Write};
use std::time::Instant;

use typ_rs_core::display::{CursorStyle, Palette, Viewport, lay_out};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{EndCondition, Input, Key, SessionState};

use crate::input::{self, StampedEvent, TerminalEvent};
use crate::render::Painter;
use crate::terminal::{Guard, micros_since};

/// Rows kept free below the prompt, so that the results have somewhere to go
/// without scrolling the prompt away.
const RESERVED_ROWS: u16 = 2;

/// How the last attempt ended, and how long each batch took to render,
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
pub fn run<E: From<io::Error>>(
    prompt: Prompt,
    end: EndCondition,
    palette: Palette,
    cursor: CursorStyle,
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
    let mut render_micros = Vec::new();
    let panic_after = induced_panic_after();

    screen.paint(&state, viewport, true)?;

    while state.outcome().is_none() {
        let batch = input::read_batch(|| screen.guard.now_micros())?;
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

        let render_started = Instant::now();
        screen.paint(
            &state,
            viewport,
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
/// marked as arriving in a burst.
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
    fn paint(&mut self, state: &SessionState, viewport: Viewport, repaint: bool) -> io::Result<()> {
        let frame = self.painter.frame(&lay_out(state, viewport), repaint);
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

/// The prompt may use every column and every row but the reserved ones.
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
    use typ_rs_core::session::EventKind;

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
}
