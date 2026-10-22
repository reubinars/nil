//! HRESULT → [`OutputError`] mapping.
//!
//! The table works on plain `i32` codes so it is unit-tested on every OS; `map_hresult` and
//! `map_wasapi` are the thin Windows adapters over it.

use bp_core::OutputError;

/// HRESULT codes the backend distinguishes (values from `audioclient.h` / `winerror.h`).
pub(crate) mod codes {
    pub const AUDCLNT_E_DEVICE_INVALIDATED: i32 = 0x8889_0004_u32 as i32;
    pub const AUDCLNT_E_UNSUPPORTED_FORMAT: i32 = 0x8889_0008_u32 as i32;
    pub const AUDCLNT_E_DEVICE_IN_USE: i32 = 0x8889_000A_u32 as i32;
    pub const AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED: i32 = 0x8889_000E_u32 as i32;
    /// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`: what `IMMDeviceEnumerator::GetDevice` returns for
    /// an unknown endpoint ID (documented as `E_NOTFOUND`).
    pub const E_NOTFOUND: i32 = 0x8007_0490_u32 as i32;
    /// Generic failure, used for wasapi-crate errors that carry no HRESULT.
    pub const E_FAIL: i32 = 0x8000_4005_u32 as i32;
}

/// Maps an HRESULT code to the error the engine acts on.
pub(crate) fn map_code(code: i32) -> OutputError {
    match code {
        codes::AUDCLNT_E_DEVICE_IN_USE => OutputError::DeviceInUse,
        codes::AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED => OutputError::ExclusiveModeNotAllowed,
        codes::AUDCLNT_E_UNSUPPORTED_FORMAT => OutputError::UnsupportedFormat,
        codes::AUDCLNT_E_DEVICE_INVALIDATED => OutputError::Invalidated,
        codes::E_NOTFOUND => OutputError::DeviceNotFound,
        other => OutputError::Os(other),
    }
}

#[cfg(windows)]
pub(crate) fn map_hresult(code: windows::core::HRESULT) -> OutputError {
    map_code(code.0)
}

/// Maps a wasapi-crate error: HRESULTs through [`map_hresult`], the crate's own variants to
/// their meaning.
#[cfg(windows)]
pub(crate) fn map_wasapi(e: ::wasapi::WasapiError) -> OutputError {
    use ::wasapi::WasapiError as W;
    match e {
        W::Windows(e) | W::RegisterNotifications(e) => map_hresult(e.code()),
        // A missing ID, or an ID that names a capture endpoint: no such render device.
        W::DeviceNotFound(_) | W::RenderToCaptureDevice => OutputError::DeviceNotFound,
        W::UnsupportedFormat => OutputError::UnsupportedFormat,
        W::EventTimeout => OutputError::Stalled,
        _ => OutputError::Os(codes::E_FAIL),
    }
}

