//! One nil at a time: a second launch brings the running window forward and exits.
//! Two instances would share the library cache and preferences.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE,
    SetForegroundWindow, ShowWindow,
};
use windows::core::{BOOL, PWSTR, w};

/// The main window's title.
const TITLE: &str = "nil";

/// Claims the single-instance lock for this login session, held until the process
/// exits. Returns false if another nil holds it. If the lock can't be created at all,
/// nil starts anyway.
pub fn acquire() -> bool {
    // The handle is deliberately never closed: the lock lasts as long as the process.
    match unsafe { CreateMutexW(None, false, w!("Local\\nil-music-player")) } {
        Ok(_) => (unsafe { GetLastError() }) != ERROR_ALREADY_EXISTS,
        Err(_) => true,
    }
}

/// Restores and focuses the running nil's window, if one is found.
pub fn show_running() {
    let Some(exe) = std::env::current_exe().ok().and_then(|p| file_name(&p)) else {
        return;
    };
    let mut found = Found { exe, hwnd: None };
    // Stopping early makes EnumWindows report an error; that is expected.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut found as *mut Found as isize)) };
    if let Some(hwnd) = found.hwnd {
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

struct Found {
    /// This program's file name, lowercased.
    exe: String,
    hwnd: Option<HWND>,
}

/// Stops at the first visible window titled `TITLE` whose process runs the same
/// program file as this one.
unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = unsafe { &mut *(lparam.0 as *mut Found) };
    let matches = unsafe { IsWindowVisible(hwnd).as_bool() }
        && window_title(hwnd) == TITLE
        && process_file_name(hwnd).is_some_and(|name| name == found.exe);
    if matches {
        found.hwnd = Some(hwnd);
    }
    BOOL::from(!matches)
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 16];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// The lowercased program file name of the process owning `hwnd`, other than this one.
fn process_file_name(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
        return None;
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let path = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .ok()
    .map(|()| OsString::from_wide(&buf[..len as usize]));
    let _ = unsafe { CloseHandle(process) };
    file_name(Path::new(&path?))
}

fn file_name(path: &Path) -> Option<String> {
    Some(path.file_name()?.to_string_lossy().to_lowercase())
}
