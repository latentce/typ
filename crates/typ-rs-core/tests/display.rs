use typ_rs_core::display::{
    Cell, CellClass, CursorShape, CursorStyle, Display, Foreground, Palette, Style, Viewport,
    lay_out,
};
use typ_rs_core::prompt::Prompt;
use typ_rs_core::session::{EndCondition, Input, Key, Outcome, SessionState};

fn session(prompt: &str, script: &str) -> SessionState {
    let mut state = SessionState::new(
        Prompt::new(prompt.split(' ')),
        EndCondition::AfterWords(usize::MAX),
    );
    type_script(&mut state, script);
    state
}

fn type_script(state: &mut SessionState, script: &str) {
    for (i, symbol) in script.chars().enumerate() {
        let key = match symbol {
            '⌫' => Key::Backspace,
            c => Key::Char(c),
        };
        state.apply_event(Input::new(i as u64 * 100_000, key));
    }
}

fn view(columns: usize, rows: usize) -> Viewport {
    Viewport { columns, rows }
}

fn text(line: &[Cell]) -> String {
    line.iter().map(|c| c.ch).collect()
}

/// `.` untyped, `=` correct, `x` incorrect, `+` extra; a double-width cell
/// repeats its legend character.
fn classes(line: &[Cell]) -> String {
    line.iter()
        .flat_map(|c| {
            let legend = match c.class {
                CellClass::Untyped => '.',
                CellClass::Correct => '=',
                CellClass::Incorrect => 'x',
                CellClass::Extra => '+',
            };
            std::iter::repeat_n(legend, c.width())
        })
        .collect()
}

/// `_` for a cell of a word submitted with an uncorrected error, a space
/// otherwise; a double-width cell repeats its legend character.
fn marks(line: &[Cell]) -> String {
    line.iter()
        .flat_map(|c| std::iter::repeat_n(if c.uncorrected { '_' } else { ' ' }, c.width()))
        .collect()
}

fn caret_of(display: &Display) -> Option<(usize, usize)> {
    display.caret().map(|c| (c.line, c.column))
}

fn texts(display: &Display) -> Vec<String> {
    display.lines().iter().map(|l| text(l)).collect()
}

fn pictures(display: &Display) -> Vec<(String, String)> {
    display
        .lines()
        .iter()
        .map(|l| (text(l), classes(l)))
        .collect()
}

fn width(line: &[Cell]) -> usize {
    line.iter().map(Cell::width).sum()
}

fn assert_fits(display: &Display, columns: usize) {
    for line in display.lines() {
        assert!(width(line) <= columns, "{:?} exceeds {columns}", text(line));
    }
}

#[test]
fn wraps_at_word_boundaries_without_splitting_a_word() {
    let state = session("the quick brown fox jumps", "");
    let display = lay_out(&state, view(12, 10));

    assert_eq!(texts(&display), ["the quick ", "brown fox ", "jumps "]);
    assert_fits(&display, 12);
    assert_eq!(display.total_lines(), 3);
    assert_eq!(display.first_line(), 0);
}

#[test]
fn a_word_and_its_following_space_must_both_fit_on_the_line() {
    let state = session("cat dog", "");
    assert_eq!(texts(&lay_out(&state, view(8, 10))), ["cat dog "]);
    assert_eq!(texts(&lay_out(&state, view(7, 10))), ["cat ", "dog "]);
}

#[test]
fn re_wraps_when_the_width_changes() {
    let state = session("the quick brown fox jumps", "the quick br");
    let narrow = lay_out(&state, view(12, 10));
    let wide = lay_out(&state, view(20, 10));

    assert_eq!(texts(&narrow), ["the quick ", "brown fox ", "jumps "]);
    assert_eq!(texts(&wide), ["the quick brown fox ", "jumps "]);
    assert_eq!(caret_of(&narrow), Some((1, 2)));
    assert_eq!(caret_of(&wide), Some((0, 12)));
}

#[test]
fn cells_are_classed_by_what_was_typed_and_the_caret_sits_on_the_next_expected_cell() {
    let state = session("cat dog", "cxt d");
    let display = lay_out(&state, view(20, 10));

    assert_eq!(
        pictures(&display),
        [("cat dog ".to_string(), "=x===...".to_string())]
    );
    assert_eq!(caret_of(&display), Some((0, 5)));
}

