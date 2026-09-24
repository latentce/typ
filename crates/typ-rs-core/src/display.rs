//! Lays a session out as display cells for a renderer to paint.
//!
//! This is a pure function of the session state and the viewport: the
//! renderer diffs successive results to update only what changed, and
//! re-lays out on resize. Nothing here writes to a terminal.

use unicode_width::UnicodeWidthChar;

use crate::session::{MAX_EXTRAS, Outcome, SessionState};

/// The area available for the prompt, in terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub columns: usize,
    pub rows: usize,
}

/// How a cell should look, before colors are chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellClass {
    /// Not yet typed, including a character never typed in a word that has
    /// been submitted; also the separator space before its word is submitted.
    Untyped,
    /// Typed correctly; also the separator space of a submitted word.
    Correct,
    /// Typed incorrectly. The cell still shows the expected character.
    Incorrect,
    /// An extra character typed past the end of a word; shows what was typed.
    Extra,
}

/// One terminal cell, or two for a double-width character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub class: CellClass,
    /// The cell is part of a word that was submitted with an uncorrected
    /// error. Every character of such a word is marked, whether or not it
    /// was itself typed wrong, so a skipped or misspelled word stands out
    /// as a whole; its separator space is not.
    pub uncorrected: bool,
}

impl Cell {
    /// Columns the cell occupies; always at least one.
    pub fn width(&self) -> usize {
        self.ch.width().unwrap_or(0).max(1)
    }

    /// The cell's class style, underlined when its word was submitted with
    /// an uncorrected error.
    pub fn style(&self, palette: Palette) -> Style {
        let style = self.class.style(palette);
        Style {
            underline: style.underline || self.uncorrected,
            ..style
        }
    }
}

/// Where the caret is within the visible lines: the cell the next keystroke
/// is expected on. The renderer places the hardware cursor here, so the
/// cell itself carries no caret styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caret {
    /// Index into [`Display::lines`].
    pub line: usize,
    pub column: usize,
}

/// The visible window of the laid-out prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    lines: Vec<Vec<Cell>>,
    first_line: usize,
    total_lines: usize,
    caret: Option<Caret>,
}

impl Display {
    /// The lines to paint, at most the viewport's rows.
    pub fn lines(&self) -> &[Vec<Cell>] {
        &self.lines
    }

    /// Index of the first visible line within the whole wrapped prompt;
    /// non-zero only when the prompt is taller than the viewport.
    pub fn first_line(&self) -> usize {
        self.first_line
    }

    /// Lines the whole wrapped prompt occupies.
    pub fn total_lines(&self) -> usize {
        self.total_lines
    }

    /// `None` once the session is over.
    pub fn caret(&self) -> Option<Caret> {
        self.caret
    }
}

/// Wraps the prompt at word boundaries into the viewport's width and picks
/// the window of lines to show.
///
/// A word, its extras, and the following space are kept together on one
/// line, so the caret always has a cell to sit on and text never re-wraps
/// because of where the caret is. A word that alone is wider than the
/// viewport is split across lines rather than overflowing. When the prompt is
/// taller than the viewport, the window keeps one line above the caret's line
/// where possible, and shows the end of the prompt once the session is over.
pub fn lay_out(state: &SessionState, viewport: Viewport) -> Display {
    let columns = viewport.columns.max(1);
    let rows = viewport.rows.max(1);

    let mut lines: Vec<Vec<Cell>> = vec![Vec::new()];
    let mut column = 0;
    let mut caret = None;
    for word in 0..state.word_count() {
        let (cells, caret_index) = word_cells(state, word);
        let width: usize = cells.iter().map(Cell::width).sum();
        if column > 0 && column + width > columns {
            lines.push(Vec::new());
            column = 0;
        }
        for (i, cell) in cells.into_iter().enumerate() {
            let cell_width = cell.width();
            if column > 0 && column + cell_width > columns {
                lines.push(Vec::new());
                column = 0;
            }
            if caret_index == Some(i) {
                caret = Some(Caret {
                    line: lines.len() - 1,
                    column,
                });
            }
            lines.last_mut().expect("at least one line").push(cell);
            column += cell_width;
        }
    }

    let total_lines = lines.len();
    let first_line = if total_lines <= rows {
        0
    } else {
        let anchor = caret.map_or(total_lines - 1, |c| c.line);
        anchor
            .saturating_sub(1)
            .max((anchor + 1).saturating_sub(rows))
            .min(total_lines - rows)
    };
    let visible: Vec<Vec<Cell>> = lines.drain(first_line..).take(rows).collect();
    let caret = caret.map(|c| Caret {
        line: c.line - first_line,
        column: c.column,
    });

    Display {
        lines: visible,
        first_line,
        total_lines,
        caret,
    }
}

