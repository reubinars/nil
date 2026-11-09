//! Whole-app UI tests: the real `BpApp` driven through egui_kittest.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bp_core::{DeviceId, EngineError, SampleEncoding};
use bp_gui::app::BpApp;
use bp_gui::backend::index::MemLibrary;
use bp_gui::backend::mock_player::demo_devices;
use bp_gui::backend::{
    AlbumId, LibrarySource, PlayerCommand, PlayerEvent, PlayerHandle, ScanStatus,
};
use bp_gui::media_session::{self as media, Button, Track, Update};
use bp_gui::plugins::rpc::{Call, RpcError};
use bp_gui::prefs::Prefs;
use bp_gui::state::nav::Page;
use bp_gui::state::now_playing::Badge;
use bp_gui::state::selection::ListId;
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
        &Page::Album { album: kob }
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
fn search_track_click_selects_it() {
    let mut f = library();
    type_search(&mut f, "green");
    f.harness
        .get_by_role_and_label(Role::ListItem, "Blue in Green")
        .click();
    f.harness.run();
    assert_eq!(
        f.harness.state().nav().current(),
        &Page::Search("green".into())
    );
    let list = ListId::SearchTracks("green".into());
    assert_eq!(f.harness.state().selection().rows(&list, &[0]), vec![0]);
}

#[test]
fn go_to_album_from_search_selects_the_track() {
    let mut f = library();
    let kob = album_id(&f, "Kind of Blue");
    type_search(&mut f, "green");
    f.right_click_track("Blue in Green");
    f.menu_item("Go to album");
    assert_eq!(
        f.harness.state().nav().current(),
        &Page::Album { album: kob }
    );
    // The selection survives the next frames.
    f.harness.run();
    let shown: Vec<usize> = (0..5).collect();
    assert_eq!(
        f.harness
            .state()
            .selection()
            .rows(&ListId::Album(kob), &shown),
        vec![2]
    );
}

#[test]
fn search_track_right_click_shows_go_to_album() {
    let mut f = library();
    type_search(&mut f, "green");
    f.right_click_track("Blue in Green");
    assert_eq!(
        f.menu_labels(),
        [
            "Play",
            "Play next",
            "Add to queue",
            "Add to playlist",
            "Go to album",
            "Go to artist"
        ]
    );
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
fn queue_row_double_click_plays_that_track() {
    let mut f = library();
    open_now_playing(&mut f);
    f.double_click_track("Blue in Green");
    match f.last_sent() {
        Some(PlayerCommand::Play { items, start }) => {
            assert_eq!(start, 2);
            assert_eq!(items, album_items(&f, "Kind of Blue"));
        }
        other => panic!("expected Play, got {other:?}"),
    }
}

#[test]
fn queue_row_click_selects_it() {
    let mut f = library();
    open_now_playing(&mut f);
    let before = f.player.sent().len();
    f.click_track_with("Blue in Green", Modifiers::NONE);
    assert_eq!(f.player.sent().len(), before);
    let shown: Vec<usize> = (0..5).collect();
    assert_eq!(
        f.harness.state().selection().rows(&ListId::Queue, &shown),
        vec![2]
    );
}

#[test]
fn upcoming_queue_row_menu() {
    let mut f = library();
    open_now_playing(&mut f);
    f.right_click_track("Blue in Green");
    assert_eq!(
        f.menu_labels(),
        [
            "Play from here",
            "Play next",
            "Add to playlist",
            "Remove from queue",
            "Go to album"
        ]
    );
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();
    // Already next: Play next would change nothing.
    f.right_click_track("Freddie Freeloader");
    assert_eq!(
        f.menu_labels(),
        [
            "Play from here",
            "Add to playlist",
            "Remove from queue",
            "Go to album"
        ]
    );
}

#[test]
fn current_queue_row_menu_is_unchanged() {
    let mut f = library();
    open_now_playing(&mut f);
    f.right_click_track("So What");
    assert_eq!(
        f.menu_labels(),
        ["Play from here", "Add to playlist", "Go to album"]
    );
}

#[test]
fn remove_from_queue_removes_the_row() {
    let mut f = library();
    open_now_playing(&mut f);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    let all = album_items(&f, "Kind of Blue");
    let rearrange = f
        .player
        .sent()
        .into_iter()
        .rfind(|c| matches!(c, PlayerCommand::Rearrange { .. }));
    assert_eq!(
        rearrange,
        Some(PlayerCommand::Rearrange {
            from: 1,
            expected: all[1..5].to_vec(),
            items: vec![all[1].clone(), all[3].clone(), all[4].clone()],
        })
    );
    f.harness.run();
    assert_eq!(
        queue_titles(&f),
        [
            "So What",
            "Freddie Freeloader",
            "All Blues",
            "Flamenco Sketches"
        ]
    );
    assert_eq!(np_notice(&f).as_deref(), Some("Removed 1 track"));
    f.harness.get_by_role_and_label(Role::Button, "Undo");
}

fn rearranges(f: &Fixture) -> Vec<PlayerCommand> {
    f.player
        .sent()
        .into_iter()
        .filter(|c| matches!(c, PlayerCommand::Rearrange { .. }))
        .collect()
}

#[test]
fn delete_removes_selected_upcoming_rows() {
    let mut f = library();
    open_now_playing(&mut f);
    f.click_track_with("Blue in Green", Modifiers::NONE);
    f.click_track_with("Flamenco Sketches", Modifiers::SHIFT);
    f.harness.key_press(egui::Key::Delete);
    f.harness.run();
    let all = album_items(&f, "Kind of Blue");
    assert_eq!(
        rearranges(&f),
        [PlayerCommand::Rearrange {
            from: 1,
            expected: all[1..5].to_vec(),
            items: vec![all[1].clone()],
        }]
    );
    assert_eq!(queue_titles(&f), ["So What", "Freddie Freeloader"]);
    assert_eq!(np_notice(&f).as_deref(), Some("Removed 3 tracks"));
}

#[test]
fn delete_with_all_rows_selected_removes_only_upcoming() {
    let mut f = library();
    open_now_playing(&mut f);
    f.harness.key_press_modifiers(CTRL, egui::Key::A);
    f.harness.run();
    f.harness.key_press(egui::Key::Delete);
    f.harness.run();
    assert_eq!(queue_titles(&f), ["So What"]);
    let sent = rearranges(&f).len();
    // Only the playing row is selected now: nothing to remove.
    f.click_track_with("So What", Modifiers::NONE);
    f.harness.key_press(egui::Key::Delete);
    f.harness.run();
    assert_eq!(rearranges(&f).len(), sent);
    assert_eq!(queue_titles(&f), ["So What"]);
}

#[test]
fn backspace_still_closes_now_playing() {
    let mut f = library();
    open_now_playing(&mut f);
    f.click_track_with("Blue in Green", Modifiers::NONE);
    f.harness.key_press(egui::Key::Backspace);
    f.harness.run();
    assert!(!f.harness.state().now_playing_open());
    assert!(rearranges(&f).is_empty());
}

#[test]
fn play_next_on_a_queue_row() {
    let mut f = library();
    open_now_playing(&mut f);
    f.right_click_track("All Blues");
    f.menu_item("Play next");
    f.harness.run();
    assert_eq!(
        queue_titles(&f),
        [
            "So What",
            "All Blues",
            "Freddie Freeloader",
            "Blue in Green",
            "Flamenco Sketches"
        ]
    );
    assert_eq!(np_notice(&f).as_deref(), Some("Playing 1 track next"));
}

#[test]
fn remove_in_the_last_two_seconds_says_already_starting() {
    let mut f = library();
    open_now_playing(&mut f);
    f.advance(298.5);
    f.right_click_track("Freddie Freeloader");
    f.menu_item("Remove from queue");
    f.harness.run();
    assert_eq!(
        np_notice(&f).as_deref(),
        Some("Already starting: Freddie Freeloader")
    );
    assert_eq!(queue_titles(&f), KIND_OF_BLUE);
    assert!(
        f.harness
            .query_by_role_and_label(Role::Button, "Undo")
            .is_none()
    );
}

#[test]
fn edit_answered_after_play_is_ignored() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    let kid_a = album_items(&f, "Kid A");
    f.harness.state_mut().send(PlayerCommand::Play {
        items: kid_a.clone(),
        start: 0,
    });
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(f.harness.state().now_playing().queue, kid_a);
    assert!(np_notice(&f).is_none_or(|n| !n.starts_with("Removed")));
    assert!(
        f.harness
            .query_by_role_and_label(Role::Button, "Undo")
            .is_none()
    );
}

#[test]
fn edit_waits_for_a_pending_insert() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    let kid_a = album_items(&f, "Kid A");
    f.harness.state_mut().send(PlayerCommand::Insert {
        items: kid_a.clone(),
    });
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    let rearranges = |f: &Fixture| -> Vec<PlayerCommand> {
        f.player
            .sent()
            .into_iter()
            .filter(|c| matches!(c, PlayerCommand::Rearrange { .. }))
            .collect()
    };
    assert!(rearranges(&f).is_empty());
    f.player.hold_events(false);
    f.harness.run();
    let all = album_items(&f, "Kind of Blue");
    let sent = rearranges(&f);
    assert_eq!(sent.len(), 1, "{sent:?}");
    let PlayerCommand::Rearrange {
        from,
        expected,
        items,
    } = &sent[0]
    else {
        unreachable!()
    };
    assert_eq!(*from, 1);
    assert_eq!(expected[0], kid_a[0]);
    assert_eq!(expected[1..], all[1..]);
    assert_eq!(
        *items,
        vec![
            kid_a[0].clone(),
            all[1].clone(),
            all[3].clone(),
            all[4].clone()
        ]
    );
    assert_eq!(
        queue_titles(&f),
        [
            "So What",
            "Everything in Its Right Place",
            "Freddie Freeloader",
            "All Blues",
            "Flamenco Sketches"
        ]
    );
}

#[test]
fn edit_held_behind_an_edit_follows_its_answer() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    f.right_click_track("All Blues");
    f.menu_item("Remove from queue");
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(
        queue_titles(&f),
        ["So What", "Freddie Freeloader", "Flamenco Sketches"]
    );
    assert_eq!(np_notice(&f).as_deref(), Some("Removed 1 track"));
}

#[test]
fn edit_held_on_a_row_the_answered_edit_removed_is_dropped() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    f.right_click_track("Blue in Green");
    f.menu_item("Play next");
    f.player.hold_events(false);
    f.harness.run();
    let rearranges = f
        .player
        .sent()
        .into_iter()
        .filter(|c| matches!(c, PlayerCommand::Rearrange { .. }))
        .count();
    assert_eq!(rearranges, 1);
    assert_eq!(
        queue_titles(&f),
        [
            "So What",
            "Freddie Freeloader",
            "All Blues",
            "Flamenco Sketches"
        ]
    );
    assert_eq!(np_notice(&f).as_deref(), Some("Removed 1 track"));
}

#[test]
fn removing_two_rows_says_two_tracks() {
    let mut f = library();
    open_now_playing(&mut f);
    f.click_track_with("Blue in Green", Modifiers::NONE);
    f.click_track_with("Freddie Freeloader", Modifiers::COMMAND);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove 2 tracks");
    f.harness.run();
    assert_eq!(
        queue_titles(&f),
        ["So What", "All Blues", "Flamenco Sketches"]
    );
    assert_eq!(np_notice(&f).as_deref(), Some("Removed 2 tracks"));
}

#[test]
fn edit_of_a_queue_that_changed_says_try_again() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    // The player's queue grows without the app having seen it yet.
    let kid_a = album_items(&f, "Kid A");
    f.player.send(PlayerCommand::Append {
        items: kid_a[..1].to_vec(),
    });
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(
        np_notice(&f).as_deref(),
        Some("The queue changed. Try again.")
    );
    assert_eq!(queue_titles(&f)[2], "Blue in Green");
    assert!(
        f.harness
            .query_by_role_and_label(Role::Button, "Undo")
            .is_none()
    );
}

#[test]
fn an_answer_with_no_edit_pending_is_ignored() {
    let mut f = library();
    open_now_playing(&mut f);
    f.click_track_with("Blue in Green", Modifiers::NONE);
    f.player
        .inject(PlayerEvent::Engine(bp_engine::Event::Rearranged {
            from: 1,
        }));
    f.harness.run();
    assert_eq!(queue_titles(&f), KIND_OF_BLUE);
    assert_eq!(queue_rows(&f), vec![2]);
    assert!(np_notice(&f).is_none());
}

fn clear_upcoming(f: &mut Fixture) {
    f.harness
        .get_by_role_and_label(Role::Button, "Clear upcoming")
        .click();
    f.harness.run();
}

#[test]
fn clear_upcoming_pill_clears() {
    let mut f = library();
    open_now_playing(&mut f);
    clear_upcoming(&mut f);
    let all = album_items(&f, "Kind of Blue");
    assert_eq!(
        rearranges(&f),
        [PlayerCommand::Rearrange {
            from: 1,
            expected: all[1..5].to_vec(),
            items: vec![],
        }]
    );
    assert_eq!(queue_titles(&f), ["So What"]);
    assert_eq!(np_notice(&f).as_deref(), Some("Cleared 4 tracks"));
    assert!(
        f.harness
            .query_by_role_and_label(Role::Button, "Clear upcoming")
            .is_none()
    );
}

#[test]
fn clear_in_the_last_two_seconds_keeps_the_starting_track() {
    let mut f = library();
    open_now_playing(&mut f);
    f.advance(298.5);
    clear_upcoming(&mut f);
    assert_eq!(queue_titles(&f), ["So What", "Freddie Freeloader"]);
    assert_eq!(np_notice(&f).as_deref(), Some("Cleared 3 tracks"));
}

#[test]
fn ctrl_z_undoes_a_queue_remove() {
    let mut f = library();
    open_now_playing(&mut f);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    f.harness.run();
    assert_eq!(queue_titles(&f).len(), 4);
    // Row 2 is All Blues now; after the undo it would be Blue in Green.
    f.click_track_with("All Blues", Modifiers::NONE);
    assert_eq!(queue_rows(&f), vec![2]);
    f.harness.key_press_modifiers(CTRL, egui::Key::Z);
    f.harness.run();
    assert!(queue_rows(&f).is_empty());
    let all = album_items(&f, "Kind of Blue");
    assert_eq!(
        rearranges(&f).last(),
        Some(&PlayerCommand::Rearrange {
            from: 1,
            expected: vec![all[1].clone(), all[3].clone(), all[4].clone()],
            items: all[1..5].to_vec(),
        })
    );
    assert_eq!(queue_titles(&f), KIND_OF_BLUE);
    assert!(np_notice(&f).is_none());
}

