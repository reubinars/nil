//! `bp`: command-line front end for the bit-perfect player.

mod display;
mod play;
mod tracks;

use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;

use bp_core::DeviceId;
use bp_engine::negotiate::candidates;
use bp_engine::profile::{self, DeviceProfile, ProfileStore, merge_probe};
use bp_engine::{Engine, EngineConfig};
use bp_output::OutputBackend;
use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

type Res<T> = Result<T, Box<dyn Error>>;

#[derive(Parser)]
#[command(name = "bp", version, about = "Bit-perfect audio player")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List audio output endpoints (id, name, default)
    Devices,
    /// Probe the device's exclusive-mode formats, save the profile and print the matrix
    Probe {
        /// Device ID (default: last used, else the system default)
        #[arg(long)]
        device: Option<String>,
    },
    /// Enable or disable DSD over PCM (DoP) for the device
    Dop {
        state: Toggle,
        #[arg(long)]
        device: Option<String>,
    },
    /// Show, per file, the playback path or why it cannot play (no audio is output)
    Check {
        /// Folders and/or files
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long)]
        device: Option<String>,
    },
    /// Play folders and/or files. Keys: space pause/resume, n next, left/right seek 10 s, q quit
    Play {
        /// Folders and/or files
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Start at this track number (1-based)
        #[arg(long, default_value_t = 1)]
        from: usize,
        #[arg(long)]
        device: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Toggle {
    On,
    Off,
}

/// The real output backend; only available on Windows.
fn new_backend() -> Option<Box<dyn OutputBackend>> {
    #[cfg(windows)]
    {
        Some(Box::new(bp_output::WasapiBackend::new()))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn require_backend() -> Result<Box<dyn OutputBackend>, ExitCode> {
    new_backend().ok_or_else(|| {
        eprintln!("bp requires Windows");
        ExitCode::from(2)
    })
}

fn profile_path() -> PathBuf {
    profile::default_dir().join("devices.toml")
}

/// Writes `bp.log` next to the profile file. Nothing is ever logged to the terminal, which
/// is in raw mode during `play`.
fn init_logging() {
    let dir = profile::default_dir();
    let _ = std::fs::create_dir_all(&dir);
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("bp.log"))
    else {
        return;
    };
    // The `wasapi` crate logs every buffer write at trace via `log`; keep it (and anything
    // else unknown) at warn or above.
    let filter = Targets::new()
        .with_default(LevelFilter::WARN)
        .with_target("bp_cli", LevelFilter::INFO)
        .with_target("bp_engine", LevelFilter::INFO)
        .with_target("bp_output", LevelFilter::INFO)
        .with_target("bp_decode", LevelFilter::INFO);
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_filter(filter);
    let _ = tracing_subscriber::registry().with(layer).try_init();
}

/// `--device`, else the profile file's last used device, else (with a backend) the system
/// default.
fn pick_device(
    arg: Option<String>,
    store: &ProfileStore,
    backend: Option<&dyn OutputBackend>,
) -> Res<DeviceId> {
    if let Some(id) = arg {
        return Ok(DeviceId(id));
    }
    if let Some(last) = store.last_device() {
        let known = match backend {
            Some(b) => b.devices()?.iter().any(|d| &d.id == last),
            None => true,
        };
        if known {
            return Ok(last.clone());
        }
    }
    match backend {
        Some(b) => Ok(b.default_device()?),
        None => Err("no device selected: pass --device <id> (or run `bp probe` on Windows)".into()),
    }
}

fn device_name(backend: Option<&dyn OutputBackend>, id: &DeviceId) -> Option<String> {
    backend?
        .devices()
        .ok()?
        .into_iter()
        .find(|d| &d.id == id)
        .map(|d| d.name)
}

fn cmd_devices() -> Res<ExitCode> {
    let backend = match require_backend() {
        Ok(b) => b,
        Err(code) => return Ok(code),
    };
    for d in backend.devices()? {
        println!(
            "{} {}  {}",
            if d.is_default { "*" } else { " " },
            d.id.0,
            d.name
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_probe(device: Option<String>) -> Res<ExitCode> {
    let backend = match require_backend() {
        Ok(b) => b,
        Err(code) => return Ok(code),
    };
    let mut store = ProfileStore::load(&profile_path())?;
    let id = pick_device(device, &store, Some(&*backend))?;
    let name = device_name(Some(&*backend), &id).unwrap_or_else(|| id.0.clone());
    println!("Probing {name} (exclusive mode, 2 channels)...");
    let caps = backend.probe(&id, 2)?;
    let profile = merge_probe(store.get(&id), name, &caps)?;
    print!("{}", display::probe_matrix(&profile));
    println!("{}", display::dop_line(&profile));
    store.upsert(&id, profile);
    store.set_last_device(Some(id));
    store.save()?;
    Ok(ExitCode::SUCCESS)
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn cmd_dop(state: Toggle, device: Option<String>) -> Res<ExitCode> {
    let backend = new_backend();
    let mut store = ProfileStore::load(&profile_path())?;
    let id = pick_device(device, &store, backend.as_deref())?;
    let on = matches!(state, Toggle::On);
    let mut profile = match store.get(&id) {
        Some(p) => p.clone(),
        None => {
            let name = device_name(backend.as_deref(), &id).ok_or_else(|| {
                format!(
                    "no saved profile for {}; run `bp probe` on Windows first",
                    id.0
                )
            })?;
            DeviceProfile {
                name,
                ..DeviceProfile::default()
            }
        }
    };
    profile.dop_enabled = on;
    println!("DoP {} for {}", on_off(on), profile.name);
    store.upsert(&id, profile);
    store.save()?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_check(inputs: Vec<PathBuf>, device: Option<String>) -> Res<ExitCode> {
    let backend = new_backend();
    let store = ProfileStore::load(&profile_path())?;
    let id = pick_device(device, &store, backend.as_deref())?;
    let Some(profile) = store.get(&id) else {
        eprintln!(
            "No saved profile for {}. Run `bp probe` on Windows first.",
            id.0
        );
        return Ok(ExitCode::from(1));
    };
    let caps = profile.capabilities();
    let files = tracks::collect_tracks(&inputs);
    if files.is_empty() {
        return Err("no audio files found".into());
    }
    for (i, path) in files.iter().enumerate() {
        let label = format!("{:02} {}", i + 1, file_name(path));
        match bp_decode::open(path) {
            Err(e) => println!("{label}  error: {e}"),
            Ok(src) => {
                let res = candidates(&src.format(), &caps, profile.dop_enabled, None);
                println!("{label}  {}", display::describe_check(&res, &profile.name));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn cmd_play(inputs: Vec<PathBuf>, from: usize, device: Option<String>) -> Res<ExitCode> {
    let backend = match require_backend() {
        Ok(b) => b,
        Err(code) => return Ok(code),
    };
    let files = tracks::collect_tracks(&inputs);
    if files.is_empty() {
        return Err("no audio files found".into());
    }
    if from == 0 || from > files.len() {
        return Err(format!("--from must be between 1 and {}", files.len()).into());
    }
    let mut store = ProfileStore::load(&profile_path())?;
    let id = pick_device(device, &store, Some(&*backend))?;
    let name = device_name(Some(&*backend), &id).unwrap_or_else(|| id.0.clone());
    store.set_last_device(Some(id.clone()));
    if let Err(e) = store.save() {
        eprintln!("bp: warning: could not save last used device: {e}");
    }
    let engine = Engine::start(
        backend,
        EngineConfig {
            device: Some(id),
            profiles: store,
        },
    );
    let code = play::run(engine, files, from - 1, &name)?;
    Ok(ExitCode::from(code))
}

fn run(cli: Cli) -> Res<ExitCode> {
    match cli.command {
        Cmd::Devices => cmd_devices(),
        Cmd::Probe { device } => cmd_probe(device),
        Cmd::Dop { state, device } => cmd_dop(state, device),
        Cmd::Check { inputs, device } => cmd_check(inputs, device),
        Cmd::Play {
            inputs,
            from,
            device,
        } => cmd_play(inputs, from, device),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("bp: {e}");
            ExitCode::from(1)
        }
    }
}
