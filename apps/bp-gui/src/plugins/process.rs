//! One plugin process: spawning, its reader, writer and stderr threads, the
//! plugin log, and (on Windows) the Job Object every child is assigned to.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select, unbounded};

use serde_json::Value;

use super::rpc::{self, MAX_LINE};

/// Lines of stderr kept in memory for the Settings page.
pub const TAIL_LINES: usize = 20;
/// Notifications queued for one plugin before new ones are dropped.
const NOTIFY_QUEUE: usize = 1024;

/// `<logs_dir>/plugins/<folder>.log` plus the in-memory stderr tail.
pub struct PluginLog {
    path: PathBuf,
    file: Mutex<Option<File>>,
    tail: Mutex<VecDeque<String>>,
}

impl PluginLog {
    pub fn new(logs_dir: &Path, folder: &str) -> PluginLog {
        PluginLog {
            path: logs_dir.join("plugins").join(format!("{folder}.log")),
            file: Mutex::new(None),
            tail: Mutex::new(VecDeque::new()),
        }
    }

    fn append(&self, line: &str) {
        let mut file = self.file.lock().unwrap();
        if file.is_none() {
            if let Some(dir) = self.path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            *file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok();
        }
        if let Some(f) = file.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }

    /// A line from nil itself, prefixed `[nil] `; it goes to the file only.
    pub fn note(&self, msg: &str) {
        self.append(&format!("[nil] {msg}"));
    }

    /// A line the owner should see next to a crash: file and tail.
    pub fn line(&self, line: &str) {
        self.append(line);
        let mut tail = self.tail.lock().unwrap();
        if tail.len() == TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line.to_string());
    }

    pub fn tail(&self) -> Vec<String> {
        self.tail.lock().unwrap().iter().cloned().collect()
    }
}

/// One line read by [`LineReader`].
#[derive(Debug, PartialEq)]
pub enum Line {
    /// The content without its `\n` or `\r\n`.
    Ok(Vec<u8>),
    /// Longer than `max`; its bytes were discarded up to the next `\n`.
    TooLong,
}

/// Splits a stream into lines without ever holding more than `max + 2` bytes of
/// one line.
pub struct LineReader<R> {
    inner: BufReader<R>,
    max: usize,
}

impl<R: Read> LineReader<R> {
    pub fn new(inner: R, max: usize) -> Self {
        Self {
            inner: BufReader::new(inner),
            max,
        }
    }

    /// The next line, or None at end of input. A last line without `\n` counts.
    pub fn next_line(&mut self) -> io::Result<Option<Line>> {
        // `max` content bytes plus a `\r`; one more marks the line as too long.
        let cap = self.max + 2;
        let mut buf = Vec::new();
        let mut too_long = false;
        let mut read_any = false;
        loop {
            let avail = match self.inner.fill_buf() {
                Ok(a) => a,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if avail.is_empty() {
                if !read_any {
                    return Ok(None);
                }
                break;
            }
            read_any = true;
            let (chunk, used, done) = match avail.iter().position(|&b| b == b'\n') {
                Some(i) => (&avail[..i], i + 1, true),
                None => (avail, avail.len(), false),
            };
            if !too_long {
                let room = cap - buf.len();
                if chunk.len() > room {
                    too_long = true;
                    buf = Vec::new();
                } else {
                    buf.extend_from_slice(chunk);
                }
            }
            self.inner.consume(used);
            if done {
                break;
            }
        }
        // This also strips a `\r` ending a last line that has no `\n` at end
        // of input; harmless, as no message ends in a raw `\r`.
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        Ok(Some(if too_long || buf.len() > self.max {
            Line::TooLong
        } else {
            Line::Ok(buf)
        }))
    }
}

/// The plugin's stdin queues. Cloning shares them. Sending never blocks.
#[derive(Clone)]
pub struct Writer {
    /// Replies and nil's own requests: never dropped.
    urgent: Sender<String>,
    /// Notifications as (method, params), encoded on the writer thread so the
    /// caller never pays for serialising them. Bounded; dropped when the
    /// plugin stops reading.
    notes: Sender<(String, Value)>,
    dropping: Arc<AtomicBool>,
    log: Arc<PluginLog>,
}

impl Writer {
    /// Queues a reply or a request from nil. These are never dropped; their
    /// number is bounded by what the plugin itself asks for.
    pub fn send(&self, line: String) {
        let _ = self.urgent.send(line);
    }

