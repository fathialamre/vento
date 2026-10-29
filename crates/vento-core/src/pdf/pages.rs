//! The ordered pages of one PDF window, with in-window undo and redo.

/// One page of the window's list: which opened source it comes from, which
/// page of that source, and any extra rotation applied in the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRef {
    /// Index of the opened source document.
    pub source: usize,
    /// Zero-based page within that source.
    pub page: usize,
    /// Extra quarter turns clockwise, 0..=3.
    pub turns: u8,
}

/// The pages a window will write, in order. Every edit keeps a whole
/// snapshot for undo, which is cheap at PDF sizes.
pub struct PageList {
    pages: Vec<PageRef>,
    /// The list as last saved (or as opened), for [`PageList::is_edited`].
    saved: Vec<PageRef>,
    undo: Vec<Vec<PageRef>>,
    redo: Vec<Vec<PageRef>>,
}

impl PageList {
    /// A list holding `pages`, counted as unedited.
    pub fn new(pages: Vec<PageRef>) -> Self {
        Self {
            saved: pages.clone(),
            pages,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// The pages in their current order.
    pub fn pages(&self) -> &[PageRef] {
        &self.pages
    }

    /// How many pages the list holds.
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Whether the list holds no pages.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Whether the list differs from how it was opened or last saved, so
    /// undoing back to that point counts as unedited.
    pub fn is_edited(&self) -> bool {
        self.pages != self.saved
    }

    /// Takes the current list as the saved one.
    pub fn mark_saved(&mut self) {
        self.saved = self.pages.clone();
    }

    /// Takes `written`, the pages a save wrote, as the saved list. Edits
    /// made while the save ran then still count as unsaved.
    pub fn mark_saved_as(&mut self, written: &[PageRef]) {
        self.saved = written.to_vec();
    }

    /// Removes the pages at `at`; the rest close up. Out-of-range
    /// indices are ignored.
    pub fn delete(&mut self, at: &[usize]) {
        let at = self.normalise(at);
        if at.is_empty() {
            return;
        }
        let kept = self
            .pages
            .iter()
            .enumerate()
            .filter(|(i, _)| at.binary_search(i).is_err())
            .map(|(_, p)| *p)
            .collect();
        self.commit(kept);
    }

    /// Moves the pages at `at` in front of what is at `before` (a position
    /// in the list as it is now, `0..=len`). They keep their relative
    /// order. Returns their new positions; moving them to where they
    /// already are is not an edit.
    pub fn move_to(&mut self, at: &[usize], before: usize) -> Vec<usize> {
        let at = self.normalise(at);
        if at.is_empty() {
            return Vec::new();
        }
        let before = before.min(self.pages.len());
        let moved: Vec<PageRef> = at.iter().map(|&i| self.pages[i]).collect();
        let mut rest: Vec<PageRef> = self
            .pages
            .iter()
            .enumerate()
            .filter(|(i, _)| at.binary_search(i).is_err())
            .map(|(_, p)| *p)
            .collect();
        // Where `before` lands once the moved pages are taken out.
        let start = before - at.iter().filter(|&&i| i < before).count();
        let count = moved.len();
        rest.splice(start..start, moved);
        self.commit(rest);
        (start..start + count).collect()
    }

    /// Turns the pages at `at` a quarter turn, clockwise or not.
    pub fn rotate(&mut self, at: &[usize], clockwise: bool) {
        let at = self.normalise(at);
        if at.is_empty() {
            return;
        }
        let mut pages = self.pages.clone();
        for &i in &at {
            let turns = pages[i].turns;
            pages[i].turns = if clockwise {
                (turns + 1) % 4
            } else {
                (turns + 3) % 4
            };
        }
        self.commit(pages);
    }

    /// Inserts `pages` in front of what is at `before` (clamped to the end).
    pub fn insert(&mut self, pages: Vec<PageRef>, before: usize) {
        if pages.is_empty() {
            return;
        }
        let before = before.min(self.pages.len());
        let mut next = self.pages.clone();
        next.splice(before..before, pages);
        self.commit(next);
    }

    /// Steps back one edit. Returns whether there was one to undo.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo.push(std::mem::replace(&mut self.pages, previous));
        true
    }

    /// Re-applies the last undone edit. Returns whether there was one.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.pages, next));
        true
    }

