//! The `Global\` file mapping that carries frames from the app to the DLL.

use std::ptr;
use std::sync::atomic::{fence, Ordering};

use windows::core::{Error, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_ALL_ACCESS, FILE_MAP_WRITE,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows::Win32::System::SystemInformation::GetTickCount64;

use crate::{Header, FRAME_BYTES, HEADER_BYTES, HEIGHT, MAGIC, MAPPING_BYTES, MAPPING_NAME, SLOTS, VERSION, WIDTH};

/// SYSTEM, LocalService, administrators and interactive users get full
/// access; the low mandatory label lets medium-integrity apps write even
/// though the creator runs at system integrity.
const SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GA;;;IU)(A;;GA;;;AU)S:(ML;;NW;;;LW)";

pub struct SharedFrames {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// The mapping is process-wide memory; all accesses go through atomics or the
// seqlock protocol below.
unsafe impl Send for SharedFrames {}
unsafe impl Sync for SharedFrames {}

impl SharedFrames {
    /// Creates (or opens an existing) mapping. Used by the DLL, which runs
    /// with the privilege to create global objects.
    pub fn create() -> Result<Self> {
        unsafe {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(SDDL), SDDL_REVISION_1, &mut sd, None)?;
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            };
            let name = HSTRING::from(MAPPING_NAME);
            let handle = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(&sa),
                PAGE_READWRITE,
                0,
                MAPPING_BYTES as u32,
                PCWSTR(name.as_ptr()),
            );
            let existed = GetLastError() == ERROR_ALREADY_EXISTS;
            let _ = LocalFree(Some(HLOCAL(sd.0)));
            let handle = handle?;
            let me = Self::map(handle, FILE_MAP_ALL_ACCESS.0)?;
            let h = me.header();
            if !existed || h.magic.load(Ordering::Acquire) != MAGIC {
                h.width.store(WIDTH, Ordering::Relaxed);
                h.height.store(HEIGHT, Ordering::Relaxed);
                h.version.store(VERSION, Ordering::Relaxed);
                h.latest.store(0, Ordering::Relaxed);
                h.magic.store(MAGIC, Ordering::Release);
            }
            Ok(me)
        }
    }

    /// Opens the mapping created by the DLL. Fails while no application is
    /// using the virtual camera.
    pub fn open() -> Result<Self> {
        unsafe {
            let name = HSTRING::from(MAPPING_NAME);
            let handle = OpenFileMappingW(FILE_MAP_WRITE.0 | 0x0004, false, PCWSTR(name.as_ptr()))?;
            let me = Self::map(handle, FILE_MAP_WRITE.0 | 0x0004)?;
            let h = me.header();
            if h.magic.load(Ordering::Acquire) != MAGIC || h.version.load(Ordering::Relaxed) != VERSION {
                return Err(Error::new(windows::Win32::Foundation::E_UNEXPECTED, "virtual camera version mismatch"));
            }
            Ok(me)
        }
    }

    unsafe fn map(handle: HANDLE, access: u32) -> Result<Self> {
        let view = MapViewOfFile(handle, windows::Win32::System::Memory::FILE_MAP(access), 0, 0, MAPPING_BYTES);
        if view.Value.is_null() {
            let e = Error::from_thread();
            let _ = CloseHandle(handle);
            return Err(e);
        }
        Ok(Self { handle, view })
    }

    pub fn header(&self) -> &Header {
        unsafe { &*(self.view.Value as *const Header) }
    }

    fn slot_ptr(&self, slot: usize) -> *mut u8 {
        unsafe { (self.view.Value as *mut u8).add(HEADER_BYTES + slot * FRAME_BYTES) }
    }

    pub fn now() -> u64 {
        unsafe { GetTickCount64() }
    }

    /// True when the DLL delivered a sample recently, i.e. some application
    /// is watching the virtual camera.
    pub fn reader_active(&self) -> bool {
        Self::now().saturating_sub(self.header().reader_tick.load(Ordering::Relaxed)) < crate::READER_TIMEOUT_MS
    }

    /// Writes one NV12 frame produced by `fill`.
    pub fn write(&self, fill: impl FnOnce(&mut [u8])) {
        let h = self.header();
        let latest = h.latest.load(Ordering::Acquire) as usize;
        let slot = if latest == 0 { 0 } else { latest % SLOTS };
        let seq = &h.slot_seq[slot];
        seq.fetch_add(1, Ordering::AcqRel); // odd: writing
        fence(Ordering::Release);
        let buf = unsafe { std::slice::from_raw_parts_mut(self.slot_ptr(slot), FRAME_BYTES) };
        fill(buf);
        fence(Ordering::Release);
        seq.fetch_add(1, Ordering::AcqRel); // even: complete
        h.latest.store(slot as u32 + 1, Ordering::Release);
        h.frame_counter.fetch_add(1, Ordering::AcqRel);
        h.writer_tick.store(Self::now(), Ordering::Release);
    }

    /// Copies the newest complete frame into `dst`. Returns its counter, or
    /// `None` when there is no fresh frame from the application.
    pub fn read_latest(&self, dst: &mut [u8]) -> Option<u64> {
        let h = self.header();
        h.reader_tick.store(Self::now(), Ordering::Relaxed);
        let writer_age = Self::now().saturating_sub(h.writer_tick.load(Ordering::Acquire));
        if writer_age > crate::WRITER_TIMEOUT_MS {
            return None;
        }
        for _ in 0..3 {
            let latest = h.latest.load(Ordering::Acquire) as usize;
            if latest == 0 || latest > SLOTS {
                return None;
            }
            let slot = latest - 1;
            let s1 = h.slot_seq[slot].load(Ordering::Acquire);
            if !s1.is_multiple_of(2) {
                continue;
            }
            let counter = h.frame_counter.load(Ordering::Acquire);
            unsafe { ptr::copy_nonoverlapping(self.slot_ptr(slot), dst.as_mut_ptr(), FRAME_BYTES.min(dst.len())) };
            fence(Ordering::Acquire);
            if h.slot_seq[slot].load(Ordering::Acquire) == s1 {
                return Some(counter);
            }
        }
        None
    }
}

impl Drop for SharedFrames {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}
