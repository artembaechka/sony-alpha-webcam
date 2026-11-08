//! PTP / Sony datasets.

use serde::Serialize;

use crate::data::Reader;
use crate::{Error, Result};

/// A decoded property value. Integers of every width are widened to `i64`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Value {
    Int(i64),
    Str(String),
    Array(Vec<i64>),
}

impl Value {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }
}

/// Standard PTP DeviceInfo dataset.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DeviceInfo {
    pub standard_version: u16,
    pub vendor_extension_id: u32,
    pub vendor_extension_version: u16,
    pub vendor_extension_desc: String,
    pub functional_mode: u16,
    pub operations: Vec<u16>,
    pub events: Vec<u16>,
    pub properties: Vec<u16>,
    pub capture_formats: Vec<u16>,
    pub image_formats: Vec<u16>,
    pub manufacturer: String,
    pub model: String,
    pub device_version: String,
    pub serial_number: String,
}

impl DeviceInfo {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        let mut r = Reader::new(buf);
        Ok(Self {
            standard_version: r.u16()?,
            vendor_extension_id: r.u32()?,
            vendor_extension_version: r.u16()?,
            vendor_extension_desc: r.string()?,
            functional_mode: r.u16()?,
            operations: r.u16_array()?,
            events: r.u16_array()?,
            properties: r.u16_array()?,
            capture_formats: r.u16_array()?,
            image_formats: r.u16_array()?,
            manufacturer: r.string()?,
            model: r.string()?,
            device_version: r.string()?,
            serial_number: r.string()?,
        })
    }
}

/// Answer to Sony `GetExtDeviceInfo` (0x9202).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ExtDeviceInfo {
    /// Protocol version, e.g. 200 (0xC8) for cameras of the A6300 generation.
    pub version: u16,
    /// Readable / writable properties.
    pub properties: Vec<u16>,
    /// Controls (buttons) that are written through SetControlDeviceB.
    pub controls: Vec<u16>,
}

impl ExtDeviceInfo {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        let mut r = Reader::new(buf);
        let version = r.u16()?;
        let properties = r.u16_array()?;
        let controls = if r.remaining() >= 4 { r.u16_array()? } else { Vec::new() };
        Ok(Self { version, properties, controls })
    }

    pub fn supports(&self, code: u16) -> bool {
        self.properties.contains(&code) || self.controls.contains(&code)
    }
}

/// Allowed values of a property.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Form {
    None,
    Range { min: Value, max: Value, step: Value },
    Enum(Vec<Value>),
}

/// One entry of Sony `GetAllExtDevicePropInfo` (0x9209).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PropDesc {
    pub code: u16,
    pub data_type: u16,
    /// 1 = read/write, 0 = read only.
    pub get_set: u8,
    /// 0 = disabled (greyed out), 1 = enabled, 2 = display only.
    pub is_enabled: u8,
    pub factory_default: Value,
    pub current: Value,
    pub form: Form,
    /// Newer protocol versions (>= 300) report a second list of values that
    /// can actually be set right now.
    pub settable_values: Option<Vec<Value>>,
}

impl PropDesc {
    /// Can be written with an absolute value (SetControlDeviceA).
    pub fn writable(&self) -> bool {
        self.get_set == 1 && self.is_enabled == 1
    }

    /// Exposure properties that bodies of the A6300 generation only change
    /// in up/down steps. They report `get_set == 0` but still accept steps.
    pub fn steppable(&self) -> bool {
        is_step_property(self.code)
    }

    /// The user can change this property right now, one way or another.
    pub fn adjustable(&self) -> bool {
        self.is_enabled == 1 && (self.get_set == 1 || self.steppable())
    }

    pub fn current_int(&self) -> Option<i64> {
        self.current.as_int()
    }

    /// Values a user can choose from, if the camera enumerates them.
    pub fn choices(&self) -> Option<&[Value]> {
        if let Some(v) = &self.settable_values {
            if !v.is_empty() {
                return Some(v);
            }
        }
        match &self.form {
            Form::Enum(v) if !v.is_empty() => Some(v),
            _ => None,
        }
    }

    fn parse(r: &mut Reader<'_>, two_enums: bool) -> Result<Self> {
        let code = r.u16()?;
        let data_type = r.u16()?;
        let get_set = r.u8()?;
        let is_enabled = r.u8()?;
        let factory_default = r.value(data_type)?;
        let current = r.value(data_type)?;
        let form_flag = r.u8()?;
        let mut settable_values = None;
        let form = match form_flag {
            0 => Form::None,
            1 => Form::Range {
                min: r.value(data_type)?,
                max: r.value(data_type)?,
                step: r.value(data_type)?,
            },
            2 => {
                let values = read_enum(r, data_type)?;
                if two_enums {
                    settable_values = Some(read_enum(r, data_type)?);
                }
                Form::Enum(values)
            }
            other => {
                return Err(Error::Parse(format!(
                    "property 0x{code:04X}: unknown form flag {other}"
                )))
            }
        };
        Ok(Self {
            code,
            data_type,
            get_set,
            is_enabled,
            factory_default,
            current,
            form,
            settable_values,
        })
    }

    /// Parses the whole 0x9209 payload.
    ///
    /// `two_enums` must be true for protocol versions that append a second
    /// enumeration ("settable values") after the first one.
    pub fn parse_all(buf: &[u8], two_enums: bool) -> Result<Vec<Self>> {
        let mut r = Reader::new(buf);
        let count = r.u64()? as usize;
        if count > 4096 {
            return Err(Error::Parse(format!("implausible property count {count}")));
        }
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let desc = PropDesc::parse(&mut r, two_enums).map_err(|e| {
                Error::Parse(format!("property #{i} of {count} at offset {}: {e}", r.pos()))
            })?;
            out.push(desc);
        }
        Ok(out)
    }
}