    /// Queues a notification, or drops it if the plugin has stopped reading.
    pub fn notify(&self, method: &str, params: Value) {
        match self.notes.try_send((method.to_string(), params)) {
            Ok(()) => self.dropping.store(false, Ordering::Relaxed),
            Err(TrySendError::Full(_)) => {
                if !self.dropping.swap(true, Ordering::Relaxed) {
                    self.log.note("plugin not reading; notifications dropped");
                }
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

fn write_loop(mut stdin: ChildStdin, urgent: Receiver<String>, notes: Receiver<(String, Value)>) {
    loop {
        // Replies go first when both queues have something.
        let line = match urgent.try_recv() {
            Ok(l) => l,
            Err(_) => select! {
                recv(urgent) -> m => match m { Ok(l) => l, Err(_) => return },
                recv(notes) -> m => match m {
                    Ok((method, params)) => rpc::encode_notification(&method, params),
                    Err(_) => return,
                },
            },
        };
        if stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .is_err()
        {
            return;
        }
    }
}

/// A Windows Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. The host
/// owns one that every plugin process joins, so plugins die when nil does even
/// if it never cleans up. Each process also gets its own job nested inside it,
/// so killing a plugin kills everything it started (e.g. the interpreter a
/// venv `python.exe` redirector launches).
#[cfg(windows)]
pub struct Job(windows::Win32::Foundation::HANDLE);

/// No Job Objects outside Windows; process groups do that work.
#[cfg(not(windows))]
pub enum Job {}

// A job handle may be used and closed from any thread.
#[cfg(windows)]
unsafe impl Send for Job {}
#[cfg(windows)]
unsafe impl Sync for Job {}

#[cfg(windows)]
impl Job {
    pub fn new() -> io::Result<Job> {
        use windows::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        let handle = unsafe { CreateJobObjectW(None, None) }.map_err(io::Error::other)?;
        let job = Job(handle);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(io::Error::other)?;
        Ok(job)
    }

    fn assign(&self, child: &Child) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::JobObjects::AssignProcessToJobObject;
        let process = HANDLE(child.as_raw_handle());
        unsafe { AssignProcessToJobObject(self.0, process) }.map_err(io::Error::other)
    }

    fn terminate(&self) {
        unsafe {
            let _ = windows::Win32::System::JobObjects::TerminateJobObject(self.0, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// A running plugin process. Killing it, or dropping it, kills the whole tree
/// it started: its own Job Object on Windows, its process group elsewhere.
pub struct Proc {
    child: Child,
    #[cfg(windows)]
    job: Option<Job>,
    pub writer: Writer,
    /// Set by the first kill. Once the child is reaped its process group id
    /// may be reused, so the tree is never signalled twice.
    killed: bool,
}

impl Proc {
    /// True once the process has exited.
    pub fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    pub fn kill(&mut self) {
        if std::mem::replace(&mut self.killed, true) {
            return;
        }
        kill_tree(
            &mut self.child,
            #[cfg(windows)]
            self.job.as_ref(),
        );
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.kill();
    }
}

fn kill_tree(child: &mut Child, #[cfg(windows)] job: Option<&Job>) {
    #[cfg(windows)]
    if let Some(job) = job {
        job.terminate();
    }
    #[cfg(unix)]
    if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
        // The child leads its own group (`process_group(0)`); the group id
        // stays reserved while any member lives, even after the leader is reaped.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Starts `exe args` in `cwd` with piped stdio and no console window, in its
/// own process group (Unix) or its own Job Object nested in `parent` (Windows).
///
/// `on_line` runs on the reader thread for every stdout line; `on_eof` runs
/// once when stdout closes. Stderr lines go to `log` (file and tail).
pub fn spawn(
    exe: &Path,
    args: &[String],
    cwd: &Path,
    parent: Option<&Job>,
    log: Arc<PluginLog>,
    on_line: impl FnMut(Line, &Writer) + Send + 'static,
    on_eof: impl FnOnce() + Send + 'static,
) -> io::Result<Proc> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;

    // Until the child joins its jobs below it runs outside them: a process it
    // starts in that short window is in neither job, so it survives both the
    // plugin being stopped and nil exiting. Spawning with CREATE_SUSPENDED
    // would close the window, but `std::process::Child` gives no handle to the
    // main thread to resume it; that would mean calling CreateProcessW
    // directly. A plugin is a program the user chose to run, so the window is
    // accepted.
    //
    // The host job first, then the plugin's own: the job a process joins
    // second nests inside the first, so closing the host job still kills it.
    #[cfg(windows)]
    let job = {
        if let Some(parent) = parent
            && let Err(e) = parent.assign(&child)
        {
            log.note(&format!("could not join the host job object: {e}"));
        }
        match Job::new().and_then(|j| j.assign(&child).map(|()| j)) {
            Ok(j) => Some(j),
            Err(e) => {
                log.note(&format!("could not create the plugin job object: {e}"));
                None
            }
        }
    };
    #[cfg(not(windows))]
    let _ = parent;

    match start_threads(&mut child, log, on_line, on_eof) {
        Ok(writer) => Ok(Proc {
            child,
            #[cfg(windows)]
            job,
            writer,
            killed: false,
        }),
        Err(e) => {
            kill_tree(
                &mut child,
                #[cfg(windows)]
                job.as_ref(),
            );
            Err(e)
        }
    }
}

fn start_threads(
    child: &mut Child,
    log: Arc<PluginLog>,
    mut on_line: impl FnMut(Line, &Writer) + Send + 'static,
    on_eof: impl FnOnce() + Send + 'static,
) -> io::Result<Writer> {
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let (urgent_tx, urgent_rx) = unbounded();
    let (notes_tx, notes_rx) = bounded(NOTIFY_QUEUE);
    let writer = Writer {
        urgent: urgent_tx,
        notes: notes_tx,
        dropping: Arc::new(AtomicBool::new(false)),
        log: log.clone(),
    };
    thread::Builder::new()
        .name("plugin-writer".into())
        .spawn(move || write_loop(stdin, urgent_rx, notes_rx))?;

    let reply = writer.clone();
    thread::Builder::new()
        .name("plugin-reader".into())
        .spawn(move || {
            let mut lines = LineReader::new(stdout, MAX_LINE);
            while let Ok(Some(line)) = lines.next_line() {
                on_line(line, &reply);
            }
            drop(reply);
            on_eof();
        })?;

    let err_log = log;
    thread::Builder::new()
        .name("plugin-stderr".into())
        .spawn(move || {
            let mut lines = LineReader::new(stderr, MAX_LINE);
            while let Ok(Some(line)) = lines.next_line() {
                match line {
                    Line::Ok(b) => err_log.line(&String::from_utf8_lossy(&b)),
                    Line::TooLong => err_log.note("over-long stderr line skipped"),
                }
            }
        })?;
    Ok(writer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(input: &[u8], max: usize) -> Vec<Line> {
        let mut r = LineReader::new(input, max);
        let mut out = Vec::new();
        while let Some(l) = r.next_line().unwrap() {
            out.push(l);
        }
        out
    }

    #[test]
    fn strips_one_newline_and_an_optional_carriage_return() {
        assert_eq!(
            lines(b"a\nbb\r\n\nlast", 10),
            vec![
                Line::Ok(b"a".to_vec()),
                Line::Ok(b"bb".to_vec()),
                Line::Ok(Vec::new()),
                Line::Ok(b"last".to_vec()),
            ]
        );
        assert!(lines(b"", 10).is_empty());
    }

    #[test]
    fn the_limit_applies_to_the_content_without_the_terminator() {
        assert_eq!(
            lines(b"12345\r\n123456\n", 5),
            vec![Line::Ok(b"12345".to_vec()), Line::TooLong]
        );
        assert_eq!(lines(b"123456\r\n", 5), vec![Line::TooLong]);
    }

    #[test]
    fn an_over_long_line_is_skipped_and_reading_continues() {
        let mut input = vec![b'x'; MAX_LINE + 10];
        input.extend_from_slice(b"\nok\n");
        assert_eq!(
            lines(&input, MAX_LINE),
            vec![Line::TooLong, Line::Ok(b"ok".to_vec())]
        );
        // Exactly MAX_LINE bytes of content is accepted.
        let mut input = vec![b'y'; MAX_LINE];
        input.extend_from_slice(b"\r\n");
        assert!(matches!(&lines(&input, MAX_LINE)[..], [Line::Ok(b)] if b.len() == MAX_LINE));
    }

    #[test]
    fn the_tail_keeps_the_last_twenty_lines() {
        let t = tempfile::tempdir().unwrap();
        let log = PluginLog::new(t.path(), "p");
        for i in 0..25 {
            log.line(&format!("l{i}"));
        }
        log.note("hello");
        let tail = log.tail();
        assert_eq!(tail.len(), TAIL_LINES);
        assert_eq!(tail[0], "l5");
        let text = fs::read_to_string(t.path().join("plugins").join("p.log")).unwrap();
        assert!(text.starts_with("l0\n"));
        assert!(text.ends_with("[nil] hello\n"));
    }
}
