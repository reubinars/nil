//! Queue edits as pure functions: each computes the new tail of the queue (everything
//! after the current track). All indices are queue indices.

use std::collections::HashSet;

use crate::backend::QueueItem;
use crate::catalog::path_key;

/// What an edit was, for its notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Remove,
    Move,
    PlayNext,
    Clear { retried: bool },
    Undo,
}

/// The upcoming part: `from = current + 1`, the rows from there on as shown.
pub struct Tail<'a> {
    pub from: usize,
    pub rows: &'a [QueueItem],
}

/// The upcoming part of `queue`; None if nothing is current.
pub fn tail(queue: &[QueueItem], current: Option<usize>) -> Option<Tail<'_>> {
    let from = current? + 1;
    Some(Tail {
        from,
        rows: queue.get(from..).unwrap_or(&[]),
    })
}

/// Positions within `tail.rows` of the listed upcoming rows, in the order given, once each.
fn upcoming(tail: &Tail, rows: &[usize]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    let mut seen = HashSet::new();
    for &r in rows {
        if r >= tail.from && r - tail.from < tail.rows.len() && seen.insert(r - tail.from) {
            out.push(r - tail.from);
        }
    }
    out
}

fn same(a: &[QueueItem], b: &[QueueItem]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| path_key(&x.path) == path_key(&y.path))
}

/// `rows` without the positions in `gone`.
fn without<T: Clone>(rows: &[T], gone: &[usize]) -> Vec<T> {
    let gone: HashSet<usize> = gone.iter().copied().collect();
    rows.iter()
        .enumerate()
        .filter(|(i, _)| !gone.contains(i))
        .map(|(_, it)| it.clone())
        .collect()
}

/// `rows` with the positions in `picked`, in that order, in front of position `gap`.
fn reordered<T: Clone>(rows: &[T], picked: &[usize], gap: usize) -> Vec<T> {
    let mut out: Vec<T> = Vec::with_capacity(rows.len());
    let set: HashSet<usize> = picked.iter().copied().collect();
    let rest = |range: std::ops::Range<usize>, out: &mut Vec<T>| {
        for i in range {
            if !set.contains(&i) {
                out.push(rows[i].clone());
            }
        }
    };
    rest(0..gap, &mut out);
    out.extend(picked.iter().map(|&i| rows[i].clone()));
    rest(gap..rows.len(), &mut out);
    out
}

/// Position `i` of the tail, for working out where each row of an edit lands.
fn positions(tail: &Tail) -> Vec<usize> {
    (0..tail.rows.len()).collect()
}

/// `rows` minus the upcoming ones listed (others ignored); None if none of them is upcoming.
pub fn remove(tail: &Tail, rows: &[usize]) -> Option<Vec<QueueItem>> {
    let gone = upcoming(tail, rows);
    if gone.is_empty() {
        return None;
    }
    Some(without(tail.rows, &gone))
}

/// For [`remove`]: the tail position each row of the new tail came from.
pub fn remove_order(tail: &Tail, rows: &[usize]) -> Vec<usize> {
    without(&positions(tail), &upcoming(tail, rows))
}

/// The upcoming rows listed, in the order given, in front of queue gap `gap`
/// (clamped to `tail.from..=tail.from + tail.rows.len()`); None if nothing moves.
pub fn moved(tail: &Tail, rows: &[usize], gap: usize) -> Option<Vec<QueueItem>> {
    let picked = upcoming(tail, rows);
    if picked.is_empty() {
        return None;
    }
    let gap = gap.clamp(tail.from, tail.from + tail.rows.len()) - tail.from;
    let out = reordered(tail.rows, &picked, gap);
    if same(&out, tail.rows) {
        None
    } else {
        Some(out)
    }
}

/// For [`moved`]: the tail position each row of the new tail came from.
pub fn moved_order(tail: &Tail, rows: &[usize], gap: usize) -> Vec<usize> {
    let gap = gap.clamp(tail.from, tail.from + tail.rows.len()) - tail.from;
    reordered(&positions(tail), &upcoming(tail, rows), gap)
}

