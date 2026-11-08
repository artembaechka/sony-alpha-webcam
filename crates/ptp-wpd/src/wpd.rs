// `GetCount` / `GetAt` write through pointers that the windows metadata
// declares as `*const`, so passing `&mut` is required despite clippy.
#![allow(clippy::unnecessary_mut_passed)]

use std::ptr;

use sony_ptp::{DataPhase, Response, Transport, TransportError};
use tracing::{debug, trace};
use windows::core::{HRESULT, PCWSTR, PWSTR};
use windows::Win32::Devices::PortableDevices::*;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::VT_UI4;

use crate::{parse_vid_pid, DeviceEntry};

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 1;
const FILE_SHARE_WRITE: u32 = 2;
const SECURITY_IMPERSONATION: u32 = 2 << 16;
const DEFAULT_CHUNK: u32 = 256 * 1024;

/// Keeps COM initialised (multithreaded apartment) for the current thread.
pub struct ComGuard {
    owned: bool,
}

impl ComGuard {
    pub fn new() -> Result<Self, TransportError> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // RPC_E_CHANGED_MODE: the thread already lives in an STA. WPD works
        // there too, we just must not uninitialise it.
        if hr.0 == 0x8001_0106u32 as i32 {
            return Ok(Self { owned: false });
        }
        hr.ok().map_err(|e| TransportError::Other(format!("CoInitializeEx: {e}")))?;
        Ok(Self { owned: true })
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owned {
            unsafe { CoUninitialize() };
        }
    }
}

fn other(ctx: &str, e: windows::core::Error) -> TransportError {
    classify(ctx, e.code())
}

/// Maps HRESULTs that mean "the device is gone" to `Disconnected`.
fn classify(ctx: &str, hr: HRESULT) -> TransportError {
    let msg = format!("{ctx}: {} (0x{:08X})", windows::core::Error::from_hresult(hr).message(), hr.0 as u32);
    const GONE: &[u32] = &[
        0x8007_048F, // ERROR_DEVICE_NOT_CONNECTED
        0x8007_001F, // ERROR_GEN_FAILURE
        0x8007_0015, // ERROR_NOT_READY
        0x8007_0006, // ERROR_INVALID_HANDLE
        0x8007_01B1, // ERROR_NO_SUCH_DEVICE
        0x8007_0651, // ERROR_DEVICE_REMOVED
        0x8007_0079, // ERROR_SEM_TIMEOUT
        0x8007_045D, // ERROR_IO_DEVICE
        0x8004_2003, // E_WPD_DEVICE_NOT_OPEN
        0x8004_2002, // E_WPD_DEVICE_ALREADY_OPENED (stale handle)
    ];
    if GONE.contains(&(hr.0 as u32)) {
        TransportError::Disconnected(msg)
    } else {
        TransportError::Other(msg)
    }
}

