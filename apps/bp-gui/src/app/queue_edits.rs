//! Queue edits: each is sent as one `Rearrange` with a ticket saying what it was for, and
//! its answer, matched to that ticket, brings the notice and the undo, or, for a plugin's
//! edit, the plugin's reply.

use bp_engine::{Event, RearrangeRefusal};
use serde_json::{Value, json};

use crate::backend::PlayerCommand;
use crate::plugin_bridge;
use crate::plugins::rpc::{Call, RpcError};
use crate::queue_edit::{self, EditKind};
use crate::state::selection::ListId;

use super::{BpApp, By, GuiEdit, Ticket, Undo};

const MOVED_ON: &str = "The queue moved on. Can't undo.";
const CHANGED: &str = "The queue changed. Try again.";

impl GuiEdit {
    /// The rows it is for; None for a clear, which is for whatever is upcoming.
    fn rows_mut(&mut self) -> Option<&mut Vec<usize>> {
        match self {
            GuiEdit::Remove(rows) | GuiEdit::PlayNext(rows) | GuiEdit::Move { rows, .. } => {
                Some(rows)
            }
            GuiEdit::Clear => None,
        }
    }

    /// Follows rows (and a move's gap) pushed down by `count` tracks inserted at queue
    /// index `at`.
    pub(super) fn shift(&mut self, at: usize, count: usize) {
        for r in self.rows_mut().into_iter().flatten() {
            if *r >= at {
                *r += count;
            }
        }
        if let GuiEdit::Move { gap, .. } = self
            && *gap >= at
        {
            *gap += count;
        }
    }

    /// Follows its rows (and a move's gap, see [`queue_edit::remap_gap`]) through an
    /// answered edit that replaced the `before_len` rows at `from` with rows from positions
    /// `order` (see [`queue_edit::remap`]). Rows that edit removed drop out; false if none
    /// is left. A clear has no rows to follow and is always kept.
    fn follow(&mut self, from: usize, before_len: usize, order: &[usize]) -> bool {
        if let GuiEdit::Move { gap, .. } = self {
            *gap = queue_edit::remap_gap(from, before_len, order, *gap);
        }
        let Some(rows) = self.rows_mut() else {
            return true;
        };
        *rows = queue_edit::remap(from, before_len, order, rows);
        !rows.is_empty()
    }
}

/// A plugin's reply to its applied edit; a clear says how many tracks went.
fn edit_done(kind: EditKind, removed: usize) -> Value {
    match kind {
        EditKind::Clear { .. } => json!({ "removed": removed }),
        _ => json!({}),
    }
}

/// `data.reason` of the reply to a plugin edit the engine refused.
fn reason(r: RearrangeRefusal) -> &'static str {
    match r {
        RearrangeRefusal::Started => "started",
        RearrangeRefusal::Changed => "changed",
        RearrangeRefusal::Stopped => "stopped",
    }
}

/// "1 track", "3 tracks".
fn tracks(n: usize) -> String {
    if n == 1 {
        "1 track".to_owned()
    } else {
        format!("{n} tracks")
    }
}

impl BpApp {
    /// Makes `edit` on the upcoming rows. While an insert or another edit is still on its
    /// way it is kept (a newer one replaces it) and made once the queue has settled.
    pub(super) fn edit_queue(&mut self, edit: GuiEdit) {
        if self.now_playing.settling() {
            self.held_edit = Some(edit);
            return;
        }
        let np = &self.now_playing;
        if np.fatal.is_some() {
            return;
        }
        let Some(tail) = queue_edit::tail(&np.queue, np.index) else {
            return;
        };
        let (kind, items, order, moved) = match &edit {
            GuiEdit::Remove(rows) => (
                EditKind::Remove,
                queue_edit::remove(&tail, rows),
                queue_edit::remove_order(&tail, rows),
                vec![],
            ),
            GuiEdit::PlayNext(rows) => {
                let (n, _) = queue_edit::counts(Some(&tail), rows);
                let moved = (tail.from..tail.from + n).collect();
                (
                    EditKind::PlayNext,
                    queue_edit::play_next(&tail, rows),
                    queue_edit::play_next_order(&tail, rows),
                    moved,
                )
            }
            GuiEdit::Move { rows, gap } => {
                let order = queue_edit::moved_order(&tail, rows, *gap);
                // Where the upcoming rows land, for the selection and the notice.
                let end = tail.from + tail.rows.len();
                let upcoming: Vec<usize> = rows
                    .iter()
                    .copied()
                    .filter(|r| (tail.from..end).contains(r))
                    .collect();
                let moved = queue_edit::remap(tail.from, tail.rows.len(), &order, &upcoming);
                (
                    EditKind::Move,
                    queue_edit::moved(&tail, rows, *gap),
                    order,
                    moved,
                )
            }
            // Every upcoming row goes, so none of them lands anywhere.
            GuiEdit::Clear => (
                EditKind::Clear { retried: false },
                (!tail.rows.is_empty()).then(Vec::new),
                Vec::new(),
                vec![],
            ),
        };
        let Some(items) = items else { return };
        let ticket = Ticket {
            from: tail.from,
            before: tail.rows.to_vec(),
            after: items,
            kind,
            by: By::Gui,
            moved,
            order: Some(order),
            void: false,
        };
        self.drop_undo();
        self.send_rearrange(ticket);
    }