/// The error for an event-wait timeout, given the result of a follow-up query on the client.
///
/// An unplugged endpoint in exclusive event mode usually just stops signalling its event, so
/// the timeout alone can't tell "unplugged" from "stalled". If querying the client then fails
/// (`AUDCLNT_E_DEVICE_INVALIDATED` → `Invalidated`), that error wins; otherwise `Stalled`.
pub(crate) fn classify_timeout(query: Result<(), OutputError>) -> OutputError {
    match query {
        Ok(()) => OutputError::Stalled,
        Err(e) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::codes::*;
    use super::*;

    #[test]
    fn timeout_with_a_healthy_client_is_stalled() {
        assert!(matches!(classify_timeout(Ok(())), OutputError::Stalled));
    }

    #[test]
    fn timeout_with_a_failing_client_reports_the_query_error() {
        assert!(matches!(
            classify_timeout(Err(map_code(AUDCLNT_E_DEVICE_INVALIDATED))),
            OutputError::Invalidated
        ));
        assert!(matches!(
            classify_timeout(Err(OutputError::Os(E_FAIL))),
            OutputError::Os(c) if c == E_FAIL
        ));
    }

    #[test]
    fn table_maps_each_code() {
        assert!(matches!(
            map_code(AUDCLNT_E_DEVICE_IN_USE),
            OutputError::DeviceInUse
        ));
        assert!(matches!(
            map_code(AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED),
            OutputError::ExclusiveModeNotAllowed
        ));
        assert!(matches!(
            map_code(AUDCLNT_E_UNSUPPORTED_FORMAT),
            OutputError::UnsupportedFormat
        ));
        assert!(matches!(
            map_code(AUDCLNT_E_DEVICE_INVALIDATED),
            OutputError::Invalidated
        ));
        assert!(matches!(map_code(E_NOTFOUND), OutputError::DeviceNotFound));
        // AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED is not special here: the stream handles it.
        assert!(matches!(
            map_code(0x8889_0019_u32 as i32),
            OutputError::Os(c) if c == 0x8889_0019_u32 as i32
        ));
        assert!(matches!(map_code(E_FAIL), OutputError::Os(c) if c == E_FAIL));
    }

    #[test]
    fn codes_are_the_documented_values() {
        assert_eq!(AUDCLNT_E_DEVICE_INVALIDATED as u32, 0x8889_0004);
        assert_eq!(AUDCLNT_E_UNSUPPORTED_FORMAT as u32, 0x8889_0008);
        assert_eq!(AUDCLNT_E_DEVICE_IN_USE as u32, 0x8889_000A);
        assert_eq!(AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED as u32, 0x8889_000E);
        assert_eq!(E_NOTFOUND as u32, 0x8007_0490);
    }

    #[cfg(windows)]
    mod windows_constants {
        use ::wasapi::WasapiError;
        use windows::Win32::Foundation::ERROR_NOT_FOUND;
        use windows::Win32::Media::Audio::{
            AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_DEVICE_INVALIDATED,
            AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED, AUDCLNT_E_UNSUPPORTED_FORMAT,
        };
        use windows::core::HRESULT;

        use super::super::*;

        #[test]
        fn map_hresult_device_in_use() {
            assert!(matches!(
                map_hresult(AUDCLNT_E_DEVICE_IN_USE),
                OutputError::DeviceInUse
            ));
        }

        #[test]
        fn map_hresult_exclusive_mode_not_allowed() {
            assert!(matches!(
                map_hresult(AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED),
                OutputError::ExclusiveModeNotAllowed
            ));
        }

        #[test]
        fn map_hresult_unsupported_format() {
            assert!(matches!(
                map_hresult(AUDCLNT_E_UNSUPPORTED_FORMAT),
                OutputError::UnsupportedFormat
            ));
        }

        #[test]
        fn map_hresult_device_invalidated() {
            assert!(matches!(
                map_hresult(AUDCLNT_E_DEVICE_INVALIDATED),
                OutputError::Invalidated
            ));
        }

        #[test]
        fn map_hresult_not_found() {
            assert!(matches!(
                map_hresult(ERROR_NOT_FOUND.to_hresult()),
                OutputError::DeviceNotFound
            ));
        }

        #[test]
        fn map_hresult_other_is_os() {
            let code = HRESULT(0x8000_4005_u32 as i32);
            assert!(matches!(map_hresult(code), OutputError::Os(c) if c == code.0));
        }

        #[test]
        fn map_wasapi_variants() {
            let e = WasapiError::Windows(windows::core::Error::from(AUDCLNT_E_DEVICE_IN_USE));
            assert!(matches!(map_wasapi(e), OutputError::DeviceInUse));
            assert!(matches!(
                map_wasapi(WasapiError::DeviceNotFound("x".into())),
                OutputError::DeviceNotFound
            ));
            assert!(matches!(
                map_wasapi(WasapiError::RenderToCaptureDevice),
                OutputError::DeviceNotFound
            ));
            assert!(matches!(
                map_wasapi(WasapiError::EventTimeout),
                OutputError::Stalled
            ));
            assert!(matches!(
                map_wasapi(WasapiError::UnsupportedFormat),
                OutputError::UnsupportedFormat
            ));
            assert!(matches!(
                map_wasapi(WasapiError::ClientNotInit),
                OutputError::Os(c) if c == codes::E_FAIL
            ));
        }
    }
}
