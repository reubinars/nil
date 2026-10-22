//! Whole-app UI tests: the real `BpApp` driven through egui_kittest.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bp_core::{DeviceId, EngineError, SampleEncoding};
use bp_gui::app::BpApp;
use bp_gui::backend::index::MemLibrary;
use bp_gui::backend::mock_player::demo_devices;
use bp_gui::backend::{LibrarySource, PlayerCommand, PlayerEvent, PlayerHandle, ScanStatus};
use bp_gui::prefs::Prefs;
use bp_gui::state::nav::Page;
use bp_gui::state::now_playing::Badge;
use common::*;
use egui::{Modifiers, PointerButton, Role};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

#[test]
fn first_run_shows_add_folder_and_adds_it() {
    let mut f = fixture(Vec::new(), Some(PathBuf::from("/music")));
    f.harness.get_by_label("Add music folder").click();
    f.harness.run();
    assert_eq!(f.lib.folders(), vec![PathBuf::from("/music")]);
    assert!(f.harness.query_by_label("Add music folder").is_none());
}

#[test]
fn first_run_add_refreshes_the_grid_without_a_status_change() {
    // The scan finished between frames: the status went Done -> Done.
    let mut f = fixture_with(Vec::new(), Vec::new(), Some(PathBuf::from("/music")));
    f.harness.run();
    f.lib.set_tracks(tracks());
    f.harness.get_by_label("Add music folder").click();
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Kind of Blue");
}

#[test]
fn first_run_add_does_not_refresh_every_frame_while_scanning() {
    let mut f = fixture_with(Vec::new(), Vec::new(), Some(PathBuf::from("/music")));
    f.harness.run();
    f.lib
        .set_scan_status(ScanStatus::Scanning { done: 0, total: 10 });
    f.harness.get_by_label("Add music folder").click();
    f.harness.run_steps(2);
    let reads = f.lib.artist_reads();
    f.harness.run_steps(10);
    assert_eq!(
        f.lib.artist_reads(),
        reads,
        "no refresh without a status change"
    );
    // The scan ends: one refresh picks up the new index.
    f.lib.set_tracks(tracks());
    f.lib.set_scan_status(ScanStatus::Done { unreadable: 0 });
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Kind of Blue");
}

#[test]
fn first_run_cancelled_pick_adds_nothing() {
    let mut f = fixture(Vec::new(), None);
    f.harness.get_by_label("Add music folder").click();
    f.harness.run();
    assert!(f.lib.folders().is_empty());
    f.harness.get_by_label("Add music folder");
}

#[test]
fn grid_shows_all_albums_without_selection() {
    let f = library();
    f.harness
        .get_by_role_and_label(Role::Button, "Kind of Blue");
    f.harness.get_by_role_and_label(Role::Button, "Kid A");
    assert!(f.harness.query_by_label("Add music folder").is_none());
}

#[test]
fn albums_page_shows_heading_count_and_format_tags() {
    let mut f = library();
    tab(&mut f, "Albums");
    f.harness.get_by_label("Albums 3 albums");
    f.harness.get_by_label("16/44.1");
    f.harness.get_by_label("Mixed");
}

#[test]
fn artists_page_opens_an_artist() {
    let mut f = library();
    let miles = f
        .lib
        .artists()
        .into_iter()
        .find(|(_, n)| n == "Miles Davis")
        .unwrap()
        .0;
    tab(&mut f, "Artists");
    f.harness.get_by_label("Artists 3 artists");
    f.harness
        .get_by_role_and_label(Role::Button, "Miles Davis")
        .click();
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Artist(miles));
    f.harness.get_by_label("Miles Davis 1 album");
    f.harness
        .get_by_role_and_label(Role::Button, "Kind of Blue");
    assert!(
        f.harness
            .query_by_role_and_label(Role::Button, "Kid A")
            .is_none()
    );
}

#[test]
fn long_artist_heading_is_cut_with_its_count_visible() {
    let artist =
        "Wiener Philharmoniker, Herbert von Karajan, Anne-Sophie Mutter, Mstislav Rostropovich";
    let mut f = fixture_sized(
        vec![st(artist, "Triple Concerto", "Allegro", 1)],
        egui::vec2(900.0, 600.0),
        false,
    );
    f.harness.run();
    tab(&mut f, "Artists");
    f.harness
        .get_by_role_and_label(Role::Button, artist)
        .click();
    f.harness.run();
    let heading = f.harness.get_by(|n| {
        n.role() == Role::Heading && n.label().is_some_and(|l| l.starts_with("Wiener"))
    });
    let label = heading.accesskit_node().label().unwrap();
    assert!(label.ends_with("… 1 album"), "{label}");
    let r = heading.accesskit_node().bounding_box().unwrap();
    assert!(r.x1 <= 900.0, "{r:?}");
}

#[test]
fn clicking_tile_opens_album_page() {
    let mut f = library();
    let kob = f
        .lib
        .albums(None)
        .into_iter()
        .find(|a| a.title == "Kind of Blue")
        .unwrap()
        .id;
    f.harness
        .get_by_role_and_label(Role::Button, "Kind of Blue")
        .click();
    f.harness.run();
    assert_eq!(
        f.harness.state().nav().current(),
        &Page::Album {
            album: kob,
            highlight: None
        }
    );
}

fn wheel(f: &mut Fixture, at: egui::Pos2, dy: f32) {
    let events = &mut f.harness.input_mut().events;
    events.push(egui::Event::PointerMoved(at));
    events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, dy),
        phase: egui::TouchPhase::Move,
        modifiers: Modifiers::default(),
        source: egui::MouseWheelSource::Wheel,
    });
    f.harness.run();
}