/// See [`PropDesc::steppable`].
pub fn is_step_property(code: u16) -> bool {
    use crate::codes::prop;
    matches!(code, prop::F_NUMBER | prop::SONY_SHUTTER_SPEED | prop::SONY_ISO | prop::EXPOSURE_BIAS)
}

fn read_enum(r: &mut Reader<'_>, ty: u16) -> Result<Vec<Value>> {
    let n = r.u16()? as usize;
    (0..n).map(|_| r.value(ty)).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::codes::{dt, prop};

    /// Builds a 0x9209 entry the way the camera serialises it.
    pub fn entry(code: u16, ty: u16, rw: bool, current: i64, values: &[i64]) -> Vec<u8> {
        let enc = |v: i64| crate::data::encode_int(ty, v).unwrap();
        let mut b = Vec::new();
        b.extend_from_slice(&code.to_le_bytes());
        b.extend_from_slice(&ty.to_le_bytes());
        b.push(rw as u8);
        b.push(1);
        b.extend(enc(current));
        b.extend(enc(current));
        if values.is_empty() {
            b.push(0);
        } else {
            b.push(2);
            b.extend_from_slice(&(values.len() as u16).to_le_bytes());
            for v in values {
                b.extend(enc(*v));
            }
        }
        b
    }

    pub fn all_props(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut b = (entries.len() as u64).to_le_bytes().to_vec();
        for e in entries {
            b.extend_from_slice(e);
        }
        b
    }

    #[test]
    fn parses_prop_list() {
        let data = all_props(&[
            entry(prop::F_NUMBER, dt::UINT16, true, 280, &[280, 320, 350]),
            entry(prop::EXPOSURE_BIAS, dt::INT16, true, -300, &[]),
            entry(prop::SONY_BATTERY_LEVEL, dt::INT8, false, 80, &[]),
        ]);
        let props = PropDesc::parse_all(&data, false).unwrap();
        assert_eq!(props.len(), 3);
        assert_eq!(props[0].current, Value::Int(280));
        assert_eq!(props[0].choices().unwrap().len(), 3);
        assert!(props[0].writable());
        assert_eq!(props[1].current, Value::Int(-300));
        assert!(props[1].choices().is_none());
        assert!(!props[2].writable());
    }

    #[test]
    fn parses_range() {
        let mut e = Vec::new();
        e.extend_from_slice(&prop::SONY_COLOR_TEMP.to_le_bytes());
        e.extend_from_slice(&dt::UINT16.to_le_bytes());
        e.extend_from_slice(&[1, 1]);
        for v in [5500u16, 5600, 2500, 9900, 100] {
            e.extend_from_slice(&v.to_le_bytes());
        }
        // Form flag must be inserted after the two values.
        e.insert(10, 1);
        let props = PropDesc::parse_all(&all_props(&[e]), false).unwrap();
        assert_eq!(
            props[0].form,
            Form::Range { min: Value::Int(2500), max: Value::Int(9900), step: Value::Int(100) }
        );
    }

    #[test]
    fn parses_ext_device_info() {
        let mut b = 200u16.to_le_bytes().to_vec();
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&0x5007u16.to_le_bytes());
        b.extend_from_slice(&0xD20Du16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&0xD2C1u16.to_le_bytes());
        let info = ExtDeviceInfo::parse(&b).unwrap();
        assert_eq!(info.version, 200);
        assert_eq!(info.properties, vec![0x5007, 0xD20D]);
        assert_eq!(info.controls, vec![0xD2C1]);
        assert!(info.supports(0xD2C1));
    }

    #[test]
    fn truncated_data_is_an_error() {
        let data = all_props(&[entry(prop::F_NUMBER, dt::UINT16, true, 280, &[280, 320])]);
        assert!(PropDesc::parse_all(&data[..data.len() - 1], false).is_err());
    }
}

#[cfg(test)]
mod fixture_tests {
    use super::*;
    use crate::codes::prop;

    /// Real 0x9209 dump from an ILCE-6300 (fw 2.0) in M mode.
    const A6300_ALL_PROPS: &[u8] = include_bytes!("../../../fixtures/a6300/all_props.bin");

    #[test]
    fn parses_real_a6300_dump() {
        let props = PropDesc::parse_all(A6300_ALL_PROPS, false).unwrap();
        assert_eq!(props.len(), 37);
        let get = |code| props.iter().find(|p| p.code == code).unwrap();

        let mode = get(prop::EXPOSURE_PROGRAM_MODE);
        assert_eq!(mode.current_int(), Some(1)); // M
        assert!(!mode.adjustable());

        for code in [prop::F_NUMBER, prop::SONY_SHUTTER_SPEED, prop::SONY_ISO, prop::EXPOSURE_BIAS] {
            let p = get(code);
            assert!(p.steppable() && p.adjustable(), "0x{code:04X}");
            assert!(!p.writable(), "0x{code:04X} is step-only on this body");
        }
        assert!(get(prop::WHITE_BALANCE).writable());
        assert!(get(prop::SONY_ISO).choices().unwrap().len() > 40);
        assert_eq!(get(prop::SONY_LIVE_VIEW_STATUS).current_int(), Some(1));
    }
}
