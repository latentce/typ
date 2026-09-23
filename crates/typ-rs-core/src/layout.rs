//! Layouts: the named physical key arrangements a profile can be bound to.
//!
//! Only the names exist so far. The key geometry each name stands for
//! (finger, hand, row, position per key), from which context features are
//! derived, is added alongside the context model; adding a layout then means
//! adding a table here and nothing else.

/// A known layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    name: &'static str,
}

const LAYOUTS: &[Layout] = &[Layout { name: "qwerty" }];

impl Layout {
    /// The layout a profile has unless another is chosen.
    pub const QWERTY: Layout = LAYOUTS[0];

    /// Every layout `typ` knows, in the order they are listed to the user.
    pub fn all() -> &'static [Layout] {
        LAYOUTS
    }

    /// The layout with the given name, if there is one.
    pub fn by_name(name: &str) -> Option<Layout> {
        LAYOUTS.iter().copied().find(|l| l.name == name)
    }

    /// The name the user sets and sees, and the form a profile stores.
    pub fn name(self) -> &'static str {
        self.name
    }
}

impl Default for Layout {
    fn default() -> Layout {
        Layout::QWERTY
    }
}
