//! Debug logging through `OutputDebugString` (view with Sysinternals
//! DebugView: the DLL lives inside a service, so there is no console).

pub fn write(msg: &str) {
    let line: Vec<u16> = format!("[sony-remote-vcam] {msg}\n").encode_utf16().chain(Some(0)).collect();
    unsafe {
        windows::Win32::System::Diagnostics::Debug::OutputDebugStringW(windows::core::PCWSTR(line.as_ptr()));
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}
