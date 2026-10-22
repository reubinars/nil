//! Interactive playback loop (crossterm raw mode). Not unit tested.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use bp_engine::{Command, Engine, Event};
use crossbeam_channel::TryRecvError;
use crossterm::event::{self, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{cursor, queue};

use crate::display::describe_path;
use crate::display::describe_reason;

const SEEK_STEP: Duration = Duration::from_secs(10);

/// Puts the terminal in raw mode and restores it on drop, including during a panic.
struct RawGuard;

impl RawGuard {
    fn new() -> io::Result<RawGuard> {
        terminal::enable_raw_mode()?;
        let previous = std::panic::take_hook();
        let main_thread = std::thread::current().id();
        std::panic::set_hook(Box::new(move |info| {
            // Restore first so the panic message is readable, but only for a main-thread
            // panic: a panic in an engine thread must not cook the terminal while `run`
            // is still going (it exits via the disconnect path instead).
            if std::thread::current().id() == main_thread {
                let _ = terminal::disable_raw_mode();
            }
            previous(info);
        }));
        Ok(RawGuard)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = writeln!(io::stdout());
    }
}

fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Prints a whole line, replacing any progress text on the current one.
fn line(to_stderr: bool, text: &str) {
    let mut out: Box<dyn Write> = if to_stderr {
        Box::new(io::stderr())
    } else {
        Box::new(io::stdout())
    };
    let _ = queue!(out, cursor::MoveToColumn(0), Clear(ClearType::CurrentLine));
    let _ = write!(out, "{text}\r\n");
    let _ = out.flush();
}

struct State {
    index: usize,
    position: Duration,
    duration: Option<Duration>,
    paused: bool,
}

impl State {
    fn show_progress(&self) {
        let mut out = io::stdout();
        let _ = queue!(out, cursor::MoveToColumn(0), Clear(ClearType::CurrentLine));
        let total = self.duration.map(clock).unwrap_or_else(|| "?:??".into());
        let _ = write!(
            out,
            "  {} / {}{}",
            clock(self.position),
            total,
            if self.paused { "  [paused]" } else { "" }
        );
        let _ = out.flush();
    }
}

/// Returns `Some(exit code)` when playback is over.
fn handle_event(ev: Event, st: &mut State, device_name: &str) -> Option<u8> {
    match ev {
        Event::TrackStarted {
            index,
            path,
            playback,
            duration,
        } => {
            st.index = index;
            st.position = Duration::ZERO;
            st.duration = duration;
            line(
                false,
                &format!(
                    "{:02} {}  {}",
                    index + 1,
                    file_name(&path),
                    describe_path(&playback)
                ),
            );
        }
        Event::Position { index, position } => {
            st.index = index;
            st.position = position;
            st.show_progress();
        }
        Event::TrackSkipped {
            index,
            path,
            reason,
        } => line(
            false,
            &format!(
                "{:02} {}  {}",
                index + 1,
                file_name(&path),
                describe_reason(&reason, device_name)
            ),
        ),
        Event::Paused => {
            st.paused = true;
            st.show_progress();
        }
        Event::Resumed => {
            st.paused = false;
            st.show_progress();
        }
        Event::Underrun { total } => line(true, &format!("underruns: {total}")),
        Event::TrackError { index, path, error } => line(
            true,
            &format!("{:02} {}  error: {error}", index + 1, file_name(&path)),
        ),
        Event::Error(e) => {
            line(true, &format!("error: {e}"));
            return Some(1);
        }
        Event::DevicesChanged => {}
        Event::QueueFinished => return Some(0),
    }
    None
}

/// Plays `tracks[start..]` until the queue finishes, the user quits or a fatal error occurs.
/// Returns the process exit code.
pub fn run(
    engine: Engine,
    tracks: Vec<PathBuf>,
    start: usize,
    device_name: &str,
) -> io::Result<u8> {
    let _guard = RawGuard::new()?;
    line(
        false,
        &format!(
            "{device_name}: {} tracks  (space pause, n next, left/right seek, q quit)",
            tracks.len() - start
        ),
    );
    engine.send(Command::Play { tracks, start });
    let mut st = State {
        index: start,
        position: Duration::ZERO,
        duration: None,
        paused: false,
    };
    loop {
        loop {
            match engine.events().try_recv() {
                Ok(ev) => {
                    if let Some(code) = handle_event(ev, &mut st, device_name) {
                        return Ok(code);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    line(true, "error: the playback engine stopped unexpectedly");
                    return Ok(1);
                }
            }
        }
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let event::Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Char(' ') => engine.send(if st.paused {
                Command::Resume
            } else {
                Command::Pause
            }),
            KeyCode::Char('n' | 'N') => engine.send(Command::Next),
            KeyCode::Left => engine.send(Command::Seek {
                position: st.position.saturating_sub(SEEK_STEP),
            }),
            KeyCode::Right => {
                let mut to = st.position + SEEK_STEP;
                if let Some(d) = st.duration {
                    to = to.min(d);
                }
                engine.send(Command::Seek { position: to });
            }
            KeyCode::Char('q' | 'Q') => return Ok(0),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(0),
            _ => {}
        }
    }
}
