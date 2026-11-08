//! Sony PTP ("PC Remote" / SDIO) protocol implementation.
//!
//! The crate is transport-agnostic: anything that can execute a PTP transaction
//! (WPD on Windows, raw USB, a mock in tests) implements [`Transport`].

pub mod camera;
pub mod codes;
pub mod data;
pub mod dataset;
pub mod format;
pub mod liveview;
pub mod transport;

pub use camera::{FrameSink, SonyCamera, StepDirection};
pub use dataset::{DeviceInfo, ExtDeviceInfo, Form, PropDesc, Value};
pub use transport::{DataPhase, Response, Transport, TransportError};

/// Errors produced by the protocol layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("camera answered 0x{code:04X} ({name}) to operation 0x{op:04X}", name = codes::response_name(*code))]
    Response { op: u16, code: u16 },
    #[error("malformed data: {0}")]
    Parse(String),
    #[error("property 0x{0:04X} is not reported by the camera")]
    UnknownProperty(u16),
    #[error("value did not change after setting property 0x{0:04X}")]
    NoEffect(u16),
    #[error("no JPEG frame in live view data")]
    NoFrame,
}

impl Error {
    /// True when the device is gone and the connection has to be re-established.
    pub fn is_disconnect(&self) -> bool {
        matches!(self, Error::Transport(TransportError::Disconnected(_)))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
