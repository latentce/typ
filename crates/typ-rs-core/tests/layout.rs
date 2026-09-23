use typ_rs_core::layout::Layout;

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