    /// Makes a plugin's `player.remove`, `player.move` or `player.clear_upcoming`, `by`.
    /// It is sent at once, never held: the engine checks the rows it was made on and a
    /// stale one is refused. None while the engine has yet to answer; an edit that changes
    /// nothing, or a check that fails, is answered now.
    pub(super) fn plugin_edit(&mut self, call: &Call, by: By) -> Option<Result<Value, RpcError>> {
        let plan = match plugin_bridge::edit(call, &self.now_playing) {
            Ok(plan) => plan,
            Err(e) => return Some(Err(e)),
        };
        if plan.items == plan.expected {
            return Some(Ok(edit_done(plan.kind, 0)));
        }
        self.send_rearrange(Ticket {
            from: plan.from,
            before: plan.expected,
            after: plan.items,
            kind: plan.kind,
            by,
            moved: Vec::new(),
            order: Some(plan.order),
            void: false,
        });
        None
    }

    /// Undoes the queue edit the undo is for: its rows go back as they were, with whatever
    /// was appended since after them. Refused once the current track is at or past where
    /// the edit started, or the rows it left have changed. The undo is used up either way.
    pub(super) fn undo_queue(&mut self) {
        let Some(Undo::Queue {
            from,
            before,
            after,
        }) = self.undo.take()
        else {
            return;
        };
        self.drop_undo();
        let np = &self.now_playing;
        let now = np.queue.get(from..).unwrap_or_default();
        let items = if np.index.is_none_or(|c| from <= c) {
            None
        } else {
            queue_edit::undo(now, &before, &after)
        };
        let Some(items) = items else {
            self.notify(MOVED_ON.to_owned(), false);
            return;
        };
        let ticket = Ticket {
            from,
            before: now.to_vec(),
            after: items,
            kind: EditKind::Undo,
            by: By::Gui,
            moved: Vec::new(),
            // Rows an undo puts back came from nowhere in `before`, which an order cannot
            // say: an edit held behind it is dropped.
            order: None,
            void: false,
        };
        self.send_rearrange(ticket);
    }

    /// Sends the Rearrange `ticket` stands for and keeps the ticket for its answer. Every
    /// Rearrange is sent from here.
    pub(super) fn send_rearrange(&mut self, ticket: Ticket) {
        let cmd = PlayerCommand::Rearrange {
            from: ticket.from,
            expected: ticket.before.clone(),
            items: ticket.after.clone(),
        };
        self.tickets.push_back(ticket);
        self.send(cmd);
    }

    /// Matches a `Rearranged` or `RearrangeRefused` to the oldest ticket. Called before
    /// `NowPlaying` applies the event, so the queue is still as it was before it.
    pub(super) fn queue_answer(&mut self, ev: &Event) {
        let refusal = match ev {
            Event::Rearranged { .. } => None,
            Event::RearrangeRefused { reason, .. } => Some(*reason),
            _ => return,
        };
        let Some(ticket) = self.tickets.pop_front() else {
            return;
        };
        if ticket.void {
            return;
        }
        // An edit held behind this one was made on the queue as it was before it.
        let follow = refusal
            .is_none()
            .then(|| (ticket.from, ticket.before.len(), ticket.order.clone()));
        match ticket.by {
            By::Gui => self.gui_answer(ticket, refusal),
            By::Plugin { .. } => self.plugin_answer(ticket, refusal),
        }
        if let Some((from, before_len, order)) = follow
            && let Some(edit) = &mut self.held_edit
        {
            match order {
                Some(order) => {
                    if !edit.follow(from, before_len, &order) {
                        self.held_edit = None;
                    }
                }
                None => {
                    self.held_edit = None;
                    self.notify(CHANGED.to_owned(), false);
                }
            }
        }
    }

