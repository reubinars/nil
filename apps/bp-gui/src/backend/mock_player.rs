//! A simulated player: plays nothing, but emits the engine's events on a clock.
//!
//! Events are generated lazily, in time order, whenever the player is touched
//! (`try_recv`, `send`, `inject`), so a test drives it with a `ManualClock`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bp_core::{
    Capabilities, DeviceFormat, DeviceId, DeviceInfo, EngineError, PROBE_ENCODINGS, PROBE_RATES,
};
use bp_engine::{AnalysisTap, Event};

use super::{DeviceCaps, PlayerCommand, PlayerEvent, PlayerHandle, QueueItem};
use crate::state::support::track_support;

/// How long opening the device stream takes.
const OPEN_TIME: Duration = Duration::from_millis(300);
/// Sample rate of the synthetic audio the mock's tap carries.
const TAP_RATE: u32 = 44_100;
/// Interval between `Position` events.
const TICK: Duration = Duration::from_millis(250);
/// `Previous` past this position restarts the current track.
const RESTART_AFTER: Duration = Duration::from_secs(3);

/// Time source for the simulation.
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

/// Wall-clock time since creation.
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        SystemClock {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

/// A clock that moves only when told to.
#[derive(Default)]
pub struct ManualClock {
    now: Mutex<Duration>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance(&self, d: Duration) {
        *self.now.lock().unwrap() += d;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        *self.now.lock().unwrap()
    }
}

#[derive(Debug, Clone)]
pub struct MockDevice {
    pub info: DeviceInfo,
    pub caps: DeviceCaps,
}

type Waker = Arc<dyn Fn() + Send + Sync>;

/// A simulated player. Clones share one simulation.
#[derive(Clone)]
pub struct MockPlayer {
    clock: Arc<dyn Clock>,
    sim: Arc<Mutex<Sim>>,
    waker: Arc<Mutex<Option<Waker>>>,
    tap: AnalysisTap,
    feed: Arc<Mutex<Feed>>,
}

/// How far the synthetic audio has been written into the tap.
#[derive(Default)]
struct Feed {
    begun: bool,
    /// Clock time written up to, while running.
    until: Option<Duration>,
    written: u64,
}

/// A few sines whose levels drift, so the demo's spectrum moves.
fn synth(n: u64) -> f32 {
    let t = n as f32 / TAP_RATE as f32;
    [(110.0, 0.7), (440.0, 1.1), (2_000.0, 1.9), (8_000.0, 2.3)]
        .iter()
        .map(|&(f, k): &(f32, f32)| {
            (0.4 + 0.3 * (t * k).sin()) * (std::f32::consts::TAU * f * t).sin()
        })
        .sum::<f32>()
        / 4.0
}

impl MockPlayer {
    /// Selects the default device, else the first.
    pub fn new(devices: Vec<MockDevice>, clock: Arc<dyn Clock>) -> Self {
        let selected = devices
            .iter()
            .find(|d| d.info.is_default)
            .or(devices.first())
            .map(|d| d.info.id.clone());
        MockPlayer {
            clock,
            sim: Arc::new(Mutex::new(Sim {
                devices,
                selected,
                queue: Vec::new(),
                phase: Phase::Stopped,
                outbox: VecDeque::new(),
                sent: Vec::new(),
            })),
            waker: Arc::new(Mutex::new(None)),
            tap: AnalysisTap::default(),
            feed: Arc::default(),
        }
    }

    /// DAC A (up to 384 kHz, selected) and DAC B (up to 192 kHz), both with DoP.
    pub fn demo() -> Self {
        Self::new(demo_devices(), Arc::new(SystemClock::new()))
    }

    /// Queues `ev` for the next `try_recv` and wakes the UI. An injected fatal
    /// `Error` also stops the simulation, as the engine stops after one.
    pub fn inject(&self, ev: PlayerEvent) {
        let now = self.clock.now();
        {
            let mut sim = self.sim.lock().unwrap();
            sim.advance(now);
            // Like the engine, the player stops after a fatal error.
            if matches!(ev, PlayerEvent::Engine(Event::Error(_))) {
                sim.phase = Phase::Stopped;
            }
            sim.outbox.push_back(ev);
        }
        self.wake();
    }

    /// Replaces the device list (selecting the default, else the first) and queues
    /// `DevicesChanged`, as the real engine does when its list is first published.
    pub fn set_devices(&self, devices: Vec<MockDevice>) {
        {
            let mut sim = self.sim.lock().unwrap();
            sim.selected = devices
                .iter()
                .find(|d| d.info.is_default)
                .or(devices.first())
                .map(|d| d.info.id.clone());
            sim.devices = devices;
        }
        self.inject(PlayerEvent::DevicesChanged);
    }

    /// Every command sent so far.
    pub fn sent(&self) -> Vec<PlayerCommand> {
        self.sim.lock().unwrap().sent.clone()
    }

    fn wake(&self) {
        let waker = self.waker.lock().unwrap().clone();
        if let Some(waker) = waker {
            waker();
        }
    }
}

/// DAC A (up to 384 kHz, default) and DAC B (up to 192 kHz), both with DoP.
pub fn demo_devices() -> Vec<MockDevice> {
    let device = |id: &str, name: &str, is_default, max_rate| MockDevice {
        info: DeviceInfo {
            id: DeviceId(id.into()),
            name: name.into(),
            is_default,
        },
        caps: stereo_caps_up_to(max_rate),
    };
    vec![
        device("dac-a", "DAC A", true, 384_000),
        device("dac-b", "DAC B", false, 192_000),
    ]
}

/// Stereo, every probed encoding at every probed rate up to `max_rate`, DoP on.
fn stereo_caps_up_to(max_rate: u32) -> DeviceCaps {
    let mut caps = Capabilities::default();
    for rate in PROBE_RATES.into_iter().filter(|r| *r <= max_rate) {
        for encoding in PROBE_ENCODINGS {
            caps.formats.insert((rate, 2, encoding));
        }
    }
    DeviceCaps {
        caps,
        dop_enabled: true,
    }
}

impl PlayerHandle for MockPlayer {
    fn send(&self, cmd: PlayerCommand) {
        let now = self.clock.now();
        let queued = {
            let mut sim = self.sim.lock().unwrap();
            sim.advance(now);
            sim.sent.push(cmd.clone());
            let before = sim.outbox.len();
            sim.command(cmd, now);
            sim.outbox.len() > before
        };
        if queued {
            self.wake();
        }
    }

    fn try_recv(&self) -> Option<PlayerEvent> {
        let now = self.clock.now();
        let mut sim = self.sim.lock().unwrap();
        sim.advance(now);
        sim.outbox.pop_front()
    }

    fn devices(&self) -> (Vec<DeviceInfo>, Option<DeviceId>) {
        let sim = self.sim.lock().unwrap();
        let infos = sim.devices.iter().map(|d| d.info.clone()).collect();
        (infos, sim.selected.clone())
    }

    fn device_caps(&self, id: &DeviceId) -> Option<DeviceCaps> {
        let sim = self.sim.lock().unwrap();
        sim.devices
            .iter()
            .find(|d| &d.info.id == id)
            .map(|d| d.caps.clone())
    }

    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        *self.waker.lock().unwrap() = Some(Arc::from(waker));
    }

    /// Writes synthetic audio for the clock time played since the last call.
    fn analysis(&self) -> Option<AnalysisTap> {
        let now = self.clock.now();
        let running = {
            let mut sim = self.sim.lock().unwrap();
            sim.advance(now);
            matches!(sim.phase, Phase::Playing { paused: false, .. })
        };
        let mut feed = self.feed.lock().unwrap();
        if running {
            if !feed.begun {
                self.tap.begin(TAP_RATE, TAP_RATE);
                feed.begun = true;
                feed.written = 0;
            }
            let from = feed.until.unwrap_or(now);
            let n = (now.saturating_sub(from).as_secs_f64() * f64::from(TAP_RATE)) as u64;
            // Never more than the tap keeps.
            let n = n.min(4 * u64::from(TAP_RATE));
            let start = feed.written;
            let samples: Vec<f32> = (start..start + n).map(synth).collect();
            self.tap.write(&samples);
            feed.written += n;
            feed.until = Some(now);
        } else {
            feed.until = None;
        }
        if feed.begun {
            self.tap.played(feed.written, Instant::now(), !running);
        }
        Some(self.tap.clone())
    }
}

