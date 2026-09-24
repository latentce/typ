//! Turns successive [`Display`]s into the bytes that update the terminal.
//!
//! The prompt is painted inline where the shell left the cursor. The
//! hardware cursor is the caret: between frames it rests on the next
//! expected cell, and it is hidden only while a frame is being written so it
//! does not visibly jump. Every movement is relative to where the cursor
//! was left, so the painter never needs to know where on the screen the
//! prompt is: if the terminal scrolls, the cursor moves with the prompt.

use crossterm::Command;
use crossterm::cursor::{Hide, MoveDown, MoveToColumn, MoveUp, Show};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{Clear, ClearType};
use typ_rs_core::display::{Cell, Display, Foreground, Palette, Style};

/// Foreground colors as the plain 16-color SGR codes, which every color
/// terminal understands: bright black, bright red for mistakes, and the
/// ordinary (darker) red for extras.
const BRIGHT_BLACK: &str = "\x1b[90m";
const RED: &str = "\x1b[91m";
const DARK_RED: &str = "\x1b[31m";

pub struct Painter {
    palette: Palette,
    previous: Option<Display>,
    buf: String,
    /// Physical row of the hardware cursor relative to the prompt's first
    /// row. Kept between frames: the next frame's movements start from it.
    row: usize,
    /// The painted line and the column within it that the cursor was left
    /// on by the last frame. Normally the line is also the row; they differ
    /// only after the terminal has re-wrapped the painted lines.
    line: usize,
    column: usize,
    style: Style,
}

impl Painter {
    pub fn new(palette: Palette) -> Painter {
        Painter {
            palette,
            previous: None,
            buf: String::new(),
            row: 0,
            line: 0,
            column: 0,
            style: Style::default(),
        }
    }

    /// Lines the last frame left on the screen.
    pub fn painted_lines(&self) -> usize {
        self.previous.as_ref().map_or(0, |d| d.lines().len())
    }

    /// Painted lines below the one the hardware cursor rests on. Meaningful
    /// right after a frame; between a resize and the repaint that follows
    /// it, the cursor's row is no longer a painted line's.
    pub fn rows_below_cursor(&self) -> usize {
        self.painted_lines().saturating_sub(self.row + 1)
    }

    /// The bytes that bring the terminal from the previous frame to
    /// `display`, ending with the cursor shown on the caret; on the first
    /// column of the last line once there is no caret, so that whatever is
    /// printed next follows the prompt. `repaint` forces every line to be
    /// rewritten, as after a resize; otherwise only cells that changed are
    /// touched. A frame that needs more lines than the last one is always
    /// written in full so that the terminal scrolls if the prompt is at the
    /// bottom of the screen.
    ///
    /// Stale cells are always erased before a line is written, never after:
    /// a line that fills the terminal's width leaves the cursor pending on
    /// its last cell, where an erase-to-end-of-line would remove that cell.
    pub fn frame(&mut self, display: &Display, repaint: bool) -> &[u8] {
        self.buf.clear();
        self.emit(Hide);
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

        let (line, column) = match display.caret() {
            Some(caret) => (caret.line, caret.column),
            None => (lines.len().saturating_sub(1), 0),
        };
        self.goto(line, column);
        self.line = line;
        self.column = column;
        self.emit(Show);
        self.previous = Some(display.clone());
        self.buf.as_bytes()
    }

    /// The terminal is now `columns` wide. A terminal that re-wraps its
    /// lines when narrowed (kitty and most others) has just spread every
    /// painted line wider than that over several rows and kept the cursor
    /// on its cell, so the cursor is further from the prompt's first row
    /// than the line it rests on says. This works out how far, so that the
    /// repaint that must follow starts from the top. Widening changes
    /// nothing: painted lines end in hard line breaks and are never joined.
    /// A terminal that clips long lines instead of wrapping them leaves the
    /// cursor where it was, and after narrowing this over-estimates.
    pub fn terminal_resized_to(&mut self, columns: usize) {
        let Some(previous) = &self.previous else {
            return;
        };
        let columns = columns.max(1);
        let rows_of = |line: &[Cell]| {
            line.iter()
                .map(Cell::width)
                .sum::<usize>()
                .div_ceil(columns)
                .max(1)
        };
        let above: usize = previous.lines()[..self.line]
            .iter()
            .map(|l| rows_of(l))
            .sum();
        self.row = above + self.column / columns;
    }