#[test]
fn pages_run_under_the_floating_bar() {
    let many: Vec<_> = (1..=24)
        .map(|i| st(&format!("Artist {i:02}"), &format!("Album {i:02}"), "t", 1))
        .collect();
    let mut f = fixture_with(many, vec![PathBuf::from("/music")], None);
    f.harness.run();
    f.open_album("Album 01");
    f.double_click_track("t");
    f.advance(0.4);
    f.harness.get_by_label("Back").click();
    f.harness.run();
    let bar = f
        .harness
        .get_by_role_and_label(Role::Group, "Now playing")
        .rect();
    assert_eq!(bar.bottom(), 800.0, "the bar reaches the window bottom");
    // The grid is drawn down to the window bottom, under the bar, not cut off above it.
    let page_clip_bottom = f
        .harness
        .output()
        .shapes
        .iter()
        .filter(|c| match &c.shape {
            egui::Shape::Text(t) => t.galley.text().starts_with("Album "),
            _ => false,
        })
        .map(|c| c.clip_rect.bottom())
        .fold(0.0f32, f32::max);
    assert_eq!(page_clip_bottom, 800.0, "the page stops above the bar");
    // Scrolled to the end, the last row clears the controls.
    for _ in 0..10 {
        wheel(&mut f, egui::pos2(640.0, 300.0), -2000.0);
    }
    let last = f
        .harness
        .get_by_role_and_label(Role::Button, "Album 24")
        .rect();
    let controls = f
        .harness
        .get_by_role_and_label(Role::Button, "Pause")
        .rect();
    assert!(
        last.bottom() <= controls.top(),
        "last tile {last:?} under the controls {controls:?}"
    );
}

#[test]
fn grid_starts_at_the_top_and_fills_the_row() {
    let f = library();
    let tile = f
        .harness
        .get_by_role_and_label(Role::Button, "Kind of Blue")
        .rect();
    // Under the 56 px top bar, the page margin and the heading.
    assert!(tile.top() < 200.0, "first tile starts at {}", tile.top());
    assert!(f.harness.query_by_label("Tile size").is_none());
}

#[test]
fn top_bar_shows_the_nil_wordmark() {
    let f = library();
    f.harness.get_by_role_and_label(Role::Image, "nil");
}

#[test]
fn gear_opens_settings() {
    let mut f = library();
    f.harness.get_by_label("Settings").click();
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Settings);
}

fn tab(f: &mut Fixture, label: &str) {
    f.harness.get_by_role_and_label(Role::Tab, label).click();
    f.harness.run();
}

#[test]
fn tabs_switch_pages() {
    let mut f = library();
    tab(&mut f, "Artists");
    assert_eq!(f.harness.state().nav().current(), &Page::Artists);
    tab(&mut f, "Albums");
    assert_eq!(f.harness.state().nav().current(), &Page::Albums);
    tab(&mut f, "Home");
    assert_eq!(f.harness.state().nav().current(), &Page::Home);
}

#[test]
fn home_is_the_start_page_and_lists_newest_albums_first() {
    let mut tracks = tracks();
    for t in &mut tracks {
        t.modified_ns = match t.album.as_deref() {
            Some("Kid A") => 300,
            Some("Test Tones") => 200,
            _ => 100,
        };
    }
    let mut f = fixture_with(tracks, vec![PathBuf::from("/music")], None);
    assert_eq!(f.harness.state().nav().current(), &Page::Home);
    f.harness.get_by_label("Recently added");
    let order: Vec<f32> = ["Kid A", "Test Tones", "Kind of Blue"]
        .iter()
        .map(|t| {
            f.harness
                .get_by_role_and_label(Role::Button, t)
                .rect()
                .left()
        })
        .collect();
    assert!(order.is_sorted(), "{order:?}");
    f.open_album("Kid A");
    assert!(matches!(
        f.harness.state().nav().current(),
        Page::Album { .. }
    ));
}

#[test]
fn back_and_forward_buttons_walk_history() {
    let mut f = library();
    tab(&mut f, "Artists");
    f.harness.get_by_label("Back").click();
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Home);
    f.harness.get_by_label("Forward").click();
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Artists);
}

fn mouse_button(f: &mut Fixture, button: PointerButton) {
    let pos = egui::pos2(400.0, 300.0);
    let events = &mut f.harness.input_mut().events;
    for pressed in [true, false] {
        events.push(egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: Modifiers::default(),
        });
    }
    f.harness.run();
}

#[test]
fn alt_arrows_and_mouse_buttons_walk_history() {
    let mut f = library();
    tab(&mut f, "Artists");
    f.harness
        .key_press_modifiers(Modifiers::ALT, egui::Key::ArrowLeft);
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Home);
    f.harness
        .key_press_modifiers(Modifiers::ALT, egui::Key::ArrowRight);
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Artists);
    mouse_button(&mut f, PointerButton::Extra1);
    assert_eq!(f.harness.state().nav().current(), &Page::Home);
    mouse_button(&mut f, PointerButton::Extra2);
    assert_eq!(f.harness.state().nav().current(), &Page::Artists);
}

#[test]
fn ctrl_f_focuses_search() {
    let mut f = library();
    f.harness.key_press_modifiers(CTRL, egui::Key::F);
    f.harness.run();
    assert!(f.harness.get_by_label("Search").is_focused());
}

#[test]
fn old_prefs_with_rail_hidden_still_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gui.toml");
    std::fs::write(&path, "rail_hidden = true\ndevice = \"x\"\n").unwrap();
    assert_eq!(Prefs::load(&path).device.as_deref(), Some("x"));
}

#[test]
fn prefs_roundtrip_and_bad_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("gui.toml");
    let p = Prefs {
        device: Some("dac-a".into()),
    };
    p.save(&path).unwrap();
    assert_eq!(Prefs::load(&path), p);

    assert_eq!(
        Prefs::load(Path::new("/nonexistent/gui.toml")),
        Prefs::default()
    );
    assert_eq!(Prefs::default().device, None);

    std::fs::write(&path, "this is = not [valid toml").unwrap();
    assert_eq!(Prefs::load(&path), Prefs::default());
}

fn repaint_delay(h: &Harness<'static, BpApp>) -> Duration {
    h.output().viewport_output[&egui::ViewportId::ROOT].repaint_delay
}

#[test]
fn ticks_only_while_scanning() {
    // No albums, so no art loads ask for repaints.
    let mut f = fixture_with(Vec::new(), vec![PathBuf::from("/music")], None);
    f.harness.step();
    assert!(repaint_delay(&f.harness) > Duration::from_secs(1));
    f.lib
        .set_scan_status(ScanStatus::Scanning { done: 1, total: 9 });
    f.harness.step();
    assert!(repaint_delay(&f.harness) <= Duration::from_millis(250));
    f.lib.set_scan_status(ScanStatus::Done { unreadable: 0 });
    f.harness.step();
    f.harness.step();
    assert!(repaint_delay(&f.harness) > Duration::from_secs(1));
}

