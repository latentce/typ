use typ_rs_core::display::{
    Cell, CellClass, CursorShape, CursorStyle, Display, Foreground, Header, Palette, Style, Tone,
    Viewport, header, lay_out,
};
use typ_rs_core::metrics::{elapsed_micros, final_characters, gross_wpm};
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
    let display = lay_out(&state, view(12, 10), None);

    assert_eq!(texts(&display), ["the quick ", "brown fox ", "jumps "]);
    assert_fits(&display, 12);
    assert_eq!(display.total_lines(), 3);
    assert_eq!(display.first_line(), 0);
}

#[test]
fn a_word_and_its_following_space_must_both_fit_on_the_line() {
    let state = session("cat dog", "");
    assert_eq!(texts(&lay_out(&state, view(8, 10), None)), ["cat dog "]);
    assert_eq!(texts(&lay_out(&state, view(7, 10), None)), ["cat ", "dog "]);
}

#[test]
fn re_wraps_when_the_width_changes() {
    let state = session("the quick brown fox jumps", "the quick br");
    let narrow = lay_out(&state, view(12, 10), None);
    let wide = lay_out(&state, view(20, 10), None);

    assert_eq!(texts(&narrow), ["the quick ", "brown fox ", "jumps "]);
    assert_eq!(texts(&wide), ["the quick brown fox ", "jumps "]);
    assert_eq!(caret_of(&narrow), Some((1, 2)));
    assert_eq!(caret_of(&wide), Some((0, 12)));
}

#[test]
fn cells_are_classed_by_what_was_typed_and_the_caret_sits_on_the_next_expected_cell() {
    let state = session("cat dog", "cxt d");
    let display = lay_out(&state, view(20, 10), None);

    assert_eq!(
        pictures(&display),
        [("cat dog ".to_string(), "=x===...".to_string())]
    );
    assert_eq!(caret_of(&display), Some((0, 5)));
}

#[test]
fn an_incorrect_cell_shows_the_expected_character_not_the_typed_one() {
    let state = session("cat", "cZ");
    let display = lay_out(&state, view(20, 10), None);
    assert_eq!(
        pictures(&display),
        [("cat ".to_string(), "=x..".to_string())]
    );
    assert_eq!(caret_of(&display), Some((0, 2)));
}

#[test]
fn the_caret_starts_on_the_first_character_and_moves_to_the_next_word_after_a_space() {
    assert_eq!(
        caret_of(&lay_out(&session("cat dog", ""), view(20, 10), None)),
        Some((0, 0))
    );
    assert_eq!(
        caret_of(&lay_out(&session("cat dog", "cat "), view(20, 10), None)),
        Some((0, 4))
    );
}

#[test]
fn a_fully_typed_word_puts_the_caret_on_its_following_space() {
    let display = lay_out(&session("cat dog", "cat"), view(20, 10), None);
    assert_eq!(classes(&display.lines()[0]), "===.....");
    assert_eq!(caret_of(&display), Some((0, 3)));
}

#[test]
fn the_caret_cell_is_styled_as_untyped_not_marked_in_any_way() {
    let display = lay_out(&session("cat dog", "ca"), view(20, 10), None);
    let line = &display.lines()[0];
    assert_eq!(line[2].class, CellClass::Untyped);
    assert!(!line[2].uncorrected);
}

#[test]
fn extras_render_after_the_word_and_push_the_following_text() {
    let state = session("cat dog", "catxx");
    let display = lay_out(&state, view(20, 10), None);

    assert_eq!(
        pictures(&display),
        [("catxx dog ".to_string(), "===++.....".to_string())]
    );
    assert_eq!(caret_of(&display), Some((0, 5)));
}

#[test]
fn extras_count_toward_wrapping() {
    let state = session("cat dog", "catxx");
    assert_eq!(
        texts(&lay_out(&state, view(8, 10), None)),
        ["catxx ", "dog "]
    );
    assert_eq!(texts(&lay_out(&state, view(10, 10), None)), ["catxx dog "]);
}

#[test]
fn a_re_entered_word_shows_its_extras_and_the_caret_after_them() {
    let state = session("cat dog", "catx ⌫");
    let display = lay_out(&state, view(20, 10), None);
    assert_eq!(classes(&display.lines()[0]), "===+.....");
    assert_eq!(caret_of(&display), Some((0, 4)));
}