    fn paint_all(&mut self, lines: &[Vec<Cell>]) {
        self.goto(0, 0);
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
            self.set_style(cell.style(self.palette));
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
            Foreground::DarkRed => self.buf.push_str(DARK_RED),
        }
        let attributes = [
            (style.dim, Attribute::Dim),
            (style.bold, Attribute::Bold),
            (style.underline, Attribute::Underlined),
        ];
        for (on, attribute) in attributes {
            if on {
                self.emit(SetAttribute(attribute));
            }
        }
        self.style = style;
    }

    /// Moves relative to where the cursor is. Zero-count moves are never
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
            let key = match c {
                '⌫' => Key::Backspace,
                c => Key::Char(c),
            };
            state.apply_event(Input::new(i as u64 * 100_000, key));
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
    const HIDE: &str = "\x1b[?25l";
    const SHOW: &str = "\x1b[?25h";
    const UNDERLINE: &str = "\x1b[4m";

    fn column(n: usize) -> String {
        format!("\x1b[{}G", n + 1)
    }

    #[test]
    fn the_first_frame_paints_every_line_and_leaves_the_cursor_on_the_caret() {
        let mut painter = Painter::new(Palette::Color);
        let display = lay_out(&session("cat dog fox", ""), view(8, 10));

        let frame = lossy(painter.frame(&display, false));

        assert_eq!(text_of(&frame), "cat dog \r\nfox ");
        assert!(frame.starts_with(HIDE), "{frame:?}");
        assert!(
            frame.ends_with(&format!("{MOVE_UP_1}{COLUMN_0}{SHOW}")),
            "{frame:?}"
        );
        assert_eq!(painter.painted_lines(), 2);
        assert_eq!(painter.rows_below_cursor(), 1);
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
            first.starts_with(&format!("{HIDE}{COLUMN_0}{CLEAR_TO_END_OF_LINE}")),
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
    fn typing_one_character_rewrites_only_that_cell_and_moves_the_cursor_on() {
        let mut painter = Painter::new(Palette::Color);
        painter.frame(&lay_out(&session("cat dog fox", ""), view(8, 10)), false);

        let frame =
            lossy(painter.frame(&lay_out(&session("cat dog fox", "c"), view(8, 10)), false));

        assert_eq!(text_of(&frame), "c");
        assert!(!frame.contains("\r\n"));
        assert!(!frame.contains(CLEAR_TO_END_OF_LINE), "{frame:?}");
        assert!(
            frame.ends_with(&format!("{}{SHOW}", column(1))),
            "{frame:?}"
        );
    }

    #[test]
    fn a_change_on_a_later_line_moves_down_to_it_and_the_cursor_stays_there() {
        let mut painter = Painter::new(Palette::Color);
        painter.frame(
            &lay_out(&session("cat dog fox", "cat dog "), view(8, 10)),
            false,
        );
        assert_eq!(painter.rows_below_cursor(), 0);

        let frame = lossy(painter.frame(
            &lay_out(&session("cat dog fox", "cat dog f"), view(8, 10)),
            false,
        ));

        assert_eq!(text_of(&frame), "f");
        assert!(!frame.contains(MOVE_DOWN_1), "{frame:?}");
        assert!(!frame.contains(MOVE_UP_1), "{frame:?}");
        assert!(
            frame.ends_with(&format!("{}{SHOW}", column(1))),
            "{frame:?}"
        );
    }

    #[test]
    fn the_cursor_moves_down_when_the_caret_crosses_to_the_next_line() {
        let mut painter = Painter::new(Palette::Color);
        painter.frame(
            &lay_out(&session("cat dog fox", "cat dog"), view(8, 10)),
            false,
        );

        let frame = lossy(painter.frame(
            &lay_out(&session("cat dog fox", "cat dog "), view(8, 10)),
            false,
        ));

        // The submitted word's space, then down to the start of "fox".
        assert_eq!(text_of(&frame), " ");
        assert!(
            frame.ends_with(&format!("{MOVE_DOWN_1}{COLUMN_0}{SHOW}")),
            "{frame:?}"
        );
        assert_eq!(painter.rows_below_cursor(), 0);
    }

    #[test]
    fn backspacing_into_the_previous_line_moves_the_cursor_back_up() {
        let mut painter = Painter::new(Palette::Color);
        painter.frame(
            &lay_out(&session("cat dog fox", "cat dxg "), view(8, 10)),
            false,
        );

        let frame = lossy(painter.frame(
            &lay_out(&session("cat dog fox", "cat dxg ⌫"), view(8, 10)),
            false,
        ));

        assert!(
            frame.ends_with(&format!("{}{SHOW}", column(7))),
            "{frame:?}"
        );
        assert!(frame.contains(MOVE_UP_1), "{frame:?}");
        assert_eq!(painter.rows_below_cursor(), 1);
    }

    #[test]
    fn an_extra_that_lengthens_a_line_erases_its_tail_then_repaints_it() {
        let mut painter = Painter::new(Palette::Color);
        painter.frame(&lay_out(&session("cat dog", "cat"), view(20, 10)), false);

        let frame =
            lossy(painter.frame(&lay_out(&session("cat dog", "catx"), view(20, 10)), false));

        // The extra, the following space, and the pushed word.
        assert_eq!(text_of(&frame), "x dog ");
        let erase = frame.find(CLEAR_TO_END_OF_LINE).expect("erases the tail");
        assert!(text_of(&frame[..erase]).is_empty(), "{frame:?}");
        assert!(
            frame.ends_with(&format!("{}{SHOW}", column(4))),
            "{frame:?}"
        );
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
            frame.ends_with(&format!("{MOVE_UP_1}{}{SHOW}", column(4))),
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
            frame.ends_with(&format!("{MOVE_UP_1}{}{SHOW}", column(3))),
            "{frame:?}"
        );
        assert_eq!(painter.painted_lines(), 1);
    }

    #[test]
    fn an_unchanged_display_only_hides_and_shows_the_cursor_where_it_is() {
        let mut painter = Painter::new(Palette::Color);
        let display = lay_out(&session("cat dog", "ca"), view(20, 10));
        painter.frame(&display, false);

        assert_eq!(
            lossy(painter.frame(&display, false)),
            format!("{HIDE}{}{SHOW}", column(2))
        );
    }

    #[test]
    fn a_forced_repaint_rewrites_everything_from_the_top() {
        let mut painter = Painter::new(Palette::Color);
        let state = session("cat dog fox", "cat dog f");
        painter.frame(&lay_out(&state, view(8, 10)), false);
        assert_eq!(painter.rows_below_cursor(), 0);

        let frame = lossy(painter.frame(&lay_out(&state, view(20, 10)), true));

        assert!(
            frame.starts_with(&format!("{HIDE}{MOVE_UP_1}{COLUMN_0}")),
            "{frame:?}"
        );
        assert_eq!(text_of(&frame), "cat dog fox ");
        assert!(
            frame.ends_with(&format!("{}{SHOW}", column(9))),
            "{frame:?}"
        );
    }

    #[test]
    fn after_a_narrowing_the_repaint_starts_from_where_the_top_now_is() {
        let mut painter = Painter::new(Palette::Color);
        let state = session("cat dog fox", "cat dog f");
        // "cat dog " / "fox ", cursor on the second line at column 1.
        painter.frame(&lay_out(&state, view(8, 10)), false);

        // At five columns "cat dog " wraps onto two rows, so the cursor is
        // now two rows below the top.
        painter.terminal_resized_to(5);
        let frame = lossy(painter.frame(&lay_out(&state, view(5, 10)), true));

        assert!(
            frame.starts_with(&format!("{HIDE}\x1b[2A{COLUMN_0}")),
            "{frame:?}"
        );
        assert_eq!(text_of(&frame), "cat \r\ndog \r\nfox ");
        assert!(
            frame.ends_with(&format!("{}{SHOW}", column(1))),
            "{frame:?}"
        );
        assert_eq!(painter.rows_below_cursor(), 0);
    }

    #[test]
    fn a_re_wrapped_cursor_line_counts_the_rows_above_the_cursor_within_it() {
        let mut painter = Painter::new(Palette::Color);
        let state = session("cat dog fox", "cat dog");
        // One line, "cat dog fox ", cursor at column 7.
        painter.frame(&lay_out(&state, view(20, 10)), false);

        // At five columns the line spans three rows and the cursor, at
        // column 7, sits on the second.
        painter.terminal_resized_to(5);
        let frame = lossy(painter.frame(&lay_out(&state, view(5, 10)), true));

        assert!(
            frame.starts_with(&format!("{HIDE}{MOVE_UP_1}{COLUMN_0}")),
            "{frame:?}"
        );
    }

    #[test]
    fn widening_changes_nothing_about_where_the_cursor_is() {
        let mut painter = Painter::new(Palette::Color);
        let state = session("cat dog fox", "cat dog f");
        painter.frame(&lay_out(&state, view(8, 10)), false);

        painter.terminal_resized_to(20);
        let frame = lossy(painter.frame(&lay_out(&state, view(20, 10)), true));

        assert!(
            frame.starts_with(&format!("{HIDE}{MOVE_UP_1}{COLUMN_0}")),
            "{frame:?}"
        );
        assert_eq!(text_of(&frame), "cat dog fox ");
    }

    #[test]
    fn once_the_session_is_over_the_cursor_rests_at_the_start_of_the_last_line() {
        let mut painter = Painter::new(Palette::Color);
        painter.frame(
            &lay_out(&session("cat dog fox", "cat dog fo"), view(8, 10)),
            false,
        );

        let frame = lossy(painter.frame(
            &lay_out(&session("cat dog fox", "cat dog fox"), view(8, 10)),
            false,
        ));

        assert!(frame.ends_with(&format!("{COLUMN_0}{SHOW}")), "{frame:?}");
        assert_eq!(painter.rows_below_cursor(), 0);
    }

    #[test]
    fn colors_follow_the_palette() {
        let state = session("cat", "cxtq");
        let display = lay_out(&state, view(20, 10));

        let color = lossy(Painter::new(Palette::Color).frame(&display, false));
        assert!(color.contains(BRIGHT_BLACK), "bright black: {color:?}");
        assert!(color.contains(RED), "red: {color:?}");
        assert!(color.contains(DARK_RED), "dark red: {color:?}");
        assert!(!color.contains("\x1b[7m"), "no reverse video: {color:?}");

        let plain = lossy(Painter::new(Palette::NoColor).frame(&display, false));
        assert!(
            !plain.contains(BRIGHT_BLACK) && !plain.contains(RED) && !plain.contains(DARK_RED),
            "{plain:?}"
        );
        assert!(plain.contains("\x1b[2m"), "dim: {plain:?}");
        assert!(plain.contains("\x1b[1m"), "bold: {plain:?}");
        assert!(plain.contains(UNDERLINE), "underline: {plain:?}");
    }

    #[test]
    fn a_word_submitted_with_an_error_is_underlined_when_it_is_submitted() {
        let mut painter = Painter::new(Palette::Color);
        let before =
            lossy(painter.frame(&lay_out(&session("cat dog", "cxt"), view(20, 10)), false));
        assert!(!before.contains(UNDERLINE), "{before:?}");

        let frame =
            lossy(painter.frame(&lay_out(&session("cat dog", "cxt "), view(20, 10)), false));

        // The whole word is repainted underlined; its space is not.
        assert_eq!(text_of(&frame), "cat ");
        let underlined = frame.find(UNDERLINE).expect("underlines the word");
        assert!(text_of(&frame[..underlined]).is_empty(), "{frame:?}");
    }
}