enum Phase {
    Stopped,
    /// The stream finishes opening at `ready`; `queue[index]` then starts at `start`,
    /// or once resumed if `paused`.
    Opening {
        index: usize,
        ready: Duration,
        start: Duration,
        paused: bool,
    },
    /// Track position is `base + (now - anchor)` while running, `base` while paused.
    /// The next `Position` event reports `next_tick`.
    Playing {
        index: usize,
        device: DeviceFormat,
        base: Duration,
        anchor: Duration,
        next_tick: Duration,
        paused: bool,
    },
}

struct Sim {
    devices: Vec<MockDevice>,
    selected: Option<DeviceId>,
    queue: Vec<QueueItem>,
    phase: Phase,
    outbox: VecDeque<PlayerEvent>,
    sent: Vec<PlayerCommand>,
}

impl Sim {
    fn emit(&mut self, ev: Event) {
        self.outbox.push_back(PlayerEvent::Engine(ev));
    }

    /// Emits every event due at or before `now`, in time order.
    fn advance(&mut self, now: Duration) {
        loop {
            match self.phase {
                Phase::Stopped | Phase::Playing { paused: true, .. } => return,
                Phase::Opening {
                    index,
                    ready,
                    start,
                    paused,
                } => {
                    if paused || ready > now {
                        return;
                    }
                    self.start_from(index, ready, start, None);
                }
                Phase::Playing {
                    index,
                    device,
                    base,
                    anchor,
                    ref mut next_tick,
                    paused: false,
                } => {
                    let duration = self.queue[index].duration;
                    if *next_tick <= duration {
                        // A tick due with the track end comes first.
                        if anchor + (*next_tick - base) > now {
                            return;
                        }
                        let position = *next_tick;
                        *next_tick += TICK;
                        self.emit(Event::Position { index, position });
                    } else {
                        let end = anchor + duration.saturating_sub(base);
                        if end > now {
                            return;
                        }
                        self.start_from(index + 1, end, Duration::ZERO, Some(device));
                    }
                }
            }
        }
    }