#[test]
fn undo_keeps_tracks_appended_since() {
    let mut f = library();
    open_now_playing(&mut f);
    f.click_track_with("Blue in Green", Modifiers::NONE);
    f.click_track_with("Freddie Freeloader", Modifiers::COMMAND);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove 2 tracks");
    f.harness.run();
    assert_eq!(
        queue_titles(&f),
        ["So What", "All Blues", "Flamenco Sketches"]
    );
    let paths = vec![
        PathBuf::from("/music/Radiohead/Kid A/Everything in Its Right Place.flac"),
        PathBuf::from("/music/Test Artist/Test Tones/Tone 128.flac"),
    ];
    let result = f
        .harness
        .state_mut()
        .answer_plugin_call(Call::PlayerAppend { paths });
    assert!(result.is_ok(), "{result:?}");
    f.harness.run();
    assert_eq!(queue_titles(&f).len(), 5);
    f.harness.key_press_modifiers(CTRL, egui::Key::Z);
    f.harness.run();
    let mut want: Vec<&str> = KIND_OF_BLUE.to_vec();
    want.extend(["Everything in Its Right Place", "Tone 128"]);
    assert_eq!(queue_titles(&f), want);
}

#[test]
fn undo_after_the_track_changed_is_refused() {
    let mut f = library();
    open_now_playing(&mut f);
    f.right_click_track("Flamenco Sketches");
    f.menu_item("Remove from queue");
    f.harness.run();
    f.advance(301.0);
    assert_eq!(f.harness.state().now_playing().index, Some(1));
    let sent = f.player.sent().len();
    f.harness.key_press_modifiers(CTRL, egui::Key::Z);
    f.harness.run();
    assert_eq!(
        np_notice(&f).as_deref(),
        Some("The queue moved on. Can't undo.")
    );
    assert_eq!(f.player.sent().len(), sent);
}

#[test]
fn closing_now_playing_drops_the_queue_undo() {
    let mut f = library();
    open_now_playing(&mut f);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    f.harness.run();
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();
    assert!(!f.harness.state().now_playing_open());
    f.harness
        .get_by_role_and_label(Role::Button, "Open now playing")
        .click();
    f.harness.run();
    assert!(f.harness.state().now_playing_open());
    let sent = f.player.sent().len();
    f.harness.key_press_modifiers(CTRL, egui::Key::Z);
    f.harness.run();
    assert_eq!(f.player.sent().len(), sent);
    assert_eq!(queue_titles(&f).len(), 4);
}

#[test]
fn an_edit_answered_after_closing_now_playing_offers_no_undo() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    f.harness
        .get_by_role_and_label(Role::Button, "Close now playing")
        .click();
    f.harness.run();
    assert!(!f.harness.state().now_playing_open());
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(queue_titles(&f).len(), 4);
    f.harness.get_by_label("Removed 1 track");
    assert!(
        f.harness
            .query_by_role_and_label(Role::Button, "Undo")
            .is_none()
    );
}

#[test]
fn queue_row_go_to_album_opens_it() {
    let mut f = library();
    let kob = album_id(&f, "Kind of Blue");
    open_now_playing(&mut f);
    f.right_click_track("Blue in Green");
    f.menu_item("Go to album");
    assert!(!f.harness.state().now_playing_open());
    assert_eq!(
        f.harness.state().nav().current(),
        &Page::Album { album: kob }
    );
    f.harness.run();
    let shown: Vec<usize> = (0..5).collect();
    assert_eq!(
        f.harness
            .state()
            .selection()
            .rows(&ListId::Album(kob), &shown),
        vec![2]
    );
}

#[test]
fn queue_rows_add_to_playlist() {
    let mut f = library();
    let files = mix_with_one_file(&f);
    open_now_playing(&mut f);
    f.click_track_with("Blue in Green", Modifiers::NONE);
    f.click_track_with("Freddie Freeloader", Modifiers::COMMAND);
    f.right_click_track("Blue in Green");
    assert_eq!(
        f.menu_labels(),
        ["Play from here", "Add to playlist", "Remove 2 tracks"]
    );
    f.menu_item("Add to playlist");
    f.menu_item("Mix");
    let all = album_items(&f, "Kind of Blue");
    assert_eq!(
        mix_paths(&f),
        vec![files[0].clone(), all[1].path.clone(), all[2].path.clone()]
    );
}

#[test]
fn queue_selection_is_cleared_when_a_new_queue_plays() {
    let mut f = library();
    open_now_playing(&mut f);
    f.click_track_with("Blue in Green", Modifiers::NONE);
    f.right_click_track("Blue in Green");
    f.menu_item("Play from here");
    assert!(f.harness.state().selection().is_empty());
}

#[test]
fn ctrl_a_selects_the_whole_queue() {
    let mut f = library();
    open_now_playing(&mut f);
    f.harness.key_press_modifiers(CTRL, egui::Key::A);
    f.harness.run();
    let shown: Vec<usize> = (0..5).collect();
    assert_eq!(
        f.harness.state().selection().rows(&ListId::Queue, &shown),
        shown
    );
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

fn album_id(f: &Fixture, title: &str) -> AlbumId {
    f.lib
        .albums(None)
        .into_iter()
        .find(|a| a.title == title)
        .unwrap()
        .id
}

fn album_selection(f: &Fixture, title: &str) -> Vec<usize> {
    let id = album_id(f, title);
    let n = f.lib.tracks(id).len();
    let shown: Vec<usize> = (0..n).collect();
    f.harness
        .state()
        .selection()
        .rows(&ListId::Album(id), &shown)
}

#[test]
fn album_track_right_click_shows_the_track_menu() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.right_click_track("Blue in Green");
    assert_eq!(
        f.menu_labels(),
        [
            "Play",
            "Play next",
            "Add to queue",
            "Add to playlist",
            "Go to artist"
        ]
    );
    assert_eq!(album_selection(&f, "Kind of Blue"), vec![2]);
}

#[test]
fn play_next_sends_insert() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.right_click_track("All Blues");
    f.menu_item("Play next");
    let all = album_items(&f, "Kind of Blue");
    match f.last_sent() {
        Some(PlayerCommand::Insert { items }) => {
            assert_eq!(items, vec![all[3].clone()]);
            assert_eq!(items[0].path, all[3].path);
        }
        other => panic!("expected Insert, got {other:?}"),
    }
}

#[test]
fn add_to_queue_sends_append() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.right_click_track("Freddie Freeloader");
    f.menu_item("Add to queue");
    let all = album_items(&f, "Kind of Blue");
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::Append {
            items: vec![all[1].clone()]
        })
    );
}

#[test]
fn menu_play_on_one_album_track_plays_the_album_from_it() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.right_click_track("Blue in Green");
    f.menu_item("Play");
    let items = album_items(&f, "Kind of Blue");
    assert_eq!(f.last_sent(), Some(PlayerCommand::Play { items, start: 2 }));
}

#[test]
fn menu_play_on_several_tracks_plays_just_those() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.click_track_with("So What", Modifiers::NONE);
    f.click_track_with("All Blues", CTRL);
    f.right_click_track("All Blues");
    f.menu_item("Play");
    let all = album_items(&f, "Kind of Blue");
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::Play {
            items: vec![all[0].clone(), all[3].clone()],
            start: 0
        })
    );
}

#[test]
fn ctrl_selection_of_three_tracks_is_queued_at_once() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.click_track_with("All Blues", Modifiers::NONE);
    f.click_track_with("So What", CTRL);
    f.click_track_with("Blue in Green", CTRL);
    assert_eq!(album_selection(&f, "Kind of Blue"), vec![0, 2, 3]);
    f.right_click_track("So What");
    assert!(
        f.menu_labels()
            .contains(&"Add 3 tracks to queue".to_owned())
    );
    f.menu_item("Add 3 tracks to queue");
    let all = album_items(&f, "Kind of Blue");
    let sent: Vec<_> = f
        .player
        .sent()
        .into_iter()
        .filter(|c| matches!(c, PlayerCommand::Append { .. }))
        .collect();
    assert_eq!(
        sent,
        vec![PlayerCommand::Append {
            items: vec![all[0].clone(), all[2].clone(), all[3].clone()]
        }]
    );
}

#[test]
fn shift_click_selects_a_range() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.click_track_with("Freddie Freeloader", Modifiers::NONE);
    f.click_track_with("All Blues", Modifiers::SHIFT);
    assert_eq!(album_selection(&f, "Kind of Blue"), vec![1, 2, 3]);
}

#[test]
fn ctrl_a_selects_every_track_on_the_album_page() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.harness.key_press_modifiers(CTRL, egui::Key::A);
    f.harness.run();
    assert_eq!(album_selection(&f, "Kind of Blue"), vec![0, 1, 2, 3, 4]);
}

#[test]
fn esc_clears_the_selection_before_going_back() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.click_track_with("So What", Modifiers::NONE);
    assert_eq!(album_selection(&f, "Kind of Blue"), vec![0]);
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();
    assert!(album_selection(&f, "Kind of Blue").is_empty());
    assert!(matches!(
        f.harness.state().nav().current(),
        Page::Album { .. }
    ));
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Home);
}

#[test]
fn leaving_the_page_clears_the_selection() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.click_track_with("So What", Modifiers::NONE);
    f.harness.get_by_label("Back").click();
    f.harness.run();
    f.open_album("Kind of Blue");
    assert!(album_selection(&f, "Kind of Blue").is_empty());
}

#[test]
fn device_items_disabled_without_a_device() {
    let mut f = fixture_full(tracks(), Vec::new(), vec![PathBuf::from("/music")], None);
    f.open_album("Kind of Blue");
    f.right_click_track("So What");
    for label in ["Play", "Play next", "Add to queue"] {
        assert!(!f.menu_item_enabled(label), "{label}");
    }
    for label in ["Add to playlist", "Go to artist"] {
        assert!(f.menu_item_enabled(label), "{label}");
    }
    f.menu_item("Play");
    assert!(
        !f.player
            .sent()
            .iter()
            .any(|c| matches!(c, PlayerCommand::Play { .. })),
        "{:?}",
        f.player.sent()
    );
}

#[test]
fn shift_f10_opens_the_menu_on_the_focused_track() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.harness
        .get_by_role_and_label(Role::ListItem, "All Blues")
        .focus();
    f.harness.run();
    f.harness
        .key_press_modifiers(Modifiers::SHIFT, egui::Key::F10);
    f.harness.run();
    assert_eq!(f.menu_labels()[0], "Play");
    f.menu_item("Add to queue");
    let all = album_items(&f, "Kind of Blue");
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::Append {
            items: vec![all[3].clone()]
        })
    );
}

#[test]
fn right_click_on_an_unselected_track_acts_on_it_alone() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.click_track_with("So What", Modifiers::NONE);
    f.click_track_with("Freddie Freeloader", CTRL);
    assert_eq!(album_selection(&f, "Kind of Blue"), vec![0, 1]);
    f.right_click_track("Flamenco Sketches");
    f.menu_item("Add to queue");
    let all = album_items(&f, "Kind of Blue");
    let sent: Vec<_> = f
        .player
        .sent()
        .into_iter()
        .filter(|c| matches!(c, PlayerCommand::Append { .. }))
        .collect();
    assert_eq!(
        sent,
        vec![PlayerCommand::Append {
            items: vec![all[4].clone()]
        }]
    );
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
fn settings_dop_switch_asks_before_turning_on() {
    let mut f = library();
    f.harness.get_by_label("Settings").click();
    f.harness.run();
    let dac_a = || DeviceId("dac-a".into());
    let toggled = |f: &Fixture| {
        f.harness
            .get_by_label("DSD over PCM (DoP)")
            .accesskit_node()
            .toggled()
    };
    assert_eq!(toggled(&f), Some(egui::accesskit::Toggled::True));
    // Turning it off needs no question.
    f.harness.get_by_label("DSD over PCM (DoP)").click();
    f.harness.run();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::SetDop {
            id: dac_a(),
            enabled: false
        })
    );
    f.harness.run();
    assert_eq!(toggled(&f), Some(egui::accesskit::Toggled::False));

    // Turning it on warns first; Cancel sends nothing.
    f.harness.get_by_label("DSD over PCM (DoP)").click();
    f.harness.run();
    f.harness.get_by_label("Turn on DoP for DAC A?");
    f.harness
        .get_by_role_and_label(Role::Button, "Cancel")
        .click();
    f.harness.run();
    assert!(f.harness.query_by_label("Turn on DoP for DAC A?").is_none());
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::SetDop {
            id: dac_a(),
            enabled: false
        })
    );
    assert_eq!(toggled(&f), Some(egui::accesskit::Toggled::False));

    f.harness.get_by_label("DSD over PCM (DoP)").click();
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Turn on")
        .click();
    f.harness.run();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::SetDop {
            id: dac_a(),
            enabled: true
        })
    );
    f.harness.run();
    assert_eq!(toggled(&f), Some(egui::accesskit::Toggled::True));
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
        home: Path::new("/nil"),
        home_note: None,
        logs_dir: Path::new("/nil/data/logs"),
        plugins: &[],
        plugin_error: None,
        plugin_panels: &NO_PANELS,
    }
}

static NO_PANELS: std::collections::BTreeMap<String, bp_gui::plugins::panel::Panel> =
    std::collections::BTreeMap::new();

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

// --- Playlists ---

fn playlists_dir(f: &Fixture) -> PathBuf {
    f.dir.path().join("playlists")
}

/// Creates empty files `name.flac` in the fixture's temp dir and returns their paths.
fn real_files(f: &Fixture, names: &[&str]) -> Vec<PathBuf> {
    names
        .iter()
        .map(|n| {
            let p = f.dir.path().join(format!("{n}.flac"));
            std::fs::write(&p, b"").unwrap();
            p
        })
        .collect()
}

fn save_playlist(f: &Fixture, name: &str, paths: &[PathBuf]) {
    let entries: Vec<_> = paths
        .iter()
        .map(|p| bp_gui::playlists::Entry {
            path: p.clone(),
            title: p.file_stem().map(|s| s.to_string_lossy().into_owned()),
            artist: None,
            duration_s: Some(60),
        })
        .collect();
    bp_gui::playlists::save(&playlists_dir(f), name, &entries).unwrap();
}

fn open_playlist(f: &mut Fixture, name: &str) {
    tab(f, "Playlists");
    f.harness.get_by_role_and_label(Role::Button, name).click();
    f.harness.run();
}

#[test]
fn playlists_tab_lists_saved_playlists() {
    let mut f = library();
    let files = real_files(&f, &["a"]);
    save_playlist(&f, "Mix B", &files);
    save_playlist(&f, "Mix A", &files);
    tab(&mut f, "Playlists");
    let a = f.harness.get_by_label("Mix A").rect();
    let b = f.harness.get_by_label("Mix B").rect();
    assert!(a.top() < b.top(), "Mix A first");
}

