//! A plugin's panel: the declarative elements it sent, drawn in nil's theme.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{Key, RichText, Role, ScrollArea, Sense, TextEdit, Ui, WidgetInfo, vec2};
use serde_json::Value;

use crate::art::ArtLoader;
use crate::backend::Album;
use crate::fmt::duration;
use crate::plugins::panel::{Element, Panel, PanelTrack};
use crate::theme::{DISABLED, TEXT, TEXT_2, TEXT_3, bold, semibold};
use crate::views::art_row::{self, ArtRow, DIM, ROW_H};
use crate::views::track_list::{self, TrackListAction, TrackRow, pill};

/// What a plugin's panel keeps between frames.
#[derive(Debug, Default)]
pub struct PanelState {
    /// The text typed so far into each input, by element id.
    pub inputs: HashMap<String, String>,
}

impl PanelState {
    /// The plugin sent `new` in place of `old`. Typed text survives while an input's id
    /// does; an input whose `value` the plugin changed starts again from that value.
    pub fn sync(&mut self, old: Option<&Panel>, new: &Panel) {
        let mut before = HashMap::new();
        if let Some(old) = old {
            input_values(&old.elements, &mut before);
        }
        let mut now = HashMap::new();
        input_values(&new.elements, &mut now);
        self.inputs.retain(|id, _| now.contains_key(id.as_str()));
        for (id, value) in now {
            if before.get(id).is_none_or(|b| *b != value) {
                self.inputs.insert(id.to_owned(), value.to_owned());
            }
        }
    }
}

fn input_values<'a>(elements: &'a [Element], out: &mut HashMap<&'a str, &'a str>) {
    for e in elements {
        match e {
            Element::Input { id, value, .. } => {
                out.insert(id, value);
            }
            Element::Row { children } => input_values(children, out),
            _ => {}
        }
    }
}

pub enum PanelAction {
    /// The user used the element `id`: a `panel.event` for the plugin.
    Event { id: String, value: Option<Value> },
    /// The user acted on a list of tracks; nil handles it itself. Carries the rows.
    Tracks(TrackListAction, Vec<TrackRow>),
}

/// What drawing a panel's elements needs besides the elements.
struct Draw<'a> {
    /// None in a settings panel, which has no inputs.
    state: Option<&'a mut PanelState>,
    /// None in a settings panel, which has no track lists.
    tracks: Option<Tracks<'a>>,
    /// The track lists drawn so far, to tell their widgets apart.
    lists: usize,
}

struct Tracks<'a> {
    /// The track now playing.
    playing: Option<&'a Path>,
    /// The library album of each listed track that has one.
    albums: &'a HashMap<PathBuf, Album>,
    art: &'a mut ArtLoader,
}

/// The plugin's tab: `title` as the page heading, then the elements.
pub fn show(
    ui: &mut Ui,
    panel: &Panel,
    state: &mut PanelState,
    playing: Option<&Path>,
    albums: &HashMap<PathBuf, Album>,
    art: &mut ArtLoader,
    title: &str,
) -> Option<PanelAction> {
    let mut d = Draw {
        state: Some(state),
        tracks: Some(Tracks {
            playing,
            albums,
            art,
        }),
        lists: 0,
    };
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 12.0;
            crate::views::heading(ui, title, None);
            let action = elements(ui, &panel.elements, &mut d);
            ui.add_space(crate::views::bar::CLEARANCE);
            action
        })
        .inner
}

/// A plugin's settings panel, inside its Settings entry: the elements only, no scroll
/// area or heading. It holds no inputs or track lists, so only events come back.
pub fn show_settings(ui: &mut Ui, panel: &Panel) -> Option<PanelAction> {
    let mut d = Draw {
        state: None,
        tracks: None,
        lists: 0,
    };
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        elements(ui, &panel.elements, &mut d)
    })
    .inner
}

/// The `panel.event` params for element `id`: `{id, value?}`.
pub fn event_params(id: String, value: Option<Value>) -> Value {
    let mut params = serde_json::json!({ "id": id });
    if let Some(v) = value {
        params["value"] = v;
    }
    params
}

fn elements(ui: &mut Ui, list: &[Element], d: &mut Draw) -> Option<PanelAction> {
    let mut action = None;
    for e in list {
        if let Some(a) = element(ui, e, d) {
            action = Some(a);
        }
    }
    action
}