#[test]
fn an_incorrect_cell_shows_the_expected_character_not_the_typed_one() {
    let state = session("cat", "cZ");
    let display = lay_out(&state, view(20, 10));
    assert_eq!(
        pictures(&display),
        [("cat ".to_string(), "=x..".to_string())]
    );
    assert_eq!(caret_of(&display), Some((0, 2)));
}

#[test]
fn the_caret_starts_on_the_first_character_and_moves_to_the_next_word_after_a_space() {
    assert_eq!(
        caret_of(&lay_out(&session("cat dog", ""), view(20, 10))),
        Some((0, 0))
    );
    assert_eq!(
        caret_of(&lay_out(&session("cat dog", "cat "), view(20, 10))),
        Some((0, 4))
    );
}

#[test]
fn a_fully_typed_word_puts_the_caret_on_its_following_space() {
    let display = lay_out(&session("cat dog", "cat"), view(20, 10));
    assert_eq!(classes(&display.lines()[0]), "===.....");
    assert_eq!(caret_of(&display), Some((0, 3)));
}

#[test]
fn the_caret_cell_is_styled_as_untyped_not_marked_in_any_way() {
    let display = lay_out(&session("cat dog", "ca"), view(20, 10));
    let line = &display.lines()[0];
    assert_eq!(line[2].class, CellClass::Untyped);
    assert!(!line[2].uncorrected);
}

#[test]
fn extras_render_after_the_word_and_push_the_following_text() {
    let state = session("cat dog", "catxx");
    let display = lay_out(&state, view(20, 10));

    assert_eq!(
        pictures(&display),
        [("catxx dog ".to_string(), "===++.....".to_string())]
    );
    assert_eq!(caret_of(&display), Some((0, 5)));
}

#[test]
fn extras_count_toward_wrapping() {
    let state = session("cat dog", "catxx");
    assert_eq!(texts(&lay_out(&state, view(8, 10))), ["catxx ", "dog "]);
    assert_eq!(texts(&lay_out(&state, view(10, 10))), ["catxx dog "]);
}

#[test]
fn a_re_entered_word_shows_its_extras_and_the_caret_after_them() {
    let state = session("cat dog", "catx ⌫");
    let display = lay_out(&state, view(20, 10));
    assert_eq!(classes(&display.lines()[0]), "===+.....");
    assert_eq!(caret_of(&display), Some((0, 4)));
}

#[test]
fn a_word_submitted_with_an_error_is_marked_as_a_whole_but_not_its_space() {
    let state = session("cat dog fox", "cxt dog");
    let display = lay_out(&state, view(20, 10));
    let line = &display.lines()[0];
    assert_eq!(classes(line), "=x=====.....");
    assert_eq!(marks(line), "___         ");
}

#[test]
fn a_word_submitted_short_is_marked_and_its_missing_letters_stay_untyped() {
    let state = session("cat dog", "c d");
    let display = lay_out(&state, view(20, 10));
    let line = &display.lines()[0];
    assert_eq!(classes(line), "=..==...");
    assert_eq!(marks(line), "___     ");
}

#[test]
fn a_word_submitted_with_extras_is_marked_including_the_extras() {
    let state = session("cat dog", "catx d");
    let display = lay_out(&state, view(20, 10));
    assert_eq!(marks(&display.lines()[0]), "____     ");
}

#[test]
fn the_mark_goes_away_when_the_word_is_re_entered_and_comes_back_when_corrected() {
    let mut state = session("cat dog", "cxt ");
    assert_eq!(marks(&lay_out(&state, view(20, 10)).lines()[0]), "___     ");

    type_script(&mut state, "⌫");
    assert_eq!(marks(&lay_out(&state, view(20, 10)).lines()[0]), "        ");

    type_script(&mut state, "⌫⌫at ");
    assert_eq!(marks(&lay_out(&state, view(20, 10)).lines()[0]), "        ");
}

#[test]
fn a_word_that_is_current_is_never_marked_even_with_errors_in_it() {
    let state = session("cat dog", "cx");
    assert_eq!(marks(&lay_out(&state, view(20, 10)).lines()[0]), "        ");
}

#[test]
fn words_left_wrong_are_marked_once_the_session_is_complete() {
    let state = session("cat dog", "cat dxg ");
    let display = lay_out(&state, view(20, 10));
    assert_eq!(display.caret(), None);
    assert_eq!(marks(&display.lines()[0]), "    ___ ");
}

