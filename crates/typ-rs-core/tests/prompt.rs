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