#[test]
fn rescan_refreshes_the_grid() {
    let mut f = library();
    f.lib.set_tracks(vec![st(
        "Bill Evans",
        "Sunday at the Village Vanguard",
        "Gloria's Step",
        1,
    )]);
    f.lib.set_scan_status(ScanStatus::Idle);
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Sunday at the Village Vanguard");
    assert!(
        f.harness
            .query_by_role_and_label(Role::Button, "Kind of Blue")
            .is_none()
    );
}

#[test]
fn finished_scan_retries_missing_art() {
    let mut f = library();
    let kob = f
        .lib
        .albums(None)
        .into_iter()
        .find(|a| a.title == "Kind of Blue")
        .unwrap()
        .id;
    let thumb = f
        .dir
        .path()
        .join("thumbs")
        .join(format!("{:016x}.png", kob.0));
    // Let the first (missing) art loads finish.
    for _ in 0..20 {
        f.harness.run();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!thumb.exists());
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(8, 8, image::Rgba([9, 9, 9, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    f.lib.set_art(kob, png.into_inner());
    f.lib
        .set_scan_status(ScanStatus::Scanning { done: 0, total: 1 });
    f.harness.run();
    f.lib.set_scan_status(ScanStatus::Done { unreadable: 0 });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !thumb.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "art never re-requested"
        );
        f.harness.run();
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn album_header_play_button_plays_from_the_first_track() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.harness
        .get_by_role_and_label(Role::Button, "Play album")
        .click();
    f.harness.run();
    match f.last_sent() {
        Some(PlayerCommand::Play { items, start }) => {
            assert_eq!(start, 0);
            assert_eq!(items.len(), 5);
        }
        other => panic!("expected Play, got {other:?}"),
    }
}

#[test]
fn album_header_artist_link_opens_the_artist() {
    let mut f = library();
    let miles = f
        .lib
        .artists()
        .into_iter()
        .find(|(_, n)| n == "Miles Davis")
        .unwrap()
        .0;
    f.open_album("Kind of Blue");
    f.harness
        .get_by_role_and_label(Role::Link, "Miles Davis")
        .click();
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Artist(miles));
}

#[test]
fn album_header_shows_facts() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.harness
        .get_by_role_and_label(Role::Heading, "Kind of Blue");
    f.harness.get_by_label("5 tracks");
    f.harness.get_by_label("25:00");
    f.harness.get_by_label("FLAC 16/44.1");
}

#[test]
fn long_cjk_title_does_not_widen_the_page() {
    let album = "長".repeat(200);
    let artist = "あ".repeat(120);
    let mut f = fixture_sized(
        vec![st(&artist, &album, "曲", 1)],
        egui::vec2(1280.0, 800.0),
        true,
    );
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, &album)
        .click();
    f.harness.run();
    let width = 1280.0;
    // Physical bounds; the harness draws at 1 pixel per point.
    for node in f.harness.root().children_recursive() {
        let Some(r) = node.accesskit_node().bounding_box() else {
            continue;
        };
        assert!(r.x1 <= width + 0.5, "{node:?} reaches {}", r.x1);
    }
}

fn type_search(f: &mut Fixture, text: &str) {
    f.harness.get_by_label("Search").focus();
    f.harness.run();
    f.harness.get_by_label("Search").type_text(text);
    f.harness.run();
}

#[test]
fn typing_opens_search_and_clearing_returns() {
    let mut f = library();
    tab(&mut f, "Artists");
    type_search(&mut f, "blue");
    assert_eq!(
        f.harness.state().nav().current(),
        &Page::Search("blue".into())
    );
    f.harness.get_by_role_and_label(Role::Heading, "Albums");
    f.harness.get_by_role_and_label(Role::Heading, "Tracks");
    assert!(
        f.harness
            .query_by_role_and_label(Role::Heading, "Artists")
            .is_none()
    );
    for _ in 0..4 {
        f.harness.key_press(egui::Key::Backspace);
        f.harness.run();
    }
    assert_eq!(f.harness.state().nav().current(), &Page::Artists);
}

#[test]
fn search_field_follows_the_page() {
    let mut f = library();
    type_search(&mut f, "blue");
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();
    tab(&mut f, "Albums");
    assert_eq!(
        f.harness.get_by_label("Search").value().as_deref(),
        Some("")
    );
    f.harness.get_by_label("Back").click();
    f.harness.run();
    assert_eq!(
        f.harness.get_by_label("Search").value().as_deref(),
        Some("blue")
    );
}

#[test]
fn search_track_click_opens_album_highlighted() {
    let mut f = library();
    type_search(&mut f, "green");
    f.harness
        .get_by_role_and_label(Role::ListItem, "Blue in Green")
        .click();
    f.harness.run();
    assert!(matches!(
        f.harness.state().nav().current(),
        Page::Album {
            highlight: Some(_),
            ..
        }
    ));
}

#[test]
fn search_track_double_click_plays_its_album_from_it() {
    let mut f = library();
    type_search(&mut f, "green");
    f.double_click_track("Blue in Green");
    match f.last_sent() {
        Some(PlayerCommand::Play { items, start }) => {
            assert_eq!(start, 2);
            assert_eq!(items.len(), 5);
        }
        other => panic!("expected Play, got {other:?}"),
    }
}

#[test]
fn search_without_hits_says_so() {
    let mut f = library();
    type_search(&mut f, "zzz");
    f.harness.get_by_label("No results for “zzz”");
}

/// Plays Kind of Blue from "So What" and opens the now-playing view.
fn open_now_playing(f: &mut Fixture) {
    f.open_album("Kind of Blue");
    f.double_click_track("So What");
    f.advance(0.4);
    f.harness
        .get_by_role_and_label(Role::Button, "Open now playing")
        .click();
    f.harness.run();
    assert!(f.harness.state().now_playing_open());
}

#[test]
fn bar_cover_opens_now_playing_and_esc_closes() {
    let mut f = library();
    open_now_playing(&mut f);
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();
    assert!(!f.harness.state().now_playing_open());
    // Esc closed the view only; the album page is still there.
    assert!(matches!(
        f.harness.state().nav().current(),
        Page::Album { .. }
    ));
}

#[test]
fn close_button_closes_now_playing() {
    let mut f = library();
    open_now_playing(&mut f);
    f.harness
        .get_by_role_and_label(Role::Button, "Close now playing")
        .click();
    f.harness.run();
    assert!(!f.harness.state().now_playing_open());
}