#[test]
fn playlist_play_skips_missing_files() {
    let mut f = library();
    let mut files = real_files(&f, &["a", "c"]);
    files.insert(1, f.dir.path().join("gone.flac"));
    save_playlist(&f, "Mix", &files);
    open_playlist(&mut f, "Mix");
    f.harness
        .get_by_role_and_label(Role::Button, "Play playlist")
        .click();
    f.harness.run();
    match f.last_sent() {
        Some(PlayerCommand::Play { items, start }) => {
            assert_eq!(items.len(), 2);
            assert_eq!(start, 0);
            assert_eq!(items[0].title, "a");
            assert_eq!(items[1].title, "c");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn missing_rows_are_hidden_and_counted() {
    let mut f = library();
    let mut files = real_files(&f, &["a", "c"]);
    let gone = f.dir.path().join("gone.flac");
    files.insert(1, gone.clone());
    save_playlist(&f, "Mix", &files);
    open_playlist(&mut f, "Mix");
    assert!(
        f.harness
            .query_by_role_and_label(Role::ListItem, "gone")
            .is_none()
    );
    f.harness.get_by_label("3 tracks · 1 not found");
    // Removing another row keeps the hidden one in the file, in its place.
    f.right_click_track("c");
    f.menu_item("Remove from playlist");
    assert_eq!(mix_paths(&f), vec![files[0].clone(), gone]);
    f.harness.get_by_label("2 tracks · 1 not found");
    f.double_click_track("a");
    assert!(matches!(f.last_sent(), Some(PlayerCommand::Play { .. })));
}

#[test]
fn playlist_queue_appends() {
    let mut f = library();
    let files = real_files(&f, &["a", "b"]);
    save_playlist(&f, "Mix", &files);
    f.open_album("Kind of Blue");
    f.double_click_track("So What");
    f.advance(0.4);
    open_playlist(&mut f, "Mix");
    f.harness
        .get_by_role_and_label(Role::Button, "Queue playlist")
        .click();
    f.harness.run();
    match f.last_sent() {
        Some(PlayerCommand::Append { items }) => assert_eq!(items.len(), 2),
        other => panic!("{other:?}"),
    }
}

fn save_queue_as(f: &mut Fixture, name: &str) {
    f.harness
        .get_by_role_and_label(Role::Button, "Save queue as playlist…")
        .click();
    f.harness.run();
    f.harness.get_by_label("Playlist name").type_text(name);
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Save")
        .click();
    f.harness.run();
}

#[test]
fn save_queue_as_playlist_writes_the_file() {
    let mut f = library();
    open_now_playing(&mut f);
    save_queue_as(&mut f, "Late");
    let file = playlists_dir(&f).join("Late.m3u8");
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text.matches("#EXTINF").count(), KIND_OF_BLUE.len());
    assert!(f.harness.query_by_label("Playlist name").is_none());
}

#[test]
fn save_over_an_existing_name_asks_first() {
    let mut f = library();
    let files = real_files(&f, &["a"]);
    save_playlist(&f, "Late", &files);
    open_now_playing(&mut f);
    save_queue_as(&mut f, "Late");
    f.harness.get_by_label("Replace \"Late\"?");
    let file = playlists_dir(&f).join("Late.m3u8");
    assert_eq!(
        std::fs::read_to_string(&file)
            .unwrap()
            .matches("#EXTINF")
            .count(),
        1,
        "unchanged until confirmed"
    );
    f.harness
        .get_by_role_and_label(Role::Button, "Replace")
        .click();
    f.harness.run();
    assert_eq!(
        std::fs::read_to_string(&file)
            .unwrap()
            .matches("#EXTINF")
            .count(),
        KIND_OF_BLUE.len()
    );
}

#[test]
fn delete_asks_then_removes() {
    let mut f = library();
    let files = real_files(&f, &["a"]);
    save_playlist(&f, "Mix", &files);
    let file = playlists_dir(&f).join("Mix.m3u8");
    open_playlist(&mut f, "Mix");
    f.harness
        .get_by_role_and_label(Role::Button, "Delete playlist")
        .click();
    f.harness.run();
    f.harness.get_by_label("Delete playlist \"Mix\"?");
    f.harness
        .get_by_role_and_label(Role::Button, "Cancel")
        .click();
    f.harness.run();
    assert!(file.exists(), "cancel keeps it");
    f.harness
        .get_by_role_and_label(Role::Button, "Delete playlist")
        .click();
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Delete")
        .click();
    f.harness.run();
    assert!(!file.exists());
    assert_eq!(f.harness.state().nav().current(), &Page::Playlists);
    assert!(f.harness.query_by_label("Mix").is_none());
}

#[test]
fn rename_changes_the_file_name() {
    let mut f = library();
    let files = real_files(&f, &["a"]);
    save_playlist(&f, "Mix", &files);
    save_playlist(&f, "Other", &files);
    open_playlist(&mut f, "Mix");
    f.harness
        .get_by_role_and_label(Role::Button, "Rename playlist")
        .click();
    f.harness.run();
    // Onto an existing playlist: refused, the dialog stays with the error.
    f.harness.get_by_label("Playlist name").type_text("Other");
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Rename")
        .click();
    f.harness.run();
    f.harness.get_by_label_contains("already exists");
    assert!(playlists_dir(&f).join("Mix.m3u8").exists());
    assert!(playlists_dir(&f).join("Other.m3u8").exists());
    f.harness.get_by_label("Playlist name").focus();
    f.harness.run();
    f.harness.key_press_modifiers(CTRL, egui::Key::A);
    f.harness.run();
    f.harness.get_by_label("Playlist name").type_text("Renamed");
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Rename")
        .click();
    f.harness.run();
    assert!(!playlists_dir(&f).join("Mix.m3u8").exists());
    assert!(playlists_dir(&f).join("Renamed.m3u8").exists());
    assert_eq!(
        f.harness.state().nav().current(),
        &Page::Playlist("Renamed".into())
    );
}

#[test]
fn reopening_a_playlist_rereads_its_files() {
    let mut f = library();
    let files = real_files(&f, &["a", "c"]);
    save_playlist(&f, "Trip", &files);
    open_playlist(&mut f, "Trip");
    std::fs::remove_file(&files[0]).unwrap();
    tab(&mut f, "Home");
    open_playlist(&mut f, "Trip");
    f.harness
        .get_by_role_and_label(Role::Button, "Play playlist")
        .click();
    f.harness.run();
    match f.last_sent() {
        Some(PlayerCommand::Play { items, .. }) => {
            let titles: Vec<_> = items.iter().map(|i| i.title.as_str()).collect();
            assert_eq!(titles, ["c"]);
        }
        other => panic!("expected Play, got {other:?}"),
    }
}

#[test]
fn back_after_a_delete_skips_the_list_it_left() {
    let mut f = library();
    let files = real_files(&f, &["a"]);
    save_playlist(&f, "Trip", &files);
    open_playlist(&mut f, "Trip");
    f.harness
        .get_by_role_and_label(Role::Button, "Delete playlist")
        .click();
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Delete")
        .click();
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Playlists);
    f.harness.get_by_label("Back").click();
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Home);
}

