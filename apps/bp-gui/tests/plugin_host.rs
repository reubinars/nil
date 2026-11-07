//! The plugin host against the scripted `fake-plugin` binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bp_gui::plugins::consent::{Consent, ConsentStore};
use bp_gui::plugins::host::{HostEvent, PluginHost, Status, Timings};
use bp_gui::plugins::panel::Place;
use bp_gui::plugins::rpc::{Call, Permission};
use serde_json::{Value, json};

const FAST: Timings = Timings {
    handshake: Duration::from_millis(300),
    backoff: [
        Duration::from_millis(50),
        Duration::from_millis(100),
        Duration::from_millis(150),
    ],
    crash_window: Duration::from_secs(5),
    crash_limit: 3,
    grace: Duration::from_millis(200),
};

struct Rig {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        Rig { _tmp: tmp, root }
    }
    fn plugins_dir(&self) -> PathBuf {
        self.root.join("plugins")
    }
    fn consent_path(&self) -> PathBuf {
        self.root.join("data").join("plugins.toml")
    }
    fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Writes `plugins/<name>/plugin.toml` running the fake plugin with `args`,
    /// and enables it with every declared permission granted.
    fn install(&self, name: &str, args: &[&str], perms: &[Permission]) {
        self.install_with(name, args, perms, 1, true);
    }

    fn install_with(
        &self,
        name: &str,
        args: &[&str],
        perms: &[Permission],
        api_version: u32,
        enabled: bool,
    ) {
        let dir = self.plugins_dir().join(name);
        fs::create_dir_all(&dir).unwrap();
        let mut command = vec![env!("CARGO_BIN_EXE_fake-plugin").to_string()];
        command.extend(args.iter().map(|a| a.to_string()));
        let names: Vec<&str> = perms.iter().map(|p| p.name()).collect();
        let toml = format!(
            "name = {name:?}\nversion = \"0\"\ndescription = \"\"\napi_version = {api_version}\n\
             command = {command:?}\npermissions = {names:?}\n"
        );
        fs::write(dir.join("plugin.toml"), toml).unwrap();
        let mut store = ConsentStore::load(&self.consent_path());
        let granted = perms.to_vec();
        store.set(name, Consent { enabled, granted }).unwrap();
    }

    fn start(&self) -> PluginHost {
        self.start_with(FAST)
    }

    fn start_with(&self, t: Timings) -> PluginHost {
        PluginHost::start(
            self.plugins_dir(),
            self.consent_path(),
            self.logs_dir(),
            t,
            Arc::new(|| {}),
        )
    }

    /// What the fake plugin received, one JSON value per line.
    fn received(&self, name: &str) -> Vec<Value> {
        self.fake_log(name)
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn fake_log(&self, name: &str) -> String {
        fs::read_to_string(self.plugins_dir().join(name).join("fake-plugin.log"))
            .unwrap_or_default()
    }

    /// `(pid, unix ms)` of every spawn.
    fn spawns(&self, name: &str) -> Vec<(u32, u128)> {
        self.fake_log(name)
            .lines()
            .filter_map(|l| l.strip_prefix("#spawn "))
            .map(|l| {
                let (pid, ms) = l.split_once(' ').unwrap();
                (pid.parse().unwrap(), ms.parse().unwrap())
            })
            .collect()
    }

    fn host_log(&self, name: &str) -> String {
        fs::read_to_string(self.logs_dir().join("plugins").join(format!("{name}.log")))
            .unwrap_or_default()
    }
}

fn status(h: &PluginHost, name: &str) -> Status {
    h.plugins()
        .into_iter()
        .find(|p| p.folder == name)
        .unwrap()
        .status
}