#[test]
fn queue_row_plays_that_track() {
    let mut f = library();
    open_now_playing(&mut f);
    f.harness
        .get_by_role_and_label(Role::ListItem, "Blue in Green")
        .click();
    f.harness.run();
    match f.last_sent() {
        Some(PlayerCommand::Play { items, start }) => {
            assert_eq!(start, 2);
            assert_eq!(items, album_items(&f, "Kind of Blue"));
        }
        other => panic!("expected Play, got {other:?}"),
    }
}

#[test]
fn cannot_open_now_playing_without_a_queue() {
    let f = library();
    assert!(
        f.harness
            .query_by_role_and_label(Role::Button, "Open now playing")
            .is_none()
    );
}

#[test]
fn queue_finished_closes_now_playing() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player
        .inject(PlayerEvent::Engine(bp_engine::Event::QueueFinished));
    f.harness.run();
    assert!(!f.harness.state().now_playing_open());
}

#[test]
fn fatal_error_in_now_playing_shows_retry() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.inject(PlayerEvent::Engine(bp_engine::Event::Error(
        EngineError::NoDevice,
    )));
    f.harness.run();
    assert!(f.harness.state().now_playing_open());
    f.harness.get_by_label_contains("no audio device available");
    f.harness
        .get_by_role_and_label(Role::Button, "Retry")
        .click();
    f.harness.run();
    assert!(matches!(f.last_sent(), Some(PlayerCommand::Play { .. })));
}

#[test]
fn min_window_keeps_controls_and_queue() {
    let mut f = fixture_sized(tracks(), egui::vec2(900.0, 600.0), false);
    f.harness.run();
    open_now_playing(&mut f);
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
    for (role, label) in [
        (Role::Button, "Pause"),
        (Role::Button, "Next"),
        (Role::ListItem, "So What"),
    ] {
        let r = f.harness.get_by_role_and_label(role, label).rect();
        assert!(window.contains_rect(r), "{label} at {r:?}");
    }
}

fn spectrum_shown(f: &Fixture) -> bool {
    f.harness
        .query_by_role_and_label(Role::Image, "Spectrum")
        .is_some()
}

#[test]
fn spectrum_shows_in_bar_and_now_playing_while_playing() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.double_click_track("So What");
    f.advance(0.5);
    f.advance(0.2);
    assert!(spectrum_shown(&f), "in the bar");
    let levels = f.harness.state().spectrum().levels();
    assert!(levels.iter().any(|&l| l > 0.3), "{levels:?}");
    f.harness
        .get_by_role_and_label(Role::Button, "Open now playing")
        .click();
    f.harness.run();
    f.advance(0.2);
    assert!(spectrum_shown(&f), "in the now-playing view");
}

#[test]
fn spectrum_stops_repainting_when_paused() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.double_click_track("So What");
    f.advance(0.5);
    f.advance(0.2);
    f.harness.step();
    assert!(repaint_delay(&f.harness) <= Duration::from_millis(40));
    f.harness.key_press(egui::Key::Space);
    f.harness.run();
    // The bars fall for under a second of real time.
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(33));
        f.harness.step();
    }
    f.harness.step();
    let d = repaint_delay(&f.harness);
    assert!(d > Duration::from_millis(100), "{d:?}");
}

#[test]
fn mini_spectrum_keeps_clear_of_the_transport_at_minimum_width() {
    let mut devices = demo_devices();
    devices[0].info.name = "Speakers (Topping D90 USB Audio Device)".into();
    let name = devices[0].info.name.clone();
    let mut f = build(
        tracks(),
        devices,
        vec![PathBuf::from("/music")],
        None,
        None,
        egui::vec2(900.0, 600.0),
        false,
    );
    f.harness.run();
    f.open_album("Kind of Blue");
    f.double_click_track("So What");
    f.advance(0.5);
    f.advance(0.2);
    let next = f.harness.get_by_role_and_label(Role::Button, "Next").rect();
    let end = f.harness.get_by_label("5:00").rect();
    let device = f.harness.get_by_role_and_label(Role::Button, &name).rect();
    if let Some(s) = f.harness.query_by_role_and_label(Role::Image, "Spectrum") {
        let s = s.rect();
        for (what, r) in [("Next", next), ("end time", end), ("device", device)] {
            assert!(!s.intersects(r), "spectrum {s:?} overlaps {what} {r:?}");
        }
    }
}

#[test]
fn no_spectrum_without_a_queue() {
    let f = library();
    assert!(!spectrum_shown(&f));
}

#[test]
fn double_click_track_plays_rest_of_album() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.double_click_track("Blue in Green");
    let items = album_items(&f, "Kind of Blue");
    assert_eq!(items.len(), 5);
    assert_eq!(f.last_sent(), Some(PlayerCommand::Play { items, start: 2 }));
}

#[test]
fn enter_on_focused_track_plays_it() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.harness
        .get_by_role_and_label(Role::ListItem, "All Blues")
        .focus();
    f.harness.run();
    f.harness.key_press(egui::Key::Enter);
    f.harness.run();
    assert!(matches!(
        f.last_sent(),
        Some(PlayerCommand::Play { start: 3, .. })
    ));
}

#[test]
fn esc_returns_to_grid_at_same_scroll() {
    let many: Vec<_> = (0..40)
        .map(|i| st("Artist", &format!("Album {i:02}"), "t", 1))
        .collect();
    let mut f = fixture_with(many, vec![PathBuf::from("/music")], None);
    for _ in 0..4 {
        f.harness
            .get_all_by(|n| {
                n.role() == Role::Button && n.label().is_some_and(|l| l.starts_with("Album"))
            })
            .next()
            .unwrap()
            .scroll_down();
        f.harness.run();
    }
    let before = f
        .harness
        .get_by_role_and_label(Role::Button, "Album 14")
        .rect();
    assert!(
        f.harness
            .query_by_role_and_label(Role::Button, "Album 00")
            .is_none(),
        "the grid scrolled"
    );
    f.open_album("Album 14");
    assert!(matches!(
        f.harness.state().nav().current(),
        Page::Album { .. }
    ));
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();
    assert!(matches!(
        f.harness.state().nav().current(),
        Page::Home | Page::Albums | Page::Artist(_)
    ));
    let after = f
        .harness
        .get_by_role_and_label(Role::Button, "Album 14")
        .rect();
    assert_eq!(before, after);

    // The Back button and Backspace leave the page too.
    f.open_album("Album 14");
    f.harness.get_by_label("Back").click();
    f.harness.run();
    assert!(matches!(
        f.harness.state().nav().current(),
        Page::Home | Page::Albums | Page::Artist(_)
    ));
    f.open_album("Album 14");
    f.harness.key_press(egui::Key::Backspace);
    f.harness.run();
    assert!(matches!(
        f.harness.state().nav().current(),
        Page::Home | Page::Albums | Page::Artist(_)
    ));
}

