//! Pixel snapshots of the main views. Baselines are generated on Windows:
//! `UPDATE_SNAPSHOTS=1 cargo test -p bp-gui --features snapshots --test snapshots`.
#![cfg(feature = "snapshots")]

mod common;

use std::path::PathBuf;

use bp_core::EngineError;
use bp_gui::backend::{LibrarySource, PlayerEvent};
use common::*;
use egui::Role;
use egui_kittest::kittest::Queryable;

#[test]
fn grid() {
    let mut f = library();
    f.harness.run();
    f.harness.get_by_role_and_label(Role::Tab, "Albums").click();
    f.harness.run();
    f.harness.snapshot("grid");
}

#[test]
fn home() {
    let mut f = library();
    f.harness.run();
    f.harness.snapshot("home");
}

#[test]
fn grid_under_the_bar() {
    let many: Vec<_> = (1..=24)
        .map(|i| st(&format!("Artist {i:02}"), &format!("Album {i:02}"), "t", 1))
        .collect();
    let mut f = fixture_with(many, vec![PathBuf::from("/music")], None);
    f.harness.run();
    f.open_album("Album 01");
    f.double_click_track("t");
    f.advance(30.0);
    // The mock writes audio as its clock moves.
    f.advance(0.2);
    f.harness.get_by_label("Back").click();
    f.harness.run();
    f.harness.snapshot("grid_under_the_bar");
}

#[test]
fn album_page() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.harness.snapshot("album_page");
}

#[test]
fn settings() {
    let mut f = library();
    f.harness.get_by_label("Settings").click();
    f.harness.run();
    f.harness.snapshot("settings");
}

#[test]
fn device_list() {
    let mut f = library();
    f.harness
        .get_by_role_and_label(Role::Button, "DAC A")
        .click();
    f.harness.run();
    f.harness.snapshot("device_list");
}

#[test]
fn playing_bar() {
    let mut f = library();
    f.open_album("Kid A");
    f.double_click_track("Everything in Its Right Place");
    f.advance(80.0);
    // The mock writes audio as its clock moves.
    f.advance(0.2);
    f.harness.snapshot("playing_bar");
}

#[test]
fn now_playing() {
    let mut f = library();
    let kob = f
        .lib
        .albums(None)
        .into_iter()
        .find(|a| a.title == "Kind of Blue")
        .unwrap()
        .id;
    let img = image::RgbaImage::from_fn(64, 64, |x, y| {
        image::Rgba([0xD7 - (x + y) as u8 / 2, 0x26, 0x3D, 255])
    });
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png).unwrap();
    f.lib.set_art(kob, png.into_inner());
    // A rescan picks the cover up.
    f.lib
        .set_scan_status(bp_gui::backend::ScanStatus::Scanning { done: 0, total: 1 });
    f.harness.run();
    f.lib
        .set_scan_status(bp_gui::backend::ScanStatus::Done { unreadable: 0 });
    f.harness.run();
    f.open_album("Kind of Blue");
    f.double_click_track("Freddie Freeloader");
    f.advance(50.0);
    // The mock writes audio as its clock moves.
    f.advance(0.2);
    f.harness
        .get_by_role_and_label(Role::Button, "Open now playing")
        .click();
    for _ in 0..50 {
        f.harness.run();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    f.harness.snapshot("now_playing");
}

#[test]
fn fatal_error_bar() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.double_click_track("Blue in Green");
    f.advance(0.4);
    f.player.inject(PlayerEvent::Engine(bp_engine::Event::Error(
        EngineError::NoDevice,
    )));
    f.harness.run();
    f.harness.snapshot("fatal_error_bar");
}

#[test]
fn first_run() {
    let mut f = fixture(Vec::<PathBuf>::new(), None);
    f.harness.run();
    f.harness.snapshot("first_run");
}
