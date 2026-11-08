//! Human readable formatting of Sony property values, plus an ordering key
//! used to decide in which direction to step a property.

use crate::codes::prop;
use crate::dataset::Value;

/// ISO value meaning "AUTO" (lower 24 bits).
pub const ISO_AUTO: i64 = 0x00FF_FFFF;

pub fn format_value(code: u16, value: &Value) -> String {
    let v = match value {
        Value::Int(v) => *v,
        Value::Str(s) => return s.clone(),
        Value::Array(a) => return format!("{a:?}"),
    };
    match code {
        prop::F_NUMBER => format_f_number(v),
        prop::SONY_SHUTTER_SPEED => format_shutter_speed(v),
        prop::SONY_ISO => format_iso(v),
        prop::EXPOSURE_BIAS => format_ev(v),
        prop::WHITE_BALANCE => white_balance_name(v).map_or_else(|| hex(v), str::to_owned),
        prop::FOCUS_MODE => focus_mode_name(v).map_or_else(|| hex(v), str::to_owned),
        prop::EXPOSURE_PROGRAM_MODE => exposure_mode_name(v).map_or_else(|| hex(v), str::to_owned),
        prop::SONY_COLOR_TEMP => format!("{v} K"),
        prop::FOCAL_LENGTH => format!("{} mm", v / 100),
        prop::SONY_BATTERY_LEVEL | prop::BATTERY_LEVEL => {
            if v < 0 {
                "—".into()
            } else {
                format!("{v}%")
            }
        }
        prop::SONY_FOCUS_FOUND => match v {
            1 => "—".into(),
            2 => "Focused".into(),
            3 => "Not focused".into(),
            4 => "Tracking".into(),
            _ => hex(v),
        },
        _ => v.to_string(),
    }
}

fn hex(v: i64) -> String {
    format!("0x{v:X}")
}

pub fn format_f_number(v: i64) -> String {
    if v <= 0 || v == 0xFFFE || v == 0xFFFF {
        return "—".into();
    }
    if v % 100 == 0 {
        format!("f/{}", v / 100)
    } else if v % 10 == 0 {
        format!("f/{}.{}", v / 100, (v % 100) / 10)
    } else {
        format!("f/{:.2}", v as f64 / 100.0)
    }
}

/// Shutter speed is `numerator << 16 | denominator`; 0 means BULB.
pub fn format_shutter_speed(v: i64) -> String {
    if v == 0 {
        return "BULB".into();
    }
    let num = (v >> 16) & 0xFFFF;
    let den = v & 0xFFFF;
    if num == 0 || den == 0 {
        return hex(v);
    }
    if num == 1 && den > 1 {
        format!("1/{den}")
    } else if num % den == 0 {
        format!("{}\"", num / den)
    } else {
        format!("{:.1}\"", num as f64 / den as f64)
    }
}

/// Exposure time in seconds (BULB = infinity).
pub fn shutter_seconds(v: i64) -> f64 {
    if v == 0 {
        return f64::INFINITY;
    }
    let num = ((v >> 16) & 0xFFFF) as f64;
    let den = (v & 0xFFFF) as f64;
    if den == 0.0 {
        f64::INFINITY
    } else {
        num / den
    }
}

pub fn format_iso(v: i64) -> String {
    let base = v & 0x00FF_FFFF;
    let mut s = if base == ISO_AUTO { "AUTO".to_string() } else { base.to_string() };
    match (v >> 24) & 0xFF {
        0 => {}
        0x01 => s.push_str(" (MFNR)"),
        0x02 => s.push_str(" (MFNR High)"),
        other => s.push_str(&format!(" (0x{other:X})")),
    }
    s
}

/// Exposure compensation is stored in 1/1000 EV.
pub fn format_ev(v: i64) -> String {
    if v == 0 {
        return "±0.0 EV".into();
    }
    let ev = v as f64 / 1000.0;
    format!("{ev:+.1} EV")
}

