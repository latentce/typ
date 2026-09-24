//! Turns successive [`Display`]s into the bytes that update the terminal.
//!
//! The prompt is painted inline where the shell left the cursor. Between
//! frames the hardware cursor is parked at the first column of the prompt's
//! first line, and every movement is relative to it, so the painter never
//! needs to know where on the screen the prompt is: if the terminal scrolls
//! or reflows, the parked cursor moves with the prompt.

use crossterm::Command;
use crossterm::cursor::{MoveDown, MoveToColumn, MoveUp};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{Clear, ClearType};
use typ_rs_core::display::{Cell, Display, Foreground, Palette, Style};

/// Foreground colors as the plain 16-color SGR codes, which every color
/// terminal understands: bright black and red.
const BRIGHT_BLACK: &str = "\x1b[90m";
const RED: &str = "\x1b[31m";

pub struct Painter {
    palette: Palette,
    previous: Option<Display>,
    buf: String,
    /// Row of the cursor relative to the prompt's first line while a frame
    /// is being composed.
    row: usize,
    style: Style,
}

impl Painter {
    pub fn new(palette: Palette) -> Painter {
        Painter {
            palette,
            previous: None,
            buf: String::new(),
            row: 0,
            style: Style::default(),
        }
    }

    /// Lines the last frame left on the screen.
    pub fn painted_lines(&self) -> usize {
        self.previous.as_ref().map_or(0, |d| d.lines().len())
    }

    /// The bytes that bring the terminal from the previous frame to
    /// `display`, ending with the cursor parked again. `repaint` forces every
    /// line to be rewritten, as after a resize; otherwise only cells that
    /// changed are touched. A frame that needs more lines than the last one
    /// is always written in full so that the terminal scrolls if the prompt
    /// is at the bottom of the screen.
    ///
    /// Stale cells are always erased before a line is written, never after:
    /// a line that fills the terminal's width leaves the cursor pending on
    /// its last cell, where an erase-to-end-of-line would remove that cell.
    pub fn frame(&mut self, display: &Display, repaint: bool) -> &[u8] {
        self.buf.clear();
        self.row = 0;
        let lines = display.lines();
        let previous_lines = self.painted_lines();

        if repaint || self.previous.is_none() || lines.len() > previous_lines {
            self.paint_all(lines);
        } else {
            let previous = self.previous.take().expect("checked above");
            for (i, line) in lines.iter().enumerate() {
                self.paint_changes(i, &previous.lines()[i], line);
            }
            if lines.len() < previous_lines {
                self.goto(lines.len(), 0);
                self.emit(Clear(ClearType::FromCursorDown));
            }
        }

        self.goto(0, 0);
        self.previous = Some(display.clone());
        self.buf.as_bytes()
    }