#[test]
fn no_caret_once_the_session_is_complete() {
    let state = session("cat dog", "cat dog");
    let display = lay_out(&state, view(20, 10));
    assert_eq!(classes(&display.lines()[0]), "========");
    assert_eq!(display.caret(), None);
}

#[test]
fn only_words_covered_by_the_end_condition_are_shown() {
    let state = SessionState::new(
        Prompt::new(["cat", "dog", "fox"]),
        EndCondition::AfterWords(2),
    );
    assert_eq!(texts(&lay_out(&state, view(20, 10))), ["cat dog "]);
}

#[test]
fn scrolls_a_window_that_keeps_one_line_of_context_above_the_caret() {
    let prompt = ["aaaa"; 10].join(" ");
    let mut state = session(&prompt, "");
    let display = lay_out(&state, view(5, 3));
    assert_eq!(display.total_lines(), 10);
    assert_eq!(display.lines().len(), 3);
    assert_eq!(display.first_line(), 0);
    assert_eq!(display.caret().map(|c| c.line), Some(0));

    type_script(&mut state, "aaaa ");
    let display = lay_out(&state, view(5, 3));
    assert_eq!(display.first_line(), 0);
    assert_eq!(display.caret().map(|c| c.line), Some(1));

    type_script(&mut state, "aaaa aaaa ");
    let display = lay_out(&state, view(5, 3));
    assert_eq!(display.first_line(), 2);
    assert_eq!(caret_of(&display), Some((1, 0)));

    type_script(&mut state, "aaaa aaaa aaaa aaaa aaaa aaaa ");
    let display = lay_out(&state, view(5, 3));
    assert_eq!(display.first_line(), 7);
    assert_eq!(display.lines().len(), 3);
    assert_eq!(display.caret().map(|c| c.line), Some(2));
}

#[test]
fn a_single_row_window_always_contains_the_caret_line() {
    let prompt = ["aaaa"; 6].join(" ");
    let mut state = session(&prompt, "");
    for line in 0..6 {
        let display = lay_out(&state, view(5, 1));
        assert_eq!(display.lines().len(), 1);
        assert_eq!(display.first_line(), line);
        assert_eq!(caret_of(&display), Some((0, 0)));
        type_script(&mut state, "aaaa ");
    }
}

#[test]
fn the_window_shows_the_end_of_the_prompt_once_the_session_is_over() {
    let prompt = ["aaaa"; 10].join(" ");
    let state = session(&prompt, &["aaaa"; 10].join(" "));
    assert_eq!(state.outcome(), Some(Outcome::Completed));

    let display = lay_out(&state, view(5, 3));
    assert_eq!(display.caret(), None);
    assert_eq!(display.first_line(), 7);
    assert_eq!(display.lines().len(), 3);
}

#[test]
fn the_whole_prompt_is_shown_when_it_fits_the_height() {
    let state = session("the quick brown fox", "");
    let display = lay_out(&state, view(10, 3));
    assert_eq!(display.total_lines(), 2);
    assert_eq!(display.lines().len(), 2);
    assert_eq!(display.first_line(), 0);
}

#[test]
fn display_width_of_typed_extras_is_respected() {
    let state = session("cat dog", "cat漢");
    let display = lay_out(&state, view(20, 10));
    let line = &display.lines()[0];

    assert_eq!(text(line), "cat漢 dog ");
    assert_eq!(classes(line), "===++.....");
    assert_eq!(width(line), 10);
    assert_eq!(display.caret().map(|c| c.column), Some(5));
    assert_eq!(texts(&lay_out(&state, view(9, 10))), ["cat漢 ", "dog "]);
}

#[test]
fn zero_width_extras_render_as_a_visible_placeholder() {
    let state = session("cat dog", "cat\u{301}");
    let display = lay_out(&state, view(20, 10));
    let line = &display.lines()[0];
    assert_eq!(text(line), "cat\u{FFFD} dog ");
    assert_eq!(width(line), 9);
    assert_eq!(classes(line), "===+.....");
    assert_eq!(caret_of(&display), Some((0, 4)));
}