#[test]
fn plugin_play_reaches_the_player() {
    let track = "/music/Miles Davis/Kind of Blue/So What.flac";
    let call = format!(r#"call:player.play:{{"paths":["{track}"]}}"#);
    let mut f = fixture_with_plugin(&["reply", &call]);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let log = loop {
        f.harness.run_steps(2);
        let log = f.plugin_log();
        if log.contains("player.changed") || std::time::Instant::now() > deadline {
            break log;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        f.player
            .sent()
            .iter()
            .any(|c| matches!(c, PlayerCommand::Play { items, start: 0 } if items.len() == 1)),
        "sent: {:?}",
        f.player.sent()
    );
    // The fake logs what it receives: the `{}` result, then the notification.
    let lines: Vec<&str> = log.lines().filter(|l| !l.starts_with('#')).collect();
    let result = lines
        .iter()
        .position(|l| l.contains(r#""result":{}"#))
        .unwrap_or_else(|| panic!("no result in {log}"));
    let changed = lines
        .iter()
        .position(|l| l.contains("player.changed"))
        .unwrap_or_else(|| panic!("no player.changed in {log}"));
    assert!(result < changed, "{log}");
    let v: serde_json::Value = serde_json::from_str(lines[changed]).unwrap();
    assert_eq!(v["params"]["state"], "playing");
    assert_eq!(v["params"]["queue_len"], 1);
    assert_eq!(v["params"]["path"], track);
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains("player.changed"))
            .count(),
        1
    );
}

#[test]
fn plugin_play_shows_each_tracks_album() {
    let a = "/music/Miles Davis/Kind of Blue/So What.flac";
    let b = "/music/Radiohead/Kid A/Everything in Its Right Place.flac";
    let call = format!(r#"call:player.play:{{"paths":["{a}","{b}"]}}"#);
    let mut f = fixture_with_plugin(&["reply", &call]);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !f.plugin_log().contains("player.changed") && std::time::Instant::now() < deadline {
        f.harness.run_steps(2);
        std::thread::sleep(Duration::from_millis(20));
    }
    f.advance(0.4);
    f.harness.get_by_label("Miles Davis · Kind of Blue");
    // Past the end of the first 5:00 track.
    f.advance(301.0);
    f.harness.get_by_label("Radiohead · Kid A");
    f.harness
        .get_by_role_and_label(Role::Button, "Open now playing")
        .click();
    f.harness.run();
    // The large view's line; the view covers the bar.
    f.harness.get_by_label("Radiohead · Kid A");
}

// ---- Plugin panels ----

fn panel_of(v: serde_json::Value) -> bp_gui::plugins::panel::Panel {
    bp_gui::plugins::panel::parse(&v).unwrap()
}

/// Shows `elements` as the panel of plugin "dj" and opens its tab.
fn open_panel(f: &mut Fixture, elements: serde_json::Value) {
    f.harness.state_mut().set_panel(
        "dj".into(),
        panel_of(serde_json::json!({ "elements": elements })),
    );
    f.harness.run();
    tab(f, "dj");
}

#[test]
fn each_element_renders() {
    let mut f = library();
    // The spinner repaints forever, which `run` rejects: it joins the panel last.
    open_panel(&mut f, serde_json::json!([]));
    f.harness.state_mut().set_panel(
        "dj".into(),
        panel_of(serde_json::json!({"elements": [
            {"type": "heading", "text": "Big title"},
            {"type": "text", "text": "Some words", "muted": true},
            {"type": "input", "id": "q", "placeholder": "Ask for a mood", "submit_label": "Go"},
            {"type": "button", "id": "b", "label": "Press me"},
            {"type": "button", "id": "d", "label": "Not yet", "enabled": false},
            {"type": "toggle", "id": "t", "label": "Shuffle", "value": true},
            {"type": "progress", "value": 0.5, "label": "Half way"},
            {"type": "progress", "value": null, "label": "Thinking"},
            {"type": "tracks", "items": [{"path": "/m/a.flac", "title": "Song A", "artist": "Band"}]},
            {"type": "row", "children": [{"type": "text", "text": "In a row"}]},
            {"type": "chart", "data": 1},
        ]}),
        ),
    );
    f.harness.run_steps(3);
    assert_eq!(
        f.harness.state().nav().current(),
        &Page::Plugin("dj".into())
    );
    for label in [
        "Big title",
        "Some words",
        "Ask for a mood",
        "Go",
        "Press me",
        "Not yet",
        "Shuffle",
        "Thinking",
        "Song A",
        "In a row",
        "unsupported element",
    ] {
        f.harness.get_by_label(label);
    }
}

#[test]
fn input_enter_sends_its_text() {
    let mut f = library();
    open_panel(
        &mut f,
        serde_json::json!([
            {"type": "input", "id": "q", "placeholder": "Ask", "submit_label": "Go"},
        ]),
    );
    let input = f.harness.get_by_label("Ask");
    input.click();
    input.type_text("sunny");
    f.harness.run();
    f.harness.key_press(egui::Key::Enter);
    f.harness.run();
    assert_eq!(
        f.harness.state().sent_panel_events,
        vec![(
            "dj".to_owned(),
            serde_json::json!({"id": "q", "value": "sunny"})
        )]
    );
}

#[test]
fn input_button_sends_its_text() {
    let mut f = library();
    open_panel(
        &mut f,
        serde_json::json!([
            {"type": "input", "id": "q", "placeholder": "Ask", "submit_label": "Go", "value": "rain"},
        ]),
    );
    f.harness.get_by_role_and_label(Role::Button, "Go").click();
    f.harness.run();
    assert_eq!(
        f.harness.state().sent_panel_events,
        vec![(
            "dj".to_owned(),
            serde_json::json!({"id": "q", "value": "rain"})
        )]
    );
}

#[test]
fn button_click_sends_its_id() {
    let mut f = library();
    open_panel(
        &mut f,
        serde_json::json!([
            {"type": "button", "id": "go", "label": "Go on"},
            {"type": "button", "id": "no", "label": "Stuck", "enabled": false},
        ]),
    );
    f.harness.get_by_label("Stuck").click();
    f.harness.run();
    assert!(f.harness.state().sent_panel_events.is_empty());
    f.harness
        .get_by_role_and_label(Role::Button, "Go on")
        .click();
    f.harness.run();
    assert_eq!(
        f.harness.state().sent_panel_events,
        vec![("dj".to_owned(), serde_json::json!({"id": "go"}))]
    );
}

#[test]
fn toggle_sends_the_new_value() {
    let mut f = library();
    open_panel(
        &mut f,
        serde_json::json!([{"type": "toggle", "id": "t", "label": "Shuffle", "value": false}]),
    );
    f.harness.get_by_label("Shuffle").click();
    f.harness.run();
    assert_eq!(
        f.harness.state().sent_panel_events,
        vec![(
            "dj".to_owned(),
            serde_json::json!({"id": "t", "value": true})
        )]
    );
}

#[test]
fn toggle_off_sends_false() {
    let mut f = library();
    open_panel(
        &mut f,
        serde_json::json!([{"type": "toggle", "id": "t", "label": "Shuffle", "value": true}]),
    );
    f.harness.get_by_label("Shuffle").click();
    f.harness.run();
    assert_eq!(
        f.harness.state().sent_panel_events,
        vec![(
            "dj".to_owned(),
            serde_json::json!({"id": "t", "value": false})
        )]
    );
}

fn panel_with_files(f: &mut Fixture) {
    let files = real_files(f, &["a", "b"]);
    let items: Vec<_> = files
        .iter()
        .map(|p| serde_json::json!({"path": p, "title": p.file_stem().unwrap().to_string_lossy()}))
        .collect();
    open_panel(f, serde_json::json!([{"type": "tracks", "items": items}]));
}

#[test]
fn panel_last_track_clears_the_bar() {
    let mut f = library();
    let names: Vec<String> = (1..=30).map(|i| format!("t{i:02}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let files = real_files(&f, &names);
    let items: Vec<_> = files
        .iter()
        .map(|p| serde_json::json!({"path": p, "title": p.file_stem().unwrap().to_string_lossy()}))
        .collect();
    open_panel(
        &mut f,
        serde_json::json!([{"type": "tracks", "items": items}]),
    );
    for _ in 0..10 {
        wheel(&mut f, egui::pos2(640.0, 300.0), -2000.0);
    }
    let last = f
        .harness
        .get_by_role_and_label(Role::ListItem, "t30")
        .rect();
    let bar = f
        .harness
        .get_by_role_and_label(Role::Group, "Now playing")
        .rect();
    assert!(
        last.bottom() <= bar.top(),
        "last track {last:?} under the bar {bar:?}"
    );
}

#[test]
fn plugin_tab_uses_the_manifest_title() {
    let mut f = library();
    let mut dj = info("dj", Status::Running, Consent::default(), vec![]);
    dj.manifest.as_mut().unwrap().title = Some("DJ".into());
    f.harness.state_mut().set_plugin_infos(vec![dj]);
    f.harness.state_mut().set_panel(
        "dj".into(),
        panel_of(serde_json::json!({"elements": [{"type": "text", "text": "hi"}]})),
    );
    f.harness.run();
    assert!(f.harness.query_by_role_and_label(Role::Tab, "dj").is_none());
    tab(&mut f, "DJ");
    f.harness.get_by_label("hi");
}

#[test]
fn a_settings_panel_alone_adds_no_tab() {
    let mut f = library();
    f.harness.state_mut().set_settings_panel(
        "dj".into(),
        Some(panel_of(
            serde_json::json!({"elements": [{"type": "text", "text": "hi"}]}),
        )),
    );
    f.harness.run();
    assert!(f.harness.state().panels().is_empty());
    assert_eq!(f.harness.state().settings_panels().len(), 1);
    assert!(f.harness.query_by_role_and_label(Role::Tab, "dj").is_none());
}

#[test]
fn plugin_tab_shows_its_title_as_heading() {
    let mut f = library();
    open_panel(&mut f, serde_json::json!([{"type": "text", "text": "hi"}]));
    let mut dj = info("dj", Status::Running, Consent::default(), vec![]);
    dj.manifest.as_mut().unwrap().title = Some("DJ".into());
    f.harness.state_mut().set_plugin_infos(vec![dj]);
    f.harness.run();
    f.harness.get_by_role_and_label(Role::Heading, "DJ");
}

#[test]
fn tracks_label_is_shown_uppercased() {
    let mut f = library();
    open_panel(
        &mut f,
        serde_json::json!([{"type": "tracks", "label": "Mix · 2 tracks", "items": [
            {"path": "/m/a.flac", "title": "Song A"},
        ]}]),
    );
    f.harness.get_by_label("MIX · 2 TRACKS");
}

#[test]
fn panel_track_rows_double_click_to_play() {
    let mut f = library();
    open_panel(
        &mut f,
        serde_json::json!([{"type": "tracks", "items": [
            {"path": "/music/Miles Davis/Kind of Blue/So What.flac", "title": "So What"},
            {"path": "/music/Miles Davis/Kind of Blue/All Blues.flac", "title": "All Blues"},
        ]}]),
    );
    f.double_click_track("All Blues");
    f.harness.run();
    match f.last_sent() {
        Some(PlayerCommand::Play { items, start }) => {
            assert_eq!(items.len(), 2);
            assert_eq!(start, 1);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn panel_tracks_outside_the_library_render() {
    let mut f = library();
    open_panel(
        &mut f,
        serde_json::json!([{"type": "tracks", "items": [
            {"path": "/nowhere/x.flac", "title": "Lost", "duration_s": 61},
        ]}]),
    );
    f.harness.run();
    f.harness.get_by_role_and_label(Role::ListItem, "Lost");
}

#[test]
fn panel_tracks_play_all_acts_in_nil() {
    let mut f = library();
    panel_with_files(&mut f);
    f.harness
        .get_by_role_and_label(Role::Button, "Play all")
        .click();
    f.harness.run();
    match f.last_sent() {
        Some(PlayerCommand::Play { items, start }) => {
            assert_eq!(items.len(), 2);
            assert_eq!(start, 0);
            assert_eq!(items[1].title, "b");
        }
        other => panic!("{other:?}"),
    }
    assert!(f.harness.state().sent_panel_events.is_empty());
}

#[test]
fn panel_tracks_double_click_plays_from_the_row() {
    let mut f = library();
    panel_with_files(&mut f);
    f.double_click_track("b");
    f.harness.run();
    match f.last_sent() {
        Some(PlayerCommand::Play { items, start }) => {
            assert_eq!(items.len(), 2);
            assert_eq!(start, 1);
        }
        other => panic!("{other:?}"),
    }
    assert!(f.harness.state().sent_panel_events.is_empty());
}

#[test]
fn panel_tracks_queue_all_appends() {
    let mut f = library();
    panel_with_files(&mut f);
    f.harness
        .get_by_role_and_label(Role::Button, "Queue all")
        .click();
    f.harness.run();
    match f.last_sent() {
        Some(PlayerCommand::Append { items }) => assert_eq!(items.len(), 2),
        other => panic!("{other:?}"),
    }
    assert!(f.harness.state().sent_panel_events.is_empty());
}

#[test]
fn panel_tracks_save_as_playlist_writes_the_file() {
    let mut f = library();
    panel_with_files(&mut f);
    f.harness
        .get_by_role_and_label(Role::Button, "Save as playlist")
        .click();
    f.harness.run();
    f.harness.get_by_label("Playlist name").type_text("From dj");
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Save")
        .click();
    f.harness.run();
    let text = std::fs::read_to_string(playlists_dir(&f).join("From dj.m3u8")).unwrap();
    assert_eq!(text.matches("#EXTINF").count(), 2);
    assert!(f.harness.query_by_label("Playlist name").is_none());
    assert!(f.harness.state().sent_panel_events.is_empty());
}

#[test]
fn plugin_tab_disappears_with_its_panel() {
    let mut f = library();
    open_panel(&mut f, serde_json::json!([{"type": "text", "text": "hi"}]));
    f.harness.state_mut().remove_panel("dj");
    f.harness.run();
    assert!(f.harness.query_by_role_and_label(Role::Tab, "dj").is_none());
    assert_eq!(f.harness.state().nav().current(), &Page::Home);
    // Back does not land on the vanished page.
    f.harness
        .key_press_modifiers(Modifiers::ALT, egui::Key::ArrowLeft);
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Home);
}

#[test]
fn removing_another_panel_keeps_the_open_page() {
    let mut f = library();
    f.harness.state_mut().set_panel(
        "other".into(),
        panel_of(serde_json::json!({"elements": []})),
    );
    open_panel(&mut f, serde_json::json!([{"type": "text", "text": "hi"}]));
    f.harness.state_mut().remove_panel("other");
    f.harness.run();
    assert_eq!(
        f.harness.state().nav().current(),
        &Page::Plugin("dj".into())
    );
    f.harness.get_by_label("hi");
}

// ---- Settings: data folder and plugins ----

use bp_gui::plugins::consent::Consent;
use bp_gui::plugins::host::{PluginInfo, Status};
use bp_gui::plugins::manifest::Manifest;
use bp_gui::plugins::rpc::Permission;
use bp_gui::views::settings::SettingsAction;

fn info(folder: &str, status: Status, consent: Consent, perms: Vec<Permission>) -> PluginInfo {
    PluginInfo {
        folder: folder.into(),
        manifest: Some(Manifest {
            name: folder.into(),
            version: "0.2".into(),
            description: format!("{folder} does things"),
            api_version: 1,
            command: vec!["x".into()],
            permissions: perms,
            title: None,
        }),
        consent,
        status,
        stderr_tail: Vec::new(),
    }
}

/// A tall window, so every Settings card is on screen.
fn tall() -> egui::Vec2 {
    egui::vec2(1280.0, 2600.0)
}

fn open_settings(f: &mut Fixture) {
    f.harness.get_by_label("Settings").click();
    f.harness.run();
}

#[test]
fn settings_shows_the_data_folder_and_fallback_note() {
    let note = "The program folder is not writable; using the per-user folder.";
    let data = bp_gui::views::settings::SettingsData {
        home: Path::new("/home/me/nil"),
        home_note: Some(note),
        ..no_devices()
    };
    let lib = Arc::new(MemLibrary::new(vec![], vec![]));
    let mut harness = themed()
        .with_size(egui::vec2(1000.0, 900.0))
        .build_ui(|ui| {
            bp_gui::views::settings::show(ui, &data, lib.as_ref(), &mut || None);
        });
    harness.run();
    harness.get_by_label("/home/me/nil");
    harness.get_by_label(note);
    harness.get_by_label("Open folder");
}

#[test]
fn open_folder_asks_to_open_the_data_folder() {
    let mut f = fixture_sized(tracks(), tall(), false);
    open_settings(&mut f);
    f.harness.get_by_label("Open folder").click();
    f.harness.run();
    assert_eq!(
        f.harness.state().plugin_actions,
        vec![SettingsAction::OpenFolder(PathBuf::from("/nil"))]
    );
}

#[test]
fn plugins_section_shows_the_warning_and_each_status() {
    let all = vec![
        Permission::Playback,
        Permission::Library,
        Permission::Playlists,
    ];
    let on = Consent {
        enabled: true,
        granted: all.clone(),
    };
    let mut f = fixture_sized(tracks(), tall(), false);
    f.harness.state_mut().set_plugin_infos(vec![
        info(
            "off",
            Status::Off,
            Consent::default(),
            vec![Permission::Library],
        ),
        info("starting", Status::Starting, on.clone(), vec![]),
        info(
            "running",
            Status::Running,
            on.clone(),
            vec![Permission::Playback],
        ),
        info("old", Status::Incompatible, Consent::default(), vec![]),
        PluginInfo {
            folder: "bad".into(),
            manifest: None,
            consent: Consent::default(),
            status: Status::InvalidManifest("missing field `name`".into()),
            stderr_tail: Vec::new(),
        },
        // Approved before, but the manifest has since asked for more.
        info(
            "grown",
            Status::Off,
            Consent {
                enabled: true,
                granted: vec![Permission::Playback],
            },
            vec![
                Permission::Playback,
                Permission::Library,
                Permission::Playlists,
            ],
        ),
    ]);
    open_settings(&mut f);
    f.harness.get_by_label(
        "A plugin is an ordinary program running as you. Permissions limit what it can do \
         through nil, not what it can do on your PC.",
    );
    f.harness.get_by_role_and_label(Role::Link, "Plugin API");
    f.harness.get_by_label("Starting…");
    f.harness.get_by_label("Running");
    f.harness.get_by_label("Incompatible (needs plugin API 1)");
    f.harness
        .get_by_label("Invalid manifest: missing field `name`");
    f.harness
        .get_by_label("Wants new permission: Library, Playlists");
    f.harness
        .get_by_label("Permissions: Playback, Library, Playlists");
    f.harness.get_by_label("Permissions: Playback");
    f.harness.get_by_label("old does things");
}

/// Settings open, with plugin "dj" running and `elements` as its settings panel.
fn settings_with_panel(elements: Option<serde_json::Value>) -> Fixture {
    let mut f = fixture_sized(tracks(), tall(), false);
    let on = Consent {
        enabled: true,
        granted: vec![],
    };
    f.harness
        .state_mut()
        .set_plugin_infos(vec![info("dj", Status::Running, on, vec![])]);
    let panel = elements.map(|e| panel_of(serde_json::json!({ "elements": e })));
    f.harness.state_mut().set_settings_panel("dj".into(), panel);
    open_settings(&mut f);
    f
}

#[test]
fn settings_panel_shows_in_the_plugin_entry() {
    let f = settings_with_panel(Some(
        serde_json::json!([{"type": "text", "text": "2,890 tracks indexed"}]),
    ));
    let text = f.harness.get_by_label("2,890 tracks indexed").rect();
    let status = f.harness.get_by_label("Running").rect();
    let restart = f.harness.get_by_label("Restart").rect();
    assert!(status.bottom() <= text.top(), "{status:?} {text:?}");
    assert!(text.bottom() <= restart.top(), "{text:?} {restart:?}");
}

#[test]
fn settings_panel_button_sends_its_place() {
    let mut f = settings_with_panel(Some(serde_json::json!([
        {"type": "button", "id": "index", "label": "Update index"},
    ])));
    f.harness.get_by_label("Update index").click();
    f.harness.run();
    assert_eq!(
        f.harness.state().sent_panel_events,
        vec![(
            "dj".to_owned(),
            serde_json::json!({"id": "index", "place": "settings"})
        )]
    );
    assert!(f.harness.state().plugin_actions.is_empty());
}

#[test]
fn settings_entry_without_a_panel_is_unchanged() {
    let f = settings_with_panel(None);
    f.harness.get_by_label("Restart");
    f.harness.get_by_label("Open log");
    // The data folder's button and the plugin's.
    assert_eq!(f.harness.get_all_by_label("Open folder").count(), 2);
    assert!(f.harness.query_by_label("2,890 tracks indexed").is_none());
}

#[test]
fn turning_a_plugin_on_asks_for_its_permissions() {
    let mut f = fixture_with_plugin_sized(&[], tall());
    let toml = f.dir.path().join("data").join("plugins.toml");
    open_settings(&mut f);
    let switch = || format!("Enable {PLUGIN}");
    // Turning it off needs no question.
    f.harness.get_by_label(&switch()).click();
    f.harness.run();
    assert!(!consent_of(&toml).enabled);
    let before = std::fs::read_to_string(&toml).unwrap();

    f.harness.get_by_label(&switch()).click();
    f.harness.run();
    f.harness.get_by_label("Turn on p?");
    f.harness.get_by_label("It will be allowed to:");
    // Each permission is listed once in the dialog, besides other places on the page.
    for name in ["Playback", "Library", "Playlists"] {
        assert!(f.harness.get_all_by_label(name).count() >= 1, "{name}");
    }
    assert_eq!(f.harness.get_all_by_label("Playlists").count(), 2);
    f.harness
        .get_by_role_and_label(Role::Button, "Cancel")
        .click();
    f.harness.run();
    assert!(f.harness.query_by_label("Allow and turn on").is_none());
    assert_eq!(std::fs::read_to_string(&toml).unwrap(), before);
    assert_eq!(
        f.harness.get_by_label(&switch()).accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::False)
    );

    f.harness.get_by_label(&switch()).click();
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Allow and turn on")
        .click();
    f.harness.run();
    let c = consent_of(&toml);
    assert!(c.enabled);
    assert_eq!(c.granted.len(), 3);
}

fn consent_of(path: &Path) -> Consent {
    bp_gui::plugins::consent::ConsentStore::load(path).get(PLUGIN)
}

#[test]
fn restart_and_open_actions_are_recorded_without_a_host() {
    let mut f = fixture_sized(tracks(), tall(), false);
    f.harness.state_mut().set_plugin_infos(vec![info(
        "dj",
        Status::Running,
        Consent::default(),
        vec![],
    )]);
    open_settings(&mut f);
    f.harness.get_by_label("Restart").click();
    f.harness.run();
    f.harness.get_by_label("Open log").click();
    f.harness.run();
    let root = PathBuf::from("/nil");
    assert_eq!(
        f.harness.state().plugin_actions,
        vec![
            SettingsAction::PluginRestart("dj".into()),
            SettingsAction::OpenFile(
                root.join("data")
                    .join("logs")
                    .join("plugins")
                    .join("dj.log")
            ),
        ]
    );
}

#[test]
fn crashed_plugin_shows_its_stderr_tail() {
    let mut crashed = info(
        "dj",
        Status::Crashed {
            reason: "exited with code 1".into(),
        },
        Consent {
            enabled: true,
            granted: vec![],
        },
        vec![],
    );
    crashed.stderr_tail = vec!["Traceback (most recent call last):".into(), "boom".into()];
    let mut f = fixture_sized(tracks(), tall(), false);
    f.harness.state_mut().set_plugin_infos(vec![crashed]);
    open_settings(&mut f);
    f.harness.get_by_label("Crashed: exited with code 1");
    f.harness
        .get_by_label_contains("Traceback (most recent call last):");
    f.harness.get_by_label_contains("boom");
}

#[test]
fn grown_permissions_show_the_switch_off_and_ask_for_the_new_ones() {
    let mut f = fixture_sized(tracks(), tall(), false);
    f.harness.state_mut().set_plugin_infos(vec![info(
        "grown",
        Status::Off,
        Consent {
            enabled: true,
            granted: vec![Permission::Playback],
        },
        vec![Permission::Playback, Permission::Library],
    )]);
    open_settings(&mut f);
    let switch = f.harness.get_by_label("Enable grown");
    assert_eq!(
        switch.accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::False)
    );
    switch.click();
    f.harness.run();
    f.harness.get_by_label("Turn on grown?");
    f.harness.get_by_label("Library (new)");
    assert!(f.harness.query_by_label("Playback (new)").is_none());
    f.harness
        .get_by_role_and_label(Role::Button, "Allow and turn on")
        .click();
    f.harness.run();
    assert_eq!(
        f.harness.state().plugin_actions,
        vec![SettingsAction::PluginEnable {
            folder: "grown".into(),
            on: true
        }]
    );
}

#[test]
fn a_confirmation_is_dropped_when_its_plugin_vanishes_or_cannot_load() {
    let off = || info("dj", Status::Off, Consent::default(), vec![]);
    for gone in [
        vec![],
        vec![info("dj", Status::Incompatible, Consent::default(), vec![])],
    ] {
        let mut f = fixture_sized(tracks(), tall(), false);
        f.harness.state_mut().set_plugin_infos(vec![off()]);
        open_settings(&mut f);
        f.harness.get_by_label("Enable dj").click();
        f.harness.run();
        f.harness.get_by_label("Turn on dj?");
        f.harness.state_mut().set_plugin_infos(gone);
        f.harness.run();
        assert!(f.harness.query_by_label("Turn on dj?").is_none());
        // Back and loadable again: the old question does not come back.
        f.harness.state_mut().set_plugin_infos(vec![off()]);
        f.harness.run();
        assert!(f.harness.query_by_label("Turn on dj?").is_none());
        assert!(f.harness.state().plugin_actions.is_empty());
    }
}

#[test]
fn a_switch_change_that_cannot_be_saved_shows_on_the_row() {
    let mut f = fixture_with_plugin_sized(&[], tall());
    // A folder where the consent file goes makes saving it fail.
    let toml = f.dir.path().join("data").join("plugins.toml");
    std::fs::remove_file(&toml).unwrap();
    std::fs::create_dir_all(toml.join("x")).unwrap();
    open_settings(&mut f);
    f.harness.get_by_label(&format!("Enable {PLUGIN}")).click();
    f.harness.run();
    f.harness.get_by_label_contains("Couldn't save the change");
    // The next action clears it.
    f.harness.get_by_label("Restart").click();
    f.harness.run();
    assert!(
        f.harness
            .query_by_label_contains("Couldn't save the change")
            .is_none()
    );
}

// --- Add to playlist from the menu ---

/// Saves "Mix" with one real file and returns its path.
fn mix_with_one_file(f: &Fixture) -> Vec<PathBuf> {
    let files = real_files(f, &["a"]);
    save_playlist(f, "Mix", &files);
    files
}

fn add_track_to(f: &mut Fixture, title: &str, playlist: &str) {
    f.right_click_track(title);
    f.menu_item("Add to playlist");
    f.menu_item(playlist);
}

fn mix_paths(f: &Fixture) -> Vec<PathBuf> {
    bp_gui::playlists::read(&playlists_dir(f), "Mix")
        .unwrap()
        .into_iter()
        .map(|e| e.path)
        .collect()
}

#[test]
fn add_to_playlist_adds_and_says_so() {
    let mut f = library();
    let files = mix_with_one_file(&f);
    f.open_album("Kind of Blue");
    add_track_to(&mut f, "Blue in Green", "Mix");
    let all = album_items(&f, "Kind of Blue");
    assert_eq!(mix_paths(&f), vec![files[0].clone(), all[2].path.clone()]);
    f.harness.get_by_label("Added 1 track to Mix");
    // The selection stays.
    assert_eq!(album_selection(&f, "Kind of Blue"), vec![2]);
}

#[test]
fn add_again_says_already_in() {
    let mut f = library();
    mix_with_one_file(&f);
    f.open_album("Kind of Blue");
    add_track_to(&mut f, "Blue in Green", "Mix");
    let before = std::fs::read(playlists_dir(&f).join("Mix.m3u8")).unwrap();
    add_track_to(&mut f, "Blue in Green", "Mix");
    f.harness.get_by_label("Already in Mix");
    assert_eq!(
        std::fs::read(playlists_dir(&f).join("Mix.m3u8")).unwrap(),
        before
    );
}

#[test]
fn add_some_already_there_counts_them() {
    let mut f = library();
    mix_with_one_file(&f);
    f.open_album("Kind of Blue");
    add_track_to(&mut f, "Blue in Green", "Mix");
    f.click_track_with("So What", Modifiers::NONE);
    f.click_track_with("Blue in Green", CTRL);
    add_track_to(&mut f, "Blue in Green", "Mix");
    f.harness
        .get_by_label("Added 1 track to Mix (1 already there)");
    f.click_track_with("All Blues", CTRL);
    add_track_to(&mut f, "All Blues", "Mix");
    f.harness
        .get_by_label("Added 1 track to Mix (2 already there)");
}

#[test]
fn notice_expires() {
    let mut f = library();
    mix_with_one_file(&f);
    f.open_album("Kind of Blue");
    add_track_to(&mut f, "Blue in Green", "Mix");
    f.harness.get_by_label("Added 1 track to Mix");
    f.advance(5.0);
    assert!(f.harness.query_by_label("Added 1 track to Mix").is_none());
}

#[test]
fn new_playlist_from_the_menu_creates_and_adds() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.right_click_track("Blue in Green");
    f.menu_item("Add to playlist");
    f.menu_item("New playlist…");
    f.harness.get_by_label("Playlist name").type_text("Fresh");
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Create")
        .click();
    f.harness.run();
    let all = album_items(&f, "Kind of Blue");
    let got = bp_gui::playlists::read(&playlists_dir(&f), "Fresh").unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].path, all[2].path);
    assert!(f.harness.query_by_label("Playlist name").is_none());
    assert!(matches!(
        f.harness.state().nav().current(),
        Page::Album { .. }
    ));
    f.harness.get_by_label("Added 1 track to Fresh");
}

#[test]
fn album_menu_lists_playlists_from_disk() {
    let mut f = library();
    mix_with_one_file(&f);
    f.open_album("Kind of Blue");
    f.right_click_track("Blue in Green");
    f.menu_item("Add to playlist");
    assert!(f.menu_labels().contains(&"Mix".to_owned()));
}

#[test]
fn add_to_a_deleted_playlist_shows_the_error() {
    let mut f = library();
    mix_with_one_file(&f);
    f.open_album("Kind of Blue");
    f.right_click_track("Blue in Green");
    f.menu_item("Add to playlist");
    std::fs::remove_file(playlists_dir(&f).join("Mix.m3u8")).unwrap();
    f.menu_item("Mix");
    f.harness
        .get_by_label("Couldn't change Mix: Mix no longer exists");
    assert!(!playlists_dir(&f).join("Mix.m3u8").exists());
}

#[test]
fn esc_closes_the_device_popover() {
    let mut f = library();
    f.harness
        .get_by_role_and_label(Role::Button, "DAC A")
        .click();
    f.harness.run();
    f.harness.get_by_label("Device capabilities…");
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();
    assert!(f.harness.query_by_label("Device capabilities…").is_none());
}

const ALBUM_MENU: [&str; 4] = [
    "Play album",
    "Add album to queue",
    "Add to playlist",
    "Go to artist",
];

#[test]
fn album_tile_right_click_shows_the_album_menu() {
    let mut f = library();
    f.right_click_tile("Kind of Blue");
    assert_eq!(f.menu_labels(), ALBUM_MENU);
    let mut f = library();
    tab(&mut f, "Albums");
    f.right_click_tile("Kind of Blue");
    assert_eq!(f.menu_labels(), ALBUM_MENU);
    // The tile did not open, and nothing was selected.
    assert_eq!(f.harness.state().nav().current(), &Page::Albums);
}

#[test]
fn play_album_from_the_tile_plays_it_in_order() {
    let mut f = library();
    f.right_click_tile("Kind of Blue");
    f.menu_item("Play album");
    let all = album_items(&f, "Kind of Blue");
    match f.last_sent() {
        Some(PlayerCommand::Play { items, start }) => {
            assert_eq!(items, all);
            assert_eq!(start, 0);
        }
        other => panic!("expected Play, got {other:?}"),
    }
}

#[test]
fn add_album_to_queue_from_search() {
    let mut f = library();
    type_search(&mut f, "blue");
    f.right_click_tile("Kind of Blue");
    assert_eq!(f.menu_labels(), ALBUM_MENU);
    f.menu_item("Add album to queue");
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::Append {
            items: album_items(&f, "Kind of Blue")
        })
    );
}