    fn paint_all(&mut self, lines: &[Vec<Cell>]) {
        self.emit(MoveToColumn(0));
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                self.buf.push_str("\r\n");
                self.row += 1;
            }
            let clear = if i + 1 == lines.len() {
                ClearType::FromCursorDown
            } else {
                ClearType::UntilNewLine
            };
            self.emit(Clear(clear));
            self.paint_cells(line);
        }
    }

    /// Rewrites the span of one line between its first and last changed
    /// cells; the whole tail when the line's length changed.
    fn paint_changes(&mut self, row: usize, old: &[Cell], new: &[Cell]) {
        let Some(first) = (0..new.len().max(old.len())).find(|&i| old.get(i) != new.get(i)) else {
            return;
        };
        let first = first.min(new.len());
        let end = if old.len() == new.len() {
            (first..new.len())
                .rev()
                .find(|&i| old[i] != new[i])
                .expect("a change exists")
                + 1
        } else {
            new.len()
        };
        let column = new[..first].iter().map(Cell::width).sum();
        self.goto(row, column);
        if old.len() != new.len() {
            self.emit(Clear(ClearType::UntilNewLine));
        }
        self.paint_cells(&new[first..end]);
    }

    fn paint_cells(&mut self, cells: &[Cell]) {
        for cell in cells {
            self.set_style(cell.class.style(self.palette));
            self.emit(Print(cell.ch));
        }
        self.set_style(Style::default());
    }

    fn set_style(&mut self, style: Style) {
        if style == self.style {
            return;
        }
        self.emit(SetAttribute(Attribute::Reset));
        match style.foreground {
            Foreground::Default => {}
            Foreground::BrightBlack => self.buf.push_str(BRIGHT_BLACK),
            Foreground::Red => self.buf.push_str(RED),
        }
        let attributes = [
            (style.dim, Attribute::Dim),
            (style.bold, Attribute::Bold),
            (style.underline, Attribute::Underlined),
            (style.reverse, Attribute::Reverse),
        ];
        for (on, attribute) in attributes {
            if on {
                self.emit(SetAttribute(attribute));
            }
        }
        self.style = style;
    }

    /// Moves relative to the parked position. Zero-count moves are never
    /// emitted: terminals treat `CSI 0 B` as a move of one.
    fn goto(&mut self, row: usize, column: usize) {
        if row > self.row {
            self.emit(MoveDown(to_u16(row - self.row)));
        } else if row < self.row {
            self.emit(MoveUp(to_u16(self.row - row)));
        }
        self.row = row;
        self.emit(MoveToColumn(to_u16(column)));
    }

    fn emit(&mut self, command: impl Command) {
        command
            .write_ansi(&mut self.buf)
            .expect("writing to a String");
    }
}

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use typ_rs_core::display::{Viewport, lay_out};
    use typ_rs_core::prompt::Prompt;
    use typ_rs_core::session::{EndCondition, Input, Key, SessionState};

    fn session(prompt: &str, script: &str) -> SessionState {
        let mut state = SessionState::new(
            Prompt::new(prompt.split(' ')),
            EndCondition::AfterWords(usize::MAX),
        );
        for (i, c) in script.chars().enumerate() {
            state.apply_event(Input::new(i as u64 * 100_000, Key::Char(c)));
        }
        state
    }

    fn view(columns: usize, rows: usize) -> Viewport {
        Viewport { columns, rows }
    }

    /// The frame with every escape sequence removed: just the text written
    /// and the line breaks between lines.
    fn text_of(frame: &str) -> String {
        let mut out = String::new();
        let mut chars = frame.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn lossy(frame: &[u8]) -> String {
        String::from_utf8_lossy(frame).into_owned()
    }

    const MOVE_UP_1: &str = "\x1b[1A";
    const MOVE_DOWN_1: &str = "\x1b[1B";
    const COLUMN_0: &str = "\x1b[1G";
    const CLEAR_TO_END_OF_LINE: &str = "\x1b[K";
    const CLEAR_BELOW: &str = "\x1b[J";

    #[test]
    fn the_first_frame_paints_every_line_and_parks_the_cursor_on_the_first() {
        let mut painter = Painter::new(Palette::Color);
        let display = lay_out(&session("cat dog fox", ""), view(8, 10));

        let frame = lossy(painter.frame(&display, false));

        assert_eq!(text_of(&frame), "cat dog \r\nfox ");
        assert!(
            frame.ends_with(&format!("{MOVE_UP_1}{COLUMN_0}")),
            "{frame:?}"
        );
        assert_eq!(painter.painted_lines(), 2);
    }

    #[test]
    fn each_line_is_erased_before_it_is_written_never_after_its_last_cell() {
        let mut painter = Painter::new(Palette::Color);
        // "cat dog " fills all eight columns.
        let display = lay_out(&session("cat dog fox", ""), view(8, 10));

        let frame = lossy(painter.frame(&display, false));

        let first_line_end = frame.find("\r\n").unwrap();
        let (first, rest) = frame.split_at(first_line_end);
        assert!(
            first.starts_with(&format!("{COLUMN_0}{CLEAR_TO_END_OF_LINE}")),
            "{first:?}"
        );
        assert!(!first.ends_with(CLEAR_TO_END_OF_LINE), "{first:?}");
        assert!(rest.starts_with(&format!("\r\n{CLEAR_BELOW}")), "{rest:?}");
        assert!(
            !rest.contains(&format!("{CLEAR_BELOW}{MOVE_UP_1}")),
            "{rest:?}"
        );
    }

    #[test]
    fn typing_one_character_rewrites_only_the_typed_cell_and_the_new_caret_cell() {
        let mut painter = Painter::new(Palette::Color);
        painter.frame(&lay_out(&session("cat dog fox", ""), view(8, 10)), false);

        let frame =
            lossy(painter.frame(&lay_out(&session("cat dog fox", "c"), view(8, 10)), false));

        assert_eq!(text_of(&frame), "ca");
        assert!(!frame.contains("\r\n"));
        assert!(!frame.contains(CLEAR_TO_END_OF_LINE), "{frame:?}");
        assert!(frame.ends_with(COLUMN_0), "{frame:?}");
    }

    #[test]
    fn a_change_on_a_later_line_moves_down_to_it_and_back_up_to_park() {
        let mut painter = Painter::new(Palette::Color);
        painter.frame(
            &lay_out(&session("cat dog fox", "cat dog "), view(8, 10)),
            false,
        );

        let frame = lossy(painter.frame(
            &lay_out(&session("cat dog fox", "cat dog f"), view(8, 10)),
            false,
        ));

        assert_eq!(text_of(&frame), "fo");
        assert!(frame.starts_with(MOVE_DOWN_1), "{frame:?}");
        assert!(
            frame.ends_with(&format!("{MOVE_UP_1}{COLUMN_0}")),
            "{frame:?}"
        );
    }

    #[test]
    fn an_extra_that_lengthens_a_line_erases_its_tail_then_repaints_it() {
        let mut painter = Painter::new(Palette::Color);
        painter.frame(&lay_out(&session("cat dog", "cat"), view(20, 10)), false);

        let frame =
            lossy(painter.frame(&lay_out(&session("cat dog", "catx"), view(20, 10)), false));

        // The extra, the caret on the following space, and the pushed word.
        assert_eq!(text_of(&frame), "x dog ");
        let erase = frame.find(CLEAR_TO_END_OF_LINE).expect("erases the tail");
        assert!(text_of(&frame[..erase]).is_empty(), "{frame:?}");
    }

    #[test]
    fn a_frame_that_needs_more_lines_is_written_in_full_so_the_terminal_can_scroll() {
        let mut painter = Painter::new(Palette::Color);
        painter.frame(&lay_out(&session("cat dog", "cat"), view(8, 10)), false);

        // Extras push "dog" onto a second line.
        let frame = lossy(painter.frame(&lay_out(&session("cat dog", "catx"), view(8, 10)), false));

        assert_eq!(text_of(&frame), "catx \r\ndog ");
        assert!(frame.contains(&format!("\r\n{CLEAR_BELOW}")), "{frame:?}");
        assert!(
            frame.ends_with(&format!("{MOVE_UP_1}{COLUMN_0}")),
            "{frame:?}"
        );
    }

    #[test]
    fn a_frame_with_fewer_lines_clears_the_lines_that_vanished() {
        let mut painter = Painter::new(Palette::Color);
        painter.frame(&lay_out(&session("cat dog", "catx"), view(8, 10)), false);

        let frame = lossy(painter.frame(&lay_out(&session("cat dog", "cat"), view(8, 10)), false));

        assert!(
            frame.contains(&format!("{MOVE_DOWN_1}{COLUMN_0}{CLEAR_BELOW}")),
            "{frame:?}"
        );
        assert!(
            frame.ends_with(&format!("{MOVE_UP_1}{COLUMN_0}")),
            "{frame:?}"
        );
        assert_eq!(painter.painted_lines(), 1);
    }

    #[test]
    fn an_unchanged_display_produces_only_the_parking_move() {
        let mut painter = Painter::new(Palette::Color);
        let display = lay_out(&session("cat dog", "ca"), view(20, 10));
        painter.frame(&display, false);

        assert_eq!(lossy(painter.frame(&display, false)), COLUMN_0);
    }

    #[test]
    fn a_forced_repaint_rewrites_everything() {
        let mut painter = Painter::new(Palette::Color);
        let state = session("cat dog fox", "cat d");
        painter.frame(&lay_out(&state, view(8, 10)), false);

        let frame = lossy(painter.frame(&lay_out(&state, view(20, 10)), true));

        assert_eq!(text_of(&frame), "cat dog fox ");
        assert!(frame.ends_with(COLUMN_0), "{frame:?}");
    }

    #[test]
    fn colors_follow_the_palette() {
        let state = session("cat", "cx");
        let display = lay_out(&state, view(20, 10));

        let color = lossy(Painter::new(Palette::Color).frame(&display, false));
        assert!(color.contains(BRIGHT_BLACK), "bright black: {color:?}");
        assert!(color.contains(RED), "red: {color:?}");
        assert!(color.contains("\x1b[7m"), "reverse caret: {color:?}");

        let plain = lossy(Painter::new(Palette::NoColor).frame(&display, false));
        assert!(
            !plain.contains(BRIGHT_BLACK) && !plain.contains(RED),
            "{plain:?}"
        );
        assert!(plain.contains("\x1b[2m"), "dim: {plain:?}");
        assert!(plain.contains("\x1b[1m"), "bold: {plain:?}");
        assert!(plain.contains("\x1b[4m"), "underline: {plain:?}");
    }
}