#[test]
fn a_word_wider_than_the_viewport_is_split_only_as_a_last_resort() {
    let state = session("abcdefghij ok", "");
    let display = lay_out(&state, view(4, 10));
    assert_eq!(texts(&display), ["abcd", "efgh", "ij ", "ok "]);
    assert_fits(&display, 4);
    assert_eq!(caret_of(&display), Some((0, 0)));
}

#[test]
fn degenerate_viewports_are_tolerated() {
    let state = session("cat dog", "ca");
    let display = lay_out(&state, view(0, 0));
    assert_eq!(display.lines().len(), 1);
    assert_eq!(display.caret().map(|c| c.line), Some(0));
    assert_eq!(text(&display.lines()[0]), "t");
}

#[test]
fn color_and_no_color_palettes_map_each_class_to_a_style() {
    let style = |class: CellClass, palette: Palette| class.style(palette);
    let plain = Style::default();
    assert_eq!(plain.foreground, Foreground::Default);
    assert!(!plain.dim && !plain.bold && !plain.underline);

    assert_eq!(
        style(CellClass::Untyped, Palette::Color),
        Style {
            foreground: Foreground::BrightBlack,
            ..plain
        }
    );
    assert_eq!(style(CellClass::Correct, Palette::Color), plain);
    assert_eq!(
        style(CellClass::Incorrect, Palette::Color),
        Style {
            foreground: Foreground::Red,
            ..plain
        }
    );
    assert_eq!(
        style(CellClass::Extra, Palette::Color),
        Style {
            foreground: Foreground::DarkRed,
            ..plain
        }
    );

    assert_eq!(
        style(CellClass::Untyped, Palette::NoColor),
        Style { dim: true, ..plain }
    );
    assert_eq!(style(CellClass::Correct, Palette::NoColor), plain);
    assert_eq!(
        style(CellClass::Incorrect, Palette::NoColor),
        Style {
            bold: true,
            underline: true,
            ..plain
        }
    );
    assert_eq!(
        style(CellClass::Extra, Palette::NoColor),
        style(CellClass::Incorrect, Palette::NoColor)
    );
}

#[test]
fn a_marked_cell_is_underlined_on_top_of_its_class_style() {
    let cell = |class, uncorrected| Cell {
        ch: 'a',
        class,
        uncorrected,
    };
    for palette in [Palette::Color, Palette::NoColor] {
        for class in [
            CellClass::Untyped,
            CellClass::Correct,
            CellClass::Incorrect,
            CellClass::Extra,
        ] {
            assert_eq!(cell(class, false).style(palette), class.style(palette));
            let marked = cell(class, true).style(palette);
            assert!(marked.underline, "{class:?} {palette:?}");
            assert_eq!(
                Style {
                    underline: false,
                    ..marked
                },
                Style {
                    underline: false,
                    ..class.style(palette)
                }
            );
        }
    }
}

#[test]
fn cursor_shapes_are_named_as_kitty_names_them_and_round_trip() {
    assert_eq!(
        CursorShape::all()
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>(),
        ["block", "beam", "underline"]
    );
    for shape in CursorShape::all() {
        assert_eq!(CursorShape::from_name(shape.name()), Some(*shape));
    }
    assert_eq!(CursorShape::from_name("bar"), None);
    assert_eq!(CursorShape::from_name("Beam"), None);
}

#[test]
fn blink_is_named_on_or_off_and_round_trips() {
    assert_eq!(CursorStyle::blink_name(true), "on");
    assert_eq!(CursorStyle::blink_name(false), "off");
    assert_eq!(CursorStyle::blink_from_name("on"), Some(true));
    assert_eq!(CursorStyle::blink_from_name("off"), Some(false));
    for bad in ["yes", "true", "1", "", "On"] {
        assert_eq!(CursorStyle::blink_from_name(bad), None, "{bad:?}");
    }
}

#[test]
fn the_default_cursor_is_a_steady_beam() {
    assert_eq!(
        CursorStyle::default(),
        CursorStyle {
            shape: CursorShape::Beam,
            blink: false
        }
    );
}

#[test]
fn palette_is_chosen_from_the_no_color_convention() {
    assert_eq!(Palette::from_no_color(None), Palette::Color);
    assert_eq!(Palette::from_no_color(Some("")), Palette::Color);
    assert_eq!(Palette::from_no_color(Some("1")), Palette::NoColor);
    assert_eq!(Palette::from_no_color(Some("anything")), Palette::NoColor);
}
