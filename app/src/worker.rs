//! Background threads: the camera thread owns the USB session (and
//! reconnects whenever it breaks), the decoder thread turns JPEG frames into
//! RGBA images for the UI and the virtual webcam.

use std::collections::{BTreeMap, HashSet};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender, TrySendError};
use ptp_wpd::{list_devices, ComGuard, DeviceEntry, WpdTransport};
use sony_ptp::{PropDesc, SonyCamera, StepDirection};
use tracing::{debug, info, warn};
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// A decoded live view frame (RGBA, 8 bit).
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// No camera in PC Remote mode found.
    Searching { hint: Option<String> },
    /// A Sony camera is attached, but in a USB mode without remote control.
    WrongMode(String),
    Connecting(String),
    Connected(String),
    /// The last session failed; we retry shortly.
    Error(String),
}

/// Camera state as seen by the UI.
#[derive(Clone, Default)]
pub struct CameraState {
    pub props: BTreeMap<u16, PropDesc>,
    pub supported: HashSet<u16>,
}

pub enum Command {
    SetValue { code: u16, value: i64 },
    Step { code: u16, dir: StepDirection },
    HalfPress(bool),
    FocusStep(i16),
    Quit,
}

pub struct Shared {
    pub status: Mutex<Status>,
    pub camera: Mutex<Arc<CameraState>>,
    pub frame: Mutex<Option<Arc<Frame>>>,
    pub fps: Mutex<f32>,
    /// A command is being executed (stepping can take a moment).
    pub busy: AtomicBool,
    /// Last non-fatal error, shown to the user for a few seconds.
    pub notice: Mutex<Option<(Instant, String)>>,
}

impl Shared {
    fn new() -> Self {
        Self {
            status: Mutex::new(Status::Searching { hint: None }),
            camera: Mutex::new(Arc::new(CameraState::default())),
            frame: Mutex::new(None),
            fps: Mutex::new(0.0),
            busy: AtomicBool::new(false),
            notice: Mutex::new(None),
        }
    }

    fn set_status(&self, s: Status) {
        let mut cur = self.status.lock().unwrap();
        if *cur != s {
            info!("status: {s:?}");
            *cur = s;
        }
    }

    fn notify(&self, msg: String) {
        warn!("{msg}");
        *self.notice.lock().unwrap() = Some((Instant::now(), msg));
    }
}

/// Called on the decoder thread for every new frame.
pub type FrameListener = Box<dyn Fn(&Arc<Frame>) + Send>;

pub struct Worker {
    tx: Sender<Command>,
    pub shared: Arc<Shared>,
}

impl Worker {
    pub fn spawn(listeners: Vec<FrameListener>) -> Self {
        let shared = Arc::new(Shared::new());
        let (tx, rx) = crossbeam_channel::unbounded();
        let (jpeg_tx, jpeg_rx) = bounded::<Vec<u8>>(2);

        let s = shared.clone();
        thread::Builder::new()
            .name("decoder".into())
            .spawn(move || decoder_thread(jpeg_rx, s, listeners))
            .expect("spawn decoder thread");

        let s = shared.clone();
        thread::Builder::new()
            .name("camera".into())
            .spawn(move || camera_thread(rx, s, jpeg_tx))
            .expect("spawn camera thread");

        Self { tx, shared }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Quit);
    }
}

fn decode(jpeg: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut dec = JpegDecoder::new_with_options(Cursor::new(jpeg), opts);
    let pixels = dec.decode().map_err(|e| debug!("JPEG decode failed: {e:?}")).ok()?;
    let (w, h) = dec.dimensions()?;
    (pixels.len() == w * h * 4).then_some((w, h, pixels))
}

fn decoder_thread(rx: Receiver<Vec<u8>>, shared: Arc<Shared>, listeners: Vec<FrameListener>) {
    let mut seq = 0u64;
    let mut window_start = Instant::now();
    let mut window_frames = 0u32;
    for jpeg in rx {
        let Some((width, height, rgba)) = decode(&jpeg) else { continue };
        seq += 1;
        let frame = Arc::new(Frame { width, height, rgba, seq });
        *shared.frame.lock().unwrap() = Some(frame.clone());
        for l in &listeners {
            l(&frame);
        }
        window_frames += 1;
        let elapsed = window_start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            *shared.fps.lock().unwrap() = window_frames as f32 / elapsed.as_secs_f32();
            window_start = Instant::now();
            window_frames = 0;
        }
    }
}

enum Found {
    Ready(DeviceEntry),
    WrongMode(String),
    Nothing,
}

fn find_camera() -> Found {
    let devices = match list_devices() {
        Ok(d) => d,
        Err(e) => {
            warn!("device enumeration failed: {e}");
            return Found::Nothing;
        }
    };
    let mut wrong = None;
    for d in devices {
        let id = d.id.to_ascii_lowercase();
        if d.is_sony() {
            match d.pid {
                Some(0x0AB9) => wrong = Some("MTP"),
                Some(0x0ABA) => wrong = Some("Mass Storage"),
                _ => return Found::Ready(d),
            }
        } else if id.contains("usbstor") && id.contains("ven_sony") {
            wrong = Some("Mass Storage");
        }
    }
    match wrong {
        Some(mode) => Found::WrongMode(format!(
            "Camera is connected in {mode} mode. Set MENU → Setup → USB Connection → PC Remote and reconnect the cable."
        )),
        None => Found::Nothing,
    }
}

/// Is Sony's own Imaging Edge Webcam running? It grabs the camera.
fn official_app_running() -> bool {
    #[cfg(windows)]
    unsafe {
        use windows::core::w;
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
        if let Ok(h) = OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("WEBCAM_APP_MUTEX")) {
            let _ = CloseHandle(h);
            return true;
        }
    }
    false
}

