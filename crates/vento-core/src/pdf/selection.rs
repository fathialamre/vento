//! Which pages of a window are selected, with the click, Shift and
//! Command rules of a file browser.

use std::collections::BTreeSet;

/// A set of selected page positions and the anchor Shift extends from.
#[derive(Default)]
pub struct Selection {
    selected: BTreeSet<usize>,
    anchor: Option<usize>,
}

impl Selection {
    /// Selects only `i` and makes it the anchor.
    pub fn click(&mut self, i: usize) {
        self.selected.clear();
        self.selected.insert(i);
        self.anchor = Some(i);
    }

    /// Selects the range from the anchor to `i`, either way round,
    /// replacing the previous range. Without an anchor it acts as a click.
    pub fn extend_to(&mut self, i: usize) {
        let anchor = *self.anchor.get_or_insert(i);
        self.selected = (anchor.min(i)..=anchor.max(i)).collect();
    }

    /// Flips `i` and makes it the anchor.
    pub fn toggle(&mut self, i: usize) {
        if !self.selected.remove(&i) {
            self.selected.insert(i);
        }
        self.anchor = Some(i);
    }

    /// Selects every position of a list `len` long.
    pub fn select_all(&mut self, len: usize) {
        self.selected = (0..len).collect();
        self.anchor = if len > 0 { Some(0) } else { None };
    }

    /// Selects nothing.
    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
    }

    /// Replaces the selection with `at`; the anchor becomes its first page.
    pub fn set(&mut self, at: Vec<usize>) {
        self.selected = at.into_iter().collect();
        self.anchor = self.selected.first().copied();
    }

    /// The selected positions, sorted.
    pub fn indices(&self) -> Vec<usize> {
        self.selected.iter().copied().collect()
    }

    /// Whether nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// Whether position `i` is selected.
    pub fn contains(&self, i: usize) -> bool {
        self.selected.contains(&i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_extends_from_anchor() {
        let mut s = Selection::default();
        s.click(2);
        s.extend_to(5);
        assert_eq!(s.indices(), [2, 3, 4, 5]);
        s.extend_to(0);
        assert_eq!(s.indices(), [0, 1, 2]);
    }
    #[test]
    fn cmd_toggles() {
        let mut s = Selection::default();
        s.click(1);
        s.toggle(4);
        s.toggle(1);
        assert_eq!(s.indices(), [4]);
    }

    #[test]
    fn select_all_clear_and_set() {
        let mut s = Selection::default();
        s.select_all(3);
        assert_eq!(s.indices(), [0, 1, 2]);
        assert!(s.contains(2) && !s.contains(3));
        s.clear();
        assert!(s.is_empty());
        s.set(vec![4, 2, 4]);
        assert_eq!(s.indices(), [2, 4]);
    }
}
