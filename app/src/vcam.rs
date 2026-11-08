//! Virtual webcam: frame writer and (un)installation.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use vcam_shared::convert::Converter;
use vcam_shared::shm::SharedFrames;
use vcam_shared::{registry, CLSID_STR, DLL_NAME, FRIENDLY_NAME, HEIGHT, WIDTH};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, MFCreateVirtualCamera, MFShutdown, MFStartup, MFVirtualCameraAccess_AllUsers,
    MFVirtualCameraLifetime_System, MFVirtualCameraType_SoftwareCameraSource, MFSTARTUP_LITE, MF_VERSION,
};

use crate::worker::{Frame, FrameListener};

/// Where the DLL is installed: the Frame Server service (LocalService)
/// cannot read files inside user profiles.
fn install_dir() -> PathBuf {
    let base = std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .unwrap_or_else(|| "C:\\Program Files".into());
    PathBuf::from(base).join("sony-remote")
}

fn bundled_dll() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dll = exe.parent().context("exe has no parent dir")?.join(DLL_NAME);
    if !dll.exists() {
        bail!("{} not found next to the executable", dll.display());
    }
    Ok(dll)
}

/// State shown in the UI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VcamState {
    NotInstalled,
    /// Installed, no application is watching it.
    Idle,
    /// An application is receiving our frames.
    Streaming,
}

pub struct VcamStatus(AtomicU8);

impl VcamStatus {
    pub fn get(&self) -> VcamState {
        match self.0.load(Ordering::Relaxed) {
            0 => VcamState::NotInstalled,
            1 => VcamState::Idle,
            _ => VcamState::Streaming,
        }
    }
    fn set(&self, s: VcamState) {
        self.0.store(s as u8, Ordering::Relaxed);
    }
    pub fn mark_installed(&self) {
        let _ = self.0.compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// Frame listener that feeds the virtual camera when someone is watching.
pub fn writer() -> (FrameListener, Arc<VcamStatus>) {
    let status = Arc::new(VcamStatus(AtomicU8::new(if is_installed() { 1 } else { 0 })));
    let st = status.clone();
    let state = Mutex::new((None::<SharedFrames>, Converter::new(), None::<Instant>));
    let listener: FrameListener = Box::new(move |frame: &Arc<Frame>| {
        let mut guard = state.lock().unwrap();
        let (shm, conv, last_try) = &mut *guard;
        if shm.is_none() {
            if last_try.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
                return;
            }
            *last_try = Some(Instant::now());
            *shm = SharedFrames::open().ok();
        }
        let Some(s) = shm.as_ref() else {
            if st.get() == VcamState::Streaming {
                st.set(VcamState::Idle);
            }
            return;
        };
        if !s.reader_active() {
            st.set(VcamState::Idle);
            // Re-open later: the DLL may have been unloaded and recreated.
            if last_try.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
                *shm = None;
            }
            return;
        }
        st.set(VcamState::Streaming);
        *last_try = Some(Instant::now());
        s.write(|dst| conv.rgba_to_nv12(&frame.rgba, frame.width, frame.height, dst, WIDTH as usize, HEIGHT as usize));
    });
    (listener, status)
}

pub fn is_installed() -> bool {
    registry::registered_path().is_some_and(|p| Path::new(&p).exists())
}

fn is_admin() -> bool {
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}

/// Re-runs this executable elevated with `args` and returns its report.
fn run_elevated(args: &str) -> Result<()> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let report = std::env::temp_dir().join(format!("sony-remote-vcam-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&report);
    let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
    let params = HSTRING::from(format!("{args} --report \"{}\"", report.display()));
    let verb = HSTRING::from("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    println!("Requesting administrator rights…");
    unsafe {
        ShellExecuteExW(&mut info).context("elevation was refused")?;
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        let text = std::fs::read_to_string(&report).unwrap_or_default();
        let _ = std::fs::remove_file(&report);
        print!("{text}");
        if code != 0 {
            bail!("elevated process failed (exit code {code})");
        }
    }
    Ok(())
}

fn with_report(report: Option<PathBuf>, f: impl FnOnce(&mut String) -> Result<()>) -> Result<()> {
    let mut out = String::new();
    let r = f(&mut out);
    if let Err(e) = &r {
        out.push_str(&format!("error: {e:#}\n"));
    }
    match report {
        Some(path) => std::fs::write(path, &out)?,
        None => print!("{out}"),
    }
    r
}

struct Mf;

impl Mf {
    fn start() -> Result<Self> {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE)? };
        Ok(Mf)
    }
}

impl Drop for Mf {
    fn drop(&mut self) {
        let _ = unsafe { MFShutdown() };
    }
}

fn create_camera() -> Result<windows::Win32::Media::MediaFoundation::IMFVirtualCamera> {
    unsafe {
        MFCreateVirtualCamera(
            MFVirtualCameraType_SoftwareCameraSource,
            MFVirtualCameraLifetime_System,
            MFVirtualCameraAccess_AllUsers,
            &HSTRING::from(FRIENDLY_NAME),
            &HSTRING::from(CLSID_STR),
            None,
        )
        .context("MFCreateVirtualCamera (needs Windows 11)")
    }
}