/// Waits up to `d` for a Quit command; other commands are dropped because
/// there is no camera to execute them on.
fn wait_or_quit(rx: &Receiver<Command>, d: Duration) -> bool {
    let until = Instant::now() + d;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => return true,
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => return false,
        }
    }
}

fn camera_thread(rx: Receiver<Command>, shared: Arc<Shared>, jpeg_tx: Sender<Vec<u8>>) {
    let _com = match ComGuard::new() {
        Ok(g) => g,
        Err(e) => {
            shared.set_status(Status::Error(e.to_string()));
            return;
        }
    };
    loop {
        let retry = match find_camera() {
            Found::Ready(entry) => match session(&entry, &rx, &shared, &jpeg_tx) {
                Ok(()) => return,
                Err(e) => {
                    shared.set_status(Status::Error(format!("{e}")));
                    Duration::from_millis(1500)
                }
            },
            Found::WrongMode(msg) => {
                shared.set_status(Status::WrongMode(msg));
                Duration::from_secs(1)
            }
            Found::Nothing => {
                let hint = official_app_running().then(|| {
                    "Sony Imaging Edge Webcam is running and may hold the camera. Close it.".to_string()
                });
                shared.set_status(Status::Searching { hint });
                Duration::from_secs(1)
            }
        };
        *shared.camera.lock().unwrap() = Arc::new(CameraState::default());
        if wait_or_quit(&rx, retry) {
            return;
        }
    }
}

fn publish(shared: &Shared, cam: &SonyCamera<WpdTransport>) {
    let mut supported: HashSet<u16> = cam.ext.properties.iter().copied().collect();
    supported.extend(cam.ext.controls.iter().copied());
    *shared.camera.lock().unwrap() = Arc::new(CameraState {
        props: cam.props().clone(),
        supported,
    });
}

fn execute(cam: &mut SonyCamera<WpdTransport>, cmd: &Command) -> sony_ptp::Result<()> {
    match *cmd {
        Command::SetValue { code, value } => cam.set_value(code, value),
        Command::Step { code, dir } => cam.step(code, dir).map(|_| ()),
        Command::HalfPress(down) => cam.half_press(down),
        Command::FocusStep(n) => cam.focus_step(n),
        Command::Quit => Ok(()),
    }
}

const PROPS_INTERVAL: Duration = Duration::from_millis(300);
const STALL_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONSECUTIVE_ERRORS: u32 = 10;

/// Runs one connection. `Ok(())` means the user asked to quit.
fn session(
    entry: &DeviceEntry,
    rx: &Receiver<Command>,
    shared: &Arc<Shared>,
    jpeg_tx: &Sender<Vec<u8>>,
) -> anyhow::Result<()> {
    shared.set_status(Status::Connecting(entry.display_name().to_owned()));
    let transport = WpdTransport::open(&entry.id)?;
    let mut cam = SonyCamera::new(transport);
    let tx = jpeg_tx.clone();
    cam.set_frame_sink(Some(Box::new(move |jpeg| {
        // Drop frames when the decoder is behind instead of queueing latency.
        if let Err(TrySendError::Full(_)) = tx.try_send(jpeg) {
            debug!("decoder busy, frame dropped");
        }
    })));
    cam.connect()?;
    let model = cam
        .device_info
        .as_ref()
        .map_or_else(|| entry.display_name().to_owned(), |i| i.model.clone());
    publish(shared, &cam);
    shared.set_status(Status::Connected(model.clone()));

    let mut last_props = Instant::now();
    let mut last_frame = Instant::now();
    let mut errors = 0u32;
    loop {
        // User commands first, they are what the user is waiting for.
        while let Ok(c) = rx.try_recv() {
            if handle(&mut cam, shared, c)? {
                return Ok(());
            }
            last_props = Instant::now();
        }

        let result = (|| -> sony_ptp::Result<bool> {
            let got = cam.pump_liveview()?;
            if last_props.elapsed() >= PROPS_INTERVAL {
                cam.refresh_props()?;
                publish(shared, &cam);
                last_props = Instant::now();
            }
            Ok(got)
        })();
        match result {
            Ok(true) => {
                errors = 0;
                last_frame = Instant::now();
            }
            Ok(false) => {
                errors = 0;
                if last_frame.elapsed() > STALL_TIMEOUT {
                    anyhow::bail!("live view stalled");
                }
                // No new frame yet: wait a little, but wake up for commands.
                match rx.recv_timeout(Duration::from_millis(4)) {
                    Ok(c) => {
                        if handle(&mut cam, shared, c)? {
                            return Ok(());
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            }
            Err(e) if e.is_disconnect() => return Err(e.into()),
            Err(e) => {
                errors += 1;
                debug!("session error {errors}: {e}");
                if errors >= MAX_CONSECUTIVE_ERRORS {
                    return Err(e.into());
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Executes a user command. Returns `Ok(true)` when the user asked to quit.
fn handle(
    cam: &mut SonyCamera<WpdTransport>,
    shared: &Shared,
    cmd: Command,
) -> anyhow::Result<bool> {
    if matches!(cmd, Command::Quit) {
        return Ok(true);
    }
    shared.busy.store(true, Ordering::Relaxed);
    let r = execute(cam, &cmd);
    shared.busy.store(false, Ordering::Relaxed);
    publish(shared, cam);
    match r {
        Ok(()) => Ok(false),
        Err(e) if e.is_disconnect() => Err(e.into()),
        Err(e) => {
            shared.notify(format!("{e}"));
            Ok(false)
        }
    }
}
