//! A scripted stand-in plugin for the host tests. Each argument is one step:
//!
//! - `reply`: answer `initialize` with `{name:"fake", version:"0"}`; later requests get -32601
//! - `crash`: write `boom` to stderr and exit with code 3
//! - `hang`: sleep forever
//! - `garbage`: write `not json`
//! - `long-line`: write one line longer than 1 MB
//! - `call:<method>:<json>`: send a request
//! - `on-shutdown:<method>:<json>`: when `shutdown` arrives, first send this
//!   request (several are sent in order); a `.` method sends a notification
//! - `raw:<text>`: write `text` as one line, unchanged
//! - `ignore-shutdown`: do not answer or exit on `shutdown`
//! - `no-read`: stop reading stdin
//! - `exit-on-shutdown`: answer `shutdown` and exit (the default)
//! - `spawn-child`: start another fake plugin running `hang` that shares this
//!   one's stdio, and log `#child <pid>`
//!
//! After the script it keeps reading stdin until `shutdown` or end of input.
//! Every line received is appended to `$FAKE_PLUGIN_LOG`, or to
//! `fake-plugin.log` in the working directory when that is not set, after a
//! first `#spawn <pid> <unix ms>` line.

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

struct Fake {
    log: std::fs::File,
    stdin: std::io::StdinLock<'static>,
    next_id: u64,
    ignore_shutdown: bool,
    /// `(method, params)` sent when `shutdown` arrives.
    on_shutdown: Vec<(String, Value)>,
}

impl Fake {
    fn record(&mut self, line: &str) {
        let _ = writeln!(self.log, "{line}");
    }

    /// Sends a request with the next id.
    fn call(&mut self, method: &str, params: Value) {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
    }

    fn send(&mut self, v: Value) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    }

    /// Reads one line and logs it; None at end of input.
    fn read(&mut self) -> Option<Value> {
        let mut line = String::new();
        if self.stdin.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        self.record(line);
        Some(serde_json::from_str(line).unwrap_or(Value::Null))
    }

    /// Answers one request the default way. Returns false when it should exit.
    fn answer(&mut self, msg: &Value) -> bool {
        let (Some(id), Some(method)) = (msg.get("id"), msg.get("method").and_then(Value::as_str))
        else {
            return true;
        };
        let id = id.clone();
        match method {
            "initialize" => {
                self.send(
                    json!({"jsonrpc": "2.0", "id": id, "result": {"name": "fake", "version": "0"}}),
                );
            }
            "shutdown" if !self.on_shutdown.is_empty() => {
                for (method, params) in std::mem::take(&mut self.on_shutdown) {
                    match method.strip_prefix('.') {
                        Some(m) => {
                            self.send(json!({"jsonrpc": "2.0", "method": m, "params": params}))
                        }
                        None => self.call(&method, params),
                    }
                }
                return self.answer(msg);
            }
            "shutdown" if self.ignore_shutdown => {}
            "shutdown" => {
                self.send(json!({"jsonrpc": "2.0", "id": id, "result": {}}));
                return false;
            }
            other => {
                let error = json!({"code": -32601, "message": format!("unknown method: {other}")});
                self.send(json!({"jsonrpc": "2.0", "id": id, "error": error}));
            }
        }
        true
    }
}

fn main() {
    let log_path = std::env::var_os("FAKE_PLUGIN_LOG").unwrap_or_else(|| "fake-plugin.log".into());
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .expect("open the fake plugin log");
    let mut fake = Fake {
        log,
        stdin: std::io::stdin().lock(),
        next_id: 100,
        ignore_shutdown: false,
        on_shutdown: Vec::new(),
    };
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    fake.record(&format!("#spawn {} {ms}", std::process::id()));

    for step in std::env::args().skip(1) {
        match step.as_str() {
            "reply" => {
                while let Some(msg) = fake.read() {
                    let init = msg.get("method").and_then(Value::as_str) == Some("initialize");
                    if !fake.answer(&msg) {
                        return;
                    }
                    if init {
                        break;
                    }
                }
            }
            "crash" => {
                eprintln!("boom");
                std::process::exit(3);
            }
            "hang" | "no-read" => loop {
                std::thread::park();
            },
            "garbage" => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(b"not json\n");
                let _ = out.flush();
            }
            "long-line" => {
                let pad = "a".repeat(1_100_000);
                fake.send(json!({"jsonrpc": "2.0", "method": "x", "params": pad}));
            }
            "spawn-child" => {
                let exe = std::env::current_exe().expect("own path");
                // Left running on purpose: the host must kill it with the tree.
                #[allow(clippy::zombie_processes)]
                let child = std::process::Command::new(exe)
                    .arg("hang")
                    .spawn()
                    .expect("spawn the grandchild");
                fake.record(&format!("#child {}", child.id()));
            }
            "ignore-shutdown" => fake.ignore_shutdown = true,
            "exit-on-shutdown" => fake.ignore_shutdown = false,
            other if other.starts_with("raw:") => {
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{}", &other["raw:".len()..]);
                let _ = out.flush();
            }
            other => {
                let (later, rest) = match other.strip_prefix("on-shutdown:") {
                    Some(rest) => (true, Some(rest)),
                    None => (false, other.strip_prefix("call:")),
                };
                let Some((method, params)) = rest.and_then(|rest| rest.split_once(':')) else {
                    panic!("unknown step: {other}");
                };
                let params: Value = serde_json::from_str(params).expect("call params are JSON");
                if later {
                    fake.on_shutdown.push((method.to_string(), params));
                } else {
                    fake.call(method, params);
                }
            }
        }
    }

    while let Some(msg) = fake.read() {
        if !fake.answer(&msg) {
            return;
        }
    }
}
