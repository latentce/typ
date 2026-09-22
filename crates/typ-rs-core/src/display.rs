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

/// How a cell should look, before colours are chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellClass {
    /// Not yet typed; also the separator space before its word is submitted.
    Untyped,
    /// Typed correctly; also the separator space of a submitted word.
    Correct,
    /// Typed incorrectly. The cell still shows the expected character.
    Incorrect,
    /// An extra character typed past the end of a word; shows what was typed.
    Extra,
    /// The next expected cell. Only one cell has this class, and none once
    /// the session is over.
    Caret,
}

/// One terminal cell, or two for a double-width character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub class: CellClass,
}

impl Cell {
    /// Columns the cell occupies; always at least one.
    pub fn width(&self) -> usize {
        self.ch.width().unwrap_or(0).max(1)
    }
}

/// Where the caret is within the visible lines.
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

    let mut cells = Vec::with_capacity(target.len() + MAX_EXTRAS + 1);
    let mut caret_index = None;
    for (i, expected) in target.chars().enumerate() {
        let class = match typed.get(i) {
            Some(&actual) if actual == expected => CellClass::Correct,
            Some(_) => CellClass::Incorrect,
            None if is_current && i == position => {
                caret_index = Some(i);
                CellClass::Caret
            }
            None => CellClass::Untyped,
        };
        cells.push(Cell {
            ch: expected,
            class,
        });
    }
    for &extra in typed.get(cells.len()..).unwrap_or(&[]) {
        cells.push(Cell {
            ch: displayable(extra),
            class: CellClass::Extra,
        });
    }
    let space_class = if is_current && caret_index.is_none() {
        caret_index = Some(cells.len());
        CellClass::Caret
    } else if submitted {
        CellClass::Correct
    } else {
        CellClass::Untyped
    };
    cells.push(Cell {
        ch: ' ',
        class: space_class,
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
    /// Colour is disabled: incorrect text is bold and underlined, untyped
    /// text is dim.
    NoColor,
}

impl Palette {
    /// Follows the `NO_COLOR` convention: colour is disabled when the
    /// variable is present with a non-empty value.
    pub fn from_no_color(value: Option<&str>) -> Palette {
        match value {
            Some(v) if !v.is_empty() => Palette::NoColor,
            _ => Palette::Color,
        }
    }
}

/// A cell's text colour; `Default` is the terminal's own foreground.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Foreground {
    #[default]
    Default,
    BrightBlack,
    Red,
}

/// Terminal attributes for one cell class under a palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub foreground: Foreground,
    pub dim: bool,
    pub bold: bool,
    pub underline: bool,
    pub reverse: bool,
}

impl CellClass {
    pub fn style(self, palette: Palette) -> Style {
        let plain = Style::default();
        match (self, palette) {
            (CellClass::Correct, _) => plain,
            (CellClass::Caret, _) => Style {
                reverse: true,
                ..plain
            },
            (CellClass::Untyped, Palette::Color) => Style {
                foreground: Foreground::BrightBlack,
                ..plain
            },
            (CellClass::Untyped, Palette::NoColor) => Style { dim: true, ..plain },
            (CellClass::Incorrect | CellClass::Extra, Palette::Color) => Style {
                foreground: Foreground::Red,
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
