//! Answers plugin calls from the app's state and describes player changes.
//!
//! Nothing here touches the player or the window: `answer` returns the command
//! a call asks for and `BpApp` sends it.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::backend::{PlayerCommand, QueueItem};
use crate::catalog::{Catalog, path_key};
use crate::playlists;
use crate::plugins::rpc::{Call, RpcError};
use crate::queue_edit::{self, EditKind};
use crate::state::now_playing::NowPlaying;

/// What a call is answered from.
pub struct BridgeCtx<'a> {
    pub catalog: &'a Catalog,
    pub np: &'a NowPlaying,
    pub folders: Vec<PathBuf>,
    pub playlists_dir: &'a Path,
    /// The player has a selected, usable output device.
    pub device_ready: bool,
}

/// The queue position that is actually playing or paused. A fatal error leaves
/// the index set but the player has nothing playing.
fn active(np: &NowPlaying) -> Option<usize> {
    np.index.filter(|_| np.fatal.is_none())
}

fn state_name(np: &NowPlaying) -> &'static str {
    match (active(np), np.paused) {
        (None, _) => "stopped",
        (Some(_), true) => "paused",
        (Some(_), false) => "playing",
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Parameters of `player.changed`: `{ state, path?, index?, queue_len }`.
pub fn changed_params(np: &NowPlaying) -> Value {
    let mut v = json!({ "state": state_name(np), "queue_len": np.queue.len() });
    if let Some(i) = active(np) {
        v["index"] = json!(i);
        if let Some(item) = np.queue.get(i) {
            v["path"] = json!(path_str(&item.path));
        }
    }
    v
}

/// What makes a `player.changed` due: the state, the queue position and the
/// queue length. Position ticks do not move it.
pub fn state_key(np: &NowPlaying) -> (u8, Option<usize>, usize) {
    let state = match state_name(np) {
        "playing" => 1,
        "paused" => 2,
        _ => 0,
    };
    (state, active(np), np.queue.len())
}

fn unknown_paths(paths: &[PathBuf]) -> RpcError {
    RpcError::not_found(
        format!("not in nil's library: {} path(s)", paths.len()),
        Some(json!({ "paths": paths.iter().map(|p| path_str(p)).collect::<Vec<_>>() })),
    )
}

fn player_state(np: &NowPlaying) -> Value {
    let mut v = json!({
        "state": state_name(np),
        "queue": np.queue.iter().map(|i| path_str(&i.path)).collect::<Vec<_>>(),
    });
    if let Some(i) = active(np) {
        v["index"] = json!(i);
        v["position_s"] = json!(np.position.as_secs_f64());
        if let Some(item) = np.queue.get(i) {
            v["path"] = json!(path_str(&item.path));
        }
    }
    v
}

/// The Rearrange a plugin's queue edit asks for.
#[derive(Debug)]
pub struct QueueEditPlan {
    pub from: usize,
    /// The queue from `from` on, as the plugin's checks saw it.
    pub expected: Vec<QueueItem>,
    /// What replaces it. Equal to `expected` when the edit changes nothing.
    pub items: Vec<QueueItem>,
    pub kind: EditKind,
    /// The position in `expected` each row of `items` came from, so a GUI edit held
    /// behind this one can follow its rows.
    pub order: Vec<usize>,
}

/// Checks a `player.remove`, `player.move` or `player.clear_upcoming` against the queue
/// and plans its Rearrange. In order: bad params (`to` past the end), nothing playing
/// (`"stopped"`), a row whose index or path does not match the queue (`"changed"`), a row
/// or `to` at or before the current track (`"started"`). A clear is never `"changed"`.
pub fn edit(call: &Call, np: &NowPlaying) -> Result<QueueEditPlan, RpcError> {
    let (tracks, to) = match call {
        Call::PlayerRemove { tracks } => (tracks.as_slice(), None),
        Call::PlayerMove { tracks, to } => (tracks.as_slice(), Some(*to)),
        Call::PlayerClearUpcoming => (&[][..], None),
        _ => return Err(RpcError::internal("not a queue edit")),
    };
    if to.is_some_and(|to| to > np.queue.len()) {
        return Err(RpcError::bad_params("to is past the end of the queue"));
    }
    let Some(current) = active(np) else {
        return Err(RpcError::queue_changed("stopped"));
    };
    let stale = |(i, path): &(usize, PathBuf)| {
        np.queue
            .get(*i)
            .is_none_or(|item| path_key(&item.path) != path_key(path))
    };
    if tracks.iter().any(stale) {
        return Err(RpcError::queue_changed("changed"));
    }
    if tracks.iter().any(|&(i, _)| i <= current) || to.is_some_and(|to| to <= current) {
        return Err(RpcError::queue_changed("started"));
    }
    let tail = queue_edit::tail(&np.queue, Some(current)).expect("a track is current");
    let rows: Vec<usize> = tracks.iter().map(|&(i, _)| i).collect();
    let expected = tail.rows.to_vec();
    let (kind, items, order) = match call {
        Call::PlayerMove { to, .. } => (
            EditKind::Move,
            // None when nothing moves: the queue stays as it is.
            queue_edit::moved(&tail, &rows, *to).unwrap_or_else(|| expected.clone()),
            queue_edit::moved_order(&tail, &rows, *to),
        ),
        Call::PlayerRemove { .. } => (
            EditKind::Remove,
            // None only for no rows at all, which parse_call refuses.
            queue_edit::remove(&tail, &rows)
                .ok_or_else(|| RpcError::bad_params("tracks must not be empty"))?,
            queue_edit::remove_order(&tail, &rows),
        ),
        _ => (EditKind::Clear { retried: false }, vec![], vec![]),
    };
    Ok(QueueEditPlan {
        from: tail.from,
        expected,
        items,
        kind,
        order,
    })
}

fn playlist_row(e: playlists::Entry, catalog: &Catalog) -> Value {
    let known = catalog.get(&e.path);
    let stem = e.path.file_stem().map(|s| s.to_string_lossy().into_owned());
    json!({
        "path": path_str(&e.path),
        "title": e.title.or_else(|| known.map(|t| t.title.clone())).or(stem).unwrap_or_default(),
        "artist": e.artist.or_else(|| known.map(|t| t.artist.clone())).unwrap_or_default(),
        "duration_s": e.duration_s.map(|s| s as f64).or_else(|| known.and_then(|t| t.duration_s)),
        "missing": !e.path.exists(),
    })
}

/// Answers `call`. The command, if any, is for the caller to send.
pub fn answer(call: Call, ctx: &BridgeCtx) -> (Result<Value, RpcError>, Option<PlayerCommand>) {
    let no_device = || RpcError::busy("no audio device");
    match call {
        Call::PlayerState => (Ok(player_state(ctx.np)), None),
        Call::PlayerPlay { paths, start } => {
            if !ctx.device_ready {
                return (Err(no_device()), None);
            }
            match ctx.catalog.resolve(&paths) {
                Ok(items) => (Ok(json!({})), Some(PlayerCommand::Play { items, start })),
                Err(unknown) => (Err(unknown_paths(&unknown)), None),
            }
        }
        Call::PlayerAppend { paths } => {
            if !ctx.device_ready {
                return (Err(no_device()), None);
            }
            match ctx.catalog.resolve(&paths) {
                Ok(items) if items.is_empty() => (Ok(json!({})), None),
                Ok(items) => (Ok(json!({})), Some(PlayerCommand::Append { items })),
                Err(unknown) => (Err(unknown_paths(&unknown)), None),
            }
        }
        Call::LibraryFolders => (
            Ok(json!(
                ctx.folders.iter().map(|p| path_str(p)).collect::<Vec<_>>()
            )),
            None,
        ),
        Call::LibraryTracks => {
            let tracks: Vec<Value> = ctx
                .catalog
                .tracks()
                .map(|t| {
                    json!({
                        "path": path_str(&t.item.path),
                        "title": t.title,
                        "artist": t.artist,
                        "album": t.album,
                        "duration_s": t.duration_s,
                    })
                })
                .collect();
            (Ok(Value::Array(tracks)), None)
        }
        Call::PlaylistsList => (
            playlists::list(ctx.playlists_dir)
                .map(|names| json!(names))
                .map_err(|e| RpcError::internal(e.to_string())),
            None,
        ),
        Call::PlaylistsRead { name } => {
            let r = match playlists::read(ctx.playlists_dir, &name) {
                Ok(entries) => Ok(Value::Array(
                    entries
                        .into_iter()
                        .map(|e| playlist_row(e, ctx.catalog))
                        .collect(),
                )),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(RpcError::not_found(
                    format!("playlist \"{name}\" not found"),
                    None,
                )),
                Err(e) => Err(RpcError::internal(e.to_string())),
            };
            (r, None)
        }
        Call::PlaylistsSave { name, paths } => {
            let items = match ctx.catalog.resolve(&paths) {
                Ok(items) => items,
                Err(unknown) => return (Err(unknown_paths(&unknown)), None),
            };
            let entries: Vec<_> = items
                .iter()
                .filter_map(|i| ctx.catalog.get(&i.path))
                .map(playlists::entry_for)
                .collect();
            // A plugin never replaces a playlist: a taken name gets " (2)", " (3)", ...
            let name = playlists::free_name(ctx.playlists_dir, &playlists::sanitize(&name));
            let r = playlists::save(ctx.playlists_dir, &name, &entries)
                .map(|stored| json!({ "name": stored }))
                .map_err(|e| RpcError::internal(e.to_string()));
            (r, None)
        }
        // Answered later, when the engine has; `BpApp` sends them through [`edit`].
        Call::PlayerRemove { .. } | Call::PlayerMove { .. } | Call::PlayerClearUpcoming => (
            Err(RpcError::internal("queue edits are answered by the app")),
            None,
        ),
        Call::PlaylistsAdd { name, paths } => {
            let items = match ctx.catalog.resolve(&paths) {
                Ok(items) => items,
                Err(unknown) => return (Err(unknown_paths(&unknown)), None),
            };
            let entries: Vec<_> = items
                .iter()
                .filter_map(|i| ctx.catalog.get(&i.path))
                .map(playlists::entry_for)
                .collect();
            let r = match playlists::add(ctx.playlists_dir, &name, &entries) {
                Ok(res) => Ok(json!({ "added": res.added, "skipped": res.skipped })),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(RpcError::not_found(
                    format!("playlist \"{name}\" not found"),
                    None,
                )),
                Err(e) => Err(RpcError::internal(e.to_string())),
            };
            (r, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use bp_core::{Codec, SourceFormat, SourceKind};

    use super::*;
    use crate::backend::index::{MemLibrary, ScannedTrack};

    fn p(name: &str) -> PathBuf {
        let sep = if cfg!(windows) { "\\" } else { "/" };
        let base = if cfg!(windows) {
            "E:\\Music\\A"
        } else {
            "/music/A"
        };
        PathBuf::from(format!("{base}{sep}{name}"))
    }

    fn scanned(name: &str, title: &str, n: u16, secs: u64) -> ScannedTrack {
        ScannedTrack {
            path: p(name),
            album_artist: Some("Ada".into()),
            artist: None,
            album: Some("First".into()),
            title: Some(title.into()),
            year: None,
            disc: Some(1),
            number: Some(n),
            compilation: false,
            modified_ns: 0,
            source: SourceFormat {
                codec: Codec::Flac,
                sample_rate: 44100,
                channels: 2,
                kind: SourceKind::Pcm {
                    bits: 16,
                    float: false,
                },
            },
            duration: Duration::from_secs(secs),
        }
    }

    struct Rig {
        catalog: Catalog,
        np: NowPlaying,
        dir: tempfile::TempDir,
        device_ready: bool,
    }

    impl Rig {
        fn new() -> Rig {
            let lib = MemLibrary::new(
                vec![
                    scanned("01.flac", "One", 1, 90),
                    scanned("02.flac", "Two", 2, 60),
                ],
                vec![PathBuf::from("/music")],
            );
            Rig {
                catalog: Catalog::build(&lib),
                np: NowPlaying::default(),
                dir: tempfile::tempdir().unwrap(),
                device_ready: true,
            }
        }

        fn ask(&self, call: Call) -> (Result<Value, RpcError>, Option<PlayerCommand>) {
            let ctx = BridgeCtx {
                catalog: &self.catalog,
                np: &self.np,
                folders: vec![PathBuf::from("/music")],
                playlists_dir: self.dir.path(),
                device_ready: self.device_ready,
            };
            answer(call, &ctx)
        }
    }

    /// Four queue items, `q0.flac` to `q3.flac`.
    fn four() -> Vec<QueueItem> {
        (0..4)
            .map(|n| QueueItem {
                path: p(&format!("q{n}.flac")),
                source: SourceFormat {
                    codec: Codec::Flac,
                    sample_rate: 44100,
                    channels: 2,
                    kind: SourceKind::Pcm {
                        bits: 16,
                        float: false,
                    },
                },
                duration: Duration::from_secs(60),
                title: format!("q{n}"),
            })
            .collect()
    }

    impl Rig {
        /// The four items of [`four`] are queued and the first is playing.
        fn playing() -> Rig {
            let mut r = Rig::new();
            r.np.on_command(&PlayerCommand::Play {
                items: four(),
                start: 0,
            });
            r
        }

        fn plan(&self, call: Call) -> Result<QueueEditPlan, RpcError> {
            edit(&call, &self.np)
        }
    }

    fn pq(n: usize) -> PathBuf {
        p(&format!("q{n}.flac"))
    }

    fn paths(items: &[QueueItem]) -> Vec<PathBuf> {
        items.iter().map(|i| i.path.clone()).collect()
    }

    fn reason(r: Result<QueueEditPlan, RpcError>) -> (i64, Option<Value>) {
        let e = r.expect_err("an error");
        (e.code, e.data)
    }

    fn queue_changed(why: &str) -> (i64, Option<Value>) {
        (-32004, Some(json!({ "reason": why })))
    }

    #[test]
    fn remove_builds_the_rearrange() {
        let r = Rig::playing();
        let plan = r
            .plan(Call::PlayerRemove {
                tracks: vec![(2, pq(2))],
            })
            .unwrap();
        assert_eq!(plan.from, 1);
        assert_eq!(paths(&plan.expected), [pq(1), pq(2), pq(3)]);
        assert_eq!(paths(&plan.items), [pq(1), pq(3)]);
        assert_eq!(plan.kind, EditKind::Remove);
    }

    #[test]
    fn move_keeps_the_given_order() {
        let r = Rig::playing();
        let plan = r
            .plan(Call::PlayerMove {
                tracks: vec![(3, pq(3)), (2, pq(2))],
                to: 1,
            })
            .unwrap();
        assert_eq!(plan.from, 1);
        assert_eq!(paths(&plan.items), [pq(3), pq(2), pq(1)]);
        assert_eq!(plan.kind, EditKind::Move);
        // Already where it would go: nothing changes.
        let plan = r
            .plan(Call::PlayerMove {
                tracks: vec![(1, pq(1))],
                to: 1,
            })
            .unwrap();
        assert_eq!(plan.items, plan.expected);
    }

    #[test]
    fn stale_path_is_changed() {
        let r = Rig::playing();
        for tracks in [vec![(2, pq(1))], vec![(9, pq(1))]] {
            let got = reason(r.plan(Call::PlayerRemove { tracks }));
            assert_eq!(got, queue_changed("changed"));
        }
    }

    #[test]
    fn current_or_played_index_is_started() {
        let r = Rig::playing();
        let got = reason(r.plan(Call::PlayerRemove {
            tracks: vec![(0, pq(0))],
        }));
        assert_eq!(got, queue_changed("started"));
        let got = reason(r.plan(Call::PlayerMove {
            tracks: vec![(2, pq(2))],
            to: 0,
        }));
        assert_eq!(got, queue_changed("started"));
    }

    #[test]
    fn nothing_playing_is_stopped() {
        let mut r = Rig::playing();
        r.np.on_command(&PlayerCommand::Stop);
        for call in [
            Call::PlayerRemove {
                tracks: vec![(2, pq(2))],
            },
            Call::PlayerMove {
                tracks: vec![(2, pq(2))],
                to: 1,
            },
            Call::PlayerClearUpcoming,
        ] {
            assert_eq!(reason(r.plan(call)), queue_changed("stopped"));
        }
        let mut r = Rig::playing();
        r.np.fatal = Some("no device".into());
        assert_eq!(
            reason(r.plan(Call::PlayerClearUpcoming)),
            queue_changed("stopped")
        );
    }

    #[test]
    fn move_to_past_the_end_is_bad_params() {
        let r = Rig::playing();
        let got = r.plan(Call::PlayerMove {
            tracks: vec![(2, pq(2))],
            to: 5,
        });
        assert_eq!(got.err().map(|e| e.code), Some(-32602));
        // The end itself is fine.
        let plan = r
            .plan(Call::PlayerMove {
                tracks: vec![(1, pq(1))],
                to: 4,
            })
            .unwrap();
        assert_eq!(paths(&plan.items), [pq(2), pq(3), pq(1)]);
    }

    #[test]
    fn clear_takes_every_upcoming_track() {
        let r = Rig::playing();
        let plan = r.plan(Call::PlayerClearUpcoming).unwrap();
        assert_eq!(plan.from, 1);
        assert_eq!(paths(&plan.expected), [pq(1), pq(2), pq(3)]);
        assert!(plan.items.is_empty());
        assert_eq!(plan.kind, EditKind::Clear { retried: false });
    }

    #[test]
    fn state_when_stopped() {
        let r = Rig::new();
        let (v, cmd) = r.ask(Call::PlayerState);
        assert_eq!(v.unwrap(), json!({ "state": "stopped", "queue": [] }));
        assert!(cmd.is_none());
    }

    #[test]
    fn play_resolves_paths_to_queue_items() {
        let r = Rig::new();
        let (v, cmd) = r.ask(Call::PlayerPlay {
            paths: vec![p("02.flac"), p("01.flac")],
            start: 1,
        });
        assert_eq!(v.unwrap(), json!({}));
        let Some(PlayerCommand::Play { items, start }) = cmd else {
            panic!("expected Play, got {cmd:?}");
        };
        assert_eq!(start, 1);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Two");
    }

    #[test]
    fn append_unknown_path_is_32002_listing_it() {
        let r = Rig::new();
        let (v, cmd) = r.ask(Call::PlayerAppend {
            paths: vec![p("01.flac"), p("nope.flac")],
        });
        let e = v.unwrap_err();
        assert_eq!(e.code, -32002);
        assert_eq!(e.message, "not in nil's library: 1 path(s)");
        assert_eq!(
            e.data,
            Some(json!({ "paths": [path_str(&p("nope.flac"))] }))
        );
        assert!(cmd.is_none());
    }

    #[test]
    fn play_without_a_device_is_32003() {
        let mut r = Rig::new();
        r.device_ready = false;
        let (v, cmd) = r.ask(Call::PlayerPlay {
            paths: vec![p("01.flac")],
            start: 0,
        });
        let e = v.unwrap_err();
        assert_eq!((e.code, e.message.as_str()), (-32003, "no audio device"));
        assert!(cmd.is_none());
        let (v, _) = r.ask(Call::PlayerAppend {
            paths: vec![p("01.flac")],
        });
        assert_eq!(v.unwrap_err().code, -32003);
    }

    #[test]
    fn library_tracks_has_the_documented_fields() {
        let r = Rig::new();
        let (v, _) = r.ask(Call::LibraryTracks);
        let v = v.unwrap();
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            let keys: BTreeSet<_> = row
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                BTreeSet::from(["path", "title", "artist", "album", "duration_s"])
            );
        }
        assert_eq!(rows[0]["artist"], "Ada");
        assert_eq!(rows[0]["duration_s"], 90.0);
    }

    #[test]
    fn playlists_save_returns_the_sanitised_name() {
        let r = Rig::new();
        let (v, _) = r.ask(Call::PlaylistsSave {
            name: "a/b".into(),
            paths: vec![p("01.flac")],
        });
        assert_eq!(v.unwrap(), json!({ "name": "a_b" }));
        let (v, _) = r.ask(Call::PlaylistsList);
        assert_eq!(v.unwrap(), json!(["a_b"]));
        let (v, _) = r.ask(Call::PlaylistsSave {
            name: "x".into(),
            paths: vec![p("zzz.flac")],
        });
        assert_eq!(v.unwrap_err().code, -32002);
    }

    fn plist_bytes(r: &Rig, name: &str) -> Vec<u8> {
        std::fs::read(r.dir.path().join(format!("{name}.m3u8"))).unwrap()
    }

    fn save_mix(r: &Rig, paths: Vec<PathBuf>) -> Value {
        r.ask(Call::PlaylistsSave {
            name: "Mix".into(),
            paths,
        })
        .0
        .unwrap()
    }

    #[test]
    fn add_appends_to_a_playlist() {
        let r = Rig::new();
        save_mix(&r, vec![p("01.flac")]);
        let (v, _) = r.ask(Call::PlaylistsAdd {
            name: "Mix".into(),
            paths: vec![p("02.flac")],
        });
        assert_eq!(v.unwrap(), json!({ "added": 1, "skipped": 0 }));
        let (v, _) = r.ask(Call::PlaylistsRead { name: "Mix".into() });
        assert_eq!(v.unwrap().as_array().unwrap().len(), 2);
    }

    #[test]
    fn add_skips_duplicates() {
        let r = Rig::new();
        save_mix(&r, vec![p("01.flac")]);
        let (v, _) = r.ask(Call::PlaylistsAdd {
            name: "Mix".into(),
            paths: vec![p("01.flac"), p("02.flac")],
        });
        assert_eq!(v.unwrap(), json!({ "added": 1, "skipped": 1 }));
    }

    #[test]
    fn add_refuses_an_unknown_playlist() {
        let r = Rig::new();
        let (v, _) = r.ask(Call::PlaylistsAdd {
            name: "Nope".into(),
            paths: vec![p("01.flac")],
        });
        assert_eq!(v.unwrap_err().code, -32002);
        let (v, _) = r.ask(Call::PlaylistsList);
        assert_eq!(v.unwrap(), json!([]));
    }

    #[test]
    fn add_refuses_an_unknown_path() {
        let r = Rig::new();
        save_mix(&r, vec![p("01.flac")]);
        let before = plist_bytes(&r, "Mix");
        let (v, _) = r.ask(Call::PlaylistsAdd {
            name: "Mix".into(),
            paths: vec![p("02.flac"), p("zzz.flac")],
        });
        assert_eq!(v.unwrap_err().code, -32002);
        assert_eq!(plist_bytes(&r, "Mix"), before);
    }

    #[test]
    fn save_to_a_taken_name_stores_name_2() {
        let r = Rig::new();
        save_mix(&r, vec![p("01.flac")]);
        let before = plist_bytes(&r, "Mix");
        let v = save_mix(&r, vec![p("02.flac")]);
        assert_eq!(v, json!({ "name": "Mix (2)" }));
        assert_eq!(plist_bytes(&r, "Mix"), before);
    }

    #[test]
    fn save_when_name_2_is_taken_stores_name_3() {
        let r = Rig::new();
        save_mix(&r, vec![p("01.flac")]);
        save_mix(&r, vec![p("01.flac")]);
        let v = save_mix(&r, vec![p("02.flac")]);
        assert_eq!(v, json!({ "name": "Mix (3)" }));
    }

    #[test]
    fn playlists_read_flags_missing() {
        let r = Rig::new();
        let real = r.dir.path().join("real.flac");
        std::fs::write(&real, b"").unwrap();
        let entries = [
            playlists::Entry {
                path: real.clone(),
                title: Some("Mine".into()),
                artist: None,
                duration_s: Some(5),
            },
            playlists::Entry {
                path: p("01.flac"),
                title: None,
                artist: None,
                duration_s: None,
            },
            playlists::Entry {
                path: r.dir.path().join("gone.flac"),
                title: None,
                artist: None,
                duration_s: None,
            },
        ];
        playlists::save(r.dir.path(), "mix", &entries).unwrap();
        let (v, _) = r.ask(Call::PlaylistsRead { name: "mix".into() });
        let v = v.unwrap();
        let rows = v.as_array().unwrap();
        assert_eq!(rows[0]["missing"], false);
        assert_eq!(rows[0]["title"], "Mine");
        assert_eq!(rows[0]["artist"], "");
        assert_eq!(rows[0]["duration_s"], 5.0);
        // Not on disk here, but known to the library: title and artist come from it.
        assert_eq!(rows[1]["missing"], !p("01.flac").exists());
        assert_eq!(rows[1]["title"], "One");
        assert_eq!(rows[1]["artist"], "Ada");
        assert_eq!(rows[2]["missing"], true);
        assert_eq!(rows[2]["title"], "gone");

        let (v, _) = r.ask(Call::PlaylistsRead {
            name: "nope".into(),
        });
        let e = v.unwrap_err();
        assert_eq!(e.code, -32002);
        assert_eq!(e.message, "playlist \"nope\" not found");
    }

    #[test]
    fn changed_params_omit_path_when_stopped() {
        let np = NowPlaying::default();
        assert_eq!(
            changed_params(&np),
            json!({ "state": "stopped", "queue_len": 0 })
        );
        assert_eq!(state_key(&np), (0, None, 0));
    }

    #[test]
    fn changed_params_and_key_follow_the_playing_track() {
        let r = Rig::new();
        let mut np = NowPlaying::default();
        let items = r.catalog.resolve(&[p("01.flac"), p("02.flac")]).unwrap();
        np.on_command(&PlayerCommand::Play { items, start: 1 });
        let v = changed_params(&np);
        assert_eq!(v["state"], "playing");
        assert_eq!(v["index"], 1);
        assert_eq!(v["queue_len"], 2);
        assert_eq!(v["path"], path_str(&p("02.flac")));
        let before = state_key(&np);
        np.position = Duration::from_secs(30);
        assert_eq!(state_key(&np), before);
        np.paused = true;
        assert_ne!(state_key(&np), before);
    }
}
