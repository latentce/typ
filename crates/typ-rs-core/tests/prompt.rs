use typ_rs_core::prompt::{Prompt, Slot};

#[test]
fn a_slot_names_a_prompt_position_and_its_pattern_is_the_surrounding_text() {
    let prompt = Prompt::new(["the", "cat"]);
    let slot = |word, position| Slot { word, position };
    assert_eq!(prompt.expected_at(slot(0, 0)), 't');
    assert_eq!(prompt.expected_at(slot(0, 3)), ' ');
    assert_eq!(prompt.expected_at(slot(1, 2)), 't');

    // The prompt reads as space-padded text: the first word's initial
    // character follows a space, and the last word ends in one.
    assert_eq!(prompt.pattern_ending_at(slot(0, 0)), " t");
    assert_eq!(prompt.pattern_ending_at(slot(0, 1)), " th");
    assert_eq!(prompt.pattern_ending_at(slot(0, 2)), "the");
    assert_eq!(prompt.pattern_ending_at(slot(0, 3)), "he ");
    assert_eq!(prompt.pattern_ending_at(slot(1, 0)), "e c");
    assert_eq!(prompt.pattern_ending_at(slot(1, 3)), "at ");
}

#[test]
fn slots_run_through_every_character_and_separating_space_but_no_trailing_space() {
    let prompt = Prompt::new(["ab", "c"]);
    let slots: Vec<(usize, usize)> = prompt.slots().map(|s| (s.word, s.position)).collect();
    assert_eq!(slots, vec![(0, 0), (0, 1), (0, 2), (1, 0)]);
    let text: String = prompt.slots().map(|s| prompt.expected_at(s)).collect();
    assert_eq!(text, prompt.text());
    assert_eq!(Prompt::new(["a"]).slots().count(), 1);
}
