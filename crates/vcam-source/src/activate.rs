//! `IMFActivate` object that the Frame Server creates from our CLSID.

use std::ffi::c_void;
use std::sync::Mutex;

use windows::core::{implement, ComObject, IUnknown, Interface, Ref, Result, BOOL, GUID, PCWSTR, PWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;

use crate::log;
use crate::source::MediaSource;

#[implement(IMFActivate)]
pub struct Activate {
    attrs: IMFAttributes,
    source: Mutex<Option<ComObject<MediaSource>>>,
}

impl Activate {
    pub fn create() -> Result<IMFActivate> {
        let mut attrs = None;
        unsafe { MFCreateAttributes(&mut attrs, 4)? };
        let attrs = attrs.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_OUTOFMEMORY))?;
        Ok(Activate { attrs, source: Mutex::new(None) }.into())
    }
}

impl IMFActivate_Impl for Activate_Impl {
    fn ActivateObject(&self, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        log!("ActivateObject");
        let mut guard = self.source.lock().unwrap();
        let source = match guard.as_ref() {
            Some(s) => s.clone(),
            None => {
                let s = MediaSource::create().inspect_err(|e| log!("MediaSource::create failed: {e}"))?;
                *guard = Some(s.clone());
                s
            }
        };
        let unk: IUnknown = source.to_interface();
        unsafe { unk.query(riid, ppv).ok() }
    }

    fn ShutdownObject(&self) -> Result<()> {
        log!("ShutdownObject");
        if let Some(s) = self.source.lock().unwrap().take() {
            s.shutdown_all();
        }
        Ok(())
    }

    fn DetachObject(&self) -> Result<()> {
        self.source.lock().unwrap().take();
        Ok(())
    }
}

/// Everything else is a plain attribute store.
impl IMFAttributes_Impl for Activate_Impl {
    fn GetItem(&self, key: *const GUID, value: *mut PROPVARIANT) -> Result<()> {
        unsafe { self.attrs.GetItem(key, (!value.is_null()).then_some(value)) }
    }
    fn GetItemType(&self, key: *const GUID) -> Result<MF_ATTRIBUTE_TYPE> {
        unsafe { self.attrs.GetItemType(key) }
    }
    fn CompareItem(&self, key: *const GUID, value: *const PROPVARIANT) -> Result<BOOL> {
        unsafe { self.attrs.CompareItem(key, value) }
    }
    fn Compare(&self, theirs: Ref<IMFAttributes>, match_type: MF_ATTRIBUTES_MATCH_TYPE) -> Result<BOOL> {
        unsafe { self.attrs.Compare(theirs.as_ref(), match_type) }
    }
    fn GetUINT32(&self, key: *const GUID) -> Result<u32> {
        unsafe { self.attrs.GetUINT32(key) }
    }
    fn GetUINT64(&self, key: *const GUID) -> Result<u64> {
        unsafe { self.attrs.GetUINT64(key) }
    }
    fn GetDouble(&self, key: *const GUID) -> Result<f64> {
        unsafe { self.attrs.GetDouble(key) }
    }
    fn GetGUID(&self, key: *const GUID) -> Result<GUID> {
        unsafe { self.attrs.GetGUID(key) }
    }
    fn GetStringLength(&self, key: *const GUID) -> Result<u32> {
        unsafe { self.attrs.GetStringLength(key) }
    }
    fn GetString(&self, key: *const GUID, value: PWSTR, size: u32, len: *mut u32) -> Result<()> {
        unsafe {
            (Interface::vtable(&self.attrs).GetString)(Interface::as_raw(&self.attrs), key, value, size, len).ok()
        }
    }
    fn GetAllocatedString(&self, key: *const GUID, value: *mut PWSTR, len: *mut u32) -> Result<()> {
        unsafe { self.attrs.GetAllocatedString(key, value, len) }
    }
    fn GetBlobSize(&self, key: *const GUID) -> Result<u32> {
        unsafe { self.attrs.GetBlobSize(key) }
    }
    fn GetBlob(&self, key: *const GUID, buf: *mut u8, size: u32, len: *mut u32) -> Result<()> {
        unsafe { (Interface::vtable(&self.attrs).GetBlob)(Interface::as_raw(&self.attrs), key, buf, size, len).ok() }
    }
    fn GetAllocatedBlob(&self, key: *const GUID, buf: *mut *mut u8, size: *mut u32) -> Result<()> {
        unsafe { self.attrs.GetAllocatedBlob(key, buf, size) }
    }
    fn GetUnknown(&self, key: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        unsafe { (Interface::vtable(&self.attrs).GetUnknown)(Interface::as_raw(&self.attrs), key, riid, ppv).ok() }
    }
    fn SetItem(&self, key: *const GUID, value: *const PROPVARIANT) -> Result<()> {
        unsafe { self.attrs.SetItem(key, value) }
    }
    fn DeleteItem(&self, key: *const GUID) -> Result<()> {
        unsafe { self.attrs.DeleteItem(key) }
    }
    fn DeleteAllItems(&self) -> Result<()> {
        unsafe { self.attrs.DeleteAllItems() }
    }
    fn SetUINT32(&self, key: *const GUID, value: u32) -> Result<()> {
        unsafe { self.attrs.SetUINT32(key, value) }
    }
    fn SetUINT64(&self, key: *const GUID, value: u64) -> Result<()> {
        unsafe { self.attrs.SetUINT64(key, value) }
    }
    fn SetDouble(&self, key: *const GUID, value: f64) -> Result<()> {
        unsafe { self.attrs.SetDouble(key, value) }
    }
    fn SetGUID(&self, key: *const GUID, value: *const GUID) -> Result<()> {
        unsafe { self.attrs.SetGUID(key, value) }
    }
    fn SetString(&self, key: *const GUID, value: &PCWSTR) -> Result<()> {
        unsafe { self.attrs.SetString(key, *value) }
    }
    fn SetBlob(&self, key: *const GUID, buf: *const u8, size: u32) -> Result<()> {
        let slice = if buf.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(buf, size as usize) } };
        unsafe { self.attrs.SetBlob(key, slice) }
    }
    fn SetUnknown(&self, key: *const GUID, unknown: Ref<IUnknown>) -> Result<()> {
        unsafe { self.attrs.SetUnknown(key, unknown.as_ref()) }
    }
    fn LockStore(&self) -> Result<()> {
        unsafe { self.attrs.LockStore() }
    }
    fn UnlockStore(&self) -> Result<()> {
        unsafe { self.attrs.UnlockStore() }
    }
    fn GetCount(&self) -> Result<u32> {
        unsafe { self.attrs.GetCount() }
    }
    fn GetItemByIndex(&self, index: u32, key: *mut GUID, value: *mut PROPVARIANT) -> Result<()> {
        unsafe { self.attrs.GetItemByIndex(index, key, (!value.is_null()).then_some(value)) }
    }
    fn CopyAllItems(&self, dest: Ref<IMFAttributes>) -> Result<()> {
        unsafe { self.attrs.CopyAllItems(dest.as_ref()) }
    }
}
