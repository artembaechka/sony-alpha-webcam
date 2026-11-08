//! Media Foundation virtual camera source for sony-remote.
//!
//! Registered with `MFCreateVirtualCamera`, the Windows Camera Frame Server
//! loads this DLL, asks [`activate::Activate`] for an `IMFMediaSource`, and
//! pulls NV12 frames from it. Frames come from the sony-remote application
//! through the shared memory defined in `vcam-shared`.

#![allow(non_snake_case)]

mod activate;
mod log;
mod source;
mod stream;

use std::ffi::c_void;

use windows::core::{implement, Interface, Ref, BOOL, GUID, HRESULT};
use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, S_FALSE, S_OK};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};

pub const CLSID: GUID = GUID::from_u128(vcam_shared::CLSID_U128);

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<windows::core::IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if ppv.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *ppv = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let activate = activate::Activate::create()?;
        unsafe { activate.query(riid, ppv).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllGetClassObject(rclsid: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    if ppv.is_null() || rclsid.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = std::ptr::null_mut() };
    if unsafe { *rclsid } != CLSID {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    log!("DllGetClassObject");
    let factory: IClassFactory = Factory.into();
    unsafe { factory.query(riid, ppv) }
}

#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    // Keep the module loaded; the Frame Server unloads us with the process.
    S_FALSE
}

#[unsafe(no_mangle)]
extern "system" fn DllRegisterServer() -> HRESULT {
    match own_path().and_then(|p| vcam_shared::registry::register(&p).map_err(|e| e.code())) {
        Ok(()) => S_OK,
        Err(hr) => hr,
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllUnregisterServer() -> HRESULT {
    match vcam_shared::registry::unregister() {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

/// Full path of this DLL.
fn own_path() -> Result<String, HRESULT> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };
    unsafe {
        let mut module = HMODULE::default();
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            windows::core::PCWSTR(DllRegisterServer as *const u16),
            &mut module,
        )
        .map_err(|e| e.code())?;
        let mut buf = vec![0u16; 1024];
        let n = GetModuleFileNameW(Some(module), &mut buf) as usize;
        Ok(String::from_utf16_lossy(&buf[..n]))
    }
}