#[test]
fn album_tile_go_to_artist_opens_the_artist() {
    let mut f = library();
    f.right_click_tile("Kind of Blue");
    f.menu_item("Go to artist");
    assert!(matches!(f.harness.state().nav().current(), Page::Artist(_)));
}

#[test]
fn album_tile_click_still_opens_it() {
    let mut f = library();
    tab(&mut f, "Albums");
    let kob = album_id(&f, "Kind of Blue");
    f.open_album("Kind of Blue");
    assert_eq!(
        f.harness.state().nav().current(),
        &Page::Album { album: kob }
    );
}

// --- Playlist rows: menu, Remove, Undo ---

fn playlist_selection(f: &Fixture, name: &str, n: usize) -> Vec<usize> {
    let shown: Vec<usize> = (0..n).collect();
    f.harness
        .state()
        .selection()
        .rows(&ListId::Playlist(name.into()), &shown)
}

fn paths_of(f: &Fixture, name: &str) -> Vec<PathBuf> {
    bp_gui::playlists::read(&playlists_dir(f), name)
        .unwrap()
        .into_iter()
        .map(|e| e.path)
        .collect()
}

/// "Mix" holding real files a, b and c, open; returns their paths.
fn open_abc(f: &mut Fixture) -> Vec<PathBuf> {
    let files = real_files(f, &["a", "b", "c"]);
    save_playlist(f, "Mix", &files);
    open_playlist(f, "Mix");
    files
}

fn remove_row(f: &mut Fixture, title: &str) {
    f.right_click_track(title);
    f.menu_item("Remove from playlist");
}

#[test]
fn playlist_row_right_click_shows_the_playlist_menu() {
    let mut f = library();
    let files = real_files(&f, &["a"]);
    save_playlist(&f, "Chill", &files);
    open_abc(&mut f);
    f.right_click_track("b");
    assert_eq!(
        f.menu_labels(),
        [
            "Play from here",
            "Play next",
            "Add to queue",
            "Add to playlist",
            "Remove from playlist"
        ]
    );
    f.menu_item("Add to playlist");
    let labels = f.menu_labels();
    assert!(labels.contains(&"Chill".to_owned()), "{labels:?}");
    assert!(!labels.contains(&"Mix".to_owned()), "{labels:?}");
}

#[test]
fn playlist_click_selects_and_ctrl_a_selects_every_shown_row() {
    let mut f = library();
    let mut files = real_files(&f, &["a", "c"]);
    files.insert(1, f.dir.path().join("gone.flac"));
    save_playlist(&f, "Mix", &files);
    open_playlist(&mut f, "Mix");
    f.click_track_with("c", Modifiers::NONE);
    // Rows are file indices.
    assert_eq!(playlist_selection(&f, "Mix", 3), vec![2]);
    f.harness.key_press_modifiers(CTRL, egui::Key::A);
    f.harness.run();
    assert_eq!(playlist_selection(&f, "Mix", 3), vec![0, 2]);
}