fn wait_for(what: &str, limit: Duration, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < limit, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_status(h: &PluginHost, name: &str, want: Status, limit: Duration) {
    wait_for(&format!("{want:?}"), limit, || status(h, name) == want);
}

/// Events seen within `d`.
fn events_for(h: &PluginHost, d: Duration) -> Vec<HostEvent> {
    let end = Instant::now() + d;
    let mut out = Vec::new();
    while Instant::now() < end {
        match h.try_recv() {
            Some(e) => out.push(e),
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    out
}

fn next_event(h: &PluginHost, limit: Duration, want: impl Fn(&HostEvent) -> bool) -> HostEvent {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        match h.try_recv() {
            Some(e) if want(&e) => return e,
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    panic!("no matching event within {limit:?}");
}

/// The response the plugin received for its request `id`.
fn response(rig: &Rig, name: &str, id: u64) -> Option<Value> {
    rig.received(name)
        .into_iter()
        .find(|m| m.get("id") == Some(&json!(id)) && m.get("method").is_none())
}

fn wait_response(rig: &Rig, name: &str, id: u64) -> Value {
    let mut got = None;
    wait_for(&format!("response {id}"), Duration::from_secs(2), || {
        got = response(rig, name, id);
        got.is_some()
    });
    got.unwrap()
}

#[cfg(not(windows))]
fn alive(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        Err(_) => false,
    }
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code).is_ok();
        let _ = CloseHandle(h);
        ok && code == STILL_ACTIVE.0 as u32
    }
}

const SEC: Duration = Duration::from_secs(1);

#[test]
fn handshake_sends_initialize_then_initialized() {
    let rig = Rig::new();
    rig.install("p", &["reply"], &[Permission::Playback]);
    let h = rig.start();
    wait_status(&h, "p", Status::Running, 2 * SEC);
    wait_for("initialized", SEC, || rig.received("p").len() >= 2);
    let got = rig.received("p");
    assert_eq!(got[0]["method"], "initialize");
    let params = &got[0]["params"];
    assert_eq!(params["api_version"], 1);
    assert_eq!(params["granted"], json!(["playback"]));
    let data_dir = Path::new(params["data_dir"].as_str().unwrap());
    assert_eq!(data_dir, rig.plugins_dir().join("p").join("data"));
    assert!(data_dir.is_dir());
    assert_eq!(got[1]["method"], "initialized");
    assert!(got[1].get("id").is_none());
    h.shutdown_all();
}

#[test]
fn start_returns_immediately() {
    let rig = Rig::new();
    rig.install("p", &["hang"], &[]);
    let t = Instant::now();
    let h = rig.start();
    assert!(
        t.elapsed() < Duration::from_millis(100),
        "{:?}",
        t.elapsed()
    );
    h.shutdown_all();
}

#[test]
fn no_reply_within_the_timeout_is_crashed() {
    let rig = Rig::new();
    rig.install("p", &["hang"], &[]);
    let h = rig.start();
    wait_status(
        &h,
        "p",
        Status::Crashed {
            reason: "didn't start".into(),
        },
        SEC,
    );
    let (pid, _) = rig.spawns("p")[0];
    wait_for("the hung process to die", SEC, || !alive(pid));
}

#[test]
fn crashes_back_off_then_stop_after_three() {
    let rig = Rig::new();
    rig.install("p", &["reply", "crash"], &[]);
    let h = rig.start();
    wait_status(
        &h,
        "p",
        Status::Crashed {
            reason: "crashed 3 times in 5 minutes".into(),
        },
        3 * SEC,
    );
    let spawns = rig.spawns("p");
    assert_eq!(spawns.len(), 3, "{spawns:?}");
    let gap1 = spawns[1].1 - spawns[0].1;
    let gap2 = spawns[2].1 - spawns[1].1;
    assert!(gap1 >= 50, "first restart after {gap1} ms");
    assert!(gap2 >= 100, "second restart after {gap2} ms");
    assert!(gap1 < 1000 && gap2 < 1000, "{gap1} / {gap2} ms");
    let info = h.plugins().remove(0);
    assert!(
        info.stderr_tail.iter().any(|l| l == "boom"),
        "{:?}",
        info.stderr_tail
    );
    // No restart after the final crash.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(rig.spawns("p").len(), 3);
}

#[test]
fn manifest_api_version_2_is_incompatible() {
    let rig = Rig::new();
    rig.install_with("p", &["reply"], &[], 2, true);
    let h = rig.start();
    assert_eq!(status(&h, "p"), Status::Incompatible);
    std::thread::sleep(Duration::from_millis(300));
    assert!(rig.spawns("p").is_empty());
}

#[test]
fn invalid_manifest_is_reported() {
    let rig = Rig::new();
    let dir = rig.plugins_dir().join("p");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("plugin.toml"), "nonsense").unwrap();
    let h = rig.start();
    assert!(matches!(status(&h, "p"), Status::InvalidManifest(_)));
}