/// The upcoming rows listed first, in the order given; None if they already are.
pub fn play_next(tail: &Tail, rows: &[usize]) -> Option<Vec<QueueItem>> {
    moved(tail, rows, tail.from)
}

/// For [`play_next`]: the tail position each row of the new tail came from.
pub fn play_next_order(tail: &Tail, rows: &[usize]) -> Vec<usize> {
    moved_order(tail, rows, tail.from)
}

/// Where queue rows `rows` are once an edit at `from` has replaced the `before_len` rows
/// there with rows that came from tail positions `order`. Rows before `from` stay, rows
/// after the old tail shift by the change in length, and rows the edit removed are left out.
pub fn remap(from: usize, before_len: usize, order: &[usize], rows: &[usize]) -> Vec<usize> {
    rows.iter()
        .filter_map(|&r| {
            if r < from {
                Some(r)
            } else if r - from >= before_len {
                Some(r - before_len + order.len())
            } else {
                order.iter().position(|&o| o == r - from).map(|p| from + p)
            }
        })
        .collect()
}

/// How many listed rows are upcoming, and whether Play next would change anything.
pub fn counts(tail: Option<&Tail>, rows: &[usize]) -> (usize, bool) {
    match tail {
        Some(t) => (upcoming(t, rows).len(), play_next(t, rows).is_some()),
        None => (0, false),
    }
}

/// What Undo sends: `before` followed by what comes after `after` in `now`; None when
/// `now` no longer starts with `after`.
pub fn undo(
    now: &[QueueItem],
    before: &[QueueItem],
    after: &[QueueItem],
) -> Option<Vec<QueueItem>> {
    if now.len() < after.len() || !same(&now[..after.len()], after) {
        return None;
    }
    let mut out = before.to_vec();
    out.extend_from_slice(&now[after.len()..]);
    Some(out)
}