    /// At time `at`, plays the first playable item from `from` (skipping the
    /// rest), starting `queue[from]` at `start`. With an open stream in
    /// `current` format, a different format reopens the stream first.
    fn start_from(
        &mut self,
        from: usize,
        at: Duration,
        start: Duration,
        current: Option<DeviceFormat>,
    ) {
        let Some(caps) = self.selected_caps() else {
            self.emit(Event::Error(EngineError::NoDevice));
            self.phase = Phase::Stopped;
            return;
        };
        for index in from..self.queue.len() {
            let item = &self.queue[index];
            let path = item.path.clone();
            match track_support(&item.source, &caps) {
                Err(reason) => self.emit(Event::TrackSkipped {
                    index,
                    path,
                    reason,
                }),
                Ok(playback) if current.is_none_or(|d| d == playback.device) => {
                    let duration = item.duration;
                    let start = if index == from { start } else { Duration::ZERO };
                    self.emit(Event::TrackStarted {
                        index,
                        path,
                        playback,
                        duration: Some(duration),
                    });
                    self.phase = Phase::Playing {
                        index,
                        device: playback.device,
                        base: start,
                        anchor: at,
                        next_tick: start + TICK,
                        paused: false,
                    };
                    return;
                }
                Ok(_) => {
                    self.phase = Phase::Opening {
                        index,
                        ready: at + OPEN_TIME,
                        start: Duration::ZERO,
                        paused: false,
                    };
                    return;
                }
            }
        }
        self.emit(Event::QueueFinished);
        self.phase = Phase::Stopped;
    }

    fn selected_caps(&self) -> Option<DeviceCaps> {
        let id = self.selected.as_ref()?;
        self.devices
            .iter()
            .find(|d| &d.info.id == id)
            .map(|d| d.caps.clone())
    }