#[test]
fn call_without_permission_gets_32001_and_is_not_forwarded() {
    let rig = Rig::new();
    rig.install(
        "p",
        &["reply", "call:library.folders:{}"],
        &[Permission::Playback],
    );
    let h = rig.start();
    let r = wait_response(&rig, "p", 100);
    assert_eq!(r["error"]["code"], -32001);
    let events = events_for(&h, Duration::from_millis(200));
    assert!(!events.iter().any(|e| matches!(e, HostEvent::Call { .. })));
    assert!(
        rig.host_log("p")
            .contains("[nil] permission denied: library.folders")
    );
    assert!(!h.granted("p", Permission::Library));
    assert!(h.granted("p", Permission::Playback));
    h.shutdown_all();
}

#[test]
fn granted_call_is_forwarded_and_reply_reaches_the_plugin() {
    let rig = Rig::new();
    rig.install(
        "p",
        &["reply", "call:library.folders:{}"],
        &[Permission::Library],
    );
    let h = rig.start();
    let HostEvent::Call { plugin, id, call } =
        next_event(&h, 2 * SEC, |e| matches!(e, HostEvent::Call { .. }))
    else {
        unreachable!()
    };
    assert_eq!(plugin, "p");
    assert_eq!(call, Call::LibraryFolders);
    h.reply("p", id, Ok(json!(["/music"])));
    assert_eq!(wait_response(&rig, "p", 100)["result"], json!(["/music"]));
    h.shutdown_all();
}

const PANEL: &str = r#"call:panel.set:{"panel":{"elements":[{"type":"text","text":"hi"}]}}"#;

#[test]
fn panel_set_emits_a_panel() {
    let rig = Rig::new();
    rig.install("p", &["reply", PANEL], &[]);
    let h = rig.start();
    let HostEvent::Panel { plugin, panel, .. } = next_event(&h, 2 * SEC, |e| {
        matches!(e, HostEvent::Panel { panel: Some(_), .. })
    }) else {
        unreachable!()
    };
    assert_eq!(plugin, "p");
    assert_eq!(panel.unwrap().elements.len(), 1);
    assert_eq!(wait_response(&rig, "p", 100)["result"], json!({}));
    h.shutdown_all();
}

const SETTINGS_PANEL: &str =
    r#"call:panel.set:{"panel":{"elements":[{"type":"text","text":"s"}]},"place":"settings"}"#;

#[test]
fn a_settings_panel_is_queued_with_its_place() {
    let rig = Rig::new();
    rig.install("p", &["reply", SETTINGS_PANEL], &[]);
    let h = rig.start();
    let HostEvent::Panel { plugin, place, .. } = next_event(&h, 2 * SEC, |e| {
        matches!(e, HostEvent::Panel { panel: Some(_), .. })
    }) else {
        unreachable!()
    };
    assert_eq!(plugin, "p");
    assert_eq!(place, Place::Settings);
    assert_eq!(wait_response(&rig, "p", 100)["result"], json!({}));
    h.shutdown_all();
}

