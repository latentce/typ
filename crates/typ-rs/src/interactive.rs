//! Runs one session interactively: reads input in batches, applies it, and
//! paints once per batch.

use std::io::{self, Stdout, Write};
use std::time::Instant;

use typ_rs_core::display::{Palette, Viewport, lay_out};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{EndCondition, Input, Key, SessionState};

use crate::input::{self, TerminalEvent};
use crate::render::Painter;
use crate::terminal::{Guard, micros_since};

/// Rows kept free below the prompt, so that the results have somewhere to go
/// without scrolling the prompt away.
const RESERVED_ROWS: u16 = 2;

/// A finished session and how long each batch took to render.
pub struct Run {
    pub state: SessionState,
    pub render_micros: Vec<u64>,
}

pub fn run(prompt: Prompt, end: EndCondition, palette: Palette) -> io::Result<Run> {
    let guard = Guard::enter()?;
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
        let mut resized = false;
        // Set once the first recorded event of the batch has been applied.
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
                    viewport = self::viewport(columns, rows);
                    resized = true;
                    burst |= state.apply_event(stamp(at, Key::Resize, burst)).recorded;
                }
            }
        }
        if panic_after.is_some_and(|n| state.events().len() >= n) {
            panic!("induced by TYP_PANIC_AFTER");
        }

        let render_started = Instant::now();
        screen.paint(&state, viewport, resized)?;
        render_micros.push(micros_since(render_started));
    }

    drop(screen);
    Ok(Run {
        state,
        render_micros,
    })
}

/// The painter together with where its frames go and the guard that must
/// know how much has been painted.
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
        self.guard.set_painted_lines(self.painter.painted_lines());
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
