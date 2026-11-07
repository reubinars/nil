//! A modal asking for a playlist name, with a confirm step before replacing one.

use egui::text::{CCursor, CCursorRange};
use egui::{Align, Color32, Id, Key, Layout, Modal, RichText, TextEdit};

use crate::playlists::sanitize;

/// Error text.
const ERROR: Color32 = Color32::from_rgb(0xE5, 0x73, 0x73);

pub enum DialogEvent {
    Cancel,
    /// The name as typed, trimmed.
    Submit(String),
}

pub struct NameDialog {
    title: String,
    ok: &'static str,
    name: String,
    /// Ask before submitting a name that `exists` reports as taken.
    confirm_replace: bool,
    asking: bool,
    focused: bool,
    /// Shown under the field; cleared when the name is edited.
    pub error: Option<String>,
}

impl NameDialog {
    pub fn new(title: &str, ok: &'static str, name: &str, confirm_replace: bool) -> Self {
        NameDialog {
            title: title.to_owned(),
            ok,
            name: name.to_owned(),
            confirm_replace,
            asking: false,
            focused: false,
            error: None,
        }
    }

    /// Draws the dialog. `exists` tells whether a playlist of that name is already there.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        exists: impl Fn(&str) -> bool,
    ) -> Option<DialogEvent> {
        let mut event = None;
        let modal = Modal::new(Id::unique("name_dialog")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading(&self.title);
            ui.add_space(12.0);
            if self.asking {
                let shown = sanitize(self.name.trim());
                ui.label(format!("Replace \"{shown}\"?"));
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Replace").clicked() {
                        event = Some(DialogEvent::Submit(self.name.trim().to_owned()));
                    }
                    if ui.button("Cancel").clicked() {
                        event = Some(DialogEvent::Cancel);
                    }
                });
                return;
            }
            let edit = TextEdit::singleline(&mut self.name).desired_width(f32::INFINITY);
            let resp = ui.add(edit);
            ui.ctx()
                .accesskit_node_builder(resp.id, |b| b.set_label("Playlist name"));
            if !self.focused {
                resp.request_focus();
                // Select the name so typing replaces it.
                if let Some(mut state) = TextEdit::load_state(ui.ctx(), resp.id) {
                    let end = CCursor::new(self.name.chars().count());
                    state
                        .cursor
                        .set_char_range(Some(CCursorRange::two(CCursor::new(0), end)));
                    state.store(ui.ctx(), resp.id);
                }
                self.focused = true;
            }
            if resp.changed() {
                self.error = None;
            }
            if let Some(e) = &self.error {
                ui.add_space(6.0);
                ui.label(RichText::new(e).color(ERROR));
            }
            ui.add_space(12.0);
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let can = !self.name.trim().is_empty();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ok = ui.add_enabled(can, egui::Button::new(self.ok));
                if ok.clicked() || (enter && can) {
                    event = self.submit(&exists);
                }
                if ui.button("Cancel").clicked() {
                    event = Some(DialogEvent::Cancel);
                }
            });
        });
        if modal.should_close() && event.is_none() {
            event = Some(DialogEvent::Cancel);
        }
        event
    }

    /// The event for pressing OK, or `None` after switching to the replace prompt.
    fn submit(&mut self, exists: &impl Fn(&str) -> bool) -> Option<DialogEvent> {
        let name = self.name.trim().to_owned();
        if self.confirm_replace && exists(&name) {
            self.asking = true;
            return None;
        }
        Some(DialogEvent::Submit(name))
    }
}