#[test]
fn a_word_submitted_with_an_error_is_marked_as_a_whole_but_not_its_space() {
    let state = session("cat dog fox", "cxt dog");
    let display = lay_out(&state, view(20, 10), None);
    let line = &display.lines()[0];
    assert_eq!(classes(line), "=x=====.....");
    assert_eq!(marks(line), "___         ");
}

#[test]
fn a_word_submitted_short_is_marked_and_its_missing_letters_stay_untyped() {
    let state = session("cat dog", "c d");
    let display = lay_out(&state, view(20, 10), None);
    let line = &display.lines()[0];
    assert_eq!(classes(line), "=..==...");
    assert_eq!(marks(line), "___     ");
}

#[test]
fn a_word_submitted_with_extras_is_marked_including_the_extras() {
    let state = session("cat dog", "catx d");
    let display = lay_out(&state, view(20, 10), None);
    assert_eq!(marks(&display.lines()[0]), "____     ");
}

#[test]
fn the_mark_goes_away_when_the_word_is_re_entered_and_comes_back_when_corrected() {
    let mut state = session("cat dog", "cxt ");
    assert_eq!(
        marks(&lay_out(&state, view(20, 10), None).lines()[0]),
        "___     "
    );

    type_script(&mut state, "⌫");
    assert_eq!(
        marks(&lay_out(&state, view(20, 10), None).lines()[0]),
        "        "
    );

    type_script(&mut state, "⌫⌫at ");
    assert_eq!(
        marks(&lay_out(&state, view(20, 10), None).lines()[0]),
        "        "
    );
}

#[test]
fn a_word_that_is_current_is_never_marked_even_with_errors_in_it() {
    let state = session("cat dog", "cx");
    assert_eq!(
        marks(&lay_out(&state, view(20, 10), None).lines()[0]),
        "        "
    );
}

#[test]
fn words_left_wrong_are_marked_once_the_session_is_complete() {
    let state = session("cat dog", "cat dxg ");
    let display = lay_out(&state, view(20, 10), None);
    assert_eq!(display.caret(), None);
    assert_eq!(marks(&display.lines()[0]), "    ___ ");
}

#[test]
fn no_caret_once_the_session_is_complete() {
    let state = session("cat dog", "cat dog");
    let display = lay_out(&state, view(20, 10), None);
    assert_eq!(classes(&display.lines()[0]), "========");
    assert_eq!(display.caret(), None);
}

#[test]
fn only_words_covered_by_the_end_condition_are_shown() {
    let state = SessionState::new(
        Prompt::new(["cat", "dog", "fox"]),
        EndCondition::AfterWords(2),
    );
    assert_eq!(texts(&lay_out(&state, view(20, 10), None)), ["cat dog "]);
}

#[test]
fn scrolls_a_window_that_keeps_one_line_of_context_above_the_caret() {
    let prompt = ["aaaa"; 10].join(" ");
    let mut state = session(&prompt, "");
    let display = lay_out(&state, view(5, 3), None);
    assert_eq!(display.total_lines(), 10);
    assert_eq!(display.lines().len(), 3);
    assert_eq!(display.first_line(), 0);
    assert_eq!(display.caret().map(|c| c.line), Some(0));

    type_script(&mut state, "aaaa ");
    let display = lay_out(&state, view(5, 3), None);
    assert_eq!(display.first_line(), 0);
    assert_eq!(display.caret().map(|c| c.line), Some(1));

    type_script(&mut state, "aaaa aaaa ");
    let display = lay_out(&state, view(5, 3), None);
    assert_eq!(display.first_line(), 2);
    assert_eq!(caret_of(&display), Some((1, 0)));

    type_script(&mut state, "aaaa aaaa aaaa aaaa aaaa aaaa ");
    let display = lay_out(&state, view(5, 3), None);
    assert_eq!(display.first_line(), 7);
    assert_eq!(display.lines().len(), 3);
    assert_eq!(display.caret().map(|c| c.line), Some(2));
}