fn element(ui: &mut Ui, e: &Element, d: &mut Draw) -> Option<PanelAction> {
    match e {
        Element::Heading { text, muted } => {
            let color = if *muted { TEXT_2 } else { TEXT };
            ui.label(RichText::new(text).size(22.0).family(bold()).color(color));
        }
        Element::Text { text, muted } => {
            let color = if *muted { TEXT_2 } else { TEXT };
            ui.label(RichText::new(text).color(color));
        }
        Element::Input {
            id,
            placeholder,
            value,
            submit_label,
        } => {
            // A settings panel refuses inputs when it is parsed.
            let state = d.state.as_deref_mut()?;
            return input(ui, state, id, placeholder, value, submit_label);
        }
        Element::Button { id, label, enabled } => {
            let clicked = if *enabled {
                pill(ui, label, label, false).clicked()
            } else {
                ui.add_enabled(
                    false,
                    egui::Button::new(RichText::new(label).color(DISABLED)),
                )
                .clicked()
            };
            if clicked {
                return Some(PanelAction::Event {
                    id: id.clone(),
                    value: None,
                });
            }
        }
        Element::Toggle { id, label, value } => {
            let mut on = *value;
            if crate::views::switch(ui, &mut on, label).changed() {
                return Some(PanelAction::Event {
                    id: id.clone(),
                    value: Some(Value::Bool(on)),
                });
            }
        }
        Element::Progress { value, label } => match value {
            Some(v) => {
                let width = ui.available_width().min(240.0);
                ui.add(egui::ProgressBar::new(*v).desired_width(width).text(label));
            }
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(label).color(TEXT_2));
                });
            }
        },
        Element::Tracks { items, label } => {
            // A settings panel refuses track lists when it is parsed.
            let t = d.tracks.as_mut()?;
            d.lists += 1;
            return ui
                .push_id(("panel_tracks", d.lists), |ui| {
                    tracks(ui, items, label.as_deref(), t)
                })
                .inner;
        }
        Element::Row { children } => {
            return ui.horizontal(|ui| elements(ui, children, d)).inner;
        }
        Element::Unsupported { .. } => {
            ui.label(RichText::new("unsupported element").color(TEXT_3));
        }
    }
    None
}

/// A track list drawn like the Now Playing queue: its label, the action pills, then a
/// row with art per track. Double-click or Enter plays from a row.
fn tracks(
    ui: &mut Ui,
    items: &[PanelTrack],
    label: Option<&str>,
    d: &mut Tracks,
) -> Option<PanelAction> {
    if let Some(label) = label {
        ui.label(
            RichText::new(label.to_uppercase())
                .size(12.0)
                .family(semibold())
                .color(DIM),
        );
    }
    let mut action = track_list::action_pills(ui);
    ui.spacing_mut().item_spacing.y = 2.0;
    for (i, t) in items.iter().enumerate() {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        resp.widget_info(|| WidgetInfo::labeled(Role::ListItem, true, &t.title));
        let enter = resp.has_focus() && ui.input(|inp| inp.key_pressed(Key::Enter));
        if !t.missing && (resp.double_clicked() || enter) {
            action = Some(TrackListAction::PlayFrom(i));
        }
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let time = t.duration_s.map_or_else(String::new, |s| {
            duration(std::time::Duration::from_secs_f64(s))
        });
        let r = ArtRow {
            title: &t.title,
            artist: &t.artist,
            time: &time,
            album: d.albums.get(&t.path),
            current: d.playing == Some(t.path.as_path()),
            selected: false,
            dim: t.missing,
        };
        art_row::paint(ui, rect, resp.hovered(), &r, d.art);
    }
    action.map(|a| PanelAction::Tracks(a, items.iter().map(row).collect()))
}

fn input(
    ui: &mut Ui,
    state: &mut PanelState,
    id: &str,
    placeholder: &str,
    value: &str,
    submit_label: &str,
) -> Option<PanelAction> {
    let text = state
        .inputs
        .entry(id.to_owned())
        .or_insert_with(|| value.to_owned());
    let mut send = false;
    ui.horizontal(|ui| {
        let edit = TextEdit::singleline(text)
            .id_salt(("panel_input", id))
            .hint_text(RichText::new(placeholder).color(TEXT_3))
            .desired_width(280.0);
        let resp = ui.add(edit);
        ui.ctx()
            .accesskit_node_builder(resp.id, |b| b.set_label(placeholder));
        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            send = true;
        }
        if pill(ui, submit_label, submit_label, true).clicked() {
            send = true;
        }
    });
    send.then(|| PanelAction::Event {
        id: id.to_owned(),
        value: Some(Value::String(text.clone())),
    })
}

fn row(t: &PanelTrack) -> TrackRow {
    TrackRow {
        path: t.path.clone(),
        title: t.title.clone(),
        artist: t.artist.clone(),
        duration_s: t.duration_s,
        missing: t.missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::panel::parse;
    use serde_json::json;

    fn panel(value: &str) -> Panel {
        parse(&json!({"elements": [
            {"type": "row", "children": [
                {"type": "input", "id": "q", "placeholder": "p", "submit_label": "Go", "value": value},
            ]},
        ]}))
        .unwrap()
    }

    #[test]
    fn typed_text_survives_while_the_plugin_value_is_unchanged() {
        let mut s = PanelState::default();
        let a = panel("x");
        s.sync(None, &a);
        assert_eq!(s.inputs["q"], "x");
        s.inputs.insert("q".into(), "typed".into());
        s.sync(Some(&a), &panel("x"));
        assert_eq!(s.inputs["q"], "typed");
        s.sync(Some(&a), &panel("y"));
        assert_eq!(s.inputs["q"], "y");
    }

    #[test]
    fn a_vanished_input_is_forgotten() {
        let mut s = PanelState::default();
        let a = panel("x");
        s.sync(None, &a);
        s.sync(Some(&a), &Panel { elements: vec![] });
        assert!(s.inputs.is_empty());
    }
}
