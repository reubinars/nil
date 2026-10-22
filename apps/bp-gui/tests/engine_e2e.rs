//! The GUI running on the real engine (capture backend, no audio hardware).

mod common;

use bp_gui::state::now_playing::Badge;
use common::{CTRL, engine_fixture};
use egui::Role;
use egui_kittest::kittest::Queryable;

fn open_and_play_first(f: &mut common::EngineFixture) {
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Real Album")
        .click();
    f.harness.run();
    let pos = f
        .harness
        .get_by_role_and_label(Role::ListItem, "First")
        .rect()
        .center();
    let events = &mut f.harness.input_mut().events;
    events.push(egui::Event::PointerMoved(pos));
    for _ in 0..2 {
        for pressed in [true, false] {
            events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
        }
    }
    f.harness.run();
}

#[test]
fn double_click_plays_through_the_real_engine() {
    let mut f = engine_fixture();
    open_and_play_first(&mut f);
    f.wait_until("the bit-perfect badge", |app| {
        app.now_playing().badge == Badge::BitPerfect
    });
    let streams = f.log.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].format.sample_rate, 44_100);
    assert!(!streams[0].bytes.is_empty());
}

#[test]
fn next_from_the_bar_reaches_the_second_track() {
    let mut f = engine_fixture();
    open_and_play_first(&mut f);
    f.wait_until("the bit-perfect badge", |app| {
        app.now_playing().badge == Badge::BitPerfect
    });
    f.harness.key_press_modifiers(CTRL, egui::Key::ArrowRight);
    f.wait_until("the second track", |app| app.now_playing().index == Some(1));
    f.harness.run();
    assert_eq!(f.harness.state().now_playing().badge, Badge::BitPerfect);
}
