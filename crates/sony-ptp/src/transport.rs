/// Direction of the optional data phase of a PTP transaction.
#[derive(Debug, Clone, Copy)]
pub enum DataPhase<'a> {
    /// No data phase.
    None,
    /// Host → device.
    Out(&'a [u8]),
    /// Device → host.
    In,
}

/// Result of a PTP transaction.
#[derive(Debug, Clone, Default)]
pub struct Response {
    pub code: u16,
    pub params: Vec<u32>,
    /// Data received in the device → host phase (empty otherwise).
    pub data: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The device has been unplugged, switched off or grabbed by someone else.
    #[error("device disconnected: {0}")]
    Disconnected(String),
    #[error("transport error: {0}")]
    Other(String),
}

/// Something that can run a single PTP transaction inside an open session.
pub trait Transport {
    fn execute(
        &mut self,
        op: u16,
        params: &[u32],
        phase: DataPhase<'_>,
    ) -> Result<Response, TransportError>;
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn execute(
        &mut self,
        op: u16,
        params: &[u32],
        phase: DataPhase<'_>,
    ) -> Result<Response, TransportError> {
        (**self).execute(op, params, phase)
    }
}
