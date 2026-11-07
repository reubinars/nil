//! The plugin host: discovers plugins, runs the enabled ones, restarts them
//! after crashes, checks permissions and hands calls and panels to the app.
//!
//! Every plugin with a usable manifest has a supervisor thread that owns its
//! lifecycle; the host talks to it through a mailbox. A running process adds
//! its reader, writer and stderr threads. No public call blocks except
//! `shutdown_all`, which waits at most the shutdown grace: turning a plugin off
//! or restarting it returns at once, and the status follows on host threads.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::{Value, json};

use super::consent::{Consent, ConsentStore, may_run};
use super::manifest::{self, Manifest};
use super::panel::{self, Panel, Place};
use super::process::{self, Job, Line, PluginLog, Proc, Writer};
use super::rpc::{self, API_VERSION, Call, Incoming, Parsed, Permission, RpcError};

/// How often a supervisor checks whether its process has exited on its own.
const POLL: Duration = Duration::from_millis(50);
/// After the process exits, how long to wait for the reader to finish the
/// output already written before treating the exit as final. (Stdout may also
/// never close, if the plugin left a child holding it.)
const DRAIN: Duration = Duration::from_millis(250);
/// Request ids nil uses for its own requests.
const INITIALIZE_ID: u64 = 1;
const SHUTDOWN_ID: u64 = 2;

/// Supervision timings, injected so tests run fast.
///
/// After a crash the plugin restarts after `backoff[0]`, then `backoff[1]`,
/// then `backoff[2]`; but `crash_limit` crashes within `crash_window` stop it
/// as crashed, so with the default limit of 3 only the first two backoffs ever
/// apply (the third crash in 5 minutes is final).
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    pub handshake: Duration,
    pub backoff: [Duration; 3],
    pub crash_window: Duration,
    pub crash_limit: usize,
    pub grace: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Timings {
            handshake: Duration::from_secs(10),
            backoff: [
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(30),
            ],
            crash_window: Duration::from_secs(300),
            crash_limit: 3,
            grace: Duration::from_secs(3),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Off,
    Starting,
    Running,
    Crashed { reason: String },
    Incompatible,
    InvalidManifest(String),
}

#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub folder: String,
    pub manifest: Option<Manifest>,
    pub consent: Consent,
    pub status: Status,
    /// The last 20 stderr lines.
    pub stderr_tail: Vec<String>,
}

#[derive(Debug)]
pub enum HostEvent {
    /// `id` is a token for [`PluginHost::reply`], unique across processes, not
    /// the plugin's own request id.
    Call {
        plugin: String,
        id: Value,
        call: Call,
    },
    /// `None` means the plugin's panel at `place` is gone.
    Panel {
        plugin: String,
        place: Place,
        panel: Option<Panel>,
    },
    /// A status changed; Settings should redraw.
    Changed,
}

/// A supervisor's mailbox.
enum Msg {
    /// Run the plugin (it was turned on).
    Enable,
    /// Shut it down politely (`shutdown`, the grace, then kill) and stay off.
    Disable,
    /// Shut down if running, then start again with a fresh crash count.
    Restart,
    /// Shut down politely and end the supervisor (`shutdown_all`).
    Exit,
    /// Kill at once and end the supervisor (the host is dropped).
    Kill,
    /// The `initialize` reply arrived from process generation `generation`.
    Ready { generation: u64, ok: bool },
    /// Stdout of process generation `generation` closed.
    Eof { generation: u64 },
}

/// State shared by the host and all supervisors.
struct Ctx {
    plugins_dir: PathBuf,
    timings: Timings,
    events: Sender<HostEvent>,
    waker: Arc<dyn Fn() + Send + Sync>,
    /// Source of `HostEvent::Call` tokens.
    next_token: AtomicU64,
    /// Set when the host drops; no process starts after that.
    closing: AtomicBool,
    /// The host's Job Object (Windows only; always None elsewhere). Its lock
    /// is held while spawning so no child starts after it closes.
    job: Mutex<Option<Job>>,
}

