use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unknown or unsupported file format")]
    UnknownFormat,
    #[error("DST-compressed DSD is not supported")]
    DstUnsupported,
    #[error("corrupt file: {0}")]
    Corrupt(String),
}

#[derive(Debug, Error)]
pub enum OutputError {
    #[error("audio device not found")]
    DeviceNotFound,
    #[error("audio device is in use by another application")]
    DeviceInUse,
    #[error(
        "exclusive mode is not allowed on this device: enable \"Allow applications to take exclusive control of this device\" in Sound settings"
    )]
    ExclusiveModeNotAllowed,
    #[error("the device does not support the requested format")]
    UnsupportedFormat,
    #[error("the audio device was invalidated (unplugged or reconfigured)")]
    Invalidated,
    #[error("the audio device stopped responding")]
    Stalled,
    #[error("the device accepted none of the probed formats in exclusive mode")]
    NoFormats,
    #[error("OS audio error {0:#010x}")]
    Os(i32),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UnsupportedReason {
    #[error("sample rate {rate} Hz exceeds the device maximum of {max} Hz")]
    RateExceedsDevice { rate: u32, max: u32 },
    #[error("DSD{multiple} exceeds the device maximum of DSD{max_multiple} over DoP")]
    DsdRateExceedsDevice { multiple: u16, max_multiple: u16 },
    #[error("DSD playback over DoP is disabled for this device")]
    DopDisabled,
    #[error("the device does not support DSD (no DoP sample rate)")]
    DopNotSupported,
    #[error("the device does not accept this sample format")]
    FormatNotAccepted,
    #[error("the device does not accept {channels} channels")]
    ChannelsNotAccepted { channels: u16 },
    #[error("DST-compressed DSD is not supported")]
    DstCompressed,
    #[error("unknown or unsupported file format")]
    UnknownFormat,
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Output(#[from] OutputError),
    #[error("no audio device available")]
    NoDevice,
    #[error("device profile error: {0}")]
    Profile(String),
    #[error("decoder thread failed: {0}")]
    DecoderFailed(String),
}
