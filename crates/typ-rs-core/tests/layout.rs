use typ_rs_core::layout::{Finger, Hand, Layout};

#[test]
fn qwerty_is_the_only_layout_and_the_default() {
    assert_eq!(Layout::all().len(), 1);
    assert_eq!(Layout::default(), Layout::QWERTY);
    assert_eq!(Layout::QWERTY.name(), "qwerty");
}

#[test]
fn layouts_are_found_by_exact_name() {
    assert_eq!(Layout::by_name("qwerty"), Some(Layout::QWERTY));
    assert_eq!(Layout::by_name("QWERTY"), None);
    assert_eq!(Layout::by_name("dvorak"), None);
    assert_eq!(Layout::by_name(""), None);
}

// --- Geometry -----------------------------------------------------------------

#[test]
fn every_lowercase_letter_and_the_space_have_a_key_and_nothing_else_does() {
    let qwerty = Layout::QWERTY;
    for c in 'a'..='z' {
        assert!(qwerty.key(c).is_some(), "{c:?}");
    }
    assert!(qwerty.key(' ').is_some());
    for c in ['A', '1', ';', '\'', '\n', 'é'] {
        assert_eq!(qwerty.key(c), None, "{c:?}");
    }
}

#[test]
fn the_home_row_keys_sit_under_the_expected_fingers() {
    let qwerty = Layout::QWERTY;
    let expect = |c: char, hand: Hand, finger: Finger| {
        let key = qwerty.key(c).unwrap();
        assert_eq!(
            (key.hand, key.finger, key.row),
            (Some(hand), finger, 1),
            "{c:?}"
        );
    };
    expect('a', Hand::Left, Finger::Pinky);
    expect('s', Hand::Left, Finger::Ring);
    expect('d', Hand::Left, Finger::Middle);
    expect('f', Hand::Left, Finger::Index);
    expect('g', Hand::Left, Finger::Index);
    expect('h', Hand::Right, Finger::Index);
    expect('j', Hand::Right, Finger::Index);
    expect('k', Hand::Right, Finger::Middle);
    expect('l', Hand::Right, Finger::Ring);
}

#[test]
fn rows_run_from_the_top_letter_row_down_to_the_space_bar() {
    let qwerty = Layout::QWERTY;
    assert_eq!(qwerty.key('q').unwrap().row, 0);
    assert_eq!(qwerty.key('a').unwrap().row, 1);
    assert_eq!(qwerty.key('z').unwrap().row, 2);
    let space = qwerty.key(' ').unwrap();
    assert_eq!(space.row, 3);
    assert_eq!((space.hand, space.finger), (None, Finger::Thumb));
}

#[test]
fn the_rows_are_staggered_so_a_column_leans_right_going_down() {
    let qwerty = Layout::QWERTY;
    let x = |c: char| qwerty.key(c).unwrap().x;
    // q, a, z share a column: the top row sits a quarter key left of the
    // home row and the bottom row half a key right of it.
    assert_eq!(x('a'), 0.0);
    assert_eq!(x('q'), -0.25);
    assert_eq!(x('z'), 0.5);
    assert_eq!(x('s') - x('a'), 1.0);
    // The space bar is centered between the hands.
    assert_eq!(x(' '), 4.5);
}

#[test]
fn key_distance_is_euclidean_over_columns_and_rows() {
    let f = Layout::QWERTY.key('f').unwrap();
    let j = Layout::QWERTY.key('j').unwrap();
    let r = Layout::QWERTY.key('r').unwrap();
    assert_eq!(f.distance_to(&j), 3.0);
    assert_eq!(f.distance_to(&f), 0.0);
    // r is one row up and a quarter key left of f.
    assert!((r.distance_to(&f) - (1.0f64 + 0.0625).sqrt()).abs() < 1e-12);
}