unsafe fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Calls one of the "query a string by device id" methods of the manager.
fn manager_string(
    f: impl Fn(PWSTR, *mut u32) -> windows::core::Result<()>,
) -> String {
    let mut len = 0u32;
    if f(PWSTR::null(), &mut len).is_err() || len == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize];
    if f(PWSTR(buf.as_mut_ptr()), &mut len).is_err() {
        return String::new();
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Lists every portable device Windows knows about (COM must be initialised).
pub fn list_devices() -> Result<Vec<DeviceEntry>, TransportError> {
    unsafe {
        let mgr: IPortableDeviceManager =
            CoCreateInstance(&PortableDeviceManager, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| other("create PortableDeviceManager", e))?;
        let _ = mgr.RefreshDeviceList();
        let mut count = 0u32;
        mgr.GetDevices(ptr::null_mut(), &mut count)
            .map_err(|e| other("GetDevices", e))?;
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut ids = vec![PWSTR::null(); count as usize];
        mgr.GetDevices(ids.as_mut_ptr(), &mut count)
            .map_err(|e| other("GetDevices", e))?;
        ids.truncate(count as usize);

        let mut out = Vec::with_capacity(ids.len());
        for p in ids {
            let id = take_pwstr(p);
            let wid = wide(&id);
            let pid = PCWSTR(wid.as_ptr());
            let friendly_name = manager_string(|b, l| mgr.GetDeviceFriendlyName(pid, b, l));
            let description = manager_string(|b, l| mgr.GetDeviceDescription(pid, b, l));
            let manufacturer = manager_string(|b, l| mgr.GetDeviceManufacturer(pid, b, l));
            let (vid, pidn) = parse_vid_pid(&id);
            out.push(DeviceEntry { id, friendly_name, description, manufacturer, vid, pid: pidn });
        }
        Ok(out)
    }
}

fn new_values() -> Result<IPortableDeviceValues, TransportError> {
    unsafe {
        CoCreateInstance(&PortableDeviceValues, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| other("create PortableDeviceValues", e))
    }
}

fn u32_variant(v: u32) -> PROPVARIANT {
    let mut pv = PROPVARIANT::default();
    unsafe {
        let inner = &mut *pv.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = v;
    }
    pv
}

/// Open WPD device used as a PTP transport.
pub struct WpdTransport {
    device: IPortableDevice,
    id: String,
}

// The device is created in the MTA with the free threaded marshaller, so it
// may be used from whichever MTA thread owns the transport.
unsafe impl Send for WpdTransport {}

impl WpdTransport {
    /// Opens a device by PnP id. COM must be initialised on this thread.
    pub fn open(id: &str) -> Result<Self, TransportError> {
        unsafe {
            let client = new_values()?;
            let name = wide("sony-remote");
            let set = |k: &PROPERTYKEY, v: u32| client.SetUnsignedIntegerValue(k, v);
            client
                .SetStringValue(&WPD_CLIENT_NAME, PCWSTR(name.as_ptr()))
                .and_then(|_| set(&WPD_CLIENT_MAJOR_VERSION, 0))
                .and_then(|_| set(&WPD_CLIENT_MINOR_VERSION, 1))
                .and_then(|_| set(&WPD_CLIENT_REVISION, 0))
                .and_then(|_| set(&WPD_CLIENT_SECURITY_QUALITY_OF_SERVICE, SECURITY_IMPERSONATION))
                .and_then(|_| set(&WPD_CLIENT_DESIRED_ACCESS, GENERIC_READ | GENERIC_WRITE))
                .and_then(|_| set(&WPD_CLIENT_SHARE_MODE, FILE_SHARE_READ | FILE_SHARE_WRITE))
                .map_err(|e| other("client info", e))?;

            let device: IPortableDevice =
                CoCreateInstance(&PortableDeviceFTM, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| other("create PortableDeviceFTM", e))?;
            let wid = wide(id);
            device
                .Open(PCWSTR(wid.as_ptr()), &client)
                .map_err(|e| other("open device", e))?;
            debug!(id, "WPD device opened");
            Ok(Self { device, id: id.to_owned() })
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn command(&self, cmd: &PROPERTYKEY) -> Result<IPortableDeviceValues, TransportError> {
        let v = new_values()?;
        unsafe {
            v.SetGuidValue(&WPD_PROPERTY_COMMON_COMMAND_CATEGORY, &cmd.fmtid)
                .and_then(|_| v.SetUnsignedIntegerValue(&WPD_PROPERTY_COMMON_COMMAND_ID, cmd.pid))
                .map_err(|e| other("command params", e))?;
        }
        Ok(v)
    }

    fn send(&self, ctx: &str, params: &IPortableDeviceValues) -> Result<IPortableDeviceValues, TransportError> {
        unsafe {
            let res = self.device.SendCommand(0, params).map_err(|e| other(ctx, e))?;
            if let Ok(hr) = res.GetErrorValue(&WPD_PROPERTY_COMMON_HRESULT) {
                if hr.is_err() {
                    return Err(classify(ctx, hr));
                }
            }
            Ok(res)
        }
    }

    fn set_operation(&self, v: &IPortableDeviceValues, op: u16, params: &[u32]) -> Result<(), TransportError> {
        unsafe {
            v.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_OPERATION_CODE, op as u32)
                .map_err(|e| other("operation code", e))?;
            let coll: IPortableDevicePropVariantCollection =
                CoCreateInstance(&PortableDevicePropVariantCollection, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| other("create PropVariantCollection", e))?;
            for &p in params {
                let mut pv = u32_variant(p);
                coll.Add(&pv).map_err(|e| other("add param", e))?;
                let _ = PropVariantClear(&mut pv);
            }
            v.SetIPortableDevicePropVariantCollectionValue(&WPD_PROPERTY_MTP_EXT_OPERATION_PARAMS, &coll)
                .map_err(|e| other("operation params", e))?;
        }
        Ok(())
    }

    fn read_response(res: &IPortableDeviceValues) -> Result<(u16, Vec<u32>), TransportError> {
        unsafe {
            let code = res
                .GetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_RESPONSE_CODE)
                .map_err(|e| other("response code", e))? as u16;
            let mut params = Vec::new();
            if let Ok(coll) = res.GetIPortableDevicePropVariantCollectionValue(&WPD_PROPERTY_MTP_EXT_RESPONSE_PARAMS) {
                let mut n = 0u32;
                let _ = coll.GetCount(&mut n);
                for i in 0..n {
                    let mut pv = PROPVARIANT::default();
                    if coll.GetAt(i, &mut pv).is_ok() {
                        let inner = &*pv.Anonymous.Anonymous;
                        params.push(inner.Anonymous.ulVal);
                        let _ = PropVariantClear(&mut pv);
                    }
                }
            }
            Ok((code, params))
        }
    }

    fn context(res: &IPortableDeviceValues) -> Result<Vec<u16>, TransportError> {
        unsafe {
            let p = res
                .GetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT)
                .map_err(|e| other("transfer context", e))?;
            Ok(wide(&take_pwstr(p)))
        }
    }

    fn end_transfer(&self, ctx: &[u16]) -> Result<(u16, Vec<u32>), TransportError> {
        let v = self.command(&WPD_COMMAND_MTP_EXT_END_DATA_TRANSFER)?;
        unsafe {
            v.SetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT, PCWSTR(ctx.as_ptr()))
                .map_err(|e| other("transfer context", e))?;
        }
        let res = self.send("end data transfer", &v)?;
        Self::read_response(&res)
    }

    fn exec_no_data(&self, op: u16, params: &[u32]) -> Result<Response, TransportError> {
        let v = self.command(&WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITHOUT_DATA_PHASE)?;
        self.set_operation(&v, op, params)?;
        let res = self.send("execute command", &v)?;
        let (code, params) = Self::read_response(&res)?;
        Ok(Response { code, params, data: Vec::new() })
    }

    fn exec_read(&self, op: u16, params: &[u32]) -> Result<Response, TransportError> {
        let v = self.command(&WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_READ)?;
        self.set_operation(&v, op, params)?;
        let res = self.send("execute command (read)", &v)?;
        let ctx = Self::context(&res)?;
        let total = unsafe { res.GetUnsignedLargeIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_TOTAL_DATA_SIZE) }
            .unwrap_or(0) as usize;
        let chunk = unsafe { res.GetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_OPTIMAL_TRANSFER_BUFFER_SIZE) }
            .unwrap_or(DEFAULT_CHUNK)
            .clamp(4096, 4 * 1024 * 1024);
        trace!(op = format_args!("0x{op:04X}"), total, chunk, "reading data phase");

        let mut data = Vec::with_capacity(total);
        let result = (|| {
            while data.len() < total {
                let want = (total - data.len()).min(chunk as usize) as u32;
                let rv = self.command(&WPD_COMMAND_MTP_EXT_READ_DATA)?;
                let scratch = vec![0u8; want as usize];
                unsafe {
                    rv.SetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT, PCWSTR(ctx.as_ptr()))
                        .and_then(|_| rv.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_NUM_BYTES_TO_READ, want))
                        .and_then(|_| rv.SetBufferValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, &scratch))
                        .map_err(|e| other("read params", e))?;
                }
                let rr = self.send("read data", &rv)?;
                let got = unsafe {
                    let mut p: *mut u8 = ptr::null_mut();
                    let mut len = 0u32;
                    rr.GetBufferValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, &mut p, &mut len)
                        .map_err(|e| other("read buffer", e))?;
                    let read = rr
                        .GetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_NUM_BYTES_READ)
                        .unwrap_or(len)
                        .min(len);
                    if !p.is_null() {
                        data.extend_from_slice(std::slice::from_raw_parts(p, read as usize));
                        CoTaskMemFree(Some(p as *const _));
                    }
                    read
                };
                if got == 0 {
                    break;
                }
            }
            Ok::<(), TransportError>(())
        })();
        // Always finish the transaction, otherwise the driver stays stuck.
        let end = self.end_transfer(&ctx);
        result?;
        let (code, params) = end?;
        Ok(Response { code, params, data })
    }

    fn exec_write(&self, op: u16, params: &[u32], payload: &[u8]) -> Result<Response, TransportError> {
        let v = self.command(&WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_WRITE)?;
        self.set_operation(&v, op, params)?;
        unsafe {
            v.SetUnsignedLargeIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_TOTAL_DATA_SIZE, payload.len() as u64)
                .map_err(|e| other("total size", e))?;
        }
        let res = self.send("execute command (write)", &v)?;
        let ctx = Self::context(&res)?;
        let result = (|| {
            let wv = self.command(&WPD_COMMAND_MTP_EXT_WRITE_DATA)?;
            unsafe {
                wv.SetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT, PCWSTR(ctx.as_ptr()))
                    .and_then(|_| wv.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_NUM_BYTES_TO_WRITE, payload.len() as u32))
                    .and_then(|_| wv.SetBufferValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, payload))
                    .map_err(|e| other("write params", e))?;
            }
            self.send("write data", &wv)?;
            Ok::<(), TransportError>(())
        })();
        let end = self.end_transfer(&ctx);
        result?;
        let (code, params) = end?;
        Ok(Response { code, params, data: Vec::new() })
    }

    /// Vendor operation codes the driver is willing to forward.
    pub fn supported_vendor_opcodes(&self) -> Result<Vec<u32>, TransportError> {
        let v = self.command(&WPD_COMMAND_MTP_EXT_GET_SUPPORTED_VENDOR_OPCODES)?;
        let res = self.send("get vendor opcodes", &v)?;
        let mut out = Vec::new();
        unsafe {
            let coll = res
                .GetIPortableDevicePropVariantCollectionValue(&WPD_PROPERTY_MTP_EXT_VENDOR_OPERATION_CODES)
                .map_err(|e| other("vendor opcodes", e))?;
            let mut n = 0u32;
            let _ = coll.GetCount(&mut n);
            for i in 0..n {
                let mut pv = PROPVARIANT::default();
                if coll.GetAt(i, &mut pv).is_ok() {
                    out.push((*pv.Anonymous.Anonymous).Anonymous.ulVal);
                    let _ = PropVariantClear(&mut pv);
                }
            }
        }
        Ok(out)
    }
}

impl Transport for WpdTransport {
    fn execute(&mut self, op: u16, params: &[u32], phase: DataPhase<'_>) -> Result<Response, TransportError> {
        let r = match phase {
            DataPhase::None => self.exec_no_data(op, params),
            DataPhase::In => self.exec_read(op, params),
            DataPhase::Out(d) => self.exec_write(op, params, d),
        };
        if let Err(e) = &r {
            debug!(op = format_args!("0x{op:04X}"), "WPD transaction failed: {e}");
        }
        r
    }
}

impl Drop for WpdTransport {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.Close();
        }
    }
}