#[test]
fn playlist_play_from_here_plays_the_shown_rows_from_it() {
    let mut f = library();
    open_abc(&mut f);
    f.right_click_track("b");
    f.menu_item("Play from here");
    match f.last_sent() {
        Some(PlayerCommand::Play { items, start }) => {
            let titles: Vec<_> = items.iter().map(|i| i.title.as_str()).collect();
            assert_eq!(titles, ["a", "b", "c"]);
            assert_eq!(start, 1);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn remove_from_playlist_then_undo_puts_rows_back() {
    let mut f = library();
    let files = open_abc(&mut f);
    remove_row(&mut f, "b");
    assert_eq!(
        paths_of(&f, "Mix"),
        vec![files[0].clone(), files[2].clone()]
    );
    f.harness.get_by_label("Removed 1 track from Mix");
    assert!(
        f.harness
            .query_by_role_and_label(Role::ListItem, "b")
            .is_none()
    );
    assert!(playlist_selection(&f, "Mix", 3).is_empty());
    f.harness
        .get_by_role_and_label(Role::Button, "Undo")
        .click();
    f.harness.run();
    assert_eq!(paths_of(&f, "Mix"), files);
    f.harness.get_by_role_and_label(Role::ListItem, "b");
}

#[test]
fn removing_several_rows_says_how_many() {
    let mut f = library();
    let files = open_abc(&mut f);
    f.click_track_with("a", Modifiers::NONE);
    f.click_track_with("c", CTRL);
    f.right_click_track("c");
    f.menu_item("Remove 2 tracks");
    assert_eq!(paths_of(&f, "Mix"), vec![files[1].clone()]);
    f.harness.get_by_label("Removed 2 tracks from Mix");
}

#[test]
fn ctrl_z_undoes_a_remove() {
    let mut f = library();
    let files = open_abc(&mut f);
    remove_row(&mut f, "a");
    assert_eq!(paths_of(&f, "Mix"), files[1..].to_vec());
    f.harness.key_press_modifiers(CTRL, egui::Key::Z);
    f.harness.run();
    assert_eq!(paths_of(&f, "Mix"), files);
    // Undo is not itself undoable.
    f.harness.key_press_modifiers(CTRL, egui::Key::Z);
    f.harness.run();
    assert_eq!(paths_of(&f, "Mix"), files);
}

#[test]
fn ctrl_z_on_another_playlist_does_nothing() {
    let mut f = library();
    let chill = real_files(&f, &["x", "y"]);
    save_playlist(&f, "Chill", &chill);
    let files = open_abc(&mut f);
    remove_row(&mut f, "b");
    open_playlist(&mut f, "Chill");
    f.harness.key_press_modifiers(CTRL, egui::Key::Z);
    f.harness.run();
    let after = vec![files[0].clone(), files[2].clone()];
    assert_eq!(paths_of(&f, "Mix"), after);
    assert_eq!(paths_of(&f, "Chill"), chill);
    // Leaving the page dropped the undo.
    open_playlist(&mut f, "Mix");
    f.harness.key_press_modifiers(CTRL, egui::Key::Z);
    f.harness.run();
    assert_eq!(paths_of(&f, "Mix"), after);
}

#[test]
fn undo_refused_after_an_outside_change() {
    let mut f = library();
    let files = open_abc(&mut f);
    remove_row(&mut f, "b");
    let outside = vec![files[2].clone(), files[0].clone()];
    save_playlist(&f, "Mix", &outside);
    f.harness
        .get_by_role_and_label(Role::Button, "Undo")
        .click();
    f.harness.run();
    f.harness.get_by_label("Mix changed since. Can't undo.");
    assert_eq!(paths_of(&f, "Mix"), outside);
}

#[test]
fn remove_refused_after_an_outside_reorder() {
    let mut f = library();
    let files = open_abc(&mut f);
    let outside = vec![files[2].clone(), files[0].clone(), files[1].clone()];
    save_playlist(&f, "Mix", &outside);
    remove_row(&mut f, "a");
    f.harness
        .get_by_label("Couldn't change Mix: Mix changed on disk. Reopen it.");
    assert_eq!(paths_of(&f, "Mix"), outside);
    let top = |f: &Fixture, t: &str| {
        f.harness
            .get_by_role_and_label(Role::ListItem, t)
            .rect()
            .top()
    };
    assert!(top(&f, "c") < top(&f, "a"), "the page shows the new order");
    assert!(
        f.harness
            .query_by_role_and_label(Role::Button, "Undo")
            .is_none()
    );
}

#[test]
fn plugin_add_to_the_open_playlist_clears_its_selection() {
    let mut f = library();
    let files = real_files(&f, &["a", "b"]);
    save_playlist(&f, "Mix", &files);
    open_playlist(&mut f, "Mix");
    f.click_track_with("b", Modifiers::NONE);
    assert_eq!(playlist_selection(&f, "Mix", 2), vec![1]);
    let so_what = PathBuf::from("/music/Miles Davis/Kind of Blue/So What.flac");
    let result = f
        .harness
        .state_mut()
        .answer_plugin_call(Call::PlaylistsAdd {
            name: "Mix".into(),
            paths: vec![so_what],
        });
    assert!(result.is_ok(), "{result:?}");
    f.harness.run();
    assert!(playlist_selection(&f, "Mix", 3).is_empty());
    // Re-read: the new row is not on disk here, so it is counted as hidden.
    f.harness.get_by_label("3 tracks · 1 not found");
    f.click_track_with("a", Modifiers::NONE);
    assert_eq!(playlist_selection(&f, "Mix", 3), vec![0]);
}

#[test]
fn open_page_rereads_after_a_plugin_save() {
    let mut f = library();
    let files = real_files(&f, &["a", "b"]);
    save_playlist(&f, "Mix", &files[..1]);
    open_playlist(&mut f, "Mix");
    assert!(
        f.harness
            .query_by_role_and_label(Role::ListItem, "b")
            .is_none()
    );
    // Changed outside nil; the page has no file watching.
    save_playlist(&f, "Mix", &files);
    let so_what = PathBuf::from("/music/Miles Davis/Kind of Blue/So What.flac");
    let result = f
        .harness
        .state_mut()
        .answer_plugin_call(Call::PlaylistsSave {
            name: "Mix".into(),
            paths: vec![so_what],
        });
    assert!(result.is_ok(), "{result:?}");
    f.harness.run();
    f.harness.get_by_role_and_label(Role::ListItem, "b");
}

const PLAYING_NOW: &str = "Playing now · changes apply next time you play it.";

fn click_new_playlist(f: &mut Fixture, name: &str) {
    f.harness
        .get_by_role_and_label(Role::Button, "New playlist")
        .click();
    f.harness.run();
    f.harness.get_by_label("Playlist name").type_text(name);
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Create")
        .click();
    f.harness.run();
}

#[test]
fn new_playlist_button_creates_and_opens_an_empty_playlist() {
    let mut f = library();
    tab(&mut f, "Playlists");
    click_new_playlist(&mut f, "Fresh");
    assert!(playlists_dir(&f).join("Fresh.m3u8").exists());
    assert_eq!(
        f.harness.state().nav().current(),
        &Page::Playlist("Fresh".into())
    );
    f.harness
        .get_by_label("No tracks yet. Right-click a track and choose Add to playlist.");
}

#[test]
fn new_playlist_dialog_refuses_a_taken_name() {
    let mut f = library();
    let files = real_files(&f, &["a"]);
    save_playlist(&f, "Mix", &files);
    tab(&mut f, "Playlists");
    click_new_playlist(&mut f, "Mix");
    f.harness.get_by_label_contains("already exists");
    assert_eq!(f.harness.state().nav().current(), &Page::Playlists);
    // The existing playlist is untouched.
    let got = bp_gui::playlists::read(&playlists_dir(&f), "Mix").unwrap();
    assert_eq!(got.len(), 1);
}

#[test]
fn playing_playlist_page_says_changes_apply_next_time() {
    let mut f = library();
    let files = real_files(&f, &["a", "b"]);
    save_playlist(&f, "Mix", &files);
    open_playlist(&mut f, "Mix");
    assert!(f.harness.query_by_label(PLAYING_NOW).is_none());
    f.harness
        .get_by_role_and_label(Role::Button, "Play playlist")
        .click();
    f.harness.run();
    assert!(matches!(f.last_sent(), Some(PlayerCommand::Play { .. })));
    f.harness.get_by_label(PLAYING_NOW);
    // Another playlist is not the one playing.
    save_playlist(&f, "Other", &files);
    tab(&mut f, "Playlists");
    f.harness
        .get_by_role_and_label(Role::Button, "Other")
        .click();
    f.harness.run();
    assert!(f.harness.query_by_label(PLAYING_NOW).is_none());
    // Stop ends it.
    open_playlist(&mut f, "Mix");
    f.harness.get_by_label(PLAYING_NOW);
    f.harness.state_mut().send(PlayerCommand::Stop);
    f.harness.run();
    assert!(f.harness.query_by_label(PLAYING_NOW).is_none());
}

#[test]
fn queue_finished_drops_the_playing_now_line() {
    let mut f = library();
    let files = real_files(&f, &["a"]);
    save_playlist(&f, "Mix", &files);
    open_playlist(&mut f, "Mix");
    f.harness
        .get_by_role_and_label(Role::Button, "Play playlist")
        .click();
    f.harness.run();
    f.harness.get_by_label(PLAYING_NOW);
    f.player
        .inject(PlayerEvent::Engine(bp_engine::Event::QueueFinished));
    f.harness.run();
    assert!(f.harness.query_by_label(PLAYING_NOW).is_none());
}

#[test]
fn playing_an_album_drops_the_playing_now_line() {
    let mut f = library();
    let files = real_files(&f, &["a"]);
    save_playlist(&f, "Mix", &files);
    open_playlist(&mut f, "Mix");
    f.harness
        .get_by_role_and_label(Role::Button, "Play playlist")
        .click();
    f.harness.run();
    f.harness.get_by_label(PLAYING_NOW);
    tab(&mut f, "Albums");
    f.open_album("Kind of Blue");
    f.harness
        .get_by_role_and_label(Role::Button, "Play album")
        .click();
    f.harness.run();
    open_playlist(&mut f, "Mix");
    assert!(f.harness.query_by_label(PLAYING_NOW).is_none());
}

#[test]
fn plugin_add_to_another_playlist_keeps_the_open_selection() {
    let mut f = library();
    let files = real_files(&f, &["a", "b"]);
    save_playlist(&f, "Mix", &files);
    save_playlist(&f, "Other", &files[..1]);
    open_playlist(&mut f, "Mix");
    f.click_track_with("b", Modifiers::NONE);
    assert_eq!(playlist_selection(&f, "Mix", 2), vec![1]);
    let result = f
        .harness
        .state_mut()
        .answer_plugin_call(Call::PlaylistsAdd {
            name: "Other".into(),
            paths: vec![PathBuf::from(
                "/music/Miles Davis/Kind of Blue/So What.flac",
            )],
        });
    assert!(result.is_ok(), "{result:?}");
    f.harness.run();
    assert_eq!(playlist_selection(&f, "Mix", 2), vec![1]);
}

#[test]
fn plugin_save_of_a_new_name_refreshes_the_playlist_list() {
    let mut f = library();
    tab(&mut f, "Playlists");
    assert!(
        f.harness
            .query_by_role_and_label(Role::Button, "Fresh")
            .is_none()
    );
    let so_what = PathBuf::from("/music/Miles Davis/Kind of Blue/So What.flac");
    let result = f
        .harness
        .state_mut()
        .answer_plugin_call(Call::PlaylistsSave {
            name: "Fresh".into(),
            paths: vec![so_what],
        });
    assert!(result.is_ok(), "{result:?}");
    f.harness.run();
    f.harness.get_by_role_and_label(Role::Button, "Fresh");
}

fn rename_open_playlist(f: &mut Fixture, to: &str) {
    f.harness
        .get_by_role_and_label(Role::Button, "Rename playlist")
        .click();
    f.harness.run();
    f.harness.get_by_label("Playlist name").focus();
    f.harness.run();
    f.harness.key_press_modifiers(CTRL, egui::Key::A);
    f.harness.run();
    f.harness.get_by_label("Playlist name").type_text(to);
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Rename")
        .click();
    f.harness.run();
}

#[test]
fn playing_now_follows_a_rename_and_not_a_new_playlist_of_the_old_name() {
    let mut f = library();
    let files = real_files(&f, &["a"]);
    save_playlist(&f, "Mix", &files);
    open_playlist(&mut f, "Mix");
    f.harness
        .get_by_role_and_label(Role::Button, "Play playlist")
        .click();
    f.harness.run();
    rename_open_playlist(&mut f, "Foo");
    f.harness.get_by_label(PLAYING_NOW);
    // A new playlist with the old name is not what is playing.
    save_playlist(&f, "Mix", &files);
    tab(&mut f, "Playlists");
    f.harness.get_by_role_and_label(Role::Button, "Mix").click();
    f.harness.run();
    assert!(f.harness.query_by_label(PLAYING_NOW).is_none());
}

#[test]
fn playing_now_ends_when_the_playlist_is_deleted() {
    let mut f = library();
    let files = real_files(&f, &["a"]);
    save_playlist(&f, "Mix", &files);
    open_playlist(&mut f, "Mix");
    f.harness
        .get_by_role_and_label(Role::Button, "Play playlist")
        .click();
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Delete playlist")
        .click();
    f.harness.run();
    f.harness
        .get_by_role_and_label(Role::Button, "Delete")
        .click();
    f.harness.run();
    save_playlist(&f, "Mix", &files);
    tab(&mut f, "Playlists");
    f.harness.get_by_role_and_label(Role::Button, "Mix").click();
    f.harness.run();
    assert!(f.harness.query_by_label(PLAYING_NOW).is_none());
}

// --- Playlist rows: drag to reorder ---

fn row_rect(f: &Fixture, title: &str) -> egui::Rect {
    f.harness
        .get_by_role_and_label(Role::ListItem, title)
        .rect()
}

fn pointer(f: &mut Fixture, pos: egui::Pos2, pressed: Option<bool>) {
    let events = &mut f.harness.input_mut().events;
    events.push(egui::Event::PointerMoved(pos));
    if let Some(pressed) = pressed {
        events.push(egui::Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        });
    }
    f.harness.run();
}

/// Presses on the row titled `title` and moves, in steps past the drag threshold, to
/// `to`, holding the button.
fn press_and_move(f: &mut Fixture, title: &str, to: egui::Pos2) {
    let start = row_rect(f, title).center();
    pointer(f, start, None);
    pointer(f, start, Some(true));
    for k in 1..=4 {
        let t = k as f32 / 4.0;
        pointer(f, start + (to - start) * t, None);
    }
}

fn drag_row(f: &mut Fixture, title: &str, to: egui::Pos2) {
    press_and_move(f, title, to);
    pointer(f, to, Some(false));
    f.harness.run();
}

/// A point just inside the top of row `title`: the gap above it.
fn above(f: &Fixture, title: &str) -> egui::Pos2 {
    let r = row_rect(f, title);
    egui::pos2(r.center().x, r.top() + 4.0)
}

fn mtime(f: &Fixture, name: &str) -> std::time::SystemTime {
    std::fs::metadata(playlists_dir(f).join(format!("{name}.m3u8")))
        .unwrap()
        .modified()
        .unwrap()
}

#[test]
fn drag_a_row_reorders_the_file() {
    let mut f = library();
    let files = open_abc(&mut f);
    let to = above(&f, "a");
    drag_row(&mut f, "b", to);
    assert_eq!(
        paths_of(&f, "Mix"),
        vec![files[1].clone(), files[0].clone(), files[2].clone()]
    );
    f.harness.get_by_label("Moved 1 track");
    assert!(row_rect(&f, "b").top() < row_rect(&f, "a").top());
    // The moved row stays selected, at its new file index.
    assert_eq!(playlist_selection(&f, "Mix", 3), vec![0]);
}

#[test]
fn drag_several_selected_rows_moves_them_together() {
    let mut f = library();
    let files = open_abc(&mut f);
    f.click_track_with("a", Modifiers::NONE);
    f.click_track_with("b", CTRL);
    let r = row_rect(&f, "c");
    drag_row(&mut f, "a", egui::pos2(r.center().x, r.bottom() - 4.0));
    assert_eq!(
        paths_of(&f, "Mix"),
        vec![files[2].clone(), files[0].clone(), files[1].clone()]
    );
    f.harness.get_by_label("Moved 2 tracks");
    assert_eq!(playlist_selection(&f, "Mix", 3), vec![1, 2]);
}

#[test]
fn drag_carries_a_hidden_row_with_it() {
    let mut f = library();
    let mut files = real_files(&f, &["a", "c"]);
    files.insert(1, f.dir.path().join("gone.flac"));
    save_playlist(&f, "Mix", &files);
    open_playlist(&mut f, "Mix");
    let to = above(&f, "a");
    drag_row(&mut f, "c", to);
    assert_eq!(
        paths_of(&f, "Mix"),
        vec![files[2].clone(), files[0].clone(), files[1].clone()]
    );
    f.harness.get_by_label("Moved 1 track");
}

#[test]
fn drag_then_undo_restores_the_order() {
    let mut f = library();
    let files = open_abc(&mut f);
    let to = above(&f, "a");
    drag_row(&mut f, "c", to);
    assert_eq!(
        paths_of(&f, "Mix"),
        vec![files[2].clone(), files[0].clone(), files[1].clone()]
    );
    f.harness
        .get_by_role_and_label(Role::Button, "Undo")
        .click();
    f.harness.run();
    assert_eq!(paths_of(&f, "Mix"), files);
    assert!(row_rect(&f, "a").top() < row_rect(&f, "c").top());
}

#[test]
fn drop_in_place_does_not_write() {
    let mut f = library();
    let files = open_abc(&mut f);
    let before = mtime(&f, "Mix");
    let to = above(&f, "b");
    drag_row(&mut f, "b", to);
    assert_eq!(paths_of(&f, "Mix"), files);
    assert_eq!(mtime(&f, "Mix"), before);
    assert!(f.harness.query_by_label("Moved 1 track").is_none());
}

#[test]
fn esc_cancels_a_drag() {
    let mut f = library();
    let files = open_abc(&mut f);
    let to = above(&f, "a");
    press_and_move(&mut f, "b", to);
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();
    pointer(&mut f, to, Some(false));
    f.harness.run();
    assert_eq!(paths_of(&f, "Mix"), files);
    assert!(f.harness.query_by_label("Moved 1 track").is_none());
    // Esc cancelled only the drag: the row it selected stays selected, the page open.
    assert_eq!(playlist_selection(&f, "Mix", 3), vec![1]);
    assert_eq!(
        f.harness.state().nav().current(),
        &Page::Playlist("Mix".into())
    );
}

#[test]
fn release_outside_the_list_cancels_a_drag() {
    let mut f = library();
    let files = open_abc(&mut f);
    // Up over the playlist's heading, above the rows.
    let r = f
        .harness
        .get_by_role_and_label(Role::Button, "Play playlist")
        .rect();
    let to = egui::pos2(row_rect(&f, "a").center().x, r.top() - 30.0);
    drag_row(&mut f, "b", to);
    assert_eq!(paths_of(&f, "Mix"), files);
    assert!(f.harness.query_by_label("Moved 1 track").is_none());
}

#[test]
fn click_on_a_playlist_row_still_selects_after_drag_support() {
    let mut f = library();
    open_abc(&mut f);
    f.click_track_with("b", Modifiers::NONE);
    assert_eq!(playlist_selection(&f, "Mix", 3), vec![1]);
    f.double_click_track("c");
    match f.last_sent() {
        Some(PlayerCommand::Play { start, .. }) => assert_eq!(start, 2),
        other => panic!("{other:?}"),
    }
}

#[test]
fn drag_near_the_bottom_scrolls_the_list() {
    let mut f = library();
    let names: Vec<String> = (0..30).map(|i| format!("t{i:02}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let files = real_files(&f, &names);
    save_playlist(&f, "Mix", &files);
    open_playlist(&mut f, "Mix");
    let top = row_rect(&f, "t00").top();
    // Just above the floating bar: within one row of the list's visible bottom.
    let screen = f.harness.ctx.content_rect();
    let to = egui::pos2(screen.center().x, screen.bottom() - 76.0 - 10.0);
    let start = row_rect(&f, "t00").center();
    pointer(&mut f, start, None);
    pointer(&mut f, start, Some(true));
    for k in 1..=4 {
        let t = k as f32 / 4.0;
        f.harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(start + (to - start) * t));
        f.harness.step();
    }
    f.harness.run_steps(20);
    assert!(
        row_rect(&f, "t00").top() < top - 50.0,
        "the list scrolled while the pointer sat near its bottom"
    );
    // Esc ends it; nothing moved.
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();
    pointer(&mut f, to, Some(false));
    assert_eq!(paths_of(&f, "Mix"), files);
}

// --- Queue rows: drag to move ---

#[test]
fn drag_an_upcoming_row_moves_it() {
    let mut f = library();
    open_now_playing(&mut f);
    let to = above(&f, "Freddie Freeloader");
    drag_row(&mut f, "Flamenco Sketches", to);
    assert_eq!(
        queue_titles(&f),
        [
            "So What",
            "Flamenco Sketches",
            "Freddie Freeloader",
            "Blue in Green",
            "All Blues"
        ]
    );
    assert_eq!(np_notice(&f).as_deref(), Some("Moved 1 track"));
    f.harness.get_by_role_and_label(Role::Button, "Undo");
    // The moved row stays selected where it landed.
    assert_eq!(queue_rows(&f), vec![1]);
}

#[test]
fn drag_above_the_current_row_lands_next() {
    let mut f = library();
    open_now_playing(&mut f);
    let to = above(&f, "So What");
    drag_row(&mut f, "All Blues", to);
    assert_eq!(
        queue_titles(&f),
        [
            "So What",
            "All Blues",
            "Freddie Freeloader",
            "Blue in Green",
            "Flamenco Sketches"
        ]
    );
    assert_eq!(np_notice(&f).as_deref(), Some("Moved 1 track"));
    assert_eq!(queue_rows(&f), vec![1]);
}

#[test]
fn the_current_row_does_not_drag() {
    let mut f = library();
    open_now_playing(&mut f);
    // Selected with an upcoming row, the current row would carry it along if it dragged.
    f.click_track_with("So What", Modifiers::NONE);
    f.click_track_with("Flamenco Sketches", CTRL);
    assert_eq!(queue_rows(&f), vec![0, 4]);
    let to = above(&f, "Freddie Freeloader");
    drag_row(&mut f, "So What", to);
    assert!(rearranges(&f).is_empty());
    assert_eq!(queue_titles(&f), KIND_OF_BLUE);
    assert_eq!(queue_rows(&f), vec![0, 4]);
}

#[test]
fn drag_held_behind_a_remove_lands_where_it_was_dropped() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    f.right_click_track("Freddie Freeloader");
    f.menu_item("Remove from queue");
    // The rows still show the queue before the remove: the gap is above All Blues.
    let to = above(&f, "All Blues");
    drag_row(&mut f, "Flamenco Sketches", to);
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(
        queue_titles(&f),
        ["So What", "Blue in Green", "Flamenco Sketches", "All Blues"]
    );
    assert_eq!(np_notice(&f).as_deref(), Some("Moved 1 track"));
}

#[test]
fn drag_held_behind_an_insert_lands_where_it_was_dropped() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    let kid_a = album_items(&f, "Kid A");
    f.harness
        .state_mut()
        .send(PlayerCommand::Insert { items: kid_a });
    let to = above(&f, "All Blues");
    drag_row(&mut f, "Flamenco Sketches", to);
    assert!(rearranges(&f).is_empty());
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(
        queue_titles(&f),
        [
            "So What",
            "Everything in Its Right Place",
            "Freddie Freeloader",
            "Blue in Green",
            "Flamenco Sketches",
            "All Blues"
        ]
    );
    assert_eq!(np_notice(&f).as_deref(), Some("Moved 1 track"));
}

// --- Final fix wave ---

#[test]
fn scan_progress_tick_keeps_a_playlist_selection() {
    let mut f = library();
    open_abc(&mut f);
    f.click_track_with("b", Modifiers::NONE);
    assert_eq!(playlist_selection(&f, "Mix", 3), vec![1]);
    f.lib
        .set_scan_status(ScanStatus::Scanning { done: 1, total: 9 });
    f.harness.run();
    f.lib
        .set_scan_status(ScanStatus::Scanning { done: 2, total: 9 });
    f.harness.run();
    assert_eq!(playlist_selection(&f, "Mix", 3), vec![1]);
    // The scan finishing may have changed the list: the selection goes.
    f.lib.set_scan_status(ScanStatus::Done { unreadable: 0 });
    f.harness.run();
    assert!(playlist_selection(&f, "Mix", 3).is_empty());
}

fn queue_rows(f: &Fixture) -> Vec<usize> {
    let shown: Vec<usize> = (0..16).collect();
    f.harness.state().selection().rows(&ListId::Queue, &shown)
}

/// The play queue's titles, in order.
fn queue_titles(f: &Fixture) -> Vec<String> {
    f.harness
        .state()
        .now_playing()
        .queue
        .iter()
        .map(|i| i.title.clone())
        .collect()
}

/// The app's notice in the Now Playing queue header (the line on its right). Only that
/// line has `Role::Status`; the bar's notice, shown when Now Playing is closed, does not.
fn np_notice(f: &Fixture) -> Option<String> {
    f.harness
        .query_by(|n| n.role() == Role::Status)
        .and_then(|n| n.accesskit_node().label())
}

#[test]
fn plugin_append_into_an_idle_queue_clears_a_queue_selection() {
    let mut f = library();
    open_now_playing(&mut f);
    f.click_track_with("Blue in Green", Modifiers::NONE);
    assert_eq!(queue_rows(&f), vec![2]);
    f.player.inject(PlayerEvent::Engine(bp_engine::Event::Error(
        EngineError::NoDevice,
    )));
    f.harness.run();
    assert!(f.harness.state().now_playing_open());
    assert_eq!(queue_rows(&f), vec![2]);
    let tone = PathBuf::from("/music/Test Artist/Test Tones/Tone 128.flac");
    let result = f
        .harness
        .state_mut()
        .answer_plugin_call(Call::PlayerAppend { paths: vec![tone] });
    assert!(result.is_ok(), "{result:?}");
    f.harness.run();
    assert!(queue_rows(&f).is_empty());
}

#[test]
fn plugin_append_into_an_idle_queue_drops_the_playing_now_line() {
    let mut f = library();
    let files = real_files(&f, &["a"]);
    save_playlist(&f, "Mix", &files);
    open_playlist(&mut f, "Mix");
    f.harness
        .get_by_role_and_label(Role::Button, "Play playlist")
        .click();
    f.harness.run();
    f.harness.get_by_label(PLAYING_NOW);
    f.player.inject(PlayerEvent::Engine(bp_engine::Event::Error(
        EngineError::NoDevice,
    )));
    f.harness.run();
    let so_what = PathBuf::from("/music/Miles Davis/Kind of Blue/So What.flac");
    let result = f
        .harness
        .state_mut()
        .answer_plugin_call(Call::PlayerAppend {
            paths: vec![so_what],
        });
    assert!(result.is_ok(), "{result:?}");
    f.harness.run();
    assert!(f.harness.query_by_label(PLAYING_NOW).is_none());
}

/// A Kind of Blue track's path.
fn kind_of_blue(title: &str) -> PathBuf {
    PathBuf::from(format!("/music/Miles Davis/Kind of Blue/{title}.flac"))
}

/// `player.remove` of the queue rows given as (index, title), as a plugin sends it.
fn plugin_remove(f: &mut Fixture, tracks: &[(usize, &str)]) -> PluginResult {
    let tracks = tracks
        .iter()
        .map(|&(i, title)| (i, kind_of_blue(title)))
        .collect();
    f.harness
        .state_mut()
        .answer_plugin_call(Call::PlayerRemove { tracks })
}

type PluginResult = Result<serde_json::Value, RpcError>;

fn plugin_replies(f: &mut Fixture) -> Vec<PluginResult> {
    f.harness.state_mut().take_plugin_replies()
}

fn queue_changed(reason: &str) -> PluginResult {
    Err(RpcError::queue_changed(reason))
}

#[test]
fn plugin_remove_is_answered_when_applied() {
    let mut f = library();
    open_now_playing(&mut f);
    let deferred = plugin_remove(&mut f, &[(2, "Blue in Green")]);
    assert_eq!(deferred, Ok(serde_json::Value::Null));
    assert!(plugin_replies(&mut f).is_empty());
    f.harness.run();
    assert_eq!(plugin_replies(&mut f), [Ok(serde_json::json!({}))]);
    assert_eq!(
        queue_titles(&f),
        [
            "So What",
            "Freddie Freeloader",
            "All Blues",
            "Flamenco Sketches"
        ]
    );
    assert!(np_notice(&f).is_none());
}

#[test]
fn plugin_remove_while_starting_is_refused() {
    let mut f = library();
    open_now_playing(&mut f);
    f.advance(298.5);
    plugin_remove(&mut f, &[(1, "Freddie Freeloader")]).unwrap();
    f.harness.run();
    assert_eq!(plugin_replies(&mut f), [queue_changed("started")]);
    assert_eq!(queue_titles(&f), KIND_OF_BLUE);
    assert!(np_notice(&f).is_none());
}

#[test]
fn plugin_clear_while_starting_counts_what_it_cleared() {
    let mut f = library();
    open_now_playing(&mut f);
    f.advance(298.5);
    let deferred = f
        .harness
        .state_mut()
        .answer_plugin_call(Call::PlayerClearUpcoming);
    assert_eq!(deferred, Ok(serde_json::Value::Null));
    f.harness.run();
    assert_eq!(
        plugin_replies(&mut f),
        [Ok(serde_json::json!({ "removed": 3 }))]
    );
    assert_eq!(queue_titles(&f), ["So What", "Freddie Freeloader"]);
    assert!(np_notice(&f).is_none());
}

#[test]
fn plugin_clear_with_nothing_upcoming_is_answered_at_once() {
    let mut f = library();
    open_now_playing(&mut f);
    f.harness
        .state_mut()
        .answer_plugin_call(Call::PlayerClearUpcoming)
        .unwrap();
    f.harness.run();
    assert_eq!(
        plugin_replies(&mut f),
        [Ok(serde_json::json!({ "removed": 4 }))]
    );
    let sent = rearranges(&f).len();
    let r = f
        .harness
        .state_mut()
        .answer_plugin_call(Call::PlayerClearUpcoming);
    assert_eq!(r, Ok(serde_json::json!({ "removed": 0 })));
    assert_eq!(rearranges(&f).len(), sent);
    assert!(plugin_replies(&mut f).is_empty());
}

#[test]
fn plugin_move_that_changes_nothing_is_answered_at_once() {
    let mut f = library();
    open_now_playing(&mut f);
    let r = f.harness.state_mut().answer_plugin_call(Call::PlayerMove {
        tracks: vec![(1, kind_of_blue("Freddie Freeloader"))],
        to: 1,
    });
    assert_eq!(r, Ok(serde_json::json!({})));
    assert!(rearranges(&f).is_empty());
    let r = f.harness.state_mut().answer_plugin_call(Call::PlayerMove {
        tracks: vec![(4, kind_of_blue("Flamenco Sketches"))],
        to: 1,
    });
    assert_eq!(r, Ok(serde_json::Value::Null));
    f.harness.run();
    assert_eq!(plugin_replies(&mut f), [Ok(serde_json::json!({}))]);
    assert_eq!(queue_titles(&f)[1], "Flamenco Sketches");
}

#[test]
fn pending_plugin_edit_gets_stopped_on_stop() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    plugin_remove(&mut f, &[(2, "Blue in Green")]).unwrap();
    f.harness.state_mut().send(PlayerCommand::Stop);
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(plugin_replies(&mut f), [queue_changed("stopped")]);
}