/// The cells for one word: its target characters, then any extras, then the
/// separator space. Returns the index of the caret cell if the caret is in
/// this word.
fn word_cells(state: &SessionState, word: usize) -> (Vec<Cell>, Option<usize>) {
    let target = state.prompt().word(word);
    let typed = state.typed(word);
    let position = typed.len();
    let is_current = state.outcome().is_none() && word == state.current_word();
    let submitted = word < state.current_word() || state.outcome() == Some(Outcome::Completed);
    let uncorrected = submitted && state.has_uncorrected_error(word);

    let mut cells = Vec::with_capacity(target.len() + MAX_EXTRAS + 1);
    let mut caret_index = None;
    for (i, expected) in target.chars().enumerate() {
        let class = match typed.get(i) {
            Some(&actual) if actual == expected => CellClass::Correct,
            Some(_) => CellClass::Incorrect,
            None => CellClass::Untyped,
        };
        if is_current && i == position {
            caret_index = Some(i);
        }
        cells.push(Cell {
            ch: expected,
            class,
            uncorrected,
        });
    }
    for &extra in typed.get(cells.len()..).unwrap_or(&[]) {
        cells.push(Cell {
            ch: displayable(extra),
            class: CellClass::Extra,
            uncorrected,
        });
    }
    if is_current && caret_index.is_none() {
        caret_index = Some(cells.len());
    }
    cells.push(Cell {
        ch: ' ',
        class: if submitted {
            CellClass::Correct
        } else {
            CellClass::Untyped
        },
        uncorrected: false,
    });
    (cells, caret_index)
}

/// Typed characters that would occupy no cell (combining marks, controls)
/// are shown as the replacement character so the extra stays visible.
fn displayable(c: char) -> char {
    if c.width().unwrap_or(0) == 0 {
        '\u{FFFD}'
    } else {
        c
    }
}

/// Which set of terminal styles to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    Color,
    /// Color is disabled: incorrect text is bold and underlined, untyped
    /// text is dim.
    NoColor,
}

impl Palette {
    /// Follows the `NO_COLOR` convention: color is disabled when the
    /// variable is present with a non-empty value.
    pub fn from_no_color(value: Option<&str>) -> Palette {
        match value {
            Some(v) if !v.is_empty() => Palette::NoColor,
            _ => Palette::Color,
        }
    }
}

/// A cell's text color; `Default` is the terminal's own foreground.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Foreground {
    #[default]
    Default,
    BrightBlack,
    Red,
    /// Extras: wrong like a mistake, but not a character of the prompt, so a
    /// step darker.
    DarkRed,
}

/// Terminal attributes for one cell class under a palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub foreground: Foreground,
    pub dim: bool,
    pub bold: bool,
    pub underline: bool,
}

impl CellClass {
    pub fn style(self, palette: Palette) -> Style {
        let plain = Style::default();
        match (self, palette) {
            (CellClass::Correct, _) => plain,
            (CellClass::Untyped, Palette::Color) => Style {
                foreground: Foreground::BrightBlack,
                ..plain
            },
            (CellClass::Untyped, Palette::NoColor) => Style { dim: true, ..plain },
            (CellClass::Incorrect, Palette::Color) => Style {
                foreground: Foreground::Red,
                ..plain
            },
            (CellClass::Extra, Palette::Color) => Style {
                foreground: Foreground::DarkRed,
                ..plain
            },
            (CellClass::Incorrect | CellClass::Extra, Palette::NoColor) => Style {
                bold: true,
                underline: true,
                ..plain
            },
        }
    }
}

/// The shape of the hardware cursor while a session runs. The names are
/// kitty's, so a setting reads the same in both places.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    Block,
    /// A thin vertical bar before the next expected character, as a typing
    /// site shows it.
    #[default]
    Beam,
    Underline,
}

impl CursorShape {
    /// Every shape, in the order they are listed to the user.
    pub fn all() -> &'static [CursorShape] {
        &[
            CursorShape::Block,
            CursorShape::Beam,
            CursorShape::Underline,
        ]
    }

    /// The name the user sets and sees, and the form a setting stores.
    pub fn name(self) -> &'static str {
        match self {
            CursorShape::Block => "block",
            CursorShape::Beam => "beam",
            CursorShape::Underline => "underline",
        }
    }

    /// The shape with the given name, if there is one.
    pub fn from_name(name: &str) -> Option<CursorShape> {
        CursorShape::all()
            .iter()
            .copied()
            .find(|s| s.name() == name)
    }
}

/// How the hardware cursor is shown while a session runs: its shape and
/// whether it blinks. The terminal is asked for this on entry and given its
/// own cursor back on exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorStyle {
    pub shape: CursorShape,
    pub blink: bool,
}

impl CursorStyle {
    /// The name the user sets and sees for a blink setting, and the form a
    /// setting stores: `on` or `off`.
    pub fn blink_name(blink: bool) -> &'static str {
        if blink { "on" } else { "off" }
    }

    /// The blink setting with the given name, if it is one.
    pub fn blink_from_name(name: &str) -> Option<bool> {
        [true, false]
            .into_iter()
            .find(|&blink| CursorStyle::blink_name(blink) == name)
    }
}
