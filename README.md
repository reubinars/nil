# nil

A bit-perfect music player for Windows, written in Rust. nil sends your files to your DAC
exactly as they are: no resampling, no volume change, no mixing with other sounds.
Nothing added.

- **Bit-perfect output** through WASAPI exclusive mode, event-driven, with a badge that says
  whether what reaches the DAC is bit-perfect.
- **Formats:** FLAC (MQA passes through untouched), WAV, AIFF, ALAC, DSF and DFF (DSD as DoP),
  and AAC-LC (lossy; decoded to 24-bit, gapless, marked not bit-perfect).
- **Strict by design:** a track the DAC can't play as it is gets skipped with the reason,
  never silently converted.
- **Library:** parallel scan, instant start from a saved index, albums grouped across
  CD1/CD2 folders and compilations, covers from the files or the folder.
- **Interface:** Home with recently added albums, Albums, Artists, search, album pages, a
  floating now-playing bar with a live spectrum, a full-window Now Playing view, and a
  capability table for your DAC.

## Download

Get the latest `nil-<version>-windows.zip` from
[Releases](https://github.com/reubinars/nil/releases), unzip it anywhere and run `nil.exe`.
There is nothing to install. The README inside the zip covers getting started, shortcuts and
troubleshooting.

Requires Windows 10 or 11 (64-bit) and an output device that allows exclusive mode.

## Build from source

You need [Rust](https://rustup.rs) (stable) on Windows.

```
cargo run --release -p bp-gui
```

To make the ready-to-use folder (`nil.exe`, README, release notes and licence notices), close
nil and run `package-nil.bat`. The folder lands next to the repo as `nil`, or in `dist\nil`
when the repo itself is cloned as `nil`.

Run the tests with `cargo test --workspace`.

## Layout

| Where | What |
|---|---|
| `crates/bp-core` | Shared types: formats, devices, errors |
| `crates/bp-decode` | Decoders (Symphonia for PCM and AAC, our own DSF/DFF readers) and DoP |
| `crates/bp-output` | WASAPI exclusive output and device capability probing |
| `crates/bp-engine` | The playback engine: queue, gapless streams, format negotiation, device profiles |
| `apps/bp-gui` | nil itself (egui) |
| `apps/bp-cli` | `bp`, a command-line player and checker used during development |
| `packaging/nil` | The README and release notes shipped with nil |

## Licence

MIT, see [LICENSE](LICENSE). The fonts and libraries built into nil keep their own licences;
the release zip lists them in `licenses/`.
