//! PTP and Sony vendor constants (names follow libgphoto2's ptp.h).

/// Standard PTP operations.
pub mod op {
    pub const GET_DEVICE_INFO: u16 = 0x1001;
    pub const OPEN_SESSION: u16 = 0x1002;
    pub const GET_OBJECT_INFO: u16 = 0x1008;
    pub const GET_OBJECT: u16 = 0x1009;

    /// SDIO connect handshake, params `(phase, 0, 0)` with phase 1, 2, 3.
    pub const SONY_SDIO_CONNECT: u16 = 0x9201;
    /// Returns protocol version and supported property / control codes.
    pub const SONY_GET_EXT_DEVICE_INFO: u16 = 0x9202;
    pub const SONY_GET_DEVICE_PROP_DESC: u16 = 0x9203;
    pub const SONY_GET_DEVICE_PROP_VALUE: u16 = 0x9204;
    /// Writes an absolute property value.
    pub const SONY_SET_CONTROL_DEVICE_A: u16 = 0x9205;
    pub const SONY_GET_CONTROL_DEVICE_DESC: u16 = 0x9206;
    /// Presses buttons / steps a property up or down.
    pub const SONY_SET_CONTROL_DEVICE_B: u16 = 0x9207;
    /// Dumps every property descriptor including current values.
    pub const SONY_GET_ALL_EXT_DEVICE_PROP_INFO: u16 = 0x9209;
}

/// PTP response codes.
pub mod rc {
    pub const OK: u16 = 0x2001;
    pub const GENERAL_ERROR: u16 = 0x2002;
    pub const SESSION_NOT_OPEN: u16 = 0x2003;
    pub const OPERATION_NOT_SUPPORTED: u16 = 0x2005;
    pub const PARAMETER_NOT_SUPPORTED: u16 = 0x2006;
    pub const INVALID_OBJECT_HANDLE: u16 = 0x2009;
    pub const DEVICE_PROP_NOT_SUPPORTED: u16 = 0x200A;
    pub const ACCESS_DENIED: u16 = 0x200F;
    pub const DEVICE_BUSY: u16 = 0x2019;
    pub const INVALID_DEVICE_PROP_VALUE: u16 = 0x201C;
    pub const SESSION_ALREADY_OPEN: u16 = 0x201E;
}

pub fn response_name(code: u16) -> &'static str {
    match code {
        rc::OK => "OK",
        rc::GENERAL_ERROR => "GeneralError",
        rc::SESSION_NOT_OPEN => "SessionNotOpen",
        rc::OPERATION_NOT_SUPPORTED => "OperationNotSupported",
        rc::PARAMETER_NOT_SUPPORTED => "ParameterNotSupported",
        rc::INVALID_OBJECT_HANDLE => "InvalidObjectHandle",
        rc::DEVICE_PROP_NOT_SUPPORTED => "DevicePropNotSupported",
        rc::ACCESS_DENIED => "AccessDenied",
        rc::DEVICE_BUSY => "DeviceBusy",
        rc::INVALID_DEVICE_PROP_VALUE => "InvalidDevicePropValue",
        rc::SESSION_ALREADY_OPEN => "SessionAlreadyOpen",
        _ => "?",
    }
}

/// Object handles with special meaning on Sony cameras.
pub mod handle {
    /// Last captured still image.
    pub const CAPTURED_IMAGE: u32 = 0xFFFF_C001;
    /// Live view JPEG frame.
    pub const LIVE_VIEW: u32 = 0xFFFF_C002;
}

/// PTP data type codes.
pub mod dt {
    pub const UNDEF: u16 = 0x0000;
    pub const INT8: u16 = 0x0001;
    pub const UINT8: u16 = 0x0002;
    pub const INT16: u16 = 0x0003;
    pub const UINT16: u16 = 0x0004;
    pub const INT32: u16 = 0x0005;
    pub const UINT32: u16 = 0x0006;
    pub const INT64: u16 = 0x0007;
    pub const UINT64: u16 = 0x0008;
    pub const INT128: u16 = 0x0009;
    pub const UINT128: u16 = 0x000A;
    pub const ARRAY_MASK: u16 = 0x4000;
    pub const STR: u16 = 0xFFFF;
}