pub fn install(report: Option<PathBuf>) -> Result<()> {
    if !is_admin() && report.is_none() {
        bundled_dll()?;
        return run_elevated("vcam install");
    }
    with_report(report, |out| {
        let src = bundled_dll()?;
        let dir = install_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let dst = dir.join(DLL_NAME);
        let same = std::fs::read(&src).ok() == std::fs::read(&dst).ok();
        if !same {
            std::fs::copy(&src, &dst).with_context(|| {
                format!("copy to {} (close applications that use the virtual camera and retry)", dst.display())
            })?;
        }
        out.push_str(&format!("installed {}\n", dst.display()));
        registry::register(&dst.to_string_lossy()).context("register COM server")?;
        out.push_str(&format!("registered CLSID {CLSID_STR}\n"));
        let _mf = Mf::start()?;
        let cam = create_camera()?;
        unsafe {
            cam.Start(None::<&IMFAsyncCallback>).context("start virtual camera")?;
            let _ = cam.Shutdown();
        }
        out.push_str(&format!("virtual camera \"{FRIENDLY_NAME}\" is ready\n"));
        Ok(())
    })
}

pub fn uninstall(report: Option<PathBuf>) -> Result<()> {
    if !is_admin() && report.is_none() {
        return run_elevated("vcam uninstall");
    }
    with_report(report, |out| {
        {
            let _mf = Mf::start()?;
            match create_camera() {
                Ok(cam) => unsafe {
                    match cam.Remove() {
                        Ok(()) => out.push_str("virtual camera removed\n"),
                        Err(e) => out.push_str(&format!("remove virtual camera: {e}\n")),
                    }
                    let _ = cam.Shutdown();
                },
                Err(e) => out.push_str(&format!("{e:#}\n")),
            }
        }
        registry::unregister().context("unregister COM server")?;
        out.push_str("COM registration removed\n");
        let dll = install_dir().join(DLL_NAME);
        match std::fs::remove_file(&dll) {
            Ok(()) => {
                let _ = std::fs::remove_dir(install_dir());
                out.push_str(&format!("deleted {}\n", dll.display()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => out.push_str(&format!("could not delete {} ({e}); it is deleted on next reboot of the camera service\n", dll.display())),
        }
        Ok(())
    })
}

pub fn status() -> Result<()> {
    match registry::registered_path() {
        Some(p) => println!(
            "COM server: {p} ({})",
            if Path::new(&p).exists() { "present" } else { "MISSING" }
        ),
        None => println!("COM server: not registered"),
    }
    match SharedFrames::open() {
        Ok(s) => println!(
            "frame buffer: open, {} frames written, reader {}",
            s.header().frame_counter.load(Ordering::Relaxed),
            if s.reader_active() { "active" } else { "idle" }
        ),
        Err(_) => println!("frame buffer: not created (no application is using the virtual camera)"),
    }
    Ok(())
}

/// Loads the media source DLL in-process and pulls a few samples, to test
/// the COM plumbing without installing anything.
pub fn selftest() -> Result<()> {
    use windows::core::{Interface, GUID};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, IClassFactory, COINIT_MULTITHREADED};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    let dll = bundled_dll()?;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _mf = Mf::start()?;
        let module = LoadLibraryW(&HSTRING::from(dll.as_os_str()))?;
        let proc = GetProcAddress(module, windows::core::s!("DllGetClassObject")).context("no DllGetClassObject")?;
        type GetClassObject = unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut std::ffi::c_void) -> windows::core::HRESULT;
        let get: GetClassObject = std::mem::transmute(proc);
        let clsid = GUID::from_u128(vcam_shared::CLSID_U128);
        let mut factory = std::ptr::null_mut();
        get(&clsid, &IClassFactory::IID, &mut factory).ok()?;
        let factory = IClassFactory::from_raw(factory);
        let activate: IMFActivate = factory.CreateInstance(None)?;
        println!("IMFActivate created");
        let source: IMFMediaSourceEx = activate.ActivateObject()?;
        println!("media source: characteristics 0x{:X}", source.GetCharacteristics()?);
        let pd = source.CreatePresentationDescriptor()?;
        let mut selected = windows::core::BOOL(0);
        let mut sd = None;
        pd.GetStreamDescriptorByIndex(0, &mut selected, &mut sd)?;
        let sd = sd.context("no stream descriptor")?;
        let mt = sd.GetMediaTypeHandler()?.GetCurrentMediaType()?;
        let size = mt.GetUINT64(&MF_MT_FRAME_SIZE)?;
        println!("stream 0: {}x{} NV12, selected={}", size >> 32, size & 0xFFFF_FFFF, selected.as_bool());
        let attrs = source.GetStreamAttributes(0)?;
        println!("stream category is video capture: {}", attrs.GetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY)? == windows::Win32::Media::KernelStreaming::PINNAME_VIDEO_CAPTURE);

        source.Start(&pd, &GUID::zeroed(), std::ptr::null())?;
        let mut stream: Option<IMFMediaStream> = None;
        let start = Instant::now();
        let mut samples = 0;
        while samples < 15 && start.elapsed() < Duration::from_secs(5) {
            let ev = source.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(1)); // MF_EVENT_FLAG_NO_WAIT
            if let Ok(ev) = ev {
                let t = ev.GetType()?;
                println!("source event {t}");
                if t == MENewStream.0 as u32 {
                    let v = ev.GetValue()?;
                    let unk = windows::core::IUnknown::try_from(&v).ok();
                    stream = unk.and_then(|u| u.cast().ok());
                    if let Some(s) = &stream {
                        for _ in 0..15 {
                            s.RequestSample(None)?;
                        }
                    }
                }
            }
            if let Some(s) = &stream {
                while let Ok(ev) = s.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(1)) {
                    if ev.GetType()? == MEMediaSample.0 as u32 {
                        samples += 1;
                    } else {
                        println!("stream event {}", ev.GetType()?);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let secs = start.elapsed().as_secs_f64();
        println!("{samples} samples in {secs:.2}s ({:.1} fps)", samples as f64 / secs);
        source.Stop()?;
        source.Shutdown()?;
        activate.ShutdownObject()?;
        if samples == 0 {
            bail!("no samples delivered");
        }
    }
    Ok(())
}

/// Reads frames from the virtual camera like a video call app would, and
/// saves the last one as a BMP. Developer check of the whole pipeline.
pub fn consume(frames: usize, bmp: Option<PathBuf>) -> Result<()> {
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _mf = Mf::start()?;
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.context("attributes")?;
        attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
        let mut list = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&attrs, &mut list, &mut count)?;
        let devices = std::slice::from_raw_parts(list, count as usize);
        let mut chosen = None;
        for d in devices.iter().flatten() {
            let mut name = windows::core::PWSTR::null();
            let mut len = 0;
            d.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut name, &mut len)?;
            let n = name.to_string().unwrap_or_default();
            CoTaskMemFree(Some(name.0 as *const _));
            println!("capture device: {n}");
            if n.starts_with(FRIENDLY_NAME) {
                chosen = Some(d.clone());
            }
        }
        CoTaskMemFree(Some(list as *const _));
        let act = chosen.with_context(|| format!("\"{FRIENDLY_NAME}\" is not listed"))?;
        let source: IMFMediaSource = act.ActivateObject()?;
        let reader = MFCreateSourceReaderFromMediaSource(&source, None)?;
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let start = Instant::now();
        let mut got = 0;
        let mut last = Vec::new();
        while got < frames && start.elapsed() < Duration::from_secs(20) {
            let mut flags = 0u32;
            let mut sample = None;
            reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))?;
            if let Some(s) = sample {
                got += 1;
                let buf = s.ConvertToContiguousBuffer()?;
                let mut p = std::ptr::null_mut();
                let mut n = 0u32;
                buf.Lock(&mut p, None, Some(&mut n))?;
                last = std::slice::from_raw_parts(p, n as usize).to_vec();
                buf.Unlock()?;
            }
        }
        let secs = start.elapsed().as_secs_f64();
        println!("{got} frames in {secs:.2}s ({:.1} fps), {} bytes each", got as f64 / secs, last.len());
        let _ = source.Shutdown();
        if let (Some(path), true) = (bmp, last.len() >= vcam_shared::FRAME_BYTES) {
            write_bmp(&path, &last, WIDTH as usize, HEIGHT as usize)?;
            println!("last frame saved to {}", path.display());
        }
        if got == 0 {
            bail!("no frames received");
        }
    }
    Ok(())
}

/// NV12 → 24-bit BMP (for eyeballing the output).
fn write_bmp(path: &Path, nv12: &[u8], w: usize, h: usize) -> Result<()> {
    let row = (w * 3).div_ceil(4) * 4;
    let mut out = Vec::with_capacity(54 + row * h);
    let size = (54 + row * h) as u32;
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for y in (0..h).rev() {
        for x in 0..w {
            let yy = nv12[y * w + x] as f32 - 16.0;
            let uv = w * h + (y / 2) * w + (x & !1);
            let u = nv12[uv] as f32 - 128.0;
            let v = nv12[uv + 1] as f32 - 128.0;
            let r = 1.164 * yy + 1.793 * v;
            let g = 1.164 * yy - 0.213 * u - 0.533 * v;
            let b = 1.164 * yy + 2.112 * u;
            out.extend_from_slice(&[b.clamp(0.0, 255.0) as u8, g.clamp(0.0, 255.0) as u8, r.clamp(0.0, 255.0) as u8]);
        }
        out.extend(std::iter::repeat_n(0, row - w * 3));
    }
    std::fs::write(path, out)?;
    Ok(())
}
