//! The single video stream of the virtual camera.
//!
//! `RequestSample` only queues the request token; a pacing thread answers
//! queued requests at the nominal frame rate with the newest frame from the
//! application (or a placeholder when the application is not running).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vcam_shared::convert::fill_placeholder;
use vcam_shared::shm::SharedFrames;
use vcam_shared::{FPS, FRAME_BYTES, HEIGHT, WIDTH};
use windows::core::{implement, IUnknown, Interface, Ref, Result, GUID, HRESULT};
use windows::Win32::Foundation::{E_POINTER, S_OK};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;

use crate::log;

struct State {
    stream_state: MF_STREAM_STATE,
    queue: Option<IMFMediaEventQueue>,
    source: Option<IMFMediaSource>,
    allocator: Option<IMFVideoSampleAllocator>,
    allocator_ready: bool,
    tokens: VecDeque<Option<IUnknown>>,
    pacer: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

// MF objects used here (event queue, allocator, samples) are free threaded.
unsafe impl Send for State {}

#[implement(IMFMediaStream2)]
pub struct MediaStream {
    state: Arc<Mutex<State>>,
    descriptor: IMFStreamDescriptor,
    media_type: IMFMediaType,
}

impl MediaStream {
    pub fn new(source: IMFMediaSource, descriptor: IMFStreamDescriptor, media_type: IMFMediaType) -> Result<Self> {
        let queue = unsafe { MFCreateEventQueue()? };
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                stream_state: MF_STREAM_STATE_STOPPED,
                queue: Some(queue),
                source: Some(source),
                allocator: None,
                allocator_ready: false,
                tokens: VecDeque::new(),
                pacer: None,
            })),
            descriptor,
            media_type,
        })
    }

    fn queue(&self) -> Result<IMFMediaEventQueue> {
        self.state.lock().unwrap().queue.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    pub fn set_allocator(&self, allocator: Option<IMFVideoSampleAllocator>) {
        let mut st = self.state.lock().unwrap();
        log!("SetDefaultAllocator: {}", allocator.is_some());
        st.allocator = allocator;
        st.allocator_ready = false;
    }

    /// Starts streaming; `me` is this stream as `IUnknown` for the pacer.
    pub fn start(&self) -> Result<()> {
        let queue = {
            let mut st = self.state.lock().unwrap();
            let queue = st.queue.clone().ok_or(windows::core::Error::from(MF_E_SHUTDOWN))?;
            if let (Some(alloc), false) = (st.allocator.clone(), st.allocator_ready) {
                match unsafe { alloc.InitializeSampleAllocator(10, &self.media_type) } {
                    Ok(()) => st.allocator_ready = true,
                    Err(e) => {
                        log!("InitializeSampleAllocator failed: {e}, using own buffers");
                        st.allocator = None;
                    }
                }
            }
            st.stream_state = MF_STREAM_STATE_RUNNING;
            if st.pacer.is_none() {
                let stop = Arc::new(AtomicBool::new(false));
                let state = self.state.clone();
                let s2 = stop.clone();
                let handle = std::thread::Builder::new()
                    .name("vcam-pacer".into())
                    .spawn(move || pacer(state, s2))
                    .map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
                st.pacer = Some((stop, handle));
            }
            queue
        };
        log!("stream started");
        unsafe { queue.QueueEventParamVar(MEStreamStarted.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null()) }
    }

    pub fn stop(&self) -> Result<()> {
        let (queue, pacer) = {
            let mut st = self.state.lock().unwrap();
            st.stream_state = MF_STREAM_STATE_STOPPED;
            st.tokens.clear();
            (st.queue.clone(), st.pacer.take())
        };
        stop_pacer(pacer);
        {
            let mut st = self.state.lock().unwrap();
            if let (Some(a), true) = (st.allocator.clone(), st.allocator_ready) {
                let _ = unsafe { a.UninitializeSampleAllocator() };
                st.allocator_ready = false;
            }
        }
        log!("stream stopped");
        match queue {
            Some(q) => unsafe { q.QueueEventParamVar(MEStreamStopped.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null()) },
            None => Err(MF_E_SHUTDOWN.into()),
        }
    }

    pub fn shutdown(&self) {
        let (queue, pacer, alloc) = {
            let mut st = self.state.lock().unwrap();
            st.stream_state = MF_STREAM_STATE_STOPPED;
            st.tokens.clear();
            st.source = None;
            let alloc = st.allocator.take().filter(|_| st.allocator_ready);
            st.allocator_ready = false;
            (st.queue.take(), st.pacer.take(), alloc)
        };
        stop_pacer(pacer);
        if let Some(a) = alloc {
            let _ = unsafe { a.UninitializeSampleAllocator() };
        }
        if let Some(q) = queue {
            let _ = unsafe { q.Shutdown() };
        }
        log!("stream shut down");
    }
}

fn stop_pacer(pacer: Option<(Arc<AtomicBool>, JoinHandle<()>)>) {
    if let Some((stop, handle)) = pacer {
        stop.store(true, Ordering::Release);
        // The pacer never blocks on the state lock for long; joining is safe
        // unless we are the pacer thread itself.
        if handle.thread().id() != std::thread::current().id() {
            let _ = handle.join();
        }
    }
}