    /// `at` sorted, without duplicates, and without out-of-range indices.
    fn normalise(&self, at: &[usize]) -> Vec<usize> {
        let mut at: Vec<usize> = at
            .iter()
            .copied()
            .filter(|&i| i < self.pages.len())
            .collect();
        at.sort_unstable();
        at.dedup();
        at
    }

    /// Makes `pages` the current list as one undo step; a list equal to
    /// the current one is not an edit.
    fn commit(&mut self, pages: Vec<PageRef>) {
        if pages == self.pages {
            return;
        }
        self.undo.push(std::mem::replace(&mut self.pages, pages));
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(n: usize) -> PageList {
        PageList::new(
            (0..n)
                .map(|p| PageRef {
                    source: 0,
                    page: p,
                    turns: 0,
                })
                .collect(),
        )
    }
    fn order(l: &PageList) -> Vec<usize> {
        l.pages().iter().map(|p| p.page).collect()
    }

    #[test]
    fn delete_closes_up() {
        let mut l = list(8);
        l.delete(&[1, 2, 6]);
        assert_eq!(order(&l), [0, 3, 4, 5, 7]);
        assert!(l.is_edited());
    }
    #[test]
    fn move_single_before() {
        let mut l = list(8);
        assert_eq!(l.move_to(&[5], 1), [1]);
        assert_eq!(order(&l), [0, 5, 1, 2, 3, 4, 6, 7]);
    }
    #[test]
    fn move_selection_keeps_relative_order() {
        let mut l = list(6);
        assert_eq!(l.move_to(&[0, 3], 6), [4, 5]);
        assert_eq!(order(&l), [1, 2, 4, 5, 0, 3]);
    }
    #[test]
    fn move_onto_itself_is_no_edit() {
        let mut l = list(4);
        l.move_to(&[2], 2);
        assert!(!l.is_edited());
    }
    #[test]
    fn rotate_wraps() {
        let mut l = list(2);
        for _ in 0..5 {
            l.rotate(&[0], true);
        }
        assert_eq!(l.pages()[0].turns, 1);
        l.rotate(&[0], false);
        l.rotate(&[0], false);
        assert_eq!(l.pages()[0].turns, 3);
    }
    #[test]
    fn undo_redo_and_edited_flag() {
        let mut l = list(3);
        l.delete(&[0]);
        assert!(l.undo());
        assert!(!l.is_edited());
        assert!(l.redo());
        assert_eq!(order(&l), [1, 2]);
        l.mark_saved();
        assert!(!l.is_edited());
        assert!(l.undo());
        assert!(l.is_edited());
    }
    #[test]
    fn edit_clears_redo() {
        let mut l = list(3);
        l.delete(&[0]);
        l.undo();
        l.rotate(&[0], true);
        assert!(!l.redo());
    }
    #[test]
    fn insert_at_position() {
        let mut l = list(2);
        l.insert(
            vec![PageRef {
                source: 1,
                page: 0,
                turns: 0,
            }],
            1,
        );
        assert_eq!(l.pages()[1].source, 1);
    }

    #[test]
    fn empty_or_out_of_range_edits_push_no_undo() {
        let mut l = list(3);
        l.delete(&[]);
        l.rotate(&[], true);
        assert!(l.move_to(&[], 1).is_empty());
        l.delete(&[9]);
        assert!(!l.undo());
        assert!(!l.is_edited());
    }

    #[test]
    fn marking_a_written_snapshot_keeps_later_edits_edited() {
        let mut l = list(3);
        l.delete(&[0]);
        let written = l.pages().to_vec();
        l.insert(
            vec![PageRef {
                source: 1,
                page: 0,
                turns: 0,
            }],
            2,
        );
        l.mark_saved_as(&written);
        assert!(l.is_edited(), "pages added after the write are unsaved");
        assert!(l.undo());
        assert!(
            !l.is_edited(),
            "undoing the add is back at what was written"
        );
    }

    #[test]
    fn marking_the_current_list_as_saved_is_unedited() {
        let mut l = list(3);
        l.rotate(&[1], true);
        let written = l.pages().to_vec();
        l.mark_saved_as(&written);
        assert!(!l.is_edited());
    }

    #[test]
    fn move_down_lands_before_target() {
        let mut l = list(6);
        assert_eq!(l.move_to(&[1], 4), [3]);
        assert_eq!(order(&l), [0, 2, 3, 1, 4, 5]);
    }
}