fn select_device(f: &mut Fixture, current: &str, name: &str) {
    f.harness
        .get_by_role_and_label(Role::Button, current)
        .click();
    f.harness.run();
    // With the list open, the target is the row, not the bar button. Selecting closes
    // the list.
    f.harness
        .get_all_by_role_and_label(Role::Button, name)
        .last()
        .unwrap()
        .click();
    f.harness.run();
}

#[test]
fn unsupported_track_is_marked_for_selected_device() {
    let mut f = library();
    f.open_album("Test Tones");
    f.hover_long(Role::ListItem, "Tone 256");
    f.harness
        .get_by_label_contains("DAC A: DSD256 exceeds the device maximum");

    f.hover_long(Role::ListItem, "Tone 128");
    assert!(
        f.harness
            .query_by_label_contains("exceeds the device maximum")
            .is_none(),
        "DSD128 plays on DAC A"
    );

    select_device(&mut f, "DAC A", "DAC B");
    f.hover_long(Role::ListItem, "Tone 128");
    f.harness
        .get_by_label_contains("DAC B: DSD128 exceeds the device maximum");
}

#[test]
fn bar_shows_signal_path_and_badge() {
    // DAC A without packed 24-bit: 24-bit audio is padded into 32-bit samples.
    let mut devices = demo_devices();
    let packed_24 = SampleEncoding::Int {
        container: 24,
        valid: 24,
    };
    devices[0]
        .caps
        .caps
        .formats
        .retain(|(_, _, e)| *e != packed_24);
    let mut f = fixture_full(tracks(), devices, vec![PathBuf::from("/music")], None);
    f.harness.get_by_label_contains("DAC A · Idle");
    f.open_album("Kid A");
    f.double_click_track("Everything in Its Right Place");
    f.advance(0.4);
    f.harness.get_by_label("Bit-perfect");
    // The album header's chip and the bar's format line.
    assert_eq!(f.harness.get_all_by_label("FLAC 24/192").count(), 2);
    // The bar's lines; the tracklist row is a list item.
    f.harness
        .get_by_role_and_label(Role::Label, "Everything in Its Right Place");
    f.harness.get_by_label("Radiohead · Kid A");
    f.harness.get_by_label("0:00");
    // The bar's total and the album header's length.
    assert_eq!(f.harness.get_all_by_label("5:00").count(), 2);
    // The full signal path is the badge's tooltip.
    f.hover_long(Role::Label, "Bit-perfect");
    f.harness
        .get_by_label_contains("FLAC 24bit 192.0kHz ▸ DAC A 32bit ▸ Exclusive ▸ Bit-perfect");
    f.harness.get_by_role_and_label(Role::Button, "Pause");
    f.harness.get_by_role_and_label(Role::Button, "Previous");
    f.harness.get_by_role_and_label(Role::Button, "Next");
    f.harness.get_by_label("Seek").click();
    f.harness.run();
    let Some(PlayerCommand::Seek { position }) = f.last_sent() else {
        panic!("clicking the seek line seeks");
    };
    let mid = position.as_secs_f64();
    assert!((140.0..160.0).contains(&mid), "{mid}");

    f.player
        .inject(PlayerEvent::Engine(bp_engine::Event::Underrun { total: 3 }));
    f.harness.run();
    f.harness.get_by_label_contains("underruns: 3");
}

#[test]
fn skip_notice_in_bar() {
    let mut f = library();
    f.open_album("Test Tones");
    f.double_click_track("Tone 256");
    f.advance(0.4);
    f.harness
        .get_by_label_contains("Skipped: Tone 256 — DSD256 exceeds the device maximum");
    f.advance(5.0);
    assert!(f.harness.query_by_label_contains("Skipped:").is_none());
}

#[test]
fn fatal_error_shows_retry_and_retry_replays() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.double_click_track("Blue in Green");
    f.advance(0.4);
    f.player.inject(PlayerEvent::Engine(bp_engine::Event::Error(
        EngineError::NoDevice,
    )));
    f.harness.run();
    f.harness.get_by_label_contains("no audio device available");
    f.harness
        .get_by_role_and_label(Role::Button, "Retry")
        .click();
    f.harness.run();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::Play {
            items: album_items(&f, "Kind of Blue"),
            start: 2
        })
    );
    assert_eq!(f.harness.state().now_playing().fatal, None);
    assert!(f.harness.query_by_label("Retry").is_none());
}

#[test]
fn select_device_after_fatal_error_keeps_retry_and_goes_idle() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.double_click_track("Blue in Green");
    f.advance(0.4);
    // A fatal error stops the player.
    f.player.inject(PlayerEvent::Engine(bp_engine::Event::Error(
        EngineError::NoDevice,
    )));
    f.harness.run();
    select_device(&mut f, "DAC A", "DAC B");
    f.advance(1.0);
    f.harness.get_by_role_and_label(Role::Button, "Retry");
    assert_eq!(f.harness.state().now_playing().badge, Badge::Idle);
    f.harness.step();
    let d = repaint_delay(&f.harness);
    assert!(d > Duration::from_secs(1), "{d:?}");
}

#[test]
fn device_list_switches_device() {
    let mut f = library();
    f.harness
        .get_by_role_and_label(Role::Button, "DAC A")
        .click();
    f.harness.run();
    // Opening the list lists the devices again (one may have been plugged in).
    assert_eq!(f.last_sent(), Some(PlayerCommand::RefreshDevices));
    f.harness
        .get_all_by_role_and_label(Role::Button, "DAC B")
        .last()
        .unwrap()
        .click();
    f.harness.run();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::SelectDevice {
            id: DeviceId("dac-b".into())
        })
    );
    assert_eq!(f.harness.state().prefs().device.as_deref(), Some("dac-b"));
    assert_eq!(
        Prefs::load(&f.prefs_path()).device.as_deref(),
        Some("dac-b")
    );
    // Selecting closes the list.
    assert!(f.harness.query_by_label("Device capabilities…").is_none());
}