pub fn white_balance_name(v: i64) -> Option<&'static str> {
    Some(match v {
        0x0002 => "Auto",
        0x0004 => "Daylight",
        0x8011 => "Shade",
        0x8010 => "Cloudy",
        0x0006 => "Incandescent",
        0x8001 => "Fluorescent: warm white",
        0x8002 => "Fluorescent: cool white",
        0x8003 => "Fluorescent: day white",
        0x8004 => "Fluorescent: daylight",
        0x0007 => "Flash",
        0x8030 => "Underwater auto",
        0x8012 => "Color temp / filter",
        0x8020 => "Custom 1",
        0x8021 => "Custom 2",
        0x8022 => "Custom 3",
        0x8023 => "Custom",
        _ => return None,
    })
}

pub fn focus_mode_name(v: i64) -> Option<&'static str> {
    Some(match v {
        0x0001 => "MF",
        0x0002 => "AF-S",
        0x8004 => "AF-C",
        0x8005 => "AF-A",
        0x8006 => "DMF",
        _ => return None,
    })
}

pub fn exposure_mode_name(v: i64) -> Option<&'static str> {
    Some(match v {
        0x0001 => "M",
        0x0002 => "P",
        0x0003 => "A",
        0x0004 => "S",
        0x8000 => "Intelligent Auto",
        0x8001 => "Superior Auto",
        0x8008 => "Sweep panorama",
        0x8011 => "Portrait",
        0x8012 => "Sport",
        0x8014 => "Macro",
        0x8013 => "Sunset",
        0x8015 => "Landscape",
        0x8016 => "Night scene",
        0x8017 => "Hand-held twilight",
        0x8018 => "Night portrait",
        0x8019 => "Anti motion blur",
        0x8050 => "Movie P",
        0x8051 => "Movie A",
        0x8052 => "Movie S",
        0x8053 => "Movie M",
        0x8054 => "Movie Auto",
        0x8084 => "HFR P",
        0x8085 => "HFR A",
        0x8086 => "HFR S",
        0x8087 => "HFR M",
        _ => return None,
    })
}

/// Monotonic key of a value in the physical sense of the property.
/// Stepping "up" with SetControlDeviceB moves this key in one direction.
pub fn order_key(code: u16, v: i64) -> f64 {
    match code {
        prop::SONY_SHUTTER_SPEED => shutter_seconds(v),
        prop::SONY_ISO => {
            let base = v & 0x00FF_FFFF;
            let flags = ((v >> 24) & 0xFF) as f64;
            let b = if base == ISO_AUTO { -1.0 } else { base as f64 };
            flags * 1e9 + b
        }
        _ => v as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f_numbers() {
        assert_eq!(format_f_number(280), "f/2.8");
        assert_eq!(format_f_number(1100), "f/11");
        assert_eq!(format_f_number(350), "f/3.5");
    }

    #[test]
    fn shutter() {
        assert_eq!(format_shutter_speed(0x0001_00FA), "1/250");
        assert_eq!(format_shutter_speed(0x012C_000A), "30\"");
        assert_eq!(format_shutter_speed(0x000D_000A), "1.3\"");
        assert_eq!(format_shutter_speed(0), "BULB");
        assert!(shutter_seconds(0x0001_00FA) < shutter_seconds(0x0001_0019));
    }

    #[test]
    fn iso_and_ev() {
        assert_eq!(format_iso(0x00FF_FFFF), "AUTO");
        assert_eq!(format_iso(800), "800");
        assert_eq!(format_iso(0x0100_0C80), "3200 (MFNR)");
        assert_eq!(format_ev(-1300), "-1.3 EV");
        assert_eq!(format_ev(700), "+0.7 EV");
        assert!(order_key(prop::SONY_ISO, ISO_AUTO) < order_key(prop::SONY_ISO, 100));
    }
}