    /// (Re)opens the stream for `queue[index]` at `start`, paused or not.
    fn open(&mut self, index: usize, now: Duration, start: Duration, paused: bool) {
        if index >= self.queue.len() {
            self.emit(Event::QueueFinished);
            self.phase = Phase::Stopped;
            return;
        }
        self.phase = Phase::Opening {
            index,
            ready: now + OPEN_TIME,
            start,
            paused,
        };
    }

    /// Paused by the user; like the engine, a reopened stream keeps this.
    fn paused(&self) -> bool {
        matches!(
            self.phase,
            Phase::Opening { paused: true, .. } | Phase::Playing { paused: true, .. }
        )
    }

    /// The current index and track position, unless stopped.
    fn current(&self, now: Duration) -> Option<(usize, Duration)> {
        match self.phase {
            Phase::Stopped => None,
            Phase::Opening { index, start, .. } => Some((index, start)),
            Phase::Playing {
                index,
                base,
                anchor,
                paused,
                ..
            } => Some((index, if paused { base } else { base + (now - anchor) })),
        }
    }

    fn command(&mut self, cmd: PlayerCommand, now: Duration) {
        match cmd {
            PlayerCommand::Play { items, start } => {
                self.queue = items;
                self.open(start, now, Duration::ZERO, false);
            }
            PlayerCommand::Pause => {
                let position = self.current(now).map(|(_, p)| p);
                match &mut self.phase {
                    Phase::Opening {
                        paused: paused @ false,
                        ..
                    } => *paused = true,
                    Phase::Playing {
                        base,
                        paused: paused @ false,
                        ..
                    } => {
                        *base = position.unwrap_or(*base);
                        *paused = true;
                    }
                    _ => return,
                }
                self.emit(Event::Paused);
            }
            PlayerCommand::Resume => {
                match &mut self.phase {
                    // An opened stream starts now.
                    Phase::Opening {
                        ready,
                        paused: paused @ true,
                        ..
                    } => {
                        *ready = (*ready).max(now);
                        *paused = false;
                    }
                    Phase::Playing {
                        anchor,
                        paused: paused @ true,
                        ..
                    } => {
                        *anchor = now;
                        *paused = false;
                    }
                    _ => return,
                }
                self.emit(Event::Resumed);
            }
            PlayerCommand::Stop => self.phase = Phase::Stopped,
            PlayerCommand::Next => {
                if let Some((index, _)) = self.current(now) {
                    self.open(index + 1, now, Duration::ZERO, self.paused());
                }
            }
            PlayerCommand::Previous => {
                if let Some((index, position)) = self.current(now) {
                    let target = if position > RESTART_AFTER || index == 0 {
                        index
                    } else {
                        index - 1
                    };
                    self.open(target, now, Duration::ZERO, self.paused());
                }
            }
            PlayerCommand::Seek { position } => {
                if let Some((index, _)) = self.current(now) {
                    self.open(index, now, position, self.paused());
                }
            }
            PlayerCommand::SelectDevice { id } => {
                if self.devices.iter().any(|d| d.info.id == id) {
                    self.selected = Some(id);
                    if let Some((index, position)) = self.current(now) {
                        self.open(index, now, position, self.paused());
                    }
                }
            }
            PlayerCommand::RefreshDevices => self.outbox.push_back(PlayerEvent::DevicesChanged),
            PlayerCommand::Reprobe { id } => {
                self.outbox.push_back(PlayerEvent::DevicesChanged);
                if self.selected.as_ref() == Some(&id)
                    && let Some((index, position)) = self.current(now)
                {
                    self.open(index, now, position, self.paused());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use bp_core::{PathKind, SourceFormat};
    use bp_engine::Event;

    use super::*;
    use crate::backend::QueueItem;
    use crate::state::support::testutil::{dsd, pcm};

    const STEP: Duration = Duration::from_millis(50);

    fn item(name: &str, source: SourceFormat, secs: u64) -> QueueItem {
        QueueItem {
            path: PathBuf::from(format!("/music/{name}")),
            source,
            duration: Duration::from_secs(secs),
            title: name.to_owned(),
        }
    }

    fn setup() -> (MockPlayer, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new());
        (MockPlayer::new(demo_devices(), clock.clone()), clock)
    }

    fn now_ms(clock: &ManualClock) -> u64 {
        clock.now().as_millis() as u64
    }

    fn summary(ev: &PlayerEvent) -> String {
        match ev {
            PlayerEvent::DevicesChanged => "devices".into(),
            PlayerEvent::Engine(e) => match e {
                Event::TrackStarted {
                    index, playback, ..
                } => {
                    let kind = match playback.kind {
                        PathKind::Native => "Native",
                        PathKind::Padded { .. } => "Padded",
                        PathKind::Dop => "Dop",
                    };
                    format!("started {index} {kind}")
                }
                Event::Position { index, position } => {
                    format!("pos {index} {}", position.as_millis())
                }
                Event::TrackSkipped { index, .. } => format!("skipped {index}"),
                Event::Paused => "paused".into(),
                Event::Resumed => "resumed".into(),
                Event::QueueFinished => "finished".into(),
                other => format!("{other:?}"),
            },
        }
    }

    /// Drains events, then steps the clock by 50 ms until `until_ms`,
    /// recording each event with the clock time it arrived at.
    fn run(p: &MockPlayer, clock: &ManualClock, until_ms: u64) -> Vec<(u64, String)> {
        let mut log = Vec::new();
        loop {
            while let Some(ev) = p.try_recv() {
                log.push((now_ms(clock), summary(&ev)));
            }
            if now_ms(clock) >= until_ms {
                return log;
            }
            clock.advance(STEP);
        }
    }

    fn milestones(log: &[(u64, String)]) -> Vec<(u64, &str)> {
        log.iter()
            .filter(|(_, s)| !s.starts_with("pos"))
            .map(|(t, s)| (*t, s.as_str()))
            .collect()
    }

    fn album() -> Vec<QueueItem> {
        vec![item("a", pcm(44_100, 16), 1), item("b", pcm(44_100, 16), 1)]
    }

    #[test]
    fn plays_album_gaplessly_then_finishes() {
        let (p, clock) = setup();
        p.send(PlayerCommand::Play {
            items: album(),
            start: 0,
        });
        let log = run(&p, &clock, 2500);
        assert_eq!(
            milestones(&log),
            vec![
                (300, "started 0 Native"),
                (1300, "started 1 Native"),
                (2300, "finished")
            ]
        );
        let first_track: Vec<_> = log
            .iter()
            .take_while(|(_, s)| s != "started 1 Native")
            .collect();
        assert_eq!(
            first_track
                .iter()
                .filter(|(_, s)| s.starts_with("pos"))
                .map(|(t, s)| (*t, s.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (550, "pos 0 250"),
                (800, "pos 0 500"),
                (1050, "pos 0 750"),
                (1300, "pos 0 1000")
            ]
        );
    }

    #[test]
    fn events_stay_in_time_order_across_a_clock_jump() {
        let (stepped, clock) = setup();
        let items = vec![
            item("a", pcm(96_000, 24), 1),
            item("x", dsd(256), 1),
            item("b", pcm(44_100, 16), 1),
            item("c", pcm(44_100, 16), 1),
        ];
        stepped.send(PlayerCommand::Play {
            items: items.clone(),
            start: 0,
        });
        let expected: Vec<String> = run(&stepped, &clock, 4000)
            .into_iter()
            .map(|(_, s)| s)
            .collect();

        let (jumped, clock) = setup();
        jumped.send(PlayerCommand::Play { items, start: 0 });
        clock.advance(Duration::from_secs(4));
        let got: Vec<String> = std::iter::from_fn(|| jumped.try_recv())
            .map(|e| summary(&e))
            .collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn format_change_reopens() {
        let (p, clock) = setup();
        p.send(PlayerCommand::Play {
            items: vec![item("a", pcm(96_000, 24), 1), item("b", pcm(44_100, 16), 1)],
            start: 0,
        });
        let log = run(&p, &clock, 3000);
        let m = milestones(&log);
        assert_eq!(m.len(), 3, "{m:?}");
        assert_eq!(m[0].0, 300);
        assert!(m[0].1.starts_with("started 0"));
        assert_eq!(m[1].0, 1600);
        assert!(m[1].1.starts_with("started 1"));
        assert_eq!(m[2], (2600, "finished"));
    }

    #[test]
    fn dsd256_is_skipped_on_dac_a_and_dsd64_plays_on_dac_b() {
        let (p, clock) = setup();
        let (_, selected) = p.devices();
        assert_eq!(selected, Some(DeviceId("dac-a".into())));
        p.send(PlayerCommand::Play {
            items: vec![item("x", dsd(256), 1), item("y", dsd(128), 1)],
            start: 0,
        });
        let log = run(&p, &clock, 400);
        assert_eq!(
            milestones(&log),
            vec![(300, "skipped 0"), (300, "started 1 Dop")]
        );

        let (p, clock) = setup();
        p.send(PlayerCommand::SelectDevice {
            id: DeviceId("dac-b".into()),
        });
        assert_eq!(p.devices().1, Some(DeviceId("dac-b".into())));
        p.send(PlayerCommand::Play {
            items: vec![item("y", dsd(128), 1), item("z", dsd(64), 1)],
            start: 0,
        });
        let log = run(&p, &clock, 400);
        assert_eq!(
            milestones(&log),
            vec![(300, "skipped 0"), (300, "started 1 Dop")]
        );
    }

    #[test]
    fn previous_rule() {
        let (p, clock) = setup();
        let items: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(|n| item(n, pcm(44_100, 16), 10))
            .collect();
        p.send(PlayerCommand::Play { items, start: 0 });
        run(&p, &clock, 4300); // index 0 at 4 s

        p.send(PlayerCommand::Previous);
        let log = run(&p, &clock, 4850);
        assert_eq!(
            log,
            vec![
                (4600, "started 0 Native".into()),
                (4850, "pos 0 250".into())
            ]
        );

        p.send(PlayerCommand::Next);
        run(&p, &clock, 7150); // index 1 at 2 s
        p.send(PlayerCommand::Previous);
        assert_eq!(
            milestones(&run(&p, &clock, 7450)),
            vec![(7450, "started 0 Native")]
        );

        run(&p, &clock, 8450); // index 0 at 1 s
        p.send(PlayerCommand::Previous);
        assert_eq!(
            milestones(&run(&p, &clock, 8750)),
            vec![(8750, "started 0 Native")]
        );
    }

    #[test]
    fn pause_freezes_position() {
        let (p, clock) = setup();
        p.send(PlayerCommand::Play {
            items: vec![item("a", pcm(44_100, 16), 10)],
            start: 0,
        });
        run(&p, &clock, 1300);
        p.send(PlayerCommand::Pause);
        assert_eq!(run(&p, &clock, 3300), vec![(1300, "paused".into())]);
        p.send(PlayerCommand::Resume);
        assert_eq!(
            run(&p, &clock, 3550),
            vec![(3300, "resumed".into()), (3550, "pos 0 1250".into())]
        );
    }

    #[test]
    fn switching_device_while_paused_reopens_paused() {
        let (p, clock) = setup();
        let items = vec![item("a", pcm(44_100, 16), 10)];
        p.send(PlayerCommand::Play { items, start: 0 });
        run(&p, &clock, 1300); // 1 s in
        p.send(PlayerCommand::Pause);
        p.send(PlayerCommand::SelectDevice {
            id: DeviceId("dac-b".into()),
        });
        assert_eq!(run(&p, &clock, 3000), vec![(1300, "paused".into())]);
        p.send(PlayerCommand::Resume);
        assert_eq!(
            run(&p, &clock, 3250),
            vec![
                (3000, "resumed".into()),
                (3000, "started 0 Native".into()),
                (3250, "pos 0 1250".into())
            ]
        );
    }

    #[test]
    fn pause_while_opening_holds_the_stream_until_resume() {
        let (p, clock) = setup();
        let items = vec![item("a", pcm(44_100, 16), 10)];
        p.send(PlayerCommand::Play { items, start: 0 });
        run(&p, &clock, 100);
        p.send(PlayerCommand::Pause);
        assert_eq!(run(&p, &clock, 1000), vec![(100, "paused".into())]);
        p.send(PlayerCommand::Resume);
        assert_eq!(
            run(&p, &clock, 1250),
            vec![
                (1000, "resumed".into()),
                (1000, "started 0 Native".into()),
                (1250, "pos 0 250".into())
            ]
        );
    }

    #[test]
    fn seek_reopens_and_restarts_the_same_track() {
        let (p, clock) = setup();
        let items = vec![item("a", pcm(44_100, 16), 120)];
        p.send(PlayerCommand::Play { items, start: 0 });
        run(&p, &clock, 1000);
        p.send(PlayerCommand::Seek {
            position: Duration::from_secs(60),
        });
        assert_eq!(
            run(&p, &clock, 1550),
            vec![
                (1300, "started 0 Native".into()),
                (1550, "pos 0 60250".into())
            ]
        );
    }

    #[test]
    fn reprobe_emits_devices_changed() {
        let (p, _clock) = setup();
        let boxed: Box<dyn PlayerHandle> = Box::new(p.clone());
        let id = DeviceId("dac-a".into());
        boxed.send(PlayerCommand::Reprobe { id: id.clone() });
        assert!(matches!(p.try_recv(), Some(PlayerEvent::DevicesChanged)));
        assert!(p.try_recv().is_none());
        assert_eq!(p.sent(), vec![PlayerCommand::Reprobe { id }]);
    }

    #[test]
    fn reprobe_of_the_selected_device_reopens_at_the_position() {
        let (p, clock) = setup();
        let items = vec![item("a", pcm(44_100, 16), 10)];
        p.send(PlayerCommand::Play { items, start: 0 });
        run(&p, &clock, 1300); // 1 s in
        p.send(PlayerCommand::Reprobe {
            id: DeviceId("dac-a".into()),
        });
        assert_eq!(
            run(&p, &clock, 1850),
            vec![
                (1300, "devices".into()),
                (1600, "started 0 Native".into()),
                (1850, "pos 0 1250".into())
            ]
        );

        // Another device: playback is untouched.
        p.send(PlayerCommand::Reprobe {
            id: DeviceId("dac-b".into()),
        });
        assert_eq!(
            run(&p, &clock, 2100),
            vec![(1850, "devices".into()), (2100, "pos 0 1500".into())]
        );
    }

    #[test]
    fn injected_fatal_error_stops_the_simulation() {
        let (p, clock) = setup();
        p.send(PlayerCommand::Play {
            items: vec![item("a", pcm(44_100, 16), 10)],
            start: 0,
        });
        run(&p, &clock, 1_000);
        p.inject(PlayerEvent::Engine(Event::Error(EngineError::NoDevice)));
        p.send(PlayerCommand::SelectDevice {
            id: DeviceId("dac-b".into()),
        });
        let log = run(&p, &clock, 3_000);
        assert_eq!(log.len(), 1, "only the error: {log:?}");
    }

    #[test]
    fn inject_queues_the_event_and_wakes() {
        let (p, _clock) = setup();
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        p.set_waker(Box::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        p.inject(PlayerEvent::Engine(Event::Underrun { total: 3 }));
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert!(matches!(
            p.try_recv(),
            Some(PlayerEvent::Engine(Event::Underrun { total: 3 }))
        ));
    }

    #[test]
    fn demo_devices_caps() {
        let p = MockPlayer::demo();
        let (devices, selected) = p.devices();
        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["DAC A", "DAC B"]);
        assert_eq!(selected, Some(devices[0].id.clone()));
        let b = p.device_caps(&devices[1].id).unwrap();
        assert!(b.dop_enabled);
        assert_eq!(b.caps.formats.iter().map(|f| f.0).max(), Some(192_000));
    }

    #[test]
    fn mock_player_tap_has_audio_while_playing() {
        let (p, clock) = setup();
        let tap = p.analysis().expect("a tap");
        p.send(PlayerCommand::Play {
            items: vec![item("a", pcm(44_100, 16), 10)],
            start: 0,
        });
        run(&p, &clock, 400);
        p.analysis();
        clock.advance(Duration::from_millis(200));
        p.analysis();
        let w = tap
            .window(1024, Instant::now())
            .expect("audio while playing");
        assert_eq!(w.rate, 44_100);
        assert!(w.samples.iter().any(|s| s.abs() > 0.05));
        p.send(PlayerCommand::Pause);
        p.analysis();
        assert_eq!(tap.window(1024, Instant::now()), None);
    }
}