impl Ctx {
    fn emit(&self, e: HostEvent) {
        self.queue(e);
        self.wake();
    }

    /// Queues an event without waking the UI. The channel is unbounded, so this
    /// never blocks and may be done while holding an instance lock.
    fn queue(&self, e: HostEvent) {
        let _ = self.events.send(e);
    }

    /// Wakes the UI. Never called with an instance lock held: the waker reaches
    /// into the UI toolkit.
    fn wake(&self) {
        (self.waker)();
    }
}

/// The current process of a plugin, as the reader threads and `reply` see it.
#[derive(Default)]
struct Instance {
    generation: u64,
    /// True from spawn until the process is gone; lines are dropped otherwise.
    live: bool,
    /// Set once the supervisor starts stopping the process: its requests get
    /// an error and nothing more reaches the app.
    stopping: bool,
    writer: Option<Writer>,
    /// Calls handed to the app and not yet answered: token → plugin's id.
    pending: HashMap<u64, Value>,
}

struct Plugin {
    folder: String,
    manifest: Option<Manifest>,
    consent: Mutex<Consent>,
    status: Mutex<Status>,
    log: Arc<PluginLog>,
    instance: Mutex<Instance>,
    /// Methods already logged as denied.
    denied: Mutex<HashSet<String>>,
    /// Set after logging a malformed line; cleared by the next good one.
    malformed: AtomicBool,
    /// The supervisor's mailbox and thread, for plugins that can run.
    supervisor: Mutex<Option<(Sender<Msg>, JoinHandle<()>)>>,
}

impl Plugin {
    fn set_status(&self, ctx: &Ctx, s: Status) {
        let mut cur = self.status.lock().unwrap();
        if *cur != s {
            *cur = s;
            drop(cur);
            ctx.emit(HostEvent::Changed);
        }
    }

    fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn granted(&self, p: Permission) -> bool {
        self.consent.lock().unwrap().granted.contains(&p)
    }

    fn may_run(&self) -> bool {
        self.manifest.as_ref().is_some_and(|m| {
            m.api_version == API_VERSION && may_run(m, &self.consent.lock().unwrap())
        })
    }

    fn send(&self, m: Msg) {
        if let Some((tx, _)) = self.supervisor.lock().unwrap().as_ref() {
            let _ = tx.send(m);
        }
    }
}

pub struct PluginHost {
    ctx: Arc<Ctx>,
    plugins: Vec<Arc<Plugin>>,
    consent: ConsentStore,
    events: Receiver<HostEvent>,
}

impl PluginHost {
    /// Discovers plugins and starts every one that may run. Returns at once:
    /// spawning and the handshake happen on host threads.
    pub fn start(
        plugins_dir: PathBuf,
        consent_path: PathBuf,
        logs_dir: PathBuf,
        timings: Timings,
        waker: Arc<dyn Fn() + Send + Sync>,
    ) -> PluginHost {
        let consent = ConsentStore::load(&consent_path);
        let (tx, rx) = unbounded();
        #[cfg(windows)]
        let job = match Job::new() {
            Ok(j) => Some(j),
            Err(e) => {
                tracing::warn!("plugin job object unavailable: {e}");
                None
            }
        };
        #[cfg(not(windows))]
        let job = None;
        let ctx = Arc::new(Ctx {
            timings,
            events: tx,
            waker,
            next_token: AtomicU64::new(1),
            closing: AtomicBool::new(false),
            job: Mutex::new(job),
            plugins_dir,
        });
        let plugins = manifest::discover(&ctx.plugins_dir)
            .into_iter()
            .map(|(folder, m)| {
                let c = consent.get(&folder);
                let (manifest, status) = match m {
                    Err(e) => (None, Status::InvalidManifest(e)),
                    Ok(m) if m.api_version != API_VERSION => (Some(m), Status::Incompatible),
                    Ok(m) => (Some(m), Status::Off),
                };
                Arc::new(Plugin {
                    log: Arc::new(PluginLog::new(&logs_dir, &folder)),
                    folder,
                    manifest,
                    consent: Mutex::new(c),
                    status: Mutex::new(status),
                    instance: Mutex::new(Instance::default()),
                    denied: Mutex::new(HashSet::new()),
                    malformed: AtomicBool::new(false),
                    supervisor: Mutex::new(None),
                })
            })
            .collect::<Vec<_>>();
        for p in &plugins {
            if p.status() == Status::Off {
                start_supervisor(&ctx, p);
            }
        }
        PluginHost {
            ctx,
            plugins,
            consent,
            events: rx,
        }
    }

