//! Aligns a word's first attempt to its target and says which slot each
//! error belongs to.
//!
//! The alignment is Damerau-Levenshtein with adjacent transpositions
//! (optimal string alignment). Where several alignments need the fewest
//! operations, the one whose errors fall latest in the word wins, comparing
//! the error slots from the last backward; alignments still tied share
//! their errors equally.

use std::collections::HashMap;

use super::Edit;

/// The errors in `typed` against `target`, each with its share of the
/// blame: one, or a fraction when tied alignments disagree. Ordered by slot.
pub(super) fn align(typed: &[char], target: &[char]) -> Vec<(Edit, f64)> {
    let table = Table::new(typed, target);
    let mut memo = HashMap::new();
    let best = table.best(typed.len(), target.len(), &mut memo);

    let share = 1.0 / best.alignments.len() as f64;
    let mut merged: Vec<(Edit, f64)> = Vec::new();
    for alignment in &best.alignments {
        for &edit in alignment {
            match merged.iter_mut().find(|(e, _)| *e == edit) {
                Some((_, weight)) => *weight += share,
                None => merged.push((edit, share)),
            }
        }
    }
    merged.sort_by_key(|(edit, _)| edit.slot());
    merged
}

/// The optimal alignments from one cell back to the origin, together with
/// the key they were chosen by: their error slots, latest first.
#[derive(Clone)]
struct Best {
    key: Vec<usize>,
    /// Each alignment's edits in word order.
    alignments: Vec<Vec<Edit>>,
}

struct Table<'a> {
    typed: &'a [char],
    target: &'a [char],
    /// `distance[i][j]`: operations to turn the first `i` typed characters
    /// into the first `j` target characters.
    distance: Vec<Vec<usize>>,
}

impl<'a> Table<'a> {
    fn new(typed: &'a [char], target: &'a [char]) -> Table<'a> {
        let (m, n) = (typed.len(), target.len());
        let mut d = vec![vec![0usize; n + 1]; m + 1];
        for (i, row) in d.iter_mut().enumerate() {
            row[0] = i;
        }
        d[0] = (0..=n).collect();
        for i in 1..=m {
            for j in 1..=n {
                let substitution = usize::from(typed[i - 1] != target[j - 1]);
                let mut best = (d[i - 1][j - 1] + substitution)
                    .min(d[i - 1][j] + 1)
                    .min(d[i][j - 1] + 1);
                if transposable(typed, target, i, j) {
                    best = best.min(d[i - 2][j - 2] + 1);
                }
                d[i][j] = best;
            }
        }
        Table {
            typed,
            target,
            distance: d,
        }
    }

    fn best(&self, i: usize, j: usize, memo: &mut HashMap<(usize, usize), Best>) -> Best {
        if let Some(found) = memo.get(&(i, j)) {
            return found.clone();
        }
        let result = if i == 0 && j == 0 {
            Best {
                key: Vec::new(),
                alignments: vec![Vec::new()],
            }
        } else {
            self.best_of_moves(i, j, memo)
        };
        memo.insert((i, j), result.clone());
        result
    }

    fn best_of_moves(&self, i: usize, j: usize, memo: &mut HashMap<(usize, usize), Best>) -> Best {
        let mut best: Option<Best> = None;
        for (edit, from) in self.optimal_moves(i, j) {
            let tail = self.best(from.0, from.1, memo);
            let mut key = Vec::with_capacity(tail.key.len() + 1);
            key.extend(edit.map(Edit::slot));
            key.extend(&tail.key);
            let alignments = tail.alignments.into_iter().map(|mut alignment| {
                alignment.extend(edit);
                alignment
            });
            match &mut best {
                Some(current) if current.key == key => current.alignments.extend(alignments),
                Some(current) if current.key > key => {}
                _ => {
                    best = Some(Best {
                        key,
                        alignments: alignments.collect(),
                    })
                }
            }
        }
        best.expect("every cell but the origin has a predecessor")
    }

    /// The moves out of a cell that lie on some optimal alignment: the edit
    /// they record (none for a match) and the cell they come from.
    fn optimal_moves(&self, i: usize, j: usize) -> Vec<(Option<Edit>, (usize, usize))> {
        let d = &self.distance;
        let here = d[i][j];
        let mut moves = Vec::with_capacity(4);
        if i > 0
            && j > 0
            && d[i - 1][j - 1] + usize::from(self.typed[i - 1] != self.target[j - 1]) == here
        {
            let edit = (self.typed[i - 1] != self.target[j - 1]).then(|| Edit::Substitution {
                slot: j - 1,
                actual: self.typed[i - 1],
            });
            moves.push((edit, (i - 1, j - 1)));
        }
        if j > 0 && d[i][j - 1] + 1 == here {
            moves.push((Some(Edit::Omission { slot: j - 1 }), (i, j - 1)));
        }
        if i > 0 && d[i - 1][j] + 1 == here {
            let edit = Edit::Insertion {
                slot: j,
                actual: self.typed[i - 1],
            };
            moves.push((Some(edit), (i - 1, j)));
        }
        if transposable(self.typed, self.target, i, j) && d[i - 2][j - 2] + 1 == here {
            moves.push((Some(Edit::Transposition { slot: j - 1 }), (i - 2, j - 2)));
        }
        moves
    }
}

/// Whether the two typed characters before `i` are the two target
/// characters before `j` in the other order.
fn transposable(typed: &[char], target: &[char], i: usize, j: usize) -> bool {
    i >= 2
        && j >= 2
        && typed[i - 1] == target[j - 2]
        && typed[i - 2] == target[j - 1]
        && typed[i - 1] != typed[i - 2]
}
