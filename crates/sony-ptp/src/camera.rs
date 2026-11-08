//! High level Sony camera session.

use std::collections::{BTreeMap, HashMap};
use std::thread::sleep;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::codes::{dt, handle, op, prop, rc};
use crate::data::encode_int;
use crate::dataset::{is_step_property, DeviceInfo, ExtDeviceInfo, PropDesc, Value};
use crate::format::order_key;
use crate::liveview::extract_jpeg;
use crate::transport::{DataPhase, Response, Transport};
use crate::{Error, Result};

/// Direction in the physical sense of a property: `Up` means a larger
/// f-number, a longer exposure, a higher ISO, more exposure compensation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepDirection {
    Up,
    Down,
}

impl StepDirection {
    fn flip(self) -> Self {
        match self {
            StepDirection::Up => StepDirection::Down,
            StepDirection::Down => StepDirection::Up,
        }
    }
}

/// Protocol version passed to GetExtDeviceInfo by older bodies.
const EXT_INFO_VERSION: u32 = 0xC8;

pub struct SonyCamera<T: Transport> {
    transport: T,
    pub device_info: Option<DeviceInfo>,
    pub ext: ExtDeviceInfo,
    props: BTreeMap<u16, PropDesc>,
    /// Learned mapping of the raw step byte: `true` when `+1` moves the value Up.
    plus_is_up: HashMap<u16, bool>,
    /// How long to wait for the camera to reflect a change.
    pub change_timeout: Duration,
    /// Polling interval while waiting for a change.
    pub poll_interval: Duration,
    /// Receives live view frames fetched while a blocking operation waits
    /// for the camera, so that preview keeps running during stepping.
    frame_sink: Option<FrameSink>,
}

pub type FrameSink = Box<dyn FnMut(Vec<u8>) + Send>;