/// Where queue gap `gap` (in front of row `gap`) is after the edit [`remap`] describes:
/// in front of the first row at or after it that the edit kept, else at the new tail's end.
pub fn remap_gap(from: usize, before_len: usize, order: &[usize], gap: usize) -> usize {
    if gap <= from {
        return gap;
    }
    (gap..from + before_len)
        .find_map(|r| remap(from, before_len, order, &[r]).first().copied())
        .unwrap_or_else(|| remap(from, before_len, order, &[gap.max(from + before_len)])[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use bp_core::{Codec, SourceFormat, SourceKind};
    use std::path::PathBuf;
    use std::time::Duration;

    fn q(n: usize) -> QueueItem {
        QueueItem {
            path: PathBuf::from(format!("/m/q{n}.flac")),
            source: SourceFormat {
                codec: Codec::Wav,
                sample_rate: 44_100,
                channels: 2,
                kind: SourceKind::Pcm {
                    bits: 16,
                    float: false,
                },
            },
            duration: Duration::from_secs(1),
            title: format!("q{n}"),
        }
    }

    fn qs(ns: &[usize]) -> Vec<QueueItem> {
        ns.iter().map(|&n| q(n)).collect()
    }

    fn queue() -> Vec<QueueItem> {
        qs(&[0, 1, 2, 3, 4, 5])
    }

    #[test]
    fn remove_drops_only_upcoming_rows() {
        let queue = queue();
        let t = tail(&queue, Some(1)).unwrap();
        assert_eq!(remove(&t, &[0, 1, 3, 5]), Some(qs(&[2, 4])));
    }

    #[test]
    fn remove_of_no_upcoming_row_is_none() {
        let queue = queue();
        let t = tail(&queue, Some(1)).unwrap();
        assert_eq!(remove(&t, &[0, 1]), None);
    }

    #[test]
    fn move_keeps_the_given_order() {
        let queue = queue();
        let t = tail(&queue, Some(1)).unwrap();
        assert_eq!(moved(&t, &[5, 3], 2), Some(qs(&[5, 3, 2, 4])));
    }

    #[test]
    fn move_clamps_the_gap_below_the_current_row() {
        let queue = queue();
        let t = tail(&queue, Some(1)).unwrap();
        assert_eq!(moved(&t, &[4], 0), Some(qs(&[4, 2, 3, 5])));
    }

    #[test]
    fn move_onto_itself_is_none() {
        let queue = queue();
        let t = tail(&queue, Some(1)).unwrap();
        assert_eq!(moved(&t, &[3], 3), None);
        assert_eq!(moved(&t, &[3], 4), None);
    }

    #[test]
    fn move_to_the_end() {
        let queue = queue();
        let t = tail(&queue, Some(1)).unwrap();
        assert_eq!(moved(&t, &[2], 6), Some(qs(&[3, 4, 5, 2])));
    }

    #[test]
    fn play_next_puts_rows_first() {
        let queue = queue();
        let t = tail(&queue, Some(1)).unwrap();
        assert_eq!(play_next(&t, &[4, 3]), Some(qs(&[4, 3, 2, 5])));
    }

    #[test]
    fn play_next_of_the_next_rows_is_none() {
        let queue = queue();
        let t = tail(&queue, Some(1)).unwrap();
        assert_eq!(play_next(&t, &[2]), None);
        assert_eq!(play_next(&t, &[2, 3]), None);
    }

    #[test]
    fn counts_ignore_current_and_played_rows() {
        let queue = queue();
        let t = tail(&queue, Some(1)).unwrap();
        assert_eq!(counts(Some(&t), &[0, 1, 2]), (1, false));
        assert_eq!(counts(Some(&t), &[1, 4]), (1, true));
        assert_eq!(counts(None, &[3]), (0, false));
    }

    #[test]
    fn orders_follow_the_edits() {
        let q = queue();
        let t = tail(&q, Some(0)).unwrap();
        assert_eq!(remove_order(&t, &[2, 4]), [0, 2, 4]);
        assert_eq!(play_next_order(&t, &[3, 2]), [2, 1, 0, 3, 4]);
        assert_eq!(moved_order(&t, &[1], 99), [1, 2, 3, 4, 0]);
    }

    #[test]
    fn remap_follows_rows_and_drops_removed_ones() {
        // Tail at 1 of 4 rows; the edit removed tail position 1 (queue row 2).
        let order = [0, 2, 3];
        assert_eq!(remap(1, 4, &order, &[0, 1, 2, 3, 4]), [0, 1, 2, 3]);
        // Rows after the old tail (appended since) shift up by one.
        assert_eq!(remap(1, 4, &order, &[6]), [5]);
        // A play next of row 4: it lands first.
        assert_eq!(remap(1, 4, &[3, 0, 1, 2], &[4, 1]), [1, 2]);
    }

    #[test]
    fn remap_gap_goes_in_front_of_the_next_kept_row() {
        // Tail at 1 of 4 rows; the edit removed queue row 2.
        let order = [0, 2, 3];
        assert_eq!(remap_gap(1, 4, &order, 1), 1);
        assert_eq!(remap_gap(1, 4, &order, 2), 2);
        assert_eq!(remap_gap(1, 4, &order, 3), 2);
        assert_eq!(remap_gap(1, 4, &order, 5), 4);
        assert_eq!(remap_gap(1, 4, &order, 6), 5);
    }

    #[test]
    fn remap_keeps_duplicates_apart() {
        // Two copies of a track; the edit removed the second (queue row 2).
        let q = qs(&[0, 1, 1, 2]);
        let t = tail(&q, Some(0)).unwrap();
        let order = remove_order(&t, &[2]);
        assert_eq!(remap(1, 3, &order, &[1]), [1]);
        assert_eq!(remap(1, 3, &order, &[2]), Vec::<usize>::new());
        assert_eq!(remap(1, 3, &order, &[3]), [2]);
    }

    #[test]
    fn undo_keeps_appended_tracks() {
        assert_eq!(
            undo(&qs(&[2, 9]), &qs(&[2, 3]), &qs(&[2])),
            Some(qs(&[2, 3, 9]))
        );
    }

    #[test]
    fn undo_is_none_when_after_is_gone() {
        assert_eq!(undo(&qs(&[3]), &qs(&[2, 3]), &qs(&[2])), None);
    }
}
