//! Multi-row selection shared by every track list (album, playlist, search, queue).

use crate::backend::AlbumId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListId {
    Album(AlbumId),
    Playlist(String),
    SearchTracks(String),
    Queue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    Plain,
    Toggle,
    Range,
}

impl Click {
    /// Shift wins over Ctrl/Cmd.
    pub fn from_modifiers(command: bool, shift: bool) -> Click {
        if shift {
            Click::Range
        } else if command {
            Click::Toggle
        } else {
            Click::Plain
        }
    }
}

#[derive(Default)]
pub struct Selection {
    list: Option<ListId>,
    /// Sorted, unique row ids.
    rows: Vec<usize>,
    anchor: Option<usize>,
}

impl Selection {
    /// Start fresh when `list` is not the list currently held.
    fn enter(&mut self, list: &ListId) {
        if self.list.as_ref() != Some(list) {
            self.list = Some(list.clone());
            self.rows.clear();
            self.anchor = None;
        }
    }

    fn only(&mut self, row: usize) {
        self.rows = vec![row];
        self.anchor = Some(row);
    }

    /// `shown`: the list's row ids in display order (file indices for a playlist).
    pub fn click(&mut self, list: &ListId, shown: &[usize], row: usize, how: Click) {
        self.enter(list);
        match how {
            Click::Plain => self.only(row),
            Click::Toggle => {
                match self.rows.binary_search(&row) {
                    Ok(i) => {
                        self.rows.remove(i);
                    }
                    Err(i) => self.rows.insert(i, row),
                }
                self.anchor = Some(row);
            }
            Click::Range => {
                let pos = |id: usize| shown.iter().position(|&s| s == id);
                match (self.anchor.and_then(pos), pos(row)) {
                    (Some(a), Some(b)) => {
                        let (lo, hi) = (a.min(b), a.max(b));
                        let mut rows = shown[lo..=hi].to_vec();
                        rows.sort_unstable();
                        rows.dedup();
                        self.rows = rows;
                    }
                    _ => self.only(row),
                }
            }
        }
    }

    /// Right-click: keeps the selection if `row` is in it, else selects only `row`.
    pub fn secondary(&mut self, list: &ListId, row: usize) {
        self.enter(list);
        if self.rows.binary_search(&row).is_err() {
            self.only(row);
        }
    }

    pub fn select_all(&mut self, list: &ListId, shown: &[usize]) {
        self.enter(list);
        self.rows = shown.to_vec();
        self.rows.sort_unstable();
        self.rows.dedup();
        self.anchor = shown.first().copied();
    }

    pub fn set(&mut self, list: &ListId, rows: Vec<usize>) {
        self.enter(list);
        let mut rows = rows;
        rows.sort_unstable();
        rows.dedup();
        self.anchor = rows.first().copied();
        self.rows = rows;
    }

    /// Selected rows of `list` in list order, dropping any not in `shown`; empty for another list.
    pub fn rows(&self, list: &ListId, shown: &[usize]) -> Vec<usize> {
        if self.list.as_ref() != Some(list) {
            return Vec::new();
        }
        shown
            .iter()
            .copied()
            .filter(|r| self.rows.binary_search(r).is_ok())
            .collect()
    }

    pub fn is_selected(&self, list: &ListId, row: usize) -> bool {
        self.list.as_ref() == Some(list) && self.rows.binary_search(&row).is_ok()
    }

    /// What a right-click on `row` acts on: the selection if `row` is in it, else `[row]`.
    pub fn target(&self, list: &ListId, shown: &[usize], row: usize) -> Vec<usize> {
        if self.is_selected(list, row) {
            self.rows(list, shown)
        } else {
            vec![row]
        }
    }

    /// The list the selection belongs to, if any.
    pub fn list(&self) -> Option<&ListId> {
        self.list.as_ref()
    }

    /// No row of any list is selected.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn clear(&mut self) {
        *self = Selection::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: ListId = ListId::Queue;

    fn shown() -> Vec<usize> {
        (0..6).collect()
    }

    #[test]
    fn plain_click_selects_only_that_row() {
        let mut s = Selection::default();
        s.click(&L, &shown(), 2, Click::Plain);
        s.click(&L, &shown(), 4, Click::Plain);
        assert_eq!(s.rows(&L, &shown()), vec![4]);
    }

    #[test]
    fn ctrl_click_toggles() {
        let mut s = Selection::default();
        s.click(&L, &shown(), 1, Click::Plain);
        s.click(&L, &shown(), 3, Click::Toggle);
        assert_eq!(s.rows(&L, &shown()), vec![1, 3]);
        s.click(&L, &shown(), 1, Click::Toggle);
        assert_eq!(s.rows(&L, &shown()), vec![3]);
    }

    #[test]
    fn shift_click_selects_from_the_anchor() {
        let mut s = Selection::default();
        s.click(&L, &shown(), 2, Click::Plain);
        s.click(&L, &shown(), 4, Click::Range);
        assert_eq!(s.rows(&L, &shown()), vec![2, 3, 4]);
        s.click(&L, &shown(), 0, Click::Range);
        assert_eq!(s.rows(&L, &shown()), vec![0, 1, 2]);
    }

    #[test]
    fn shift_click_without_anchor_acts_plain() {
        let mut s = Selection::default();
        s.click(&L, &shown(), 3, Click::Range);
        assert_eq!(s.rows(&L, &shown()), vec![3]);
    }

    #[test]
    fn shift_range_skips_ids_not_shown() {
        let sh = [0, 2, 3];
        let mut s = Selection::default();
        s.click(&L, &sh, 0, Click::Plain);
        s.click(&L, &sh, 3, Click::Range);
        assert_eq!(s.rows(&L, &sh), vec![0, 2, 3]);
    }

    #[test]
    fn ctrl_a_selects_all() {
        let mut s = Selection::default();
        s.select_all(&L, &shown());
        assert_eq!(s.rows(&L, &shown()), shown());
    }

    #[test]
    fn right_click_on_a_selected_row_keeps_the_selection() {
        let mut s = Selection::default();
        s.click(&L, &shown(), 1, Click::Plain);
        s.click(&L, &shown(), 3, Click::Toggle);
        s.secondary(&L, 3);
        assert_eq!(s.target(&L, &shown(), 3), vec![1, 3]);
    }

    #[test]
    fn right_click_on_an_unselected_row_selects_it() {
        let mut s = Selection::default();
        s.click(&L, &shown(), 1, Click::Plain);
        assert_eq!(s.target(&L, &shown(), 4), vec![4]);
        s.secondary(&L, 4);
        assert_eq!(s.rows(&L, &shown()), vec![4]);
    }

    #[test]
    fn click_on_another_list_clears() {
        let other = ListId::Playlist("a".into());
        let mut s = Selection::default();
        s.click(&L, &shown(), 1, Click::Plain);
        s.click(&other, &shown(), 3, Click::Toggle);
        assert_eq!(s.rows(&other, &shown()), vec![3]);
        assert!(s.rows(&L, &shown()).is_empty());
    }

    #[test]
    fn rows_past_the_end_are_dropped() {
        let mut s = Selection::default();
        s.set(&L, vec![1, 4]);
        assert_eq!(s.rows(&L, &[0, 1, 2]), vec![1]);
    }
}
