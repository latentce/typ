//! Layouts: the named physical key arrangements a profile can be bound to.
//!
//! A layout is a table giving, for every key it knows, which hand and finger
//! strike it and where it sits: its row and its horizontal position in key
//! widths. The context model derives its layout features (same finger, same
//! hand, row change, key distance) from this table and nothing else, so
//! adding a layout means adding a table here.

/// A known layout. Two layouts are the same layout when they have the same
/// name.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    name: &'static str,
    keys: &'static [(char, Key)],
}

impl PartialEq for Layout {
    fn eq(&self, other: &Layout) -> bool {
        self.name == other.name
    }
}

impl Eq for Layout {}

/// Which hand strikes a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hand {
    Left,
    Right,
}

/// Which finger of a hand strikes a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finger {
    Pinky,
    Ring,
    Middle,
    Index,
    Thumb,
}

/// Where a key sits and what strikes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key {
    /// `None` for the space bar, struck by either thumb.
    pub hand: Option<Hand>,
    /// The finger that strikes the key in touch typing.
    pub finger: Finger,
    /// 0 is the top letter row, 1 the home row, 2 the bottom letter row,
    /// 3 the space bar.
    pub row: u8,
    /// Horizontal position in key widths, with the home row's leftmost
    /// letter at 0; the other rows carry the usual stagger.
    pub x: f64,
}

impl Key {
    /// Straight-line distance between two keys, in key widths, with rows
    /// one key width apart.
    pub fn distance_to(&self, other: &Key) -> f64 {
        let dx = self.x - other.x;
        let dy = f64::from(self.row) - f64::from(other.row);
        (dx * dx + dy * dy).sqrt()
    }
}

const fn key(hand: Hand, finger: Finger, row: u8, x: f64) -> Key {
    Key {
        hand: Some(hand),
        finger,
        row,
        x,
    }
}

/// The eight fingers over the ten letter columns, left to right; the index
/// fingers each cover two columns.
const COLUMN_FINGERS: [(Hand, Finger); 10] = [
    (Hand::Left, Finger::Pinky),
    (Hand::Left, Finger::Ring),
    (Hand::Left, Finger::Middle),
    (Hand::Left, Finger::Index),
    (Hand::Left, Finger::Index),
    (Hand::Right, Finger::Index),
    (Hand::Right, Finger::Index),
    (Hand::Right, Finger::Middle),
    (Hand::Right, Finger::Ring),
    (Hand::Right, Finger::Pinky),
];

/// A letter key by its row and column, with the standard row stagger: the
/// top row a quarter key left of the home row, the bottom row half a key
/// right of it.
const fn letter(row: u8, column: usize) -> Key {
    let (hand, finger) = COLUMN_FINGERS[column];
    let stagger = match row {
        0 => -0.25,
        1 => 0.0,
        _ => 0.5,
    };
    key(hand, finger, row, column as f64 + stagger)
}

const SPACE: Key = Key {
    hand: None,
    finger: Finger::Thumb,
    row: 3,
    x: 4.5,
};

const QWERTY_KEYS: &[(char, Key)] = &[
    ('q', letter(0, 0)),
    ('w', letter(0, 1)),
    ('e', letter(0, 2)),
    ('r', letter(0, 3)),
    ('t', letter(0, 4)),
    ('y', letter(0, 5)),
    ('u', letter(0, 6)),
    ('i', letter(0, 7)),
    ('o', letter(0, 8)),
    ('p', letter(0, 9)),
    ('a', letter(1, 0)),
    ('s', letter(1, 1)),
    ('d', letter(1, 2)),
    ('f', letter(1, 3)),
    ('g', letter(1, 4)),
    ('h', letter(1, 5)),
    ('j', letter(1, 6)),
    ('k', letter(1, 7)),
    ('l', letter(1, 8)),
    ('z', letter(2, 0)),
    ('x', letter(2, 1)),
    ('c', letter(2, 2)),
    ('v', letter(2, 3)),
    ('b', letter(2, 4)),
    ('n', letter(2, 5)),
    ('m', letter(2, 6)),
    (' ', SPACE),
];

const LAYOUTS: &[Layout] = &[Layout {
    name: "qwerty",
    keys: QWERTY_KEYS,
}];

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

    /// The key that types a character; `None` for a character the layout
    /// has no key for.
    pub fn key(self, c: char) -> Option<Key> {
        self.keys.iter().find(|(k, _)| *k == c).map(|(_, key)| *key)
    }
}

impl Default for Layout {
    fn default() -> Layout {
        Layout::QWERTY
    }
}