    /// The notice, undo and selection for the answer to an edit made in the GUI.
    fn gui_answer(&mut self, t: Ticket, refusal: Option<RearrangeRefusal>) {
        let undoing = t.kind == EditKind::Undo;
        match refusal {
            None => {
                let text = match t.kind {
                    EditKind::Remove => {
                        format!("Removed {}", tracks(t.before.len() - t.after.len()))
                    }
                    EditKind::Move => format!("Moved {}", tracks(t.moved.len())),
                    EditKind::PlayNext => format!("Playing {} next", tracks(t.moved.len())),
                    EditKind::Clear { .. } => format!("Cleared {}", tracks(t.before.len())),
                    // The undo's own notice goes; nothing is left to undo. The rows are
                    // back where they were, so a selection made since means nothing.
                    EditKind::Undo => {
                        self.notice = None;
                        self.clear_queue_selection();
                        return;
                    }
                };
                // The rows moved: a move keeps its rows selected where they landed.
                if t.kind == EditKind::Move {
                    self.queue_rows_moved = true;
                    self.selection.set(&ListId::Queue, t.moved);
                } else {
                    self.clear_queue_selection();
                }
                // Undo works only in Now Playing; closed before the answer, it has none.
                let undo = self.now_playing_open;
                if undo {
                    self.undo = Some(Undo::Queue {
                        from: t.from,
                        before: t.before,
                        after: t.after,
                    });
                }
                self.notify(text, undo);
            }
            Some(RearrangeRefusal::Started) if undoing => self.notify(MOVED_ON.to_owned(), false),
            Some(RearrangeRefusal::Started) => {
                let text = match self.now_playing.queue.get(t.from) {
                    Some(item) => format!("Already starting: {}", item.title),
                    None => CHANGED.to_owned(),
                };
                self.notify(text, false);
                if t.kind == (EditKind::Clear { retried: false }) {
                    self.clear_again(t.by, t.from + 1);
                }
            }
            Some(RearrangeRefusal::Changed) if undoing => self.notify(MOVED_ON.to_owned(), false),
            Some(RearrangeRefusal::Changed) => {
                self.notify(CHANGED.to_owned(), false);
            }
            // The queue already shows that playback stopped.
            Some(RearrangeRefusal::Stopped) => {}
        }
    }

    /// A clear refused once is sent again, once, for `by`, clearing everything from queue
    /// row `from` on: after the starting track when the next track was starting, from the
    /// next track when a plugin's clear found the queue changed. With nothing left there
    /// nothing is sent: a plugin is told none went, and the GUI keeps its "Already
    /// starting" notice, with no undo.
    fn clear_again(&mut self, by: By, from: usize) {
        let before = self
            .now_playing
            .queue
            .get(from..)
            .unwrap_or_default()
            .to_vec();
        if before.is_empty() {
            self.reply(&by, Ok(edit_done(EditKind::Clear { retried: true }, 0)));
            return;
        }
        self.send_rearrange(Ticket {
            from,
            before,
            after: Vec::new(),
            kind: EditKind::Clear { retried: true },
            by,
            moved: Vec::new(),
            order: Some(Vec::new()),
            void: false,
        });
    }

    /// The reply to a plugin's edit, once the engine has answered it. An applied edit
    /// shows no notice; the queue undo and a queue selection, made on the rows as they
    /// were, go.
    fn plugin_answer(&mut self, t: Ticket, refusal: Option<RearrangeRefusal>) {
        let result = match refusal {
            None => {
                self.drop_queue_undo();
                self.clear_queue_selection();
                Ok(edit_done(t.kind, t.before.len()))
            }
            Some(RearrangeRefusal::Started) if t.kind == (EditKind::Clear { retried: false }) => {
                // Answered when the clear after the starting track is.
                self.clear_again(t.by, t.from + 1);
                return;
            }
            // A clear names no rows, so nothing the plugin saw is stale: it is planned
            // again on the queue as it is now.
            Some(RearrangeRefusal::Changed) if t.kind == (EditKind::Clear { retried: false }) => {
                match plugin_bridge::edit(&Call::PlayerClearUpcoming, &self.now_playing) {
                    Ok(plan) => self.clear_again(t.by, plan.from),
                    Err(e) => self.reply(&t.by, Err(e)),
                }
                return;
            }
            Some(r) => Err(RpcError::queue_changed(reason(r))),
        };
        self.reply(&t.by, result);
    }

    /// Play, Stop, the queue's end or a fatal error: the queue the tickets were for is
    /// gone. Each ticket stays, to be popped by its own answer, which is then ignored; a
    /// plugin waiting on one is told playback stopped now. A held edit and the queue undo
    /// are dropped.
    pub(super) fn void_tickets(&mut self) {
        let mut waiting = Vec::new();
        for t in &mut self.tickets {
            if !t.void {
                waiting.push(t.by.clone());
            }
            t.void = true;
        }
        for by in &waiting {
            self.reply(by, Err(RpcError::queue_changed("stopped")));
        }
        self.held_edit = None;
        self.drop_queue_undo();
    }
}