#[test]
fn device_list_has_no_table_and_links_to_settings() {
    let mut f = library();
    f.harness
        .get_by_role_and_label(Role::Button, "DAC A")
        .click();
    f.harness.run();
    assert!(f.harness.query_by_label("384").is_none());
    assert!(f.harness.query_by_label("Re-probe").is_none());
    f.harness.get_by_label("Device capabilities…").click();
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Settings);
    assert!(f.harness.query_by_label("Device capabilities…").is_none());
}

#[test]
fn settings_page_shows_capabilities_and_reprobes() {
    let mut f = library();
    f.harness.get_by_label("Settings").click();
    f.harness.run();
    f.harness.get_by_role_and_label(Role::Heading, "Audio");
    f.harness.get_by_label("Output device");
    // DAC A's table: 384 kHz and DSD256 are there.
    f.harness.get_by_label("384");
    f.harness.get_by_label("DSD256");
    f.harness.get_by_label("Re-probe").click();
    f.harness.run();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::Reprobe {
            id: DeviceId("dac-a".into())
        })
    );
}

#[test]
fn settings_output_dropdown_selects_device() {
    let mut f = library();
    f.harness.get_by_label("Settings").click();
    f.harness.run();
    f.harness.get_by_label("Output device").click();
    f.harness.run();
    f.harness.get_all_by_label("DAC B").last().unwrap().click();
    f.harness.run();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::SelectDevice {
            id: DeviceId("dac-b".into())
        })
    );
    assert_eq!(f.harness.state().prefs().device.as_deref(), Some("dac-b"));
    // The table now shows DAC B, which tops out at 192 kHz.
    assert!(f.harness.query_by_label("384").is_none());
    f.harness.get_by_label("192");
}

fn transport_sent(f: &Fixture) -> Vec<PlayerCommand> {
    f.player
        .sent()
        .into_iter()
        .filter(|c| !matches!(c, PlayerCommand::Play { .. }))
        .collect()
}

#[test]
fn keys_in_search_do_not_control_playback() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.double_click_track("So What");
    f.advance(0.4);
    let view = f.harness.state().nav().current().clone();

    f.harness.key_press_modifiers(CTRL, egui::Key::F);
    f.harness.run();
    assert!(f.harness.get_by_label("Search").is_focused());
    f.harness.key_press(egui::Key::Space);
    f.harness.get_by_label("Search").type_text(" ");
    f.harness.run();
    f.harness.key_press(egui::Key::ArrowRight);
    f.harness.run();
    assert_eq!(
        f.harness.get_by_label("Search").value().as_deref(),
        Some(" ")
    );
    f.harness.key_press(egui::Key::Backspace);
    f.harness.run();
    assert_eq!(
        f.harness.get_by_label("Search").value().as_deref(),
        Some("")
    );
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();

    assert_eq!(transport_sent(&f), vec![]);
    assert_eq!(f.harness.state().nav().current(), &view);
}

#[test]
fn space_and_ctrl_arrows_control_playback() {
    let mut f = library();
    f.open_album("Kind of Blue");
    // Idle on an album page: Space plays it from the top.
    f.harness.key_press(egui::Key::Space);
    f.harness.run();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::Play {
            items: album_items(&f, "Kind of Blue"),
            start: 0
        })
    );
    f.advance(0.4);
    f.harness.key_press(egui::Key::Space);
    f.harness.run();
    assert_eq!(f.last_sent(), Some(PlayerCommand::Pause));
    f.harness.get_by_role_and_label(Role::Button, "Play");
    f.harness.key_press(egui::Key::Space);
    f.harness.run();
    assert_eq!(f.last_sent(), Some(PlayerCommand::Resume));

    f.harness.key_press_modifiers(CTRL, egui::Key::ArrowRight);
    f.harness.run();
    assert_eq!(f.last_sent(), Some(PlayerCommand::Next));
    f.harness.key_press_modifiers(CTRL, egui::Key::ArrowLeft);
    f.harness.run();
    assert_eq!(f.last_sent(), Some(PlayerCommand::Previous));

    f.advance(1.0);
    let pos = f.harness.state().now_playing().position;
    f.harness.key_press(egui::Key::ArrowRight);
    f.harness.run();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::Seek {
            position: pos + Duration::from_secs(10)
        })
    );
    f.harness.key_press(egui::Key::ArrowLeft);
    f.harness.key_press(egui::Key::ArrowLeft);
    f.harness.run();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::Seek {
            position: Duration::ZERO
        })
    );
}

#[test]
fn transport_keys_do_nothing_without_a_track() {
    let mut f = library();
    f.harness.key_press(egui::Key::Space);
    f.harness.key_press_modifiers(CTRL, egui::Key::ArrowRight);
    f.harness.key_press(egui::Key::ArrowRight);
    f.harness.run();
    assert_eq!(f.player.sent(), vec![]);
}

#[test]
fn long_title_does_not_widen_bar() {
    let title = "A".repeat(300);
    let mut f = fixture_with(
        vec![st("Artist", "Album", &title, 1)],
        vec![PathBuf::from("/music")],
        None,
    );
    f.open_album("Album");
    f.double_click_track(&title);
    f.advance(0.4);
    let bar = f.harness.get_by_role_and_label(Role::Group, "Now playing");
    assert_eq!(bar.rect().width(), 1280.0);
    let line = f
        .harness
        .get_by(|n| n.role() == Role::Label && n.value().is_some_and(|l| l.starts_with("AAAA")));
    let text = line.accesskit_node().value().unwrap();
    assert!(text.ends_with('…'), "{text}");
    assert!(line.rect().right() <= 1280.0);
}

#[test]
fn track_error_is_marked_on_its_row() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.double_click_track("So What");
    f.advance(0.4);
    let path = album_items(&f, "Kind of Blue")[0].path.clone();
    f.player
        .inject(PlayerEvent::Engine(bp_engine::Event::TrackError {
            index: 0,
            path,
            error: bp_core::DecodeError::Corrupt("bad frame".into()),
        }));
    f.harness.run();
    f.hover_long(Role::ListItem, "So What");
    f.harness.get_by_label_contains("corrupt file: bad frame");
}

#[test]
fn skip_notice_shows_when_the_last_track_is_skipped() {
    let mut f = library();
    f.open_album("Test Tones");
    select_device(&mut f, "DAC A", "DAC B");
    f.double_click_track("Tone 128");
    f.advance(0.4);
    assert_eq!(
        f.harness.state().now_playing().index,
        None,
        "queue finished"
    );
    f.harness.get_by_label_contains("Skipped: Tone 128");
}

