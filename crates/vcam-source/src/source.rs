//! The media source: one NV12 1280×720@30 video stream.

use std::ffi::c_void;
use std::sync::Mutex;

use vcam_shared::{FPS, FRAME_BYTES, HEIGHT, WIDTH};
use windows::core::{implement, ComObject, IUnknown, Interface, Ref, Result, GUID, HRESULT};
use windows::Win32::Foundation::{E_INVALIDARG, E_POINTER, S_OK};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;

use crate::log;
use crate::stream::MediaStream;

/// HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND): "no such KS property set".
const SET_NOT_FOUND: HRESULT = HRESULT(0x8007_0492_u32 as i32);
const STREAM_ID: u32 = 0;

struct Inner {
    queue: Option<IMFMediaEventQueue>,
    stream: Option<ComObject<MediaStream>>,
    started: bool,
}

#[implement(IMFMediaSourceEx, IMFGetService, IKsControl, IMFSampleAllocatorControl)]
pub struct MediaSource {
    inner: Mutex<Inner>,
    attributes: IMFAttributes,
    stream_attributes: IMFAttributes,
    pd: IMFPresentationDescriptor,
}

fn new_attributes(n: u32) -> Result<IMFAttributes> {
    let mut a = None;
    unsafe { MFCreateAttributes(&mut a, n)? };
    a.ok_or_else(|| E_POINTER.into())
}

fn media_type() -> Result<IMFMediaType> {
    unsafe {
        let mt = MFCreateMediaType()?;
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        mt.SetUINT64(&MF_MT_FRAME_SIZE, ((WIDTH as u64) << 32) | HEIGHT as u64)?;
        mt.SetUINT64(&MF_MT_FRAME_RATE, ((FPS as u64) << 32) | 1)?;
        mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        mt.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        mt.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        mt.SetUINT32(&MF_MT_SAMPLE_SIZE, FRAME_BYTES as u32)?;
        mt.SetUINT32(&MF_MT_DEFAULT_STRIDE, WIDTH)?;
        mt.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        mt.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        Ok(mt)
    }
}