    pub fn plugins(&self) -> Vec<PluginInfo> {
        self.plugins
            .iter()
            .map(|p| PluginInfo {
                folder: p.folder.clone(),
                manifest: p.manifest.clone(),
                consent: p.consent.lock().unwrap().clone(),
                status: p.status(),
                stderr_tail: p.log.tail(),
            })
            .collect()
    }

    pub fn try_recv(&self) -> Option<HostEvent> {
        self.events.try_recv().ok()
    }

    fn find(&self, folder: &str) -> Option<&Arc<Plugin>> {
        self.plugins.iter().find(|p| p.folder == folder)
    }

    /// Answers a `Call`. Dropped unless that call came from the plugin's
    /// current process and is still unanswered.
    pub fn reply(&self, plugin: &str, id: Value, result: Result<Value, RpcError>) {
        let (Some(p), Some(token)) = (self.find(plugin), id.as_u64()) else {
            return;
        };
        let mut inst = p.instance.lock().unwrap();
        if let Some(own_id) = inst.pending.remove(&token)
            && let Some(w) = &inst.writer
        {
            w.send(rpc::encode_response(&own_id, &result));
        }
    }

    /// Sends a notification to a running plugin; otherwise drops it.
    pub fn notify(&self, plugin: &str, method: &str, params: Value) {
        let Some(p) = self.find(plugin) else { return };
        if p.status() != Status::Running {
            return;
        }
        if let Some(w) = &p.instance.lock().unwrap().writer {
            w.notify(method, params);
        }
    }

    pub fn granted(&self, plugin: &str, p: Permission) -> bool {
        self.find(plugin).is_some_and(|pl| pl.granted(p))
    }

    /// Turning on grants the manifest's permissions, saves consent and starts
    /// the plugin. Turning off saves consent and asks the plugin to shut down
    /// (it is killed after `grace`); the status becomes `Off` and the panel is
    /// removed once it is gone. Returns without waiting either way.
    pub fn set_enabled(&mut self, folder: &str, on: bool) -> io::Result<()> {
        let Some(p) = self.find(folder).cloned() else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no plugin {folder:?}"),
            ));
        };
        let mut c = p.consent.lock().unwrap().clone();
        c.enabled = on;
        if on && let Some(m) = &p.manifest {
            c.granted = m.permissions.clone();
        }
        self.consent.set(folder, c.clone())?;
        *p.consent.lock().unwrap() = c;
        p.send(if on { Msg::Enable } else { Msg::Disable });
        Ok(())
    }

    /// Restarts a plugin that may run (e.g. after it crashed), with a fresh
    /// crash count. Returns without waiting.
    pub fn restart(&mut self, folder: &str) {
        if let Some(p) = self.find(folder)
            && p.may_run()
        {
            p.send(Msg::Restart);
        }
    }

    /// Shuts every plugin down in parallel; returns within about `grace`.
    pub fn shutdown_all(self) {
        let sups: Vec<_> = self
            .plugins
            .iter()
            .filter_map(|p| p.supervisor.lock().unwrap().take())
            .collect();
        for (tx, _) in &sups {
            let _ = tx.send(Msg::Exit);
        }
        for (_, h) in sups {
            let _ = h.join();
        }
    }
}