#[test]
fn idle_skip_notice_schedules_its_own_expiry() {
    let mut f = library();
    f.open_album("Test Tones");
    select_device(&mut f, "DAC A", "DAC B");
    f.double_click_track("Tone 128");
    f.advance(0.4);
    f.advance(1.0);
    assert_eq!(f.harness.state().now_playing().index, None);
    f.harness.get_by_label_contains("Skipped: Tone 128");
    f.harness.step();
    let d = repaint_delay(&f.harness);
    assert!(d <= Duration::from_secs(3), "{d:?}");
}

#[test]
fn arrows_seek_only_without_a_focused_widget() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.double_click_track("So What");
    f.advance(1.0);
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();
    assert!(matches!(
        f.harness.state().nav().current(),
        Page::Home | Page::Albums | Page::Artist(_)
    ));

    // A focused text field keeps its arrow keys.
    f.harness.get_by_label("Search").focus();
    f.harness.run();
    f.harness.key_press(egui::Key::ArrowRight);
    f.harness.run();
    assert!(
        !transport_sent(&f)
            .iter()
            .any(|c| matches!(c, PlayerCommand::Seek { .. }))
    );

    // Nothing focused: the arrow seeks.
    f.harness
        .ctx
        .memory_mut(|m| m.surrender_focus(m.focused().unwrap()));
    f.harness.run();
    let pos = f.harness.state().now_playing().position;
    f.harness.key_press(egui::Key::ArrowRight);
    f.harness.run();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::Seek {
            position: pos + Duration::from_secs(10)
        })
    );
}

#[test]
fn saved_device_is_restored_at_startup() {
    let prefs = Prefs {
        device: Some("dac-b".into()),
    };
    let mut f = fixture_prefs(
        tracks(),
        demo_devices(),
        vec![PathBuf::from("/music")],
        None,
        Some(prefs),
    );
    f.harness.run();
    assert_eq!(f.player.devices().1, Some(DeviceId("dac-b".into())));
    assert_eq!(
        f.player.sent(),
        vec![PlayerCommand::SelectDevice {
            id: DeviceId("dac-b".into())
        }]
    );
    f.harness.get_by_role_and_label(Role::Button, "DAC B");

    let prefs = Prefs {
        device: Some("gone".into()),
    };
    let mut f = fixture_prefs(
        tracks(),
        demo_devices(),
        vec![PathBuf::from("/music")],
        None,
        Some(prefs),
    );
    f.harness.run();
    assert_eq!(f.player.sent(), vec![]);
}

#[test]
fn keyboard_seek_clamps_to_track_length_without_engine_duration() {
    let mut short = st("Artist", "Album", "Short", 1);
    short.duration = Duration::from_secs(5);
    let mut f = fixture_with(vec![short], vec![PathBuf::from("/music")], None);
    f.open_album("Album");
    f.double_click_track("Short");
    // The engine may not know the length.
    let item = album_items(&f, "Album").remove(0);
    f.player
        .inject(PlayerEvent::Engine(bp_engine::Event::TrackStarted {
            index: 0,
            path: item.path,
            playback: bp_core::PlaybackPath {
                source: item.source,
                device: bp_core::DeviceFormat {
                    sample_rate: 44_100,
                    channels: 2,
                    encoding: SampleEncoding::Int {
                        container: 16,
                        valid: 16,
                    },
                },
                kind: bp_core::PathKind::Native,
            },
            duration: None,
        }));
    f.harness.run();
    f.harness.key_press(egui::Key::ArrowRight);
    f.harness.run();
    let Some(PlayerCommand::Seek { position }) = f.last_sent() else {
        panic!("seek sent");
    };
    assert!(position <= Duration::from_secs(5), "{position:?}");
}

// Settings page, driven directly with no devices; the app routing is covered by
// `gear_opens_settings` and the settings page tests above.

fn no_devices() -> bp_gui::views::settings::SettingsData<'static> {
    bp_gui::views::settings::SettingsData {
        devices: &[],
        selected: None,
        table: None,
    }
}

fn settings_harness<'a>(lib: &'a Arc<MemLibrary>, picked: &'a mut Vec<PathBuf>) -> Harness<'a, ()> {
    themed().build_ui(move |ui| {
        bp_gui::views::settings::show(ui, &no_devices(), lib.as_ref(), &mut || picked.pop());
    })
}

/// A harness with the app's theme, whose fonts the views name.
fn themed() -> egui_kittest::HarnessBuilder {
    Harness::builder().with_context_setup(bp_gui::theme::apply)
}

#[test]
fn settings_lists_missing_folder() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("gone");
    let lib = Arc::new(MemLibrary::new(
        Vec::new(),
        vec![dir.path().to_path_buf(), gone.clone()],
    ));
    let mut picked = Vec::new();
    let mut h = settings_harness(&lib, &mut picked);
    h.run();
    h.get_by_label(&gone.display().to_string());
    h.get_by_label(&dir.path().display().to_string());
    assert_eq!(h.get_all_by_label("(missing)").count(), 1);
    assert_eq!(h.get_all_by_label("Remove").count(), 2);
    h.get_by_label_contains("bitperfect ");
}

#[test]
fn settings_add_remove_rescan() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Arc::new(MemLibrary::new(Vec::new(), Vec::new()));
    let mut picked = vec![dir.path().to_path_buf()];
    let mut h = settings_harness(&lib, &mut picked);
    h.run();
    h.get_by_label("Add folder…").click();
    h.run();
    assert_eq!(lib.folders(), vec![dir.path().to_path_buf()]);
    h.get_by_label("Rescan").click();
    h.run();
    assert_eq!(lib.rescan_count(), 1);
    h.get_by_label("Remove").click();
    h.run();
    assert!(lib.folders().is_empty());
}

#[test]
fn settings_reports_library_changes() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Arc::new(MemLibrary::new(Vec::new(), Vec::new()));
    let mut picked = vec![dir.path().to_path_buf()];
    let changed = std::cell::Cell::new(false);
    let mut h = themed().build_ui(|ui| {
        let out =
            bp_gui::views::settings::show(ui, &no_devices(), lib.as_ref(), &mut || picked.pop());
        changed.set(changed.get() | out.library_changed);
    });
    h.run();
    assert!(!changed.get(), "showing the page changes nothing");
    for button in ["Add folder…", "Rescan", "Remove"] {
        h.get_by_label(button).click();
        h.run();
        assert!(changed.replace(false), "{button}");
    }
}