#[test]
fn plugin_edit_voided_by_play_ignores_its_late_answer() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    plugin_remove(&mut f, &[(2, "Blue in Green")]).unwrap();
    let paths = ["Flamenco Sketches", "All Blues", "Blue in Green"]
        .map(kind_of_blue)
        .to_vec();
    let played = f
        .harness
        .state_mut()
        .answer_plugin_call(Call::PlayerPlay { paths, start: 0 });
    assert_eq!(played, Ok(serde_json::json!({})));
    plugin_remove(&mut f, &[(1, "All Blues")]).unwrap();
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(
        plugin_replies(&mut f),
        [queue_changed("stopped"), Ok(serde_json::json!({}))]
    );
    assert_eq!(queue_titles(&f), ["Flamenco Sketches", "Blue in Green"]);
}

#[test]
fn plugin_edit_drops_the_queue_undo() {
    let mut f = library();
    open_now_playing(&mut f);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    f.harness.run();
    f.harness.get_by_role_and_label(Role::Button, "Undo");
    plugin_remove(&mut f, &[(3, "Flamenco Sketches")]).unwrap();
    f.harness.run();
    assert_eq!(plugin_replies(&mut f), [Ok(serde_json::json!({}))]);
    assert_eq!(
        queue_titles(&f),
        ["So What", "Freddie Freeloader", "All Blues"]
    );
    let sent = f.player.sent().len();
    f.harness.key_press_modifiers(CTRL, egui::Key::Z);
    f.harness.run();
    assert_eq!(f.player.sent().len(), sent);
    assert_eq!(
        queue_titles(&f),
        ["So What", "Freddie Freeloader", "All Blues"]
    );
}

