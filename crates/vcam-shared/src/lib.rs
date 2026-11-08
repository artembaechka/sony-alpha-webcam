//! Shared definitions between the `sony-remote` application (frame writer)
//! and the virtual camera media source DLL (frame reader).
//!
//! The DLL is loaded by the Windows Camera Frame Server service (session 0,
//! LocalService), so the frames travel through a named `Global\` file
//! mapping. The DLL creates it; the application opens it and writes NV12
//! frames into a triple buffer guarded by per-slot sequence counters.

pub mod convert;
#[cfg(windows)]
pub mod registry;
#[cfg(windows)]
pub mod shm;

/// CLSID of the media source (`IMFActivate`) implemented by the DLL.
pub const CLSID_STR: &str = "{691670D2-1780-43DB-99E2-A2AE6DB1BC05}";
pub const CLSID_U128: u128 = 0x691670d2_1780_43db_99e2_a2ae6db1bc05;

/// Name under which the virtual camera shows up in applications.
pub const FRIENDLY_NAME: &str = "Sony Camera (sony-remote)";
/// File name of the media source DLL.
pub const DLL_NAME: &str = "sony_remote_vcam.dll";

pub const MAPPING_NAME: &str = "Global\\SonyRemoteVirtualCamera";
pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
pub const FPS: u32 = 30;
pub const FRAME_BYTES: usize = (WIDTH * HEIGHT * 3 / 2) as usize;
pub const SLOTS: usize = 3;
pub const MAGIC: u32 = u32::from_le_bytes(*b"SRVC");
pub const VERSION: u32 = 1;
/// Offset of the first frame slot (keeps frames page aligned).
pub const HEADER_BYTES: usize = 4096;
pub const MAPPING_BYTES: usize = HEADER_BYTES + SLOTS * FRAME_BYTES;

/// Frames older than this are considered stale: the DLL shows a placeholder.
pub const WRITER_TIMEOUT_MS: u64 = 1500;
/// The app only converts frames while the DLL has delivered one recently.
pub const READER_TIMEOUT_MS: u64 = 2000;

use std::sync::atomic::{AtomicU32, AtomicU64};

/// Layout of the start of the mapping.
#[repr(C)]
pub struct Header {
    pub magic: AtomicU32,
    pub version: AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
    /// Slot index + 1 of the newest complete frame, 0 = none yet.
    pub latest: AtomicU32,
    pub _reserved: AtomicU32,
    pub frame_counter: AtomicU64,
    /// `GetTickCount64()` of the last frame written by the application.
    pub writer_tick: AtomicU64,
    /// `GetTickCount64()` of the last sample delivered by the DLL.
    pub reader_tick: AtomicU64,
    /// Seqlock counters: odd while the slot is being written.
    pub slot_seq: [AtomicU64; SLOTS],
}

const _: () = assert!(std::mem::size_of::<Header>() <= HEADER_BYTES);