#[test]
fn a_single_row_window_always_contains_the_caret_line() {
    let prompt = ["aaaa"; 6].join(" ");
    let mut state = session(&prompt, "");
    for line in 0..6 {
        let display = lay_out(&state, view(5, 1), None);
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

    let display = lay_out(&state, view(5, 3), None);
    assert_eq!(display.caret(), None);
    assert_eq!(display.first_line(), 7);
    assert_eq!(display.lines().len(), 3);
}

#[test]
fn the_whole_prompt_is_shown_when_it_fits_the_height() {
    let state = session("the quick brown fox", "");
    let display = lay_out(&state, view(10, 3), None);
    assert_eq!(display.total_lines(), 2);
    assert_eq!(display.lines().len(), 2);
    assert_eq!(display.first_line(), 0);
}

#[test]
fn display_width_of_typed_extras_is_respected() {
    let state = session("cat dog", "cat漢");
    let display = lay_out(&state, view(20, 10), None);
    let line = &display.lines()[0];

    assert_eq!(text(line), "cat漢 dog ");
    assert_eq!(classes(line), "===++.....");
    assert_eq!(width(line), 10);
    assert_eq!(display.caret().map(|c| c.column), Some(5));
    assert_eq!(
        texts(&lay_out(&state, view(9, 10), None)),
        ["cat漢 ", "dog "]
    );
}

#[test]
fn zero_width_extras_render_as_a_visible_placeholder() {
    let state = session("cat dog", "cat\u{301}");
    let display = lay_out(&state, view(20, 10), None);
    let line = &display.lines()[0];
    assert_eq!(text(line), "cat\u{FFFD} dog ");
    assert_eq!(width(line), 9);
    assert_eq!(classes(line), "===+.....");
    assert_eq!(caret_of(&display), Some((0, 4)));
}

#[test]
fn a_word_wider_than_the_viewport_is_split_only_as_a_last_resort() {
    let state = session("abcdefghij ok", "");
    let display = lay_out(&state, view(4, 10), None);
    assert_eq!(texts(&display), ["abcd", "efgh", "ij ", "ok "]);
    assert_fits(&display, 4);
    assert_eq!(caret_of(&display), Some((0, 0)));
}

#[test]
fn degenerate_viewports_are_tolerated() {
    let state = session("cat dog", "ca");
    let display = lay_out(&state, view(0, 0), None);
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

/// The session clock reads this at the first printable keystroke: well
/// after raw-mode entry, so that elapsed time measured from zero would show.
const STARTED: u64 = 5_000_000;
const SECOND: u64 = 1_000_000;

/// Types `script` into a fresh session on `prompt`, one keystroke per
/// millisecond from `STARTED`.
fn typed(prompt: &str, script: &str) -> SessionState {
    let mut state = SessionState::new(
        Prompt::new(prompt.split(' ')),
        EndCondition::AfterWords(usize::MAX),
    );
    for (i, c) in script.chars().enumerate() {
        state.apply_event(Input::new(STARTED + i as u64 * 1_000, Key::Char(c)));
    }
    state
}

/// A prompt of `words` four-letter words, so that typing `n` of them with
/// their spaces gives exactly `5n` final characters.
fn long_prompt(words: usize) -> String {
    vec!["aaaa"; words].join(" ")
}

fn wpm_of(header: Header) -> Option<u32> {
    header.wpm
}

#[test]
fn the_header_is_a_placeholder_before_the_first_printable_keystroke() {
    let placeholder = Header {
        wpm: None,
        elapsed_micros: 0,
        tone: Tone::Placeholder,
    };
    assert_eq!(header(&typed("cat dog", ""), 0, Some(80.0)), placeholder);
    assert_eq!(
        header(&typed("cat dog", ""), 30 * SECOND, None),
        placeholder
    );
    assert_eq!(placeholder.text(), "— wpm   0:00");

    // A paste is recorded but never applied, so it does not start the clock.
    let mut pasted = typed("cat dog", "");
    pasted.apply_event(Input::new(STARTED, Key::Char('c')).in_paste());
    assert_eq!(pasted.started_at_micros(), None);
    assert_eq!(header(&pasted, STARTED + 3 * SECOND, None), placeholder);
}

#[test]
fn no_wpm_under_one_second_and_a_wpm_at_exactly_one_second() {
    // Six final characters: "cat", its space, "do".
    let state = typed("cat dog", "cat do");

    let early = header(&state, STARTED + SECOND - 1, None);
    assert_eq!(early.wpm, None);
    assert_eq!(early.elapsed_micros, SECOND - 1);
    assert_eq!(early.tone, Tone::Neutral);
    assert_eq!(early.text(), "— wpm   0:00");

    let at_one = header(&state, STARTED + SECOND, None);
    assert_eq!(at_one.wpm, Some(72));
    assert_eq!(at_one.elapsed_micros, SECOND);
    assert_eq!(at_one.text(), "72 wpm   0:01");
}

#[test]
fn live_wpm_is_measured_to_now_so_a_pause_pulls_it_down() {
    let state = typed("cat dog", "cat do");
    assert_eq!(wpm_of(header(&state, STARTED + SECOND, None)), Some(72));
    assert_eq!(wpm_of(header(&state, STARTED + 2 * SECOND, None)), Some(36));
    assert_eq!(wpm_of(header(&state, STARTED + 4 * SECOND, None)), Some(18));
}

#[test]
fn extras_and_submitted_spaces_count_as_the_results_count_them() {
    // "catxx" is five typed characters, its space one more, "dog" three.
    let state = typed("cat dog fox", "catxx dog");
    assert_eq!(final_characters(&state), 9);
    assert_eq!(wpm_of(header(&state, STARTED + SECOND, None)), Some(108));
}

#[test]
fn a_paste_moves_the_header_only_as_far_as_it_moves_the_results_count() {
    let mut state = typed("cat dog", "cat ");
    state.apply_event(Input::new(STARTED + SECOND / 2, Key::Char('d')).in_paste());
    state.apply_event(Input::new(STARTED + SECOND / 2, Key::Char('o')).in_paste());
    state.apply_event(Input::new(STARTED + SECOND / 2, Key::Char('g')).in_paste());

    assert_eq!(final_characters(&state), 4);
    assert_eq!(wpm_of(header(&state, STARTED + SECOND, None)), Some(48));
}

#[test]
fn the_tone_is_neutral_without_a_recent_average() {
    let state = typed("cat dog", "cat do");
    assert_eq!(header(&state, STARTED + SECOND, None).tone, Tone::Neutral);
    assert_eq!(
        header(&state, STARTED + 60 * SECOND, None).tone,
        Tone::Neutral
    );
}

#[test]
fn the_tone_compares_the_live_wpm_with_the_recent_average_at_five_percent() {
    // At one minute elapsed, n four-letter words with their spaces are
    // exactly n words per minute, so the boundaries around a recent
    // average of 20 fall on whole words.
    let prompt = long_prompt(30);
    let tone_after = |words: usize| {
        let script = vec!["aaaa "; words].concat();
        header(&typed(&prompt, &script), STARTED + 60 * SECOND, Some(20.0)).tone
    };
    assert_eq!(tone_after(22), Tone::Faster);
    assert_eq!(tone_after(21), Tone::Neutral, "exactly 5% above");
    assert_eq!(tone_after(20), Tone::Neutral);
    assert_eq!(tone_after(19), Tone::Neutral, "exactly 5% below");
    assert_eq!(tone_after(18), Tone::Slower);
}

#[test]
fn the_tone_is_neutral_while_the_wpm_is_absent_even_with_a_recent_average() {
    let state = typed("cat dog", "cat do");
    let early = header(&state, STARTED + SECOND / 2, Some(1.0));
    assert_eq!(early.wpm, None);
    assert_eq!(early.tone, Tone::Neutral);
}

#[test]
fn a_completed_session_freezes_at_its_last_event_whatever_now_is() {
    let mut state = typed("cat dog", "cat do");
    state.apply_event(Input::new(STARTED + 2 * SECOND, Key::Char('g')));
    assert_eq!(state.outcome(), Some(Outcome::Completed));
    // Seven final characters over two seconds.
    assert_eq!(gross_wpm(&state), Some(42.0));

    let frozen = header(&state, STARTED + 90 * SECOND, Some(40.0));
    assert_eq!(frozen.wpm, Some(42));
    assert_eq!(frozen.elapsed_micros, elapsed_micros(&state).unwrap());
    assert_eq!(frozen.text(), "42 wpm   0:02");
    assert_eq!(frozen.tone, Tone::Neutral, "exactly 5% above");
    assert_eq!(header(&state, 0, Some(40.0)), frozen);
    assert_eq!(
        header(&state, STARTED + 90 * SECOND, Some(20.0)).tone,
        Tone::Faster
    );
    assert_eq!(
        header(&state, STARTED + 90 * SECOND, Some(100.0)).tone,
        Tone::Slower
    );
}

#[test]
fn a_completed_header_rounds_its_wpm_as_the_results_line_does() {
    // Seven final characters over eight seconds is exactly 10.5 wpm, which
    // the results line prints as 10.
    let mut state = typed("cat dog", "cat do");
    state.apply_event(Input::new(STARTED + 8 * SECOND, Key::Char('g')));
    let wpm = gross_wpm(&state).unwrap();
    assert_eq!(wpm, 10.5);

    let frozen = header(&state, STARTED + 8 * SECOND, None);
    assert_eq!(frozen.wpm, Some(10));
    assert_eq!(frozen.wpm, Some(format!("{wpm:.0}").parse().unwrap()));
}

#[test]
fn an_interrupted_session_is_muted_and_frozen() {
    let mut state = typed("cat dog", "cat d");
    state.apply_event(Input::new(STARTED + 2 * SECOND, Key::Interrupt));
    assert_eq!(state.outcome(), Some(Outcome::Interrupted));

    // Five final characters over the two seconds to the interrupt.
    let frozen = header(&state, STARTED + 30 * SECOND, Some(1.0));
    assert_eq!(frozen.wpm, Some(30));
    assert_eq!(frozen.elapsed_micros, 2 * SECOND);
    assert_eq!(frozen.tone, Tone::Muted);
    assert_eq!(header(&state, 0, None), frozen);
}

#[test]
fn the_header_text_shows_minutes_and_zero_padded_seconds() {
    let text = |wpm: Option<u32>, elapsed_micros: u64| {
        Header {
            wpm,
            elapsed_micros,
            tone: Tone::Neutral,
        }
        .text()
    };
    assert_eq!(text(None, 0), "— wpm   0:00");
    assert_eq!(text(Some(42), 9_500_000), "42 wpm   0:09");
    assert_eq!(text(Some(7), 65 * SECOND), "7 wpm   1:05");
    assert_eq!(text(Some(100), 720 * SECOND), "100 wpm   12:00");
}

#[test]
fn the_header_style_follows_its_tone_and_the_palette() {
    let style = |tone: Tone, palette: Palette| {
        Header {
            wpm: Some(50),
            elapsed_micros: SECOND,
            tone,
        }
        .style(palette)
    };
    let plain = Style::default();
    let bright_black = Style {
        foreground: Foreground::BrightBlack,
        ..plain
    };
    let dim = Style { dim: true, ..plain };

    for tone in [Tone::Placeholder, Tone::Muted] {
        assert_eq!(style(tone, Palette::Color), bright_black, "{tone:?}");
        assert_eq!(style(tone, Palette::NoColor), dim, "{tone:?}");
    }
    assert_eq!(style(Tone::Neutral, Palette::Color), plain);
    assert_eq!(style(Tone::Neutral, Palette::NoColor), plain);
    assert_eq!(
        style(Tone::Faster, Palette::Color),
        Style {
            foreground: Foreground::Green,
            ..plain
        }
    );
    assert_eq!(
        style(Tone::Slower, Palette::Color),
        Style {
            foreground: Foreground::Red,
            ..plain
        }
    );
    assert_eq!(style(Tone::Faster, Palette::NoColor), plain);
    assert_eq!(style(Tone::Slower, Palette::NoColor), plain);
}

#[test]
fn a_header_takes_one_row_from_the_prompt() {
    let state = session(&long_prompt(10), "");
    let placeholder = header(&state, 0, None);

    let without = lay_out(&state, view(5, 3), None);
    let with = lay_out(&state, view(5, 3), Some(placeholder));

    assert_eq!(without.lines().len(), 3);
    assert_eq!(without.header(), None);
    assert_eq!(with.lines().len(), 2);
    assert_eq!(with.header(), Some(placeholder));
    assert_eq!(with.lines(), &without.lines()[..2]);
    assert_eq!(caret_of(&with), caret_of(&without));
}

#[test]
fn a_header_changes_nothing_else_about_the_layout() {
    let state = session("the quick brown fox jumps", "the quick br");
    let placeholder = header(&state, 0, None);

    let without = lay_out(&state, view(12, 10), None);
    let with = lay_out(&state, view(12, 10), Some(placeholder));

    assert_eq!(with.lines(), without.lines());
    assert_eq!(with.caret(), without.caret());
    assert_eq!(with.first_line(), without.first_line());
    assert_eq!(with.total_lines(), without.total_lines());
    assert_eq!(with.columns(), 12);
}

#[test]
fn a_single_row_viewport_still_shows_the_caret_line_under_a_header() {
    let state = session(&long_prompt(6), "aaaa ");
    let placeholder = header(&state, 0, None);
    let display = lay_out(&state, view(5, 1), Some(placeholder));
    assert_eq!(display.lines().len(), 1);
    assert_eq!(caret_of(&display), Some((0, 0)));
}
