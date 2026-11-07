//! The track list shared by the control thread and the decoder. The decoder takes its next
//! track from it, so tracks inserted before it gets there play in their new place.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::RearrangeRefusal;

pub(crate) type Shared = Arc<Mutex<Queue>>;

/// Locks `queue`. Its state stays consistent even if a holder panicked.
pub(crate) fn lock(queue: &Shared) -> MutexGuard<'_, Queue> {
    queue.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) struct Queue {
    tracks: Vec<PathBuf>,
    /// The index the decoder last asked for (it may be past the end).
    decoding: usize,
    /// Just past the tracks inserted so far, while the decoder has not reached it: the next
    /// insert goes there, so several keep their order.
    insert_end: Option<usize>,
    /// The first index the decoder has not been handed: tracks from here on can still be
    /// rearranged.
    taken: usize,
}

impl Queue {
    pub fn new(tracks: Vec<PathBuf>, start: usize) -> Queue {
        Queue {
            tracks,
            decoding: start,
            insert_end: None,
            taken: start,
        }
    }

    /// The decoder moves to track `index`; `None` past the end.
    pub fn take(&mut self, index: usize) -> Option<PathBuf> {
        self.decoding = index;
        if self.insert_end.is_some_and(|end| index >= end) {
            self.insert_end = None;
        }
        let track = self.tracks.get(index).cloned();
        self.taken = if track.is_some() { index + 1 } else { index };
        track
    }

    /// Adds `tracks` at the end.
    pub fn append(&mut self, tracks: Vec<PathBuf>) {
        self.tracks.extend(tracks);
    }

    /// Puts `tracks` after the decoder's track, or after the tracks inserted before them
    /// while the decoder has not reached those. Returns the index of the first one.
    pub fn insert(&mut self, tracks: Vec<PathBuf>) -> usize {
        let at = self
            .insert_end
            .unwrap_or(0)
            .max(self.decoding + 1)
            .min(self.tracks.len());
        let n = tracks.len();
        self.tracks.splice(at..at, tracks);
        self.insert_end = Some(at + n);
        at
    }

    /// Replaces the tracks from `from` on with `tracks`, if the decoder has not taken any of
    /// them and they still are `expected`. Resets the insert point: the next insert goes right
    /// after the decoder's track. Never returns `Stopped`.
    pub fn rearrange(
        &mut self,
        from: usize,
        expected: &[PathBuf],
        tracks: Vec<PathBuf>,
    ) -> Result<(), RearrangeRefusal> {
        if from < self.taken {
            return Err(RearrangeRefusal::Started);
        }
        if from > self.tracks.len() || self.tracks[from..] != *expected {
            return Err(RearrangeRefusal::Changed);
        }
        self.tracks.splice(from.., tracks);
        self.insert_end = None;
        Ok(())
    }

    pub fn tracks(&self) -> &[PathBuf] {
        &self.tracks
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(|s| p(s)).collect()
    }

    #[test]
    fn insert_before_the_decoder_moves_on_goes_next() {
        let mut q = Queue::new(paths(&["a", "b"]), 0);
        assert_eq!(q.take(0), Some(p("a")));
        assert_eq!(q.insert(paths(&["x"])), 1);
        assert_eq!(q.tracks(), paths(&["a", "x", "b"]));
    }

    #[test]
    fn insert_after_the_decoder_moved_on_goes_after_its_track() {
        let mut q = Queue::new(paths(&["a", "b"]), 0);
        q.take(0);
        q.take(1);
        assert_eq!(q.insert(paths(&["x"])), 2);
        assert_eq!(q.tracks(), paths(&["a", "b", "x"]));
    }

    #[test]
    fn two_inserts_keep_their_order() {
        let mut q = Queue::new(paths(&["a", "b"]), 0);
        q.take(0);
        assert_eq!(q.insert(paths(&["x"])), 1);
        assert_eq!(q.insert(paths(&["y"])), 2);
        assert_eq!(q.tracks(), paths(&["a", "x", "y", "b"]));
    }

    #[test]
    fn insert_point_resets_after_the_inserted_tracks() {
        let mut q = Queue::new(paths(&["a", "b", "c"]), 0);
        q.take(0);
        q.insert(paths(&["x"]));
        assert_eq!(q.take(1), Some(p("x")));
        assert_eq!(q.take(2), Some(p("b")));
        assert_eq!(q.insert(paths(&["y"])), 3);
        assert_eq!(q.tracks(), paths(&["a", "x", "b", "y", "c"]));
    }

    #[test]
    fn insert_after_the_last_track_appends() {
        let mut q = Queue::new(paths(&["a"]), 0);
        q.take(0);
        assert_eq!(q.take(1), None);
        assert_eq!(q.insert(paths(&["x"])), 1);
        assert_eq!(q.tracks(), paths(&["a", "x"]));
    }

    #[test]
    fn append_does_not_move_the_insert_point() {
        let mut q = Queue::new(paths(&["a", "b"]), 0);
        q.take(0);
        q.insert(paths(&["x"]));
        q.append(paths(&["z"]));
        assert_eq!(q.insert(paths(&["y"])), 2);
        assert_eq!(q.tracks(), paths(&["a", "x", "y", "b", "z"]));
        assert_eq!(q.len(), 5);
    }

    #[test]
    fn taken_follows_the_decoder() {
        let mut q = Queue::new(paths(&["a", "b"]), 0);
        assert_eq!(q.taken, 0);
        q.take(0);
        assert_eq!(q.taken, 1);
        q.take(1);
        q.take(2);
        assert_eq!(q.taken, 2); // past the end: None, not taken
    }

    #[test]
    fn rearrange_refuses_a_taken_track() {
        let mut q = Queue::new(paths(&["a", "b", "c"]), 0);
        q.take(0);
        q.take(1);
        assert_eq!(
            q.rearrange(1, &paths(&["b", "c"]), paths(&["c"])),
            Err(RearrangeRefusal::Started)
        );
        assert_eq!(q.tracks(), paths(&["a", "b", "c"]));
    }

    #[test]
    fn rearrange_refuses_a_changed_tail() {
        let mut q = Queue::new(paths(&["a", "b", "c"]), 0);
        q.take(0);
        assert_eq!(
            q.rearrange(1, &paths(&["c", "b"]), paths(&[])),
            Err(RearrangeRefusal::Changed)
        );
        assert_eq!(
            q.rearrange(4, &[], paths(&["x"])),
            Err(RearrangeRefusal::Changed)
        );
    }

    #[test]
    fn rearrange_replaces_the_tail() {
        let mut q = Queue::new(paths(&["a", "b", "c", "d"]), 0);
        q.take(0);
        q.rearrange(1, &paths(&["b", "c", "d"]), paths(&["d", "b"]))
            .unwrap();
        assert_eq!(q.tracks(), paths(&["a", "d", "b"]));
        q.rearrange(1, &paths(&["d", "b"]), paths(&["d", "b", "x", "y"]))
            .unwrap();
        assert_eq!(q.len(), 5);
        q.rearrange(1, &paths(&["d", "b", "x", "y"]), vec![])
            .unwrap();
        assert_eq!(q.tracks(), paths(&["a"]));
    }

    #[test]
    fn rearrange_resets_the_insert_point() {
        let mut q = Queue::new(paths(&["a", "b", "c"]), 0);
        q.take(0);
        assert_eq!(q.insert(paths(&["x"])), 1); // insert_end = 2
        q.rearrange(2, &paths(&["b", "c"]), paths(&["c", "b"]))
            .unwrap();
        assert_eq!(
            q.insert(paths(&["y"])),
            1,
            "right after the decoder's track"
        );
    }
}
