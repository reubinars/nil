//! Windows' System Media Transport Controls for nil's window.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Receiver;
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
    SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs,
    SystemMediaTransportControlsTimelineProperties,
};
use windows::Storage::StorageFile;
use windows::Storage::Streams::RandomAccessStreamReference;
use windows::Win32::Foundation::{E_FAIL, HWND};
use windows::Win32::System::WinRT::{
    ISystemMediaTransportControlsInterop, RO_INIT_MULTITHREADED, RoInitialize,
};
use windows::core::{Error, HSTRING, Ref, Result, factory};

use super::{Button, MediaSession, Status, Timeline, Track, Update};

/// Registers nil's window `hwnd` with SMTC. Presses go to the returned session and call
/// `wake`. Updates are applied on a thread of their own, so a thumbnail being read never
/// holds up a frame.
pub fn start(hwnd: isize, wake: Arc<dyn Fn() + Send + Sync>) -> Result<MediaSession> {
    let interop = factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()?;
    let smtc: SystemMediaTransportControls = unsafe { interop.GetForWindow(HWND(hwnd as *mut _)) }?;
    smtc.SetIsPlayEnabled(true)?;
    smtc.SetIsPauseEnabled(true)?;
    smtc.SetIsNextEnabled(true)?;
    smtc.SetIsPreviousEnabled(true)?;
    // Hidden until something plays.
    smtc.SetIsEnabled(false)?;

    let (button_tx, button_rx) = crossbeam_channel::unbounded();
    {
        let tx = button_tx.clone();
        let wake = wake.clone();
        smtc.ButtonPressed(&TypedEventHandler::new(
            move |_, args: Ref<SystemMediaTransportControlsButtonPressedEventArgs>| {
                let button = match args.ok()?.Button()? {
                    SystemMediaTransportControlsButton::Play => Button::Play,
                    SystemMediaTransportControlsButton::Pause => Button::Pause,
                    SystemMediaTransportControlsButton::Next => Button::Next,
                    SystemMediaTransportControlsButton::Previous => Button::Previous,
                    _ => return Ok(()),
                };
                let _ = tx.send(button);
                wake();
                Ok(())
            },
        ))?;
    }
    smtc.PlaybackPositionChangeRequested(&TypedEventHandler::new(
        move |_, args: Ref<PlaybackPositionChangeRequestedEventArgs>| {
            let ticks = args.ok()?.RequestedPlaybackPosition()?.Duration;
            let _ = button_tx.send(Button::Seek(from_ticks(ticks)));
            wake();
            Ok(())
        },
    ))?;

    let (update_tx, update_rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("media-session".into())
        .spawn(move || apply_all(smtc, update_rx))
        .map_err(|e| Error::new(E_FAIL, e.to_string()))?;
    Ok(MediaSession::new(update_tx, button_rx))
}

/// Applies updates until the app's end of the session is dropped.
fn apply_all(smtc: SystemMediaTransportControls, updates: Receiver<Update>) {
    // WinRT calls need an apartment; the multithreaded one needs no message pump.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    for update in updates {
        if let Err(e) = apply(&smtc, &update) {
            tracing::warn!("media session: {update:?} failed: {e}");
        }
    }
}

fn apply(smtc: &SystemMediaTransportControls, update: &Update) -> Result<()> {
    match update {
        Update::Track(Some(track)) => show_track(smtc, track),
        Update::Track(None) => {
            let display = smtc.DisplayUpdater()?;
            display.ClearAll()?;
            display.Update()
        }
        Update::Timeline(t) => show_timeline(smtc, *t),
        Update::Status(status) => {
            smtc.SetPlaybackStatus(match status {
                Status::Playing => MediaPlaybackStatus::Playing,
                Status::Paused => MediaPlaybackStatus::Paused,
                Status::Stopped => MediaPlaybackStatus::Stopped,
            })?;
            // With nothing loaded there is nothing to control: the session hides.
            smtc.SetIsEnabled(*status != Status::Stopped)
        }
    }
}

fn show_track(smtc: &SystemMediaTransportControls, track: &Track) -> Result<()> {
    let display = smtc.DisplayUpdater()?;
    // Drops the last track's thumbnail when this one has none.
    display.ClearAll()?;
    display.SetType(MediaPlaybackType::Music)?;
    let music = display.MusicProperties()?;
    music.SetTitle(&HSTRING::from(track.title.as_str()))?;
    music.SetArtist(&HSTRING::from(track.artist.as_str()))?;
    music.SetAlbumArtist(&HSTRING::from(track.artist.as_str()))?;
    music.SetAlbumTitle(&HSTRING::from(track.album.as_str()))?;
    if let Some(path) = &track.art {
        match thumbnail(path) {
            Ok(art) => display.SetThumbnail(&art)?,
            Err(e) => tracing::warn!("media session: no art from {}: {e}", path.display()),
        }
    }
    display.Update()
}

fn thumbnail(path: &Path) -> Result<RandomAccessStreamReference> {
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))?.join()?;
    RandomAccessStreamReference::CreateFromFile(&file)
}

fn show_timeline(smtc: &SystemMediaTransportControls, t: Timeline) -> Result<()> {
    let props = SystemMediaTransportControlsTimelineProperties::new()?;
    props.SetStartTime(ticks(Duration::ZERO))?;
    props.SetEndTime(ticks(t.duration))?;
    props.SetMinSeekTime(ticks(Duration::ZERO))?;
    props.SetMaxSeekTime(ticks(t.duration))?;
    props.SetPosition(ticks(t.position))?;
    smtc.UpdateTimelineProperties(&props)
}

/// A `TimeSpan` counts 100 ns ticks.
fn ticks(d: Duration) -> TimeSpan {
    TimeSpan {
        Duration: (d.as_nanos() / 100) as i64,
    }
}

fn from_ticks(ticks: i64) -> Duration {
    Duration::from_nanos(ticks.max(0) as u64 * 100)
}
