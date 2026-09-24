//! Puts the terminal into the mode a session needs and guarantees it is put
//! back.
//!
//! Raw mode, bracketed paste, and the hardware cursor's shape and visibility
//! (the painter hides it while writing a frame) are all process-wide
//! terminal state, so restoring them is a single idempotent step that runs
//! from the guard's `Drop`, and also from a panic hook so that the panic
//! message is printed to a usable terminal rather than a raw one.
//! Nothing here calls `process::exit`: every exit path unwinds through the
//! guard.

use std::io::{self, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use crossterm::cursor::SetCursorStyle;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::style::{Attribute, ResetColor, SetAttribute};
use crossterm::{cursor, execute, terminal};
use typ_rs_core::display::{CursorShape, CursorStyle};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static PASTE_ENABLED: AtomicBool = AtomicBool::new(false);
static ROWS_BELOW_CURSOR: AtomicUsize = AtomicUsize::new(0);
static PANIC_HOOK: Once = Once::new();

/// The terminal in session mode. `t = 0` for the session's clock is the
/// moment raw mode was entered.
pub struct Guard {
    entered_at: Instant,
}

impl Guard {
    /// Enters raw mode, enables bracketed paste where the platform supports
    /// it, and gives the hardware cursor the shape the session wants. At
    /// most one guard exists at a time.
    pub fn enter(cursor: CursorStyle) -> io::Result<Guard> {
        assert!(
            !ACTIVE.swap(true, Ordering::SeqCst),
            "the terminal guard is already active"
        );
        PANIC_HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                previous(info);
            }));
        });

        if let Err(e) = terminal::enable_raw_mode() {
            ACTIVE.store(false, Ordering::SeqCst);
            return Err(e);
        }
        let entered_at = Instant::now();

        let mut stdout = io::stdout();
        let paste = execute!(stdout, EnableBracketedPaste).is_ok();
        PASTE_ENABLED.store(paste, Ordering::SeqCst);
        // A terminal without cursor-shape support ignores the sequence and
        // shows its own cursor, which is fine.
        let _ = execute!(stdout, cursor_style(cursor));
        Ok(Guard { entered_at })
    }

    /// Microseconds since raw mode was entered.
    pub fn now_micros(&self) -> u64 {
        micros_since(self.entered_at)
    }

    /// Records how many painted lines lie below the one the hardware cursor
    /// rests on, so that restoring the terminal can step past them and leave
    /// whatever is printed next (results, a panic message) below the prompt.
    pub fn set_rows_below_cursor(&self, rows: usize) {
        ROWS_BELOW_CURSOR.store(rows, Ordering::SeqCst);
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}

/// Microseconds elapsed since `instant`, saturating.
pub fn micros_since(instant: Instant) -> u64 {
    u64::try_from(instant.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Undoes everything `enter` did, once, and moves the cursor to a fresh line
/// below the painted prompt. Safe to call when no guard is active; errors
/// are ignored because there is nothing left to do about them.
fn restore() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut stdout = io::stdout();
    let below = ROWS_BELOW_CURSOR.swap(0, Ordering::SeqCst);
    if below > 0 {
        let down = u16::try_from(below).unwrap_or(u16::MAX);
        let _ = execute!(stdout, cursor::MoveDown(down));
    }
    if PASTE_ENABLED.swap(false, Ordering::SeqCst) {
        let _ = execute!(stdout, DisableBracketedPaste);
    }
    let _ = execute!(
        stdout,
        SetAttribute(Attribute::Reset),
        ResetColor,
        SetCursorStyle::DefaultUserShape,
        cursor::Show
    );
    let _ = stdout.write_all(b"\r\n");
    let _ = stdout.flush();
    let _ = terminal::disable_raw_mode();
}

/// The DECSCUSR request for a cursor style.
fn cursor_style(cursor: CursorStyle) -> SetCursorStyle {
    match (cursor.shape, cursor.blink) {
        (CursorShape::Block, true) => SetCursorStyle::BlinkingBlock,
        (CursorShape::Block, false) => SetCursorStyle::SteadyBlock,
        (CursorShape::Beam, true) => SetCursorStyle::BlinkingBar,
        (CursorShape::Beam, false) => SetCursorStyle::SteadyBar,
        (CursorShape::Underline, true) => SetCursorStyle::BlinkingUnderScore,
        (CursorShape::Underline, false) => SetCursorStyle::SteadyUnderScore,
    }
}