impl<T: Transport> SonyCamera<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            device_info: None,
            ext: ExtDeviceInfo::default(),
            props: BTreeMap::new(),
            plus_is_up: HashMap::new(),
            change_timeout: Duration::from_millis(1500),
            poll_interval: Duration::from_millis(40),
            frame_sink: None,
        }
    }

    pub fn set_frame_sink(&mut self, sink: Option<FrameSink>) {
        self.frame_sink = sink;
    }

    /// Fetches a live view frame if one is ready and hands it to the frame
    /// sink. Returns whether a frame was delivered.
    pub fn pump_liveview(&mut self) -> Result<bool> {
        if self.frame_sink.is_none() {
            return Ok(false);
        }
        match self.try_liveview_jpeg() {
            Ok(Some(jpeg)) => {
                if let Some(sink) = self.frame_sink.as_mut() {
                    sink(jpeg);
                }
                Ok(true)
            }
            Ok(None) | Err(Error::NoFrame) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Sleeps for `d`, pumping live view frames in the meantime.
    fn idle(&mut self, d: Duration) -> Result<()> {
        let until = Instant::now() + d;
        loop {
            self.pump_liveview()?;
            let now = Instant::now();
            if now >= until {
                return Ok(());
            }
            sleep((until - now).min(Duration::from_millis(5)));
        }
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Runs a transaction and turns a non-OK response code into an error.
    pub fn call(&mut self, opcode: u16, params: &[u32], phase: DataPhase<'_>) -> Result<Response> {
        let resp = self.transport.execute(opcode, params, phase)?;
        if resp.code != rc::OK {
            return Err(Error::Response { op: opcode, code: resp.code });
        }
        Ok(resp)
    }

    pub fn get_device_info(&mut self) -> Result<DeviceInfo> {
        let resp = self.call(op::GET_DEVICE_INFO, &[], DataPhase::In)?;
        DeviceInfo::parse(&resp.data)
    }

    fn sdio_connect(&mut self, phase: u32) -> Result<()> {
        self.call(op::SONY_SDIO_CONNECT, &[phase, 0, 0], DataPhase::In)?;
        Ok(())
    }

    pub fn get_ext_device_info(&mut self) -> Result<ExtDeviceInfo> {
        let resp = self.call(op::SONY_GET_EXT_DEVICE_INFO, &[EXT_INFO_VERSION], DataPhase::In)?;
        ExtDeviceInfo::parse(&resp.data)
    }

    /// Performs the Sony "PC Remote" handshake and loads the property table.
    pub fn connect(&mut self) -> Result<()> {
        match self.get_device_info() {
            Ok(info) => {
                info!(model = %info.model, version = %info.device_version, "device info");
                self.device_info = Some(info);
            }
            Err(e) => warn!("GetDeviceInfo failed: {e}"),
        }
        self.sdio_connect(1)?;
        self.sdio_connect(2)?;
        self.ext = self.get_ext_device_info()?;
        info!(
            version = self.ext.version,
            props = self.ext.properties.len(),
            controls = self.ext.controls.len(),
            "ext device info"
        );
        self.sdio_connect(3)?;

        // The camera needs a moment after the handshake before it reports
        // properties and live view.
        // Live view status (D221) turns 1 once frames are available.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.refresh_props() {
                Ok(()) if self.is_ready() => break,
                Ok(()) => {}
                Err(e) if e.is_disconnect() => return Err(e),
                Err(e) => debug!("waiting for properties: {e}"),
            }
            if Instant::now() > deadline {
                warn!("camera did not report any properties after connect");
                break;
            }
            sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    /// Uses the second enumeration in 0x9209 entries (newer protocol).
    fn two_enums(&self) -> bool {
        self.ext.version >= 300
    }

    pub fn get_all_props_raw(&mut self) -> Result<Vec<u8>> {
        Ok(self.call(op::SONY_GET_ALL_EXT_DEVICE_PROP_INFO, &[], DataPhase::In)?.data)
    }

    pub fn refresh_props(&mut self) -> Result<()> {
        let data = self.get_all_props_raw()?;
        let list = PropDesc::parse_all(&data, self.two_enums())?;
        self.props = list.into_iter().map(|p| (p.code, p)).collect();
        Ok(())
    }

    pub fn props(&self) -> &BTreeMap<u16, PropDesc> {
        &self.props
    }

    pub fn prop(&self, code: u16) -> Option<&PropDesc> {
        self.props.get(&code)
    }

    fn current(&self, code: u16) -> Result<i64> {
        self.prop(code)
            .and_then(PropDesc::current_int)
            .ok_or(Error::UnknownProperty(code))
    }

    /// Supported by the camera, either as a property or as a control.
    pub fn supports(&self, code: u16) -> bool {
        self.ext.supports(code) || self.props.contains_key(&code)
    }

    /// Properties loaded and live view reported as running.
    fn is_ready(&self) -> bool {
        if self.props.is_empty() {
            return false;
        }
        match self.prop(prop::SONY_LIVE_VIEW_STATUS).and_then(PropDesc::current_int) {
            Some(status) => status != 0,
            None => true,
        }
    }

    /// Raw live view object (header + JPEG).
    pub fn liveview_raw(&mut self) -> Result<Vec<u8>> {
        Ok(self.call(op::GET_OBJECT, &[handle::LIVE_VIEW], DataPhase::In)?.data)
    }

    /// Next live view JPEG, or `None` when the camera has no new frame yet.
    ///
    /// The body answers `AccessDenied` until a new frame is ready (≈30 fps)
    /// and for about a second after the handshake.
    pub fn try_liveview_jpeg(&mut self) -> Result<Option<Vec<u8>>> {
        match self.liveview_raw() {
            Ok(raw) => extract_jpeg(&raw).map(|j| Some(j.to_vec())).ok_or(Error::NoFrame),
            Err(Error::Response { code: rc::ACCESS_DENIED | rc::DEVICE_BUSY, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Waits up to `timeout` for the next live view JPEG.
    pub fn liveview_jpeg(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(j) = self.try_liveview_jpeg()? {
                return Ok(j);
            }
            if Instant::now() >= deadline {
                return Err(Error::NoFrame);
            }
            sleep(Duration::from_millis(4));
        }
    }

    /// SetControlDeviceA: writes an absolute value.
    pub fn set_control_a(&mut self, code: u16, data_type: u16, value: i64) -> Result<()> {
        let data = encode_int(data_type, value)?;
        self.call(op::SONY_SET_CONTROL_DEVICE_A, &[code as u32], DataPhase::Out(&data))?;
        Ok(())
    }

    /// SetControlDeviceB: buttons and up/down steps.
    pub fn set_control_b(&mut self, code: u16, data_type: u16, value: i64) -> Result<()> {
        let data = encode_int(data_type, value)?;
        self.call(op::SONY_SET_CONTROL_DEVICE_B, &[code as u32], DataPhase::Out(&data))?;
        Ok(())
    }

    /// Shutter half press (starts AF in AF modes).
    pub fn half_press(&mut self, down: bool) -> Result<()> {
        self.set_control_b(prop::SONY_S1, dt::UINT16, if down { 2 } else { 1 })
    }

    /// Manual focus step: negative = near, positive = far (camera accepts ±1..±7).
    pub fn focus_step(&mut self, amount: i16) -> Result<()> {
        self.set_control_b(prop::SONY_NEAR_FAR, dt::INT16, amount.clamp(-7, 7) as i64)
    }

    /// Polls until the current value of `code` differs from `old`.
    fn wait_change(&mut self, code: u16, old: &Value) -> Result<Option<i64>> {
        let deadline = Instant::now() + self.change_timeout;
        loop {
            self.idle(self.poll_interval)?;
            self.refresh_props()?;
            if let Some(p) = self.prop(code) {
                if &p.current != old {
                    return Ok(p.current_int());
                }
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    fn data_type(&self, code: u16) -> Result<u16> {
        Ok(self.prop(code).ok_or(Error::UnknownProperty(code))?.data_type)
    }

    /// Properties that older bodies only accept as up/down steps.
    pub fn uses_stepping(&self, code: u16) -> bool {
        is_step_property(code) && !self.two_enums()
    }

    /// Whether a `+1` step moves the property Up. Learned at runtime; the
    /// defaults match what an ILCE-6300 does (`+1` = faster shutter).
    fn plus_is_up(&self, code: u16) -> bool {
        self.plus_is_up
            .get(&code)
            .copied()
            .unwrap_or(code != prop::SONY_SHUTTER_SPEED)
    }

    fn send_step(&mut self, code: u16, dir: StepDirection) -> Result<()> {
        let plus_is_up = self.plus_is_up(code);
        let raw = if (dir == StepDirection::Up) == plus_is_up { 0x01 } else { 0xFF };
        self.set_control_b(code, dt::UINT8, raw)
    }

    /// Moves a property one notch in `dir` and returns the new value.
    ///
    /// The mapping between the raw step byte and the physical direction is
    /// learned: if the camera moves the other way we flip the mapping and
    /// correct the value.
    pub fn step(&mut self, code: u16, dir: StepDirection) -> Result<i64> {
        let old = self.current(code)?;
        let old_value = Value::Int(old);
        self.send_step(code, dir)?;
        let Some(new) = self.wait_change(code, &old_value)? else {
            return Err(Error::NoEffect(code));
        };
        let went_up = order_key(code, new) > order_key(code, old);
        if went_up != (dir == StepDirection::Up) {
            let learned = !self.plus_is_up(code);
            debug!(code = format_args!("0x{code:04X}"), plus_is_up = learned, "flipping step direction");
            self.plus_is_up.insert(code, learned);
            // Undo the wrong step and do the intended one.
            for _ in 0..2 {
                let before = Value::Int(self.current(code)?);
                self.send_step(code, dir)?;
                if self.wait_change(code, &before)?.is_none() {
                    break;
                }
            }
            return self.current(code);
        }
        Ok(new)
    }

    /// Sets `code` to `target`, either directly or by stepping, depending on
    /// what the camera accepts.
    pub fn set_value(&mut self, code: u16, target: i64) -> Result<()> {
        let desc = self.prop(code).ok_or(Error::UnknownProperty(code))?.clone();
        if desc.current_int() == Some(target) {
            return Ok(());
        }
        if !self.uses_stepping(code) {
            self.set_control_a(code, desc.data_type, target)?;
            match self.wait_change(code, &desc.current)? {
                Some(v) if v == target => return Ok(()),
                Some(_) => return Ok(()), // camera picked the nearest valid value
                None if !is_step_property(code) => {
                    return Err(Error::NoEffect(code))
                }
                None => debug!("direct set had no effect, falling back to stepping"),
            }
        }
        self.step_to(code, target)
    }

    fn step_to(&mut self, code: u16, target: i64) -> Result<()> {
        let target_key = order_key(code, target);
        let mut last_dir = None;
        let mut reversals = 0;
        for _ in 0..96 {
            let cur = self.current(code)?;
            if cur == target {
                return Ok(());
            }
            let dir = if target_key > order_key(code, cur) {
                StepDirection::Up
            } else {
                StepDirection::Down
            };
            if last_dir.is_some_and(|d: StepDirection| d == dir.flip()) {
                reversals += 1;
                if reversals >= 2 {
                    // Target lies between two reachable values: stay where we are.
                    return Ok(());
                }
            }
            last_dir = Some(dir);
            self.step(code, dir)?;
        }
        Err(Error::NoEffect(code))
    }

    /// Value type of a property, for callers that need to encode values.
    pub fn property_type(&self, code: u16) -> Result<u16> {
        self.data_type(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::tests::{all_props, entry};
    use crate::transport::TransportError;

    /// Simulated body: f-number list stepping where `+1` *decreases* the
    /// value, to exercise direction learning.
    struct FakeCamera {
        fnumbers: Vec<i64>,
        idx: usize,
        wb: i64,
        log: Vec<(u16, Vec<u8>)>,
    }

    impl FakeCamera {
        fn new() -> Self {
            Self { fnumbers: vec![350, 400, 450, 500, 560, 630], idx: 2, wb: 2, log: Vec::new() }
        }
    }

    impl Transport for FakeCamera {
        fn execute(
            &mut self,
            opcode: u16,
            params: &[u32],
            phase: DataPhase<'_>,
        ) -> Result<Response, TransportError> {
            let mut resp = Response { code: rc::OK, ..Default::default() };
            if let DataPhase::Out(d) = phase {
                self.log.push((opcode, d.to_vec()));
            }
            match opcode {
                op::SONY_SDIO_CONNECT => resp.data = vec![0; 8],
                op::SONY_GET_EXT_DEVICE_INFO => {
                    let mut b = 200u16.to_le_bytes().to_vec();
                    b.extend_from_slice(&2u32.to_le_bytes());
                    b.extend_from_slice(&prop::F_NUMBER.to_le_bytes());
                    b.extend_from_slice(&prop::WHITE_BALANCE.to_le_bytes());
                    b.extend_from_slice(&0u32.to_le_bytes());
                    resp.data = b;
                }
                op::SONY_GET_ALL_EXT_DEVICE_PROP_INFO => {
                    resp.data = all_props(&[
                        entry(prop::F_NUMBER, dt::UINT16, true, self.fnumbers[self.idx], &[]),
                        entry(prop::WHITE_BALANCE, dt::UINT16, true, self.wb, &[2, 4, 6]),
                    ]);
                }
                op::SONY_SET_CONTROL_DEVICE_B => {
                    let DataPhase::Out(d) = phase else { unreachable!() };
                    if params[0] == prop::F_NUMBER as u32 {
                        if d[0] == 0x01 {
                            self.idx = self.idx.saturating_sub(1);
                        } else if self.idx + 1 < self.fnumbers.len() {
                            self.idx += 1;
                        }
                    }
                }
                op::SONY_SET_CONTROL_DEVICE_A => {
                    let DataPhase::Out(d) = phase else { unreachable!() };
                    if params[0] == prop::WHITE_BALANCE as u32 {
                        self.wb = u16::from_le_bytes([d[0], d[1]]) as i64;
                    }
                }
                op::GET_DEVICE_INFO => resp.code = rc::OPERATION_NOT_SUPPORTED,
                _ => resp.code = rc::OPERATION_NOT_SUPPORTED,
            }
            Ok(resp)
        }
    }

    fn camera() -> SonyCamera<FakeCamera> {
        let mut cam = SonyCamera::new(FakeCamera::new());
        cam.poll_interval = Duration::from_millis(1);
        cam.change_timeout = Duration::from_millis(20);
        cam.connect().unwrap();
        cam
    }

    #[test]
    fn connect_loads_props() {
        let cam = camera();
        assert_eq!(cam.ext.version, 200);
        assert_eq!(cam.prop(prop::F_NUMBER).unwrap().current_int(), Some(450));
        assert!(cam.supports(prop::WHITE_BALANCE));
    }

    #[test]
    fn step_learns_direction() {
        let mut cam = camera();
        assert_eq!(cam.step(prop::F_NUMBER, StepDirection::Up).unwrap(), 500);
        // Second step uses the learned mapping straight away.
        let sent_before = cam.transport_mut().log.len();
        assert_eq!(cam.step(prop::F_NUMBER, StepDirection::Up).unwrap(), 560);
        assert_eq!(cam.transport_mut().log.len(), sent_before + 1);
    }

    #[test]
    fn set_value_by_stepping() {
        let mut cam = camera();
        cam.set_value(prop::F_NUMBER, 350).unwrap();
        assert_eq!(cam.prop(prop::F_NUMBER).unwrap().current_int(), Some(350));
        cam.set_value(prop::F_NUMBER, 630).unwrap();
        assert_eq!(cam.prop(prop::F_NUMBER).unwrap().current_int(), Some(630));
        // At the end of the range further steps have no effect.
        assert!(matches!(
            cam.step(prop::F_NUMBER, StepDirection::Up),
            Err(Error::NoEffect(_))
        ));
    }

    #[test]
    fn set_value_direct() {
        let mut cam = camera();
        cam.set_value(prop::WHITE_BALANCE, 6).unwrap();
        assert_eq!(cam.prop(prop::WHITE_BALANCE).unwrap().current_int(), Some(6));
        assert_eq!(cam.transport_mut().log.last().unwrap().0, op::SONY_SET_CONTROL_DEVICE_A);
    }
}