/// Device property codes.
pub mod prop {
    // Standard PTP.
    pub const BATTERY_LEVEL: u16 = 0x5001;
    pub const WHITE_BALANCE: u16 = 0x5005;
    pub const F_NUMBER: u16 = 0x5007;
    pub const FOCAL_LENGTH: u16 = 0x5008;
    pub const FOCUS_MODE: u16 = 0x500A;
    pub const METERING_MODE: u16 = 0x500B;
    pub const FLASH_MODE: u16 = 0x500C;
    pub const EXPOSURE_PROGRAM_MODE: u16 = 0x500E;
    pub const EXPOSURE_BIAS: u16 = 0x5010;
    pub const STILL_CAPTURE_MODE: u16 = 0x5013;
    pub const FOCUS_METERING_MODE: u16 = 0x501C;

    // Sony.
    pub const SONY_DRO_HDR: u16 = 0xD201;
    pub const SONY_IMAGE_SIZE: u16 = 0xD203;
    pub const SONY_SHUTTER_SPEED: u16 = 0xD20D;
    pub const SONY_COLOR_TEMP: u16 = 0xD20F;
    pub const SONY_ASPECT_RATIO: u16 = 0xD211;
    pub const SONY_FOCUS_FOUND: u16 = 0xD213;
    pub const SONY_OBJECT_IN_MEMORY: u16 = 0xD215;
    pub const SONY_BATTERY_LEVEL: u16 = 0xD218;
    pub const SONY_PICTURE_EFFECT: u16 = 0xD21B;
    pub const SONY_ISO: u16 = 0xD21E;
    pub const SONY_LIVE_VIEW_STATUS: u16 = 0xD221;
    pub const SONY_FOCUS_AREA: u16 = 0xD22C;

    // Sony controls (written through SetControlDeviceB).
    /// Shutter half press (S1): 2 = down, 1 = up.
    pub const SONY_S1: u16 = 0xD2C1;
    /// Shutter full press (S2): 2 = down, 1 = up.
    pub const SONY_S2: u16 = 0xD2C2;
    pub const SONY_AEL: u16 = 0xD2C3;
    pub const SONY_MOVIE_REC: u16 = 0xD2C8;
    /// Manual focus step, signed: negative = near, positive = far.
    pub const SONY_NEAR_FAR: u16 = 0xD2D1;
}

/// Human readable property name.
pub fn prop_name(code: u16) -> Option<&'static str> {
    use prop::*;
    Some(match code {
        BATTERY_LEVEL => "Battery level",
        WHITE_BALANCE => "White balance",
        F_NUMBER => "Aperture",
        FOCAL_LENGTH => "Focal length",
        FOCUS_MODE => "Focus mode",
        METERING_MODE => "Metering mode",
        FLASH_MODE => "Flash mode",
        EXPOSURE_PROGRAM_MODE => "Exposure mode",
        EXPOSURE_BIAS => "Exposure compensation",
        STILL_CAPTURE_MODE => "Drive mode",
        FOCUS_METERING_MODE => "Focus area",
        SONY_DRO_HDR => "DRO / HDR",
        SONY_IMAGE_SIZE => "Image size",
        SONY_SHUTTER_SPEED => "Shutter speed",
        SONY_COLOR_TEMP => "Color temperature",
        SONY_ASPECT_RATIO => "Aspect ratio",
        SONY_FOCUS_FOUND => "Focus indication",
        SONY_OBJECT_IN_MEMORY => "Objects in memory",
        SONY_BATTERY_LEVEL => "Battery",
        SONY_PICTURE_EFFECT => "Picture effect",
        SONY_ISO => "ISO",
        SONY_LIVE_VIEW_STATUS => "Live view status",
        SONY_FOCUS_AREA => "Focus area",
        SONY_S1 => "Half press (AF)",
        SONY_S2 => "Shutter release",
        SONY_AEL => "AE lock",
        SONY_MOVIE_REC => "Movie record",
        SONY_NEAR_FAR => "Manual focus step",
        _ => return None,
    })
}