/// Delivers samples at the frame rate while requests are pending.
fn pacer(state: Arc<Mutex<State>>, stop: Arc<AtomicBool>) {
    let period = Duration::from_nanos(1_000_000_000 / FPS as u64);
    let mut frame = vec![0u8; FRAME_BYTES];
    fill_placeholder(&mut frame, WIDTH as usize, HEIGHT as usize);
    let mut shm: Option<SharedFrames> = None;
    let mut last_open_attempt: Option<Instant> = None;
    let mut showing_placeholder = true;
    let mut next = Instant::now();

    while !stop.load(Ordering::Acquire) {
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > period * 4 {
            next = now; // fell far behind, don't burst
        }

        let (token, queue, alloc) = {
            let mut st = state.lock().unwrap();
            if st.stream_state != MF_STREAM_STATE_RUNNING {
                continue;
            }
            let Some(token) = st.tokens.pop_front() else { continue };
            (token, st.queue.clone(), st.allocator.clone().filter(|_| st.allocator_ready))
        };
        let Some(queue) = queue else { break };

        if shm.is_none() && last_open_attempt.is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
            last_open_attempt = Some(Instant::now());
            match SharedFrames::create() {
                Ok(s) => {
                    log!("shared memory ready");
                    shm = Some(s);
                }
                Err(e) => log!("cannot create shared memory: {e}"),
            }
        }
        let fresh = shm.as_ref().and_then(|s| s.read_latest(&mut frame)).is_some();
        if fresh {
            showing_placeholder = false;
        } else if !showing_placeholder && !shm.as_ref().is_some_and(writer_active) {
            fill_placeholder(&mut frame, WIDTH as usize, HEIGHT as usize);
            showing_placeholder = true;
        }

        match make_sample(alloc.as_ref(), &frame, token) {
            Ok(sample) => unsafe {
                if let Err(e) = queue.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample) {
                    log!("QueueEvent(MEMediaSample) failed: {e}");
                }
            },
            Err(e) => log!("cannot build sample: {e}"),
        }
    }
}

fn writer_active(s: &SharedFrames) -> bool {
    let tick = s.header().writer_tick.load(Ordering::Acquire);
    SharedFrames::now().saturating_sub(tick) < vcam_shared::WRITER_TIMEOUT_MS
}

fn make_sample(alloc: Option<&IMFVideoSampleAllocator>, frame: &[u8], token: Option<IUnknown>) -> Result<IMFSample> {
    unsafe {
        let sample = match alloc {
            Some(a) => a.AllocateSample()?,
            None => {
                let s = MFCreateSample()?;
                let b = MFCreateMemoryBuffer(FRAME_BYTES as u32)?;
                s.AddBuffer(&b)?;
                s
            }
        };
        let buffer = sample.GetBufferByIndex(0)?;
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        if let Ok(b2) = buffer.cast::<IMF2DBuffer2>() {
            let mut scan0 = std::ptr::null_mut();
            let mut pitch = 0i32;
            let mut start = std::ptr::null_mut();
            let mut len = 0u32;
            b2.Lock2DSize(MF2DBuffer_LockFlags_Write, &mut scan0, &mut pitch, &mut start, &mut len)?;
            let p = pitch as usize;
            if pitch >= w as i32 && (len as usize) >= p * h * 3 / 2 {
                for row in 0..h * 3 / 2 {
                    std::ptr::copy_nonoverlapping(frame.as_ptr().add(row * w), scan0.add(row * p), w);
                }
            }
            b2.Unlock2D()?;
        } else {
            let mut p = std::ptr::null_mut();
            let mut max = 0u32;
            buffer.Lock(&mut p, Some(&mut max), None)?;
            if p.is_null() {
                buffer.Unlock()?;
                return Err(E_POINTER.into());
            }
            std::ptr::copy_nonoverlapping(frame.as_ptr(), p, FRAME_BYTES.min(max as usize));
            buffer.Unlock()?;
            buffer.SetCurrentLength(FRAME_BYTES as u32)?;
        }
        sample.SetSampleTime(MFGetSystemTime())?;
        sample.SetSampleDuration(10_000_000 / FPS as i64)?;
        if let Some(t) = token {
            sample.SetUnknown(&MFSampleExtension_Token, &t)?;
        }
        Ok(sample)
    }
}

impl IMFMediaEventGenerator_Impl for MediaStream_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        let q = self.queue()?;
        unsafe { q.GetEvent(flags.0) }
    }
    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> Result<()> {
        let q = self.queue()?;
        unsafe { q.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        let q = self.queue()?;
        unsafe { q.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(&self, met: u32, ext: *const GUID, hr: HRESULT, value: *const PROPVARIANT) -> Result<()> {
        let q = self.queue()?;
        unsafe { q.QueueEventParamVar(met, ext, hr, value) }
    }
}

impl IMFMediaStream_Impl for MediaStream_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        self.state.lock().unwrap().source.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }
    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        if self.state.lock().unwrap().queue.is_none() {
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(self.descriptor.clone())
    }
    fn RequestSample(&self, token: Ref<IUnknown>) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        if st.queue.is_none() {
            return Err(MF_E_SHUTDOWN.into());
        }
        if st.stream_state != MF_STREAM_STATE_RUNNING {
            return Err(MF_E_INVALIDREQUEST.into());
        }
        st.tokens.push_back(token.as_ref().cloned());
        Ok(())
    }
}

impl IMFMediaStream2_Impl for MediaStream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> Result<()> {
        let current = self.state.lock().unwrap().stream_state;
        if current == value {
            return Ok(());
        }
        match value {
            MF_STREAM_STATE_RUNNING => self.start(),
            MF_STREAM_STATE_STOPPED | MF_STREAM_STATE_PAUSED => {
                self.stop()?;
                self.state.lock().unwrap().stream_state = value;
                Ok(())
            }
            _ => Err(MF_E_INVALID_STATE_TRANSITION.into()),
        }
    }
    fn GetStreamState(&self) -> Result<MF_STREAM_STATE> {
        Ok(self.state.lock().unwrap().stream_state)
    }
}
