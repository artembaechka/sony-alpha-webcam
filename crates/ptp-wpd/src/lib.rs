//! PTP transport over Windows Portable Devices.
//!
//! Sony cameras in "PC Remote" USB mode are bound by Windows to the inbox MTP
//! driver (`wpdmtp.inf`). That driver forwards raw PTP transactions, including
//! Sony vendor operations, through the `WPD_COMMAND_MTP_EXT_*` commands, so no
//! extra driver (WinUSB / libusb) is required.

#[cfg(windows)]
mod wpd;

#[cfg(windows)]
pub use wpd::{list_devices, ComGuard, WpdTransport};

/// A portable device known to Windows.
#[derive(Debug, Clone)]
pub struct DeviceEntry {
    /// PnP device interface path, used to open the device.
    pub id: String,
    pub friendly_name: String,
    pub description: String,
    pub manufacturer: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
}

pub const SONY_VID: u16 = 0x054C;

impl DeviceEntry {
    pub fn is_sony(&self) -> bool {
        self.vid == Some(SONY_VID)
    }

    pub fn display_name(&self) -> &str {
        if !self.friendly_name.is_empty() {
            &self.friendly_name
        } else if !self.description.is_empty() {
            &self.description
        } else {
            &self.id
        }
    }
}

/// Extracts `vid_XXXX` / `pid_XXXX` from a PnP device path.
pub fn parse_vid_pid(id: &str) -> (Option<u16>, Option<u16>) {
    let lower = id.to_ascii_lowercase();
    let grab = |tag: &str| {
        let pos = lower.find(tag)? + tag.len();
        u16::from_str_radix(lower.get(pos..pos + 4)?, 16).ok()
    };
    (grab("vid_"), grab("pid_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vid_pid() {
        let id = r"\\?\usb#vid_054c&pid_079c#ca2e203a43ec#{6ac27878-a6fa-4155-ba85-f98f491d4f33}";
        assert_eq!(parse_vid_pid(id), (Some(0x054C), Some(0x079C)));
        assert_eq!(parse_vid_pid("nothing"), (None, None));
    }
}