#[test]
fn stopping_clears_both_places() {
    let rig = Rig::new();
    rig.install("p", &["reply", PANEL, SETTINGS_PANEL], &[]);
    let mut h = rig.start();
    wait_response(&rig, "p", 101);
    h.set_enabled("p", false).unwrap();
    wait_status(&h, "p", Status::Off, 2 * SEC);
    let cleared: Vec<Place> = events_for(&h, Duration::from_millis(300))
        .into_iter()
        .filter_map(|e| match e {
            HostEvent::Panel {
                place, panel: None, ..
            } => Some(place),
            _ => None,
        })
        .collect();
    assert!(cleared.contains(&Place::Tab), "{cleared:?}");
    assert!(cleared.contains(&Place::Settings), "{cleared:?}");
}

#[test]
fn bad_panel_is_rejected_and_old_panel_kept() {
    let rig = Rig::new();
    rig.install(
        "p",
        &["reply", PANEL, r#"call:panel.set:{"panel":{"elements":5}}"#],
        &[],
    );
    let h = rig.start();
    assert_eq!(wait_response(&rig, "p", 101)["error"]["code"], -32602);
    let panels = events_for(&h, Duration::from_millis(200))
        .into_iter()
        .filter(|e| matches!(e, HostEvent::Panel { .. }))
        .count();
    assert_eq!(panels, 1);
    h.shutdown_all();
}

#[test]
fn unknown_method_gets_32601() {
    let rig = Rig::new();
    rig.install("p", &["reply", "call:nope:{}"], &[]);
    let h = rig.start();
    assert_eq!(wait_response(&rig, "p", 100)["error"]["code"], -32601);
    h.shutdown_all();
}

#[test]
fn garbage_line_is_ignored() {
    let rig = Rig::new();
    rig.install("p", &["reply", "garbage"], &[]);
    let h = rig.start();
    wait_status(&h, "p", Status::Running, 2 * SEC);
    wait_for("the malformed line note", SEC, || {
        rig.host_log("p").contains("[nil] malformed line ignored")
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(status(&h, "p"), Status::Running);
    h.shutdown_all();
}

#[test]
fn a_line_over_one_megabyte_is_skipped_and_reading_continues() {
    let rig = Rig::new();
    rig.install("p", &["reply", "long-line", PANEL], &[]);
    let h = rig.start();
    next_event(&h, 2 * SEC, |e| {
        matches!(e, HostEvent::Panel { panel: Some(_), .. })
    });
    assert!(rig.host_log("p").contains("[nil] malformed line ignored"));
    assert_eq!(status(&h, "p"), Status::Running);
    h.shutdown_all();
}

#[test]
fn shutdown_kills_a_plugin_that_ignores_it() {
    let rig = Rig::new();
    rig.install("p", &["reply", "ignore-shutdown"], &[]);
    let mut h = rig.start();
    wait_status(&h, "p", Status::Running, 2 * SEC);
    let (pid, _) = rig.spawns("p")[0];
    let t = Instant::now();
    h.set_enabled("p", false).unwrap();
    let took = t.elapsed();
    assert!(
        took < Duration::from_millis(10),
        "set_enabled blocked: {took:?}"
    );
    wait_for(
        "the plugin to die",
        FAST.grace + Duration::from_millis(500),
        || !alive(pid),
    );
    let died = t.elapsed();
    assert!(
        died >= FAST.grace - Duration::from_millis(20),
        "killed before the grace period: {died:?}"
    );
    wait_status(&h, "p", Status::Off, SEC);
    assert!(rig.received("p").iter().any(|m| m["method"] == "shutdown"));
    next_event(&h, SEC, |e| {
        matches!(e, HostEvent::Panel { panel: None, .. })
    });
    assert!(!ConsentStore::load(&rig.consent_path()).get("p").enabled);
}

#[test]
fn a_plugin_that_obeys_shutdown_exits_without_waiting_the_grace() {
    let rig = Rig::new();
    rig.install("p", &["reply"], &[]);
    let mut h = rig.start_with(Timings {
        grace: Duration::from_secs(3),
        ..FAST
    });
    wait_status(&h, "p", Status::Running, 2 * SEC);
    let t = Instant::now();
    h.set_enabled("p", false).unwrap();
    wait_status(&h, "p", Status::Off, SEC);
    assert!(t.elapsed() < SEC, "{:?}", t.elapsed());
}

#[test]
fn turning_off_during_backoff_cancels_the_restart() {
    let rig = Rig::new();
    rig.install("p", &["reply", "crash"], &[]);
    let mut h = rig.start_with(Timings {
        backoff: [SEC, SEC, SEC],
        ..FAST
    });
    wait_for("the first process to crash", 2 * SEC, || {
        rig.spawns("p").first().is_some_and(|&(pid, _)| !alive(pid))
    });
    // Exit detection takes at most a poll plus the drain; the backoff is 1 s.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(status(&h, "p"), Status::Starting);
    h.set_enabled("p", false).unwrap();
    wait_status(&h, "p", Status::Off, Duration::from_millis(200));
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(rig.spawns("p").len(), 1);
    assert_eq!(status(&h, "p"), Status::Off);
}

#[test]
fn turning_on_grants_saves_and_starts() {
    let rig = Rig::new();
    rig.install_with("p", &["reply"], &[Permission::Library], 1, false);
    let mut h = rig.start();
    assert_eq!(status(&h, "p"), Status::Off);
    h.set_enabled("p", true).unwrap();
    wait_status(&h, "p", Status::Running, 2 * SEC);
    assert!(h.granted("p", Permission::Library));
    let saved = ConsentStore::load(&rig.consent_path()).get("p");
    assert!(saved.enabled);
    assert_eq!(saved.granted, [Permission::Library]);
    h.shutdown_all();
}

#[test]
fn restart_replaces_a_running_instance() {
    let rig = Rig::new();
    rig.install("p", &["reply"], &[]);
    let mut h = rig.start();
    wait_status(&h, "p", Status::Running, 2 * SEC);
    let (first, _) = rig.spawns("p")[0];
    let t = Instant::now();
    h.restart("p");
    assert!(t.elapsed() < Duration::from_millis(10), "{:?}", t.elapsed());
    wait_for("a second process", 2 * SEC, || rig.spawns("p").len() == 2);
    wait_status(&h, "p", Status::Running, 2 * SEC);
    assert!(!alive(first));
    h.shutdown_all();
}

#[test]
fn restart_from_crashed_resets_the_crash_count() {
    let rig = Rig::new();
    rig.install("p", &["reply", "crash"], &[]);
    let mut h = rig.start();
    let crashed = Status::Crashed {
        reason: "crashed 3 times in 5 minutes".into(),
    };
    wait_status(&h, "p", crashed.clone(), 3 * SEC);
    assert_eq!(rig.spawns("p").len(), 3);
    h.restart("p");
    // A fresh count: three more runs before it stops again, not one.
    wait_for("three more runs", 3 * SEC, || rig.spawns("p").len() == 6);
    wait_status(&h, "p", crashed, 3 * SEC);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(rig.spawns("p").len(), 6);
    h.shutdown_all();
}

/// Pid of the grandchild started by the `spawn-child` step.
fn grandchild(rig: &Rig, name: &str) -> Option<u32> {
    rig.fake_log(name)
        .lines()
        .find_map(|l| l.strip_prefix("#child "))
        .map(|p| p.parse().unwrap())
}

#[test]
fn turning_off_kills_the_whole_process_tree() {
    let rig = Rig::new();
    rig.install("p", &["spawn-child", "reply", "ignore-shutdown"], &[]);
    let mut h = rig.start();
    wait_status(&h, "p", Status::Running, 2 * SEC);
    let child = grandchild(&rig, "p").expect("the grandchild pid");
    assert!(alive(child));
    h.set_enabled("p", false).unwrap();
    wait_status(&h, "p", Status::Off, 2 * SEC);
    wait_for("the grandchild to die", SEC, || !alive(child));
}

#[test]
fn a_crash_kills_the_grandchild_holding_stdout() {
    let rig = Rig::new();
    rig.install("p", &["spawn-child", "reply", "crash"], &[]);
    let h = rig.start_with(Timings {
        backoff: [SEC, SEC, SEC],
        ..FAST
    });
    wait_for("the grandchild", 2 * SEC, || {
        grandchild(&rig, "p").is_some()
    });
    let child = grandchild(&rig, "p").unwrap();
    wait_for("the grandchild to die after the crash", 2 * SEC, || {
        !alive(child)
    });
    h.shutdown_all();
}

#[test]
fn a_reply_to_a_call_from_an_old_instance_is_dropped() {
    let rig = Rig::new();
    rig.install(
        "p",
        &["reply", "call:library.folders:{}"],
        &[Permission::Library],
    );
    let mut h = rig.start();
    let is_call = |e: &HostEvent| matches!(e, HostEvent::Call { .. });
    let HostEvent::Call { id: old, .. } = next_event(&h, 2 * SEC, is_call) else {
        unreachable!()
    };
    h.restart("p");
    let HostEvent::Call { id: new, .. } = next_event(&h, 3 * SEC, is_call) else {
        unreachable!()
    };
    assert_ne!(old, new);
    assert_eq!(rig.spawns("p").len(), 2);
    h.reply("p", old, Ok(json!(["/stale"])));
    h.reply("p", new.clone(), Ok(json!(["/fresh"])));
    // Answering twice does nothing either.
    h.reply("p", new, Ok(json!(["/twice"])));
    wait_for("the fresh reply", SEC, || {
        rig.received("p")
            .iter()
            .any(|m| m["result"] == json!(["/fresh"]))
    });
    std::thread::sleep(Duration::from_millis(100));
    let text = rig.fake_log("p");
    assert!(!text.contains("/stale"), "{text}");
    assert!(!text.contains("/twice"), "{text}");
    h.shutdown_all();
}

#[test]
fn host_never_blocks_on_a_plugin_that_does_not_read() {
    let rig = Rig::new();
    rig.install("p", &["reply", "no-read"], &[]);
    let h = rig.start();
    wait_status(&h, "p", Status::Running, 2 * SEC);
    let payload = json!({ "pad": "x".repeat(64 * 1024) });
    // More than the 1024 the queue holds, so some are dropped.
    let mut times = Vec::new();
    for _ in 0..1100 {
        let params = payload.clone();
        let t = Instant::now();
        h.notify("p", "player.changed", params);
        times.push(t.elapsed());
    }
    let total: Duration = times.iter().sum();
    times.sort();
    let p99 = times[times.len() * 99 / 100];
    assert!(p99 < Duration::from_millis(10), "p99 {p99:?}");
    assert!(total < Duration::from_millis(200), "total {total:?}");
    wait_for("the dropped-notifications note", SEC, || {
        rig.host_log("p")
            .contains("[nil] plugin not reading; notifications dropped")
    });
    let t = Instant::now();
    h.shutdown_all();
    let took = t.elapsed();
    assert!(took < FAST.grace + Duration::from_millis(500), "{took:?}");
}

#[test]
fn notifications_reach_a_running_plugin() {
    let rig = Rig::new();
    rig.install("p", &["reply"], &[]);
    let h = rig.start();
    wait_status(&h, "p", Status::Running, 2 * SEC);
    h.notify("p", "player.changed", json!({"state": "playing"}));
    wait_for("the notification", SEC, || {
        rig.received("p")
            .iter()
            .any(|m| m["method"] == "player.changed" && m["params"]["state"] == "playing")
    });
    h.shutdown_all();
}

#[cfg(windows)]
#[test]
fn job_object_kills_children_when_the_host_drops() {
    let rig = Rig::new();
    rig.install("p", &["reply", "ignore-shutdown"], &[]);
    let h = rig.start();
    wait_status(&h, "p", Status::Running, 2 * SEC);
    let (pid, _) = rig.spawns("p")[0];
    assert!(alive(pid));
    // Dropping without shutdown_all: only the job's kill-on-close stops it.
    drop(h);
    wait_for("the child to die with the job", SEC, || !alive(pid));
}

#[cfg(not(windows))]
#[test]
fn dropping_the_host_kills_children() {
    let rig = Rig::new();
    rig.install("p", &["reply", "ignore-shutdown"], &[]);
    let h = rig.start();
    wait_status(&h, "p", Status::Running, 2 * SEC);
    let (pid, _) = rig.spawns("p")[0];
    drop(h);
    wait_for("the child to die", SEC, || !alive(pid));
}

#[test]
fn disabled_plugins_are_not_started() {
    let rig = Rig::new();
    rig.install_with("p", &["reply"], &[], 1, false);
    let h = rig.start();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(status(&h, "p"), Status::Off);
    assert!(rig.spawns("p").is_empty());
}

#[test]
fn a_permission_not_yet_granted_keeps_the_plugin_off() {
    let rig = Rig::new();
    rig.install("p", &["reply"], &[Permission::Playback]);
    // The manifest now asks for more than the owner approved.
    let toml = rig.plugins_dir().join("p").join("plugin.toml");
    let text = fs::read_to_string(&toml)
        .unwrap()
        .replace(r#"["playback"]"#, r#"["playback", "library"]"#);
    fs::write(&toml, text).unwrap();
    let h = rig.start();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(status(&h, "p"), Status::Off);
    assert!(rig.spawns("p").is_empty());
}

#[test]
fn calls_during_the_shutdown_grace_get_32603_and_are_not_forwarded() {
    let rig = Rig::new();
    let late_panel = PANEL.replacen("call:", "on-shutdown:", 1);
    rig.install(
        "p",
        &[
            "reply",
            "ignore-shutdown",
            "on-shutdown:library.folders:{}",
            &late_panel,
            "on-shutdown:.note:{}",
        ],
        &[Permission::Library],
    );
    let mut h = rig.start_with(Timings { grace: SEC, ..FAST });
    wait_status(&h, "p", Status::Running, 2 * SEC);
    while h.try_recv().is_some() {}
    h.set_enabled("p", false).unwrap();
    for id in [100, 101] {
        let r = wait_response(&rig, "p", id);
        assert_eq!(r["error"]["code"], -32603, "{r}");
        assert_eq!(r["error"]["message"], "plugin is stopping");
    }
    wait_status(&h, "p", Status::Off, 2 * SEC);
    let events = events_for(&h, Duration::from_millis(100));
    assert!(
        !events.iter().any(|e| matches!(
            e,
            HostEvent::Call { .. } | HostEvent::Panel { panel: Some(_), .. }
        )),
        "{events:?}"
    );
}

#[test]
fn a_request_with_a_null_id_gets_32600_and_is_not_run() {
    let rig = Rig::new();
    rig.install(
        "p",
        &[
            "reply",
            r#"raw:{"jsonrpc":"2.0","id":null,"method":"library.folders","params":{}}"#,
        ],
        &[Permission::Library],
    );
    let h = rig.start();
    let mut reply = None;
    wait_for("the null-id reply", 2 * SEC, || {
        reply = rig
            .received("p")
            .into_iter()
            .find(|m| m.get("id") == Some(&Value::Null) && m.get("error").is_some());
        reply.is_some()
    });
    let reply = reply.unwrap();
    assert_eq!(reply["error"]["code"], -32600);
    assert_eq!(
        reply["error"]["message"],
        "invalid request: id must be a number or string"
    );
    let events = events_for(&h, Duration::from_millis(200));
    assert!(!events.iter().any(|e| matches!(e, HostEvent::Call { .. })));
    h.shutdown_all();
}
