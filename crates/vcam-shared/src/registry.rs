//! COM registration of the media source (HKLM, because the Frame Server
//! service does not see per-user classes).

use windows::core::{Error, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

use crate::{CLSID_STR, FRIENDLY_NAME};

fn key_path() -> String {
    format!("SOFTWARE\\Classes\\CLSID\\{CLSID_STR}")
}

fn check(e: WIN32_ERROR) -> Result<()> {
    if e.0 == 0 {
        Ok(())
    } else {
        Err(Error::from_hresult(e.to_hresult()))
    }
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn create(path: &str) -> Result<Key> {
    let mut key = HKEY::default();
    unsafe {
        check(RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        ))?;
    }
    Ok(Key(key))
}

fn set_string(key: &Key, name: Option<&str>, value: &str) -> Result<()> {
    let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
    let name = name.map(HSTRING::from);
    let name_ptr = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
    unsafe { check(RegSetValueExW(key.0, name_ptr, None, REG_SZ, Some(bytes))) }
}

/// Registers `dll_path` as the in-process server for the media source CLSID.
pub fn register(dll_path: &str) -> Result<()> {
    let root = create(&key_path())?;
    set_string(&root, None, FRIENDLY_NAME)?;
    let inproc = create(&format!("{}\\InprocServer32", key_path()))?;
    set_string(&inproc, None, dll_path)?;
    set_string(&inproc, Some("ThreadingModel"), "Both")?;
    Ok(())
}

pub fn unregister() -> Result<()> {
    let e = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(key_path())) };
    if e == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(e)
}

/// Path of the registered DLL, if any.
pub fn registered_path() -> Option<String> {
    let mut key = HKEY::default();
    unsafe {
        let path = HSTRING::from(format!("{}\\InprocServer32", key_path()));
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, &path, None, KEY_READ, &mut key).0 != 0 {
            return None;
        }
        let key = Key(key);
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        if RegQueryValueExW(key.0, PCWSTR::null(), None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut len)).0 != 0 {
            return None;
        }
        let n = (len as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..n]);
        Some(s.trim_end_matches('\0').to_owned())
    }
}