#[test]
fn edit_held_behind_a_plugin_edit_lands_on_the_clicked_row() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    plugin_remove(&mut f, &[(1, "Freddie Freeloader")]).unwrap();
    f.right_click_track("All Blues");
    f.menu_item("Remove from queue");
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(plugin_replies(&mut f), [Ok(serde_json::json!({}))]);
    assert_eq!(
        queue_titles(&f),
        ["So What", "Blue in Green", "Flamenco Sketches"]
    );
    assert_eq!(np_notice(&f).as_deref(), Some("Removed 1 track"));
}

#[test]
fn inserted_splice_clears_a_queue_selection() {
    let mut f = library();
    open_now_playing(&mut f);
    f.click_track_with("Blue in Green", Modifiers::NONE);
    assert_eq!(queue_rows(&f), vec![2]);
    let items = album_items(&f, "Kid A");
    f.harness.state_mut().send(PlayerCommand::Insert { items });
    // The rows move only when the engine says where the insert went.
    assert_eq!(queue_rows(&f), vec![2]);
    f.harness.run();
    assert_eq!(f.harness.state().now_playing().queue.len(), 6);
    assert!(queue_rows(&f).is_empty());
}

#[test]
fn secondary_drag_does_not_reorder() {
    let mut f = library();
    let files = open_abc(&mut f);
    let start = row_rect(&f, "b").center();
    let to = above(&f, "a");
    let press = |f: &mut Fixture, pos: egui::Pos2, pressed: Option<bool>| {
        let events = &mut f.harness.input_mut().events;
        events.push(egui::Event::PointerMoved(pos));
        if let Some(pressed) = pressed {
            events.push(egui::Event::PointerButton {
                pos,
                button: PointerButton::Secondary,
                pressed,
                modifiers: Modifiers::default(),
            });
        }
        f.harness.run();
    };
    press(&mut f, start, None);
    press(&mut f, start, Some(true));
    for k in 1..=4 {
        let t = k as f32 / 4.0;
        press(&mut f, start + (to - start) * t, None);
    }
    press(&mut f, to, Some(false));
    f.harness.run();
    assert_eq!(paths_of(&f, "Mix"), files);
    assert!(f.harness.query_by_label("Moved 1 track").is_none());
    assert!(row_rect(&f, "a").top() < row_rect(&f, "b").top());
}

#[test]
fn undo_works_with_now_playing_open() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.double_click_track("So What");
    f.advance(0.4);
    let files = open_abc(&mut f);
    remove_row(&mut f, "b");
    assert_eq!(
        paths_of(&f, "Mix"),
        vec![files[0].clone(), files[2].clone()]
    );
    f.harness
        .get_by_role_and_label(Role::Button, "Open now playing")
        .click();
    f.harness.run();
    assert!(f.harness.state().now_playing_open());
    // Now Playing has no bar, so Undo is the shortcut here.
    f.harness.key_press_modifiers(CTRL, egui::Key::Z);
    f.harness.run();
    assert_eq!(paths_of(&f, "Mix"), files);
}

#[test]
fn backspace_with_a_selection_goes_back() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.click_track_with("So What", Modifiers::NONE);
    assert_eq!(album_selection(&f, "Kind of Blue"), vec![0]);
    f.harness.key_press(egui::Key::Backspace);
    f.harness.run();
    assert_eq!(f.harness.state().nav().current(), &Page::Home);
}

#[test]
fn esc_with_the_device_popover_open_closes_it_and_keeps_the_selection() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.click_track_with("So What", Modifiers::NONE);
    f.harness
        .get_by_role_and_label(Role::Button, "DAC A")
        .click();
    f.harness.run();
    f.harness.get_by_label("Device capabilities…");
    assert_eq!(album_selection(&f, "Kind of Blue"), vec![0]);
    f.harness.key_press(egui::Key::Escape);
    f.harness.run();
    assert!(f.harness.query_by_label("Device capabilities…").is_none());
    assert_eq!(album_selection(&f, "Kind of Blue"), vec![0]);
    assert!(matches!(
        f.harness.state().nav().current(),
        Page::Album { .. }
    ));
}

#[test]
fn add_to_playlist_row_matches_its_siblings() {
    let mut f = library();
    open_now_playing(&mut f);
    f.right_click_track("Blue in Green");
    let menu = f.harness.get_all_by_role(Role::Menu).last().unwrap();
    let rect = |label: &str| menu.get_by_role_and_label(Role::Button, label).rect();
    let sibling = rect("Play from here");
    let sub = rect("Add to playlist");
    assert_eq!(sub.height(), sibling.height());
    assert_eq!(sub.height(), 40.0);
    assert_eq!(sub.left(), sibling.left());
    assert_eq!(sub.width(), sibling.width());
}

// --- Final review fixes ---

/// Plays Kind of Blue from `title` and opens the now-playing view.
fn open_now_playing_from(f: &mut Fixture, title: &str) {
    f.open_album("Kind of Blue");
    f.double_click_track(title);
    f.advance(0.4);
    f.harness
        .get_by_role_and_label(Role::Button, "Open now playing")
        .click();
    f.harness.run();
    assert!(f.harness.state().now_playing_open());
}

#[test]
fn plugin_append_while_an_edit_is_on_its_way_is_kept() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    let tone = PathBuf::from("/music/Test Artist/Test Tones/Tone 128.flac");
    let r = f
        .harness
        .state_mut()
        .answer_plugin_call(Call::PlayerAppend { paths: vec![tone] });
    assert_eq!(r, Ok(serde_json::json!({})));
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(
        queue_titles(&f),
        [
            "So What",
            "Freddie Freeloader",
            "All Blues",
            "Flamenco Sketches",
            "Tone 128"
        ]
    );
    assert_eq!(np_notice(&f).as_deref(), Some("Removed 1 track"));
}

#[test]
fn add_to_queue_while_an_edit_is_on_its_way_is_kept() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    f.harness
        .get_by_role_and_label(Role::Button, "Close now playing")
        .click();
    f.harness.run();
    f.right_click_track("So What");
    f.menu_item("Add to queue");
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(
        queue_titles(&f),
        [
            "So What",
            "Freddie Freeloader",
            "All Blues",
            "Flamenco Sketches",
            "So What"
        ]
    );
}

#[test]
fn plugin_clear_behind_a_gui_edit_clears_what_is_left() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    f.right_click_track("Blue in Green");
    f.menu_item("Remove from queue");
    let deferred = f
        .harness
        .state_mut()
        .answer_plugin_call(Call::PlayerClearUpcoming);
    assert_eq!(deferred, Ok(serde_json::Value::Null));
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(
        plugin_replies(&mut f),
        [Ok(serde_json::json!({ "removed": 3 }))]
    );
    assert_eq!(queue_titles(&f), ["So What"]);
}

#[test]
fn clear_of_only_the_starting_track_says_already_starting() {
    let mut f = library();
    open_now_playing_from(&mut f, "All Blues");
    f.advance(298.5);
    clear_upcoming(&mut f);
    assert_eq!(rearranges(&f).len(), 1);
    assert_eq!(
        np_notice(&f).as_deref(),
        Some("Already starting: Flamenco Sketches")
    );
    assert_eq!(queue_titles(&f), KIND_OF_BLUE);
    assert!(
        f.harness
            .query_by_role_and_label(Role::Button, "Undo")
            .is_none()
    );
}

#[test]
fn plugin_clear_of_only_the_starting_track_removes_none() {
    let mut f = library();
    open_now_playing_from(&mut f, "All Blues");
    f.advance(298.5);
    f.harness
        .state_mut()
        .answer_plugin_call(Call::PlayerClearUpcoming)
        .unwrap();
    f.harness.run();
    assert_eq!(
        plugin_replies(&mut f),
        [Ok(serde_json::json!({ "removed": 0 }))]
    );
    assert_eq!(rearranges(&f).len(), 1);
    assert_eq!(queue_titles(&f), KIND_OF_BLUE);
}

#[test]
fn plugin_clear_refused_started_twice_is_answered_once() {
    let mut f = library();
    open_now_playing(&mut f);
    f.advance(298.5);
    f.player.hold_events(true);
    f.harness
        .state_mut()
        .answer_plugin_call(Call::PlayerClearUpcoming)
        .unwrap();
    // The next track plays and nears its end before the refusal is seen: the clear
    // sent again after it is refused too.
    f.advance(300.0);
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(plugin_replies(&mut f), [queue_changed("started")]);
    assert_eq!(rearranges(&f).len(), 2);
    assert_eq!(queue_titles(&f), KIND_OF_BLUE);
}

#[test]
fn delete_with_a_queue_row_menu_open_sends_nothing() {
    let mut f = library();
    open_now_playing(&mut f);
    f.click_track_with("Blue in Green", Modifiers::NONE);
    f.right_click_track("Blue in Green");
    f.harness.key_press(egui::Key::Delete);
    f.harness.run();
    assert!(rearranges(&f).is_empty());
    assert_eq!(queue_titles(&f), KIND_OF_BLUE);
}

#[test]
fn plugin_edit_mid_drag_ends_the_drag() {
    let mut f = library();
    open_now_playing(&mut f);
    let to = above(&f, "Freddie Freeloader");
    press_and_move(&mut f, "All Blues", to);
    plugin_remove(&mut f, &[(2, "Blue in Green")]).unwrap();
    f.harness.run();
    assert_eq!(plugin_replies(&mut f), [Ok(serde_json::json!({}))]);
    // Row 3, dragged as All Blues, is Flamenco Sketches now: the release moves nothing.
    pointer(&mut f, to, Some(false));
    f.harness.run();
    assert_eq!(rearranges(&f).len(), 1);
    assert_eq!(
        queue_titles(&f),
        [
            "So What",
            "Freddie Freeloader",
            "All Blues",
            "Flamenco Sketches"
        ]
    );
}

#[test]
fn plugin_edit_closes_an_open_queue_row_menu() {
    let mut f = library();
    open_now_playing(&mut f);
    f.right_click_track("All Blues");
    assert!(f.harness.query_all_by_role(Role::Menu).next().is_some());
    // Row 3, whose menu is open, becomes Flamenco Sketches.
    plugin_remove(&mut f, &[(1, "Freddie Freeloader")]).unwrap();
    f.harness.run();
    assert_eq!(plugin_replies(&mut f), [Ok(serde_json::json!({}))]);
    assert!(f.harness.query_all_by_role(Role::Menu).next().is_none());
}

#[test]
fn edit_held_behind_an_insert_ignores_a_stale_insert_answer() {
    let mut f = library();
    open_now_playing(&mut f);
    f.player.hold_events(true);
    let kid_a = album_items(&f, "Kid A");
    f.harness
        .state_mut()
        .send(PlayerCommand::Insert { items: kid_a });
    let all = album_items(&f, "Kind of Blue");
    f.harness.state_mut().send(PlayerCommand::Play {
        items: all.clone(),
        start: 0,
    });
    let tones = album_items(&f, "Test Tones");
    f.harness.state_mut().send(PlayerCommand::Insert {
        items: tones[..1].to_vec(),
    });
    f.harness.run();
    f.right_click_track("All Blues");
    f.menu_item("Remove from queue");
    assert!(rearranges(&f).is_empty());
    f.player.hold_events(false);
    f.harness.run();
    assert_eq!(
        queue_titles(&f),
        [
            "So What",
            "Tone 256",
            "Freddie Freeloader",
            "Blue in Green",
            "Flamenco Sketches"
        ]
    );
}

/// The last track the media session was told about.
fn media_track(updates: &[Update]) -> Option<Track> {
    updates.iter().rev().find_map(|u| match u {
        Update::Track(t) => Some(t.clone()),
        _ => None,
    })?
}

#[test]
fn the_media_session_shows_the_track_and_its_buttons_control_playback() {
    let mut f = library();
    f.open_album("Kind of Blue");
    f.double_click_track("Freddie Freeloader");
    f.advance(0.4);
    let updates: Vec<Update> = f.media_updates.try_iter().collect();
    let track = media_track(&updates).expect("a track");
    assert_eq!(
        (
            track.title.as_str(),
            track.artist.as_str(),
            track.album.as_str()
        ),
        ("Freddie Freeloader", "Miles Davis", "Kind of Blue")
    );
    assert!(updates.contains(&Update::Status(media::Status::Playing)));

    f.media_buttons.send(Button::Pause).unwrap();
    f.harness.run();
    assert_eq!(f.last_sent(), Some(PlayerCommand::Pause));
    assert!(
        f.media_updates
            .try_iter()
            .any(|u| u == Update::Status(media::Status::Paused))
    );

    f.media_buttons.send(Button::Play).unwrap();
    f.harness.run();
    assert_eq!(f.last_sent(), Some(PlayerCommand::Resume));

    f.media_buttons
        .send(Button::Seek(Duration::from_secs(60)))
        .unwrap();
    f.harness.run();
    assert_eq!(
        f.last_sent(),
        Some(PlayerCommand::Seek {
            position: Duration::from_secs(60)
        })
    );

    f.media_buttons.send(Button::Next).unwrap();
    f.harness.run();
    assert_eq!(f.last_sent(), Some(PlayerCommand::Next));
    f.advance(0.4);
    let updates: Vec<Update> = f.media_updates.try_iter().collect();
    assert_eq!(
        media_track(&updates).expect("the next track").title,
        "Blue in Green"
    );
}

#[test]
fn media_session_presses_with_nothing_loaded_do_nothing() {
    let mut f = library();
    for b in [Button::Play, Button::Pause, Button::Next, Button::Previous] {
        f.media_buttons.send(b).unwrap();
    }
    f.harness.run();
    assert_eq!(f.player.sent(), vec![]);
}