impl MediaSource {
    pub fn create() -> Result<ComObject<MediaSource>> {
        unsafe {
            let mt = media_type()?;

            let stream_attributes = new_attributes(4)?;
            stream_attributes.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
            stream_attributes.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, STREAM_ID)?;
            stream_attributes.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
            stream_attributes.SetUINT32(&MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MFFrameSourceTypes_Color.0 as u32)?;

            let sd = MFCreateStreamDescriptor(STREAM_ID, &[Some(mt.clone())])?;
            sd.GetMediaTypeHandler()?.SetCurrentMediaType(&mt)?;
            stream_attributes.CopyAllItems(&sd.cast::<IMFAttributes>()?)?;

            let pd = MFCreatePresentationDescriptor(Some(&[Some(sd.clone())]))?;
            pd.SelectStream(0)?;

            let source = ComObject::new(MediaSource {
                inner: Mutex::new(Inner { queue: Some(MFCreateEventQueue()?), stream: None, started: false }),
                attributes: new_attributes(1)?,
                stream_attributes,
                pd,
            });
            let ex: IMFMediaSourceEx = source.to_interface();
            let stream = ComObject::new(MediaStream::new(ex.into(), sd, mt)?);
            source.inner.lock().unwrap().stream = Some(stream);
            log!("media source created");
            Ok(source)
        }
    }

    fn queue(&self) -> Result<IMFMediaEventQueue> {
        self.inner.lock().unwrap().queue.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    fn stream(&self) -> Result<ComObject<MediaStream>> {
        self.inner.lock().unwrap().stream.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    /// Shuts down the source and its stream (also breaks the reference
    /// cycle between them).
    pub fn shutdown_all(&self) {
        let (queue, stream) = {
            let mut inner = self.inner.lock().unwrap();
            (inner.queue.take(), inner.stream.take())
        };
        if let Some(s) = stream {
            s.shutdown();
        }
        if let Some(q) = queue {
            let _ = unsafe { q.Shutdown() };
        }
        log!("media source shut down");
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
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

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        self.queue()?;
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        self.queue()?;
        unsafe { self.pd.Clone() }
    }

    fn Start(
        &self,
        pd: Ref<IMFPresentationDescriptor>,
        time_format: *const GUID,
        start: *const PROPVARIANT,
    ) -> Result<()> {
        log!("Start");
        let queue = self.queue()?;
        let pd = pd.as_ref().ok_or(windows::core::Error::from(E_POINTER))?;
        if !time_format.is_null() && unsafe { *time_format } != GUID::zeroed() {
            return Err(MF_E_UNSUPPORTED_TIME_FORMAT.into());
        }
        unsafe {
            if pd.GetStreamDescriptorCount()? != 1 {
                return Err(E_INVALIDARG.into());
            }
            let mut selected = windows::core::BOOL(0);
            let mut sd = None;
            pd.GetStreamDescriptorByIndex(0, &mut selected, &mut sd)?;
            let stream = self.stream()?;
            if selected.as_bool() {
                self.pd.SelectStream(0)?;
                stream.start()?;
                let was_started = std::mem::replace(&mut self.inner.lock().unwrap().started, true);
                let unk: IUnknown = stream.to_interface();
                let met = if was_started { MEUpdatedStream } else { MENewStream };
                queue.QueueEventParamUnk(met.0 as u32, &GUID::zeroed(), S_OK, &unk)?;
            } else {
                self.pd.DeselectStream(0)?;
                let _ = stream.stop();
            }
            queue.QueueEventParamVar(MESourceStarted.0 as u32, &GUID::zeroed(), S_OK, start)
        }
    }

    fn Stop(&self) -> Result<()> {
        log!("Stop");
        let queue = self.queue()?;
        let _ = self.stream()?.stop();
        unsafe { queue.QueueEventParamVar(MESourceStopped.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null()) }
    }

    fn Pause(&self) -> Result<()> {
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> Result<()> {
        log!("Shutdown");
        self.shutdown_all();
        Ok(())
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        self.queue()?;
        Ok(self.attributes.clone())
    }

    fn GetStreamAttributes(&self, id: u32) -> Result<IMFAttributes> {
        self.queue()?;
        if id != STREAM_ID {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        Ok(self.stream_attributes.clone())
    }

    fn SetD3DManager(&self, _manager: Ref<IUnknown>) -> Result<()> {
        // We produce system memory samples; nothing to do.
        Ok(())
    }
}

impl IMFGetService_Impl for MediaSource_Impl {
    fn GetService(&self, _service: *const GUID, _riid: *const GUID, _ppv: *mut *mut c_void) -> Result<()> {
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

impl IKsControl_Impl for MediaSource_Impl {
    fn KsProperty(&self, _p: *const KSIDENTIFIER, _pl: u32, _d: *mut c_void, _dl: u32, _ret: *mut u32) -> Result<()> {
        Err(SET_NOT_FOUND.into())
    }
    fn KsMethod(&self, _m: *const KSIDENTIFIER, _ml: u32, _d: *mut c_void, _dl: u32, _ret: *mut u32) -> Result<()> {
        Err(SET_NOT_FOUND.into())
    }
    fn KsEvent(&self, _e: *const KSIDENTIFIER, _el: u32, _d: *mut c_void, _dl: u32, _ret: *mut u32) -> Result<()> {
        Err(SET_NOT_FOUND.into())
    }
}

impl IMFSampleAllocatorControl_Impl for MediaSource_Impl {
    fn SetDefaultAllocator(&self, id: u32, allocator: Ref<IUnknown>) -> Result<()> {
        if id != STREAM_ID {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        let alloc = allocator.as_ref().and_then(|a| a.cast::<IMFVideoSampleAllocator>().ok());
        self.stream()?.set_allocator(alloc);
        Ok(())
    }

    fn GetAllocatorUsage(&self, id: u32, input_id: *mut u32, usage: *mut MFSampleAllocatorUsage) -> Result<()> {
        if id != STREAM_ID {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        if input_id.is_null() || usage.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            *input_id = id;
            *usage = MFSampleAllocatorUsage_UsesProvidedAllocator;
        }
        Ok(())
    }
}