fn start_supervisor(ctx: &Arc<Ctx>, p: &Arc<Plugin>) {
    let (tx, rx) = unbounded();
    let sup = Supervisor {
        ctx: ctx.clone(),
        plugin: p.clone(),
        rx,
        mailbox: tx.clone(),
        generation: 0,
        intent: if p.may_run() {
            Intent::Run
        } else {
            Intent::Off
        },
        restart: false,
        kill: false,
        halted: false,
    };
    match thread::Builder::new()
        .name(format!("plugin-{}", p.folder))
        .spawn(move || sup.run())
    {
        Ok(handle) => *p.supervisor.lock().unwrap() = Some((tx, handle)),
        Err(e) => {
            p.log
                .line(&format!("[nil] cannot start a supervisor thread: {e}"));
            p.set_status(
                ctx,
                Status::Crashed {
                    reason: "didn't start".into(),
                },
            );
        }
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        // On Windows closing the host job kills every plugin tree at once.
        let had_job = {
            let mut job = self.ctx.job.lock().unwrap();
            self.ctx.closing.store(true, Ordering::SeqCst);
            job.take().is_some()
        };
        let sups: Vec<_> = self
            .plugins
            .iter()
            .filter_map(|p| p.supervisor.lock().unwrap().take())
            .collect();
        for (tx, _) in &sups {
            let _ = tx.send(Msg::Kill);
        }
        // Without a job, wait for the supervisors to kill their processes.
        if !had_job {
            for (_, h) in sups {
                let _ = h.join();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Intent {
    Run,
    Off,
    Exit,
}

/// What a supervisor waits for while a process runs.
enum Event {
    Ready,
    Failed,
    Exited,
    /// A control message changed what the supervisor wants.
    Control,
    Timeout,
}

/// How a process ended.
enum Outcome {
    Crashed,
    DidntStart,
    /// Stopped because of a control message.
    Stopped,
}

/// The lifecycle of one plugin: spawn, handshake, run, restart with backoff,
/// following the host's Enable / Disable / Restart / Exit / Kill messages.
struct Supervisor {
    ctx: Arc<Ctx>,
    plugin: Arc<Plugin>,
    rx: Receiver<Msg>,
    /// Given to each process's reader thread. Because the supervisor holds a
    /// sender, its mailbox never disconnects.
    mailbox: Sender<Msg>,
    generation: u64,
    intent: Intent,
    /// A restart was asked for; the current run ends and a fresh one begins.
    restart: bool,
    /// Kill instead of a polite shutdown.
    kill: bool,
    /// Stopped after crashing; waits for Enable or Restart.
    halted: bool,
}

/// A spawned process and what the supervisor knows about its end.
struct Running {
    proc: Proc,
    eof: bool,
    exited_at: Option<Instant>,
}

impl Supervisor {
    fn set_status(&self, s: Status) {
        self.plugin.set_status(&self.ctx, s);
    }

    /// Applies a control message. Process messages are ignored here.
    fn control(&mut self, m: Msg) {
        if self.intent == Intent::Exit {
            return;
        }
        match m {
            Msg::Enable => {
                self.intent = Intent::Run;
                self.halted = false;
            }
            Msg::Disable => {
                self.intent = Intent::Off;
                self.halted = false;
            }
            Msg::Restart => {
                self.intent = Intent::Run;
                self.halted = false;
                self.restart = true;
            }
            Msg::Exit => self.intent = Intent::Exit,
            Msg::Kill => {
                self.intent = Intent::Exit;
                self.kill = true;
            }
            Msg::Ready { .. } | Msg::Eof { .. } => {}
        }
    }

    /// True when the running process should be stopped.
    fn should_stop(&self) -> bool {
        self.intent != Intent::Run || self.restart
    }

    fn run(mut self) {
        loop {
            self.restart = false;
            match self.intent {
                Intent::Exit => return,
                Intent::Run if !self.halted => {
                    self.active();
                    continue;
                }
                Intent::Off => self.set_status(Status::Off),
                Intent::Run => {}
            }
            // Off or crashed: wait for the next control message.
            if let Ok(m) = self.rx.recv() {
                self.control(m);
            }
        }
    }

    /// Runs the plugin, restarting it after crashes, until a control message
    /// or the crash limit ends the run.
    fn active(&mut self) {
        let t = self.ctx.timings;
        let mut crashes: VecDeque<Instant> = VecDeque::new();
        while !self.should_stop() {
            self.set_status(Status::Starting);
            let outcome = match self.spawn() {
                Err(None) => {
                    self.intent = Intent::Exit;
                    return;
                }
                Err(Some(e)) => {
                    self.plugin.log.line(&format!("[nil] cannot start: {e}"));
                    Outcome::Crashed
                }
                Ok(mut r) => {
                    let o = self.session(&mut r);
                    r.proc.kill();
                    self.end_instance();
                    o
                }
            };
            if self.ctx.closing.load(Ordering::SeqCst) {
                self.intent = Intent::Exit;
                return;
            }
            match outcome {
                Outcome::Stopped => return,
                Outcome::DidntStart => {
                    self.set_status(Status::Crashed {
                        reason: "didn't start".into(),
                    });
                    self.halted = true;
                    return;
                }
                Outcome::Crashed => {}
            }
            let now = Instant::now();
            crashes.push_back(now);
            while crashes
                .front()
                .is_some_and(|&c| now.duration_since(c) > t.crash_window)
            {
                crashes.pop_front();
            }
            if crashes.len() >= t.crash_limit {
                self.set_status(Status::Crashed {
                    reason: format!("crashed {} times in 5 minutes", crashes.len()),
                });
                self.halted = true;
                return;
            }
            // Waiting to restart counts as starting.
            self.set_status(Status::Starting);
            let until = Instant::now() + t.backoff[(crashes.len() - 1).min(2)];
            while !self.should_stop() {
                match self.rx.recv_deadline(until) {
                    Ok(m) => self.control(m),
                    Err(_) => break,
                }
            }
        }
    }

    /// Spawns the process and sends `initialize`. `Err(None)` means the host
    /// is closing.
    fn spawn(&mut self) -> Result<Running, Option<io::Error>> {
        let p = self.plugin.clone();
        let m = p.manifest.as_ref().expect("only valid plugins run");
        let folder = self.ctx.plugins_dir.join(&p.folder);
        let data_dir = folder.join("data");
        fs::create_dir_all(&data_dir).map_err(Some)?;
        let (exe, args) = manifest::program(&folder, m);
        self.generation += 1;
        let generation = self.generation;

        let on_line = {
            let (ctx, plugin) = (self.ctx.clone(), p.clone());
            let mailbox = self.mailbox.clone();
            move |line: Line, w: &Writer| handle_line(&ctx, &plugin, &mailbox, generation, line, w)
        };
        let on_eof = {
            let mailbox = self.mailbox.clone();
            move || {
                let _ = mailbox.send(Msg::Eof { generation });
            }
        };

        let proc = {
            let job = self.ctx.job.lock().unwrap();
            if self.ctx.closing.load(Ordering::SeqCst) {
                return Err(None);
            }
            {
                // Lines from this process count from the moment it exists.
                let mut inst = p.instance.lock().unwrap();
                inst.generation = generation;
                inst.live = true;
                inst.stopping = false;
                inst.pending.clear();
            }
            process::spawn(
                &exe,
                &args,
                &folder,
                job.as_ref(),
                p.log.clone(),
                on_line,
                on_eof,
            )
            .map_err(|e| {
                p.instance.lock().unwrap().live = false;
                Some(io::Error::new(e.kind(), format!("{}: {e}", exe.display())))
            })?
        };
        p.instance.lock().unwrap().writer = Some(proc.writer.clone());

        let granted: Vec<&str> = p
            .consent
            .lock()
            .unwrap()
            .granted
            .iter()
            .map(|g| g.name())
            .collect();
        proc.writer.send(rpc::encode_request(
            INITIALIZE_ID,
            "initialize",
            json!({
                "api_version": API_VERSION,
                "nil_version": env!("CARGO_PKG_VERSION"),
                "granted": granted,
                "data_dir": data_dir.to_string_lossy(),
            }),
        ));
        Ok(Running {
            proc,
            eof: false,
            exited_at: None,
        })
    }

    /// The process is gone: its lines, replies and panel go with it.
    fn end_instance(&self) {
        {
            let mut inst = self.plugin.instance.lock().unwrap();
            inst.live = false;
            inst.writer = None;
            inst.pending.clear();
            // Queued under the lock: the reader queues Panel and Call events
            // under it too, after checking the instance is live, so nothing
            // from this process can follow.
            for place in [Place::Tab, Place::Settings] {
                self.ctx.queue(HostEvent::Panel {
                    plugin: self.plugin.folder.clone(),
                    place,
                    panel: None,
                });
            }
        }
        self.ctx.wake();
    }

    /// Waits for the next thing that matters to the current process, or until
    /// `until`. Stale messages from earlier processes are skipped.
    fn wait(&mut self, r: &mut Running, until: Option<Instant>) -> Event {
        loop {
            if r.eof || r.exited_at.is_some_and(|t| t.elapsed() >= DRAIN) {
                return Event::Exited;
            }
            let now = Instant::now();
            if until.is_some_and(|u| now >= u) {
                return Event::Timeout;
            }
            let step = until.map_or(POLL, |u| (u - now).min(POLL));
            match self.rx.recv_timeout(step) {
                Ok(Msg::Ready { generation, ok }) => {
                    if generation == self.generation {
                        return if ok { Event::Ready } else { Event::Failed };
                    }
                }
                Ok(Msg::Eof { generation }) => {
                    if generation == self.generation {
                        r.eof = true;
                    }
                }
                Ok(m) => {
                    self.control(m);
                    return Event::Control;
                }
                // Timed out: the mailbox cannot disconnect (see `mailbox`).
                Err(_) => {
                    if r.exited_at.is_none() && r.proc.exited() {
                        r.exited_at = Some(Instant::now());
                    }
                }
            }
        }
    }

    /// Runs one process until it ends or is stopped.
    fn session(&mut self, r: &mut Running) -> Outcome {
        let deadline = Instant::now() + self.ctx.timings.handshake;
        loop {
            match self.wait(r, Some(deadline)) {
                Event::Ready => break,
                Event::Exited => return Outcome::Crashed,
                Event::Failed | Event::Timeout => return Outcome::DidntStart,
                Event::Control if self.should_stop() => {
                    self.shut_down(r);
                    return Outcome::Stopped;
                }
                Event::Control => {}
            }
        }
        r.proc
            .writer
            .send(rpc::encode_notification("initialized", json!({})));
        self.set_status(Status::Running);
        loop {
            match self.wait(r, None) {
                Event::Exited => return Outcome::Crashed,
                Event::Control if self.should_stop() => {
                    self.shut_down(r);
                    return Outcome::Stopped;
                }
                _ => {}
            }
        }
    }

    /// `shutdown`, then up to `grace` for the process to exit (unless told to
    /// kill). The caller kills whatever is left.
    fn shut_down(&mut self, r: &mut Running) {
        self.plugin.instance.lock().unwrap().stopping = true;
        if self.kill {
            return;
        }
        r.proc
            .writer
            .send(rpc::encode_request(SHUTDOWN_ID, "shutdown", json!({})));
        let deadline = Instant::now() + self.ctx.timings.grace;
        while !self.kill {
            match self.wait(r, Some(deadline)) {
                Event::Exited | Event::Timeout => break,
                _ => {}
            }
        }
    }
}

/// Handles one line from a plugin's stdout, on its reader thread. Lines from
/// a process that is no longer the plugin's current one are dropped.
fn handle_line(
    ctx: &Ctx,
    plugin: &Plugin,
    mailbox: &Sender<Msg>,
    generation: u64,
    line: Line,
    w: &Writer,
) {
    let current = |inst: &Instance| inst.live && inst.generation == generation;
    if !current(&plugin.instance.lock().unwrap()) {
        return;
    }
    let decoded = match line {
        Line::Ok(bytes) => rpc::decode(&bytes),
        Line::TooLong => Err(RpcError::parse()),
    };
    let msg = match decoded {
        Ok(m) => {
            plugin.malformed.store(false, Ordering::Relaxed);
            m
        }
        Err(_) => {
            if !plugin.malformed.swap(true, Ordering::Relaxed) {
                plugin.log.note("malformed line ignored");
            }
            return;
        }
    };
    match msg {
        Incoming::Response { id, result } => {
            if id == json!(INITIALIZE_ID) {
                let _ = mailbox.send(Msg::Ready {
                    generation,
                    ok: result.is_ok(),
                });
            }
        }
        // Plugins send no notifications today; ignore them (also while stopping).
        Incoming::Notification { .. } => {}
        Incoming::InvalidRequest => w.send(rpc::encode_response(
            &Value::Null,
            &Err(RpcError::invalid_request(
                "invalid request: id must be a number or string",
            )),
        )),
        Incoming::Request { id, method, params } => {
            // Decided without the lock; acted on under it.
            let action = match rpc::parse_call(&method, params) {
                Err(e) => Action::Reply(Err(e)),
                Ok(Parsed::PanelSet(v)) => match panel::parse_set(&v) {
                    Ok((place, panel)) => Action::Panel(place, panel),
                    Err(e) => Action::Reply(Err(e)),
                },
                Ok(Parsed::Call(call)) => {
                    let need = rpc::required(&call);
                    if plugin.granted(need) {
                        Action::Call(call)
                    } else {
                        Action::Reply(Err(RpcError::denied(need)))
                    }
                }
            };
            let denied = matches!(&action, Action::Reply(Err(e)) if e.code == RpcError::DENIED);
            {
                // Panel and Call events are queued under the instance lock after
                // checking it is still current and not stopping, so none can
                // follow the `Panel { None }` that `end_instance` queues.
                let mut inst = plugin.instance.lock().unwrap();
                if !current(&inst) {
                    return;
                }
                if inst.stopping {
                    w.send(rpc::encode_response(
                        &id,
                        &Err(RpcError::internal("plugin is stopping")),
                    ));
                    return;
                }
                match action {
                    Action::Reply(r) => w.send(rpc::encode_response(&id, &r)),
                    Action::Panel(place, panel) => {
                        w.send(rpc::encode_response(&id, &Ok(json!({}))));
                        ctx.queue(HostEvent::Panel {
                            plugin: plugin.folder.clone(),
                            place,
                            panel: Some(panel),
                        });
                    }
                    Action::Call(call) => {
                        let token = ctx.next_token.fetch_add(1, Ordering::Relaxed);
                        inst.pending.insert(token, id);
                        ctx.queue(HostEvent::Call {
                            plugin: plugin.folder.clone(),
                            id: json!(token),
                            call,
                        });
                    }
                }
            }
            ctx.wake();
            if denied && plugin.denied.lock().unwrap().insert(method.clone()) {
                plugin.log.note(&format!("permission denied: {method}"));
            }
        }
    }
}

/// What the host does with one request from a plugin.
enum Action {
    Reply(Result<Value, RpcError>),
    Panel(Place, Panel),
    Call(Call),
}