#[test]
fn settings_cancelled_picker_adds_nothing() {
    let lib = Arc::new(MemLibrary::new(Vec::new(), Vec::new()));
    let mut picked = Vec::new();
    let mut h = settings_harness(&lib, &mut picked);
    h.run();
    h.get_by_label("Add folder…").click();
    h.run();
    assert!(lib.folders().is_empty());
}

#[test]
fn settings_reports_unreadable() {
    let lib = Arc::new(MemLibrary::new(Vec::new(), Vec::new()));
    lib.set_scan_status(ScanStatus::Done { unreadable: 3 });
    let mut picked = Vec::new();
    let mut h = settings_harness(&lib, &mut picked);
    h.run();
    h.get_by_label("3 files couldn't be read");
}

#[test]
fn settings_lists_unreadable_files() {
    let lib = Arc::new(MemLibrary::new(Vec::new(), Vec::new()));
    lib.set_scan_status(ScanStatus::Done { unreadable: 2 });
    lib.set_unreadable(vec![
        (
            PathBuf::from("/m/a/x.m4a"),
            "unknown or unsupported file format".into(),
        ),
        (
            PathBuf::from("/m/b/y.flac"),
            "corrupt file: bad header".into(),
        ),
    ]);
    let mut picked = Vec::new();
    let mut h = settings_harness(&lib, &mut picked);
    h.run();
    assert!(h.query_by_label("x.m4a").is_none());
    h.get_by_label("2 files couldn't be read").click();
    h.run();
    h.get_by_label("x.m4a");
    h.get_by_label("y.flac");
    h.get_by_label_contains("unknown or unsupported file format");
    h.get_by_label_contains("corrupt file: bad header");
}

#[test]
fn settings_disables_rescan_while_scanning() {
    let lib = Arc::new(MemLibrary::new(Vec::new(), Vec::new()));
    lib.set_scan_status(ScanStatus::Scanning { done: 4, total: 9 });
    let mut picked = Vec::new();
    let mut h = settings_harness(&lib, &mut picked);
    h.run();
    h.get_by_label("Scanning 4 / 9");
    h.get_by_label("Rescan").click();
    h.run();
    assert_eq!(lib.rescan_count(), 0);
}

fn settings_repaint_delay(status: ScanStatus) -> Duration {
    let lib = Arc::new(MemLibrary::new(Vec::new(), Vec::new()));
    lib.set_scan_status(status);
    let mut picked = Vec::new();
    let mut h = settings_harness(&lib, &mut picked);
    h.run();
    h.output().viewport_output[&egui::ViewportId::ROOT].repaint_delay
}

#[test]
fn settings_idle_schedules_no_repaint() {
    let d = settings_repaint_delay(ScanStatus::Done { unreadable: 0 });
    assert!(d > Duration::from_secs(1), "{d:?}");
}

#[test]
fn settings_scanning_repaints_within_250ms() {
    let d = settings_repaint_delay(ScanStatus::Scanning { done: 1, total: 2 });
    assert!(d <= Duration::from_millis(250), "{d:?}");
}

fn dac_b_prefs() -> Prefs {
    Prefs {
        device: Some("dac-b".into()),
    }
}

fn dac_b() -> DeviceId {
    DeviceId("dac-b".into())
}

#[test]
fn saved_device_is_restored_when_devices_arrive_late() {
    let mut f = fixture_prefs(
        tracks(),
        Vec::new(),
        vec![PathBuf::from("/music")],
        None,
        Some(dac_b_prefs()),
    );
    f.harness.run();
    assert_eq!(f.last_sent(), None);
    f.player.set_devices(demo_devices());
    f.harness.run();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::SelectDevice { id: dac_b() })
    );
}

#[test]
fn a_late_devices_changed_does_not_undo_the_users_choice() {
    let mut f = fixture_prefs(
        tracks(),
        Vec::new(),
        vec![PathBuf::from("/music")],
        None,
        Some(dac_b_prefs()),
    );
    f.harness.run();
    f.player.set_devices(demo_devices());
    f.harness.run();
    select_device(&mut f, "DAC B", "DAC A");
    let sent = f.player.sent().len();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::SelectDevice {
            id: DeviceId("dac-a".into())
        })
    );
    // A DevicesChanged queued before the engine handled the user's choice: the
    // player still reports B.
    f.player.inject(PlayerEvent::DevicesChanged);
    f.harness.run();
    assert_eq!(f.player.sent().len(), sent, "no further commands");
}

/// Clicks the button labelled `label` and returns the window commands of that frame.
fn window_commands(f: &mut Fixture, label: &str) -> Vec<egui::ViewportCommand> {
    f.harness.get_by_role_and_label(Role::Button, label).click();
    f.harness.step();
    f.harness.output().viewport_output[&egui::ViewportId::ROOT]
        .commands
        .clone()
}

fn set_maximized(f: &mut Fixture, maximized: bool) {
    f.harness
        .input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .maximized = Some(maximized);
    f.harness.run();
}

#[test]
fn window_buttons_minimize_maximize_and_close() {
    let mut f = library();
    let sent = window_commands(&mut f, "Minimize");
    assert!(
        sent.contains(&egui::ViewportCommand::Minimized(true)),
        "{sent:?}"
    );
    let sent = window_commands(&mut f, "Maximize");
    assert!(
        sent.contains(&egui::ViewportCommand::Maximized(true)),
        "{sent:?}"
    );
    let sent = window_commands(&mut f, "Close");
    assert!(sent.contains(&egui::ViewportCommand::Close), "{sent:?}");
}

#[test]
fn maximized_window_shows_restore() {
    let mut f = library();
    set_maximized(&mut f, true);
    assert!(f.harness.query_by_label("Maximize").is_none());
    let sent = window_commands(&mut f, "Restore");
    assert!(
        sent.contains(&egui::ViewportCommand::Maximized(false)),
        "{sent:?}"
    );
}

#[test]
fn now_playing_has_the_window_buttons() {
    let mut f = library();
    open_now_playing(&mut f);
    let sent = window_commands(&mut f, "Minimize");
    assert!(
        sent.contains(&egui::ViewportCommand::Minimized(true)),
        "{sent:?}"
    );
    f.harness.get_by_role_and_label(Role::Button, "Maximize");
    f.harness.get_by_role_and_label(Role::Button, "Close");
}
